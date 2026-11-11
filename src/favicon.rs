//! Small, shared tab icons obtained from WebView2, never from a third-party service.
use base64::Engine;
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io::Cursor,
    rc::{Rc, Weak},
};
use webview2_com::{
    take_pwstr, FaviconChangedEventHandler, GetFaviconCompletedHandler,
    Microsoft::Web::WebView2::Win32::*, NavigationStartingEventHandler,
};
use windows_core::{Interface, PWSTR};
use wry::WebViewExtWindows;
pub struct Icon {
    pub key: String,
    pub data: String,
}
#[derive(Default)]
pub struct Cache {
    items: HashMap<String, Weak<Icon>>,
}
pub type SharedCache = Rc<RefCell<Cache>>;
pub struct State {
    image: RefCell<Option<Rc<Icon>>>,
    sequence: Cell<u64>,
}
pub type Shared = Rc<State>;
impl State {
    pub fn new(image: Option<Rc<Icon>>) -> Shared {
        Rc::new(Self {
            image: RefCell::new(image),
            sequence: Cell::new(0),
        })
    }
    pub fn get(&self) -> Option<Rc<Icon>> {
        self.image.borrow().clone()
    }
    pub fn clear(&self) {
        self.image.borrow_mut().take();
    }
    fn invalidate(&self) -> u64 {
        let seq = self.sequence.get().wrapping_add(1);
        self.sequence.set(seq);
        seq
    }
}
fn normalize(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() > 65536
        || bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n")
        || bytes.get(12..16) != Some(b"IHDR")
    {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    if width == 0 || height == 0 || width > 256 || height > 256 {
        return None;
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(256);
    limits.max_image_height = Some(256);
    limits.max_alloc = Some(1024 * 1024);
    reader.limits(limits);
    let image = image::DynamicImage::ImageRgba8(
        reader
            .decode()
            .ok()?
            .resize_exact(32, 32, image::imageops::FilterType::Triangle)
            .to_rgba8(),
    );
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    let png = png.into_inner();
    (png.len() <= 8192).then_some(png)
}
impl Cache {
    fn get(&mut self, bytes: &[u8]) -> Option<Rc<Icon>> {
        let png = normalize(bytes)?;
        let key = format!("{:x}", Sha256::digest(&png));
        if let Some(icon) = self.items.get(&key).and_then(Weak::upgrade) {
            return Some(icon);
        }
        self.items.retain(|_, v| v.strong_count() > 0);
        if self.items.len() >= 256 {
            if let Some(key) = self.items.keys().next().cloned() {
                self.items.remove(&key);
            }
        }
        let icon = Rc::new(Icon {
            key: key.clone(),
            data: format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            ),
        });
        self.items.insert(key, Rc::downgrade(&icon));
        Some(icon)
    }
}
unsafe fn read_icon(stream: &windows::Win32::System::Com::IStream) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    stream
        .Seek(0, windows::Win32::System::Com::STREAM_SEEK_SET, None)
        .ok()?;
    loop {
        let mut count = 0;
        stream
            .Read(
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                Some(&mut count),
            )
            .ok()
            .ok()?;
        if count == 0 {
            break;
        }
        if bytes.len() + count as usize > 65536 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count as usize]);
    }
    Some(bytes)
}
pub fn install(
    wv: &wry::WebView,
    state: &Shared,
    cache: &SharedCache,
    changed: impl Fn() + 'static,
) -> windows_core::Result<()> {
    let state = Rc::downgrade(state);
    let cache = Rc::downgrade(cache);
    let changed = Rc::new(changed);
    unsafe {
        let core = wv.controller().CoreWebView2()?;
        let view = core.cast::<ICoreWebView2_15>()?;
        let weak = state.clone();
        let mut navigation = Default::default();
        core.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, _| {
                if let Some(s) = weak.upgrade() {
                    s.invalidate();
                }
                Ok(())
            })),
            &mut navigation,
        )?;
        let mut token = Default::default();
        view.add_FaviconChanged(
            &FaviconChangedEventHandler::create(Box::new(move |sender, _| {
                let (Some(core), Some(s)) = (sender, state.upgrade()) else {
                    return Ok(());
                };
                let seq = s.invalidate();
                let view = core.cast::<ICoreWebView2_15>()?;
                let mut uri = PWSTR::null();
                view.FaviconUri(&mut uri)?;
                let uri = take_pwstr(uri);
                if uri.is_empty() {
                    s.clear();
                    changed();
                    return Ok(());
                }
                let state = state.clone();
                let cache = cache.clone();
                let changed = changed.clone();
                view.GetFavicon(
                    COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG,
                    &GetFaviconCompletedHandler::create(Box::new(move |result, stream| {
                        let (Some(s), Some(cache)) = (state.upgrade(), cache.upgrade()) else {
                            return Ok(());
                        };
                        if s.sequence.get() != seq {
                            return Ok(());
                        }
                        let icon = if result.is_ok() {
                            stream
                                .and_then(|v| read_icon(&v))
                                .and_then(|bytes| cache.borrow_mut().get(&bytes))
                        } else {
                            None
                        };
                        let different =
                            s.get().as_ref().map(|i| &i.key) != icon.as_ref().map(|i| &i.key);
                        if different {
                            *s.image.borrow_mut() = icon;
                            changed();
                        }
                        Ok(())
                    })),
                )?;
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn png() -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(64, 64)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }
    #[test]
    fn icons_are_bounded_and_shared() {
        let mut cache = Cache::default();
        let a = cache.get(&png()).unwrap();
        let b = cache.get(&png()).unwrap();
        assert!(Rc::ptr_eq(&a, &b));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(a.data.split_once(',').unwrap().1)
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (32, 32));
        let weak = Rc::downgrade(&a);
        drop(a);
        drop(b);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn invalid_and_oversized_images_are_rejected() {
        assert!(normalize(b"not PNG").is_none());
        assert!(normalize(&vec![0; 65537]).is_none());
        let mut bytes = png();
        bytes[16..20].copy_from_slice(&100000u32.to_be_bytes());
        assert!(normalize(&bytes).is_none());
    }
}
