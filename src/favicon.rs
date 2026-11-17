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
    Microsoft::Web::WebView2::Win32::*, NavigationStartingEventHandler, WebMessageReceivedEventHandler,
};
use windows_core::{Interface, PWSTR};
use wry::{WebView, WebViewExtWindows};
pub struct Icon {
    pub key: String,
    pub data: String,
    pub size: u32,
}
#[derive(Default)]
pub struct Cache {
    items: HashMap<String, Weak<Icon>>,
}
pub type SharedCache = Rc<RefCell<Cache>>;
pub struct State {
    image: RefCell<Option<Rc<Icon>>>,
    detailed: RefCell<Option<(url::Origin, Rc<Icon>)>>,
    request: RefCell<Option<(url::Origin, String)>>,
    requested: Cell<bool>,
    sequence: Cell<u64>,
}
pub type Shared = Rc<State>;
impl State {
    pub fn new(image: Option<Rc<Icon>>) -> Shared {
        Rc::new(Self {
            image: RefCell::new(image),
            detailed: RefCell::new(None),
            request: RefCell::new(None),
            requested: Cell::new(false),
            sequence: Cell::new(0),
        })
    }
    pub fn get(&self) -> Option<Rc<Icon>> {
        self.image.borrow().clone()
    }
    pub fn favorite_icon(&self, origin: &url::Origin) -> Option<Rc<Icon>> {
        let regular = self.get();
        self.detailed.borrow().as_ref().filter(|(source, _)| source == origin)
            .filter(|(_, image)| regular.as_ref().is_none_or(|base| image.size > base.size))
            .map(|(_, image)| image.clone()).or(regular)
    }

    pub fn request_detailed(&self, view: &WebView, origin: url::Origin) {
        if self.requested.get() || self.get().is_some_and(|icon| icon.size >= ICON_SIZE) { return; }
        if !view.url().ok().and_then(|raw| url::Url::parse(&raw).ok()).is_some_and(|url| url.origin() == origin) { return; }
        let nonce = crate::security::newtab_token();
        let script = format!("{}({})", include_str!("favorite-icon.js"), serde_json::to_string(&nonce).unwrap());
        *self.request.borrow_mut() = Some((origin, nonce));
        self.requested.set(true);
        if view.evaluate_script(&script).is_err() { self.request.borrow_mut().take(); }
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
const ICON_SIZE: u32 = 64;
const MAX_PNG_BYTES: usize = 10 * 1024;

fn png_size(bytes: &[u8]) -> Option<u32> {
    if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16) != Some(b"IHDR") { return None; }
    let w = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    (w > 0 && h > 0).then_some(w.max(h))
}

pub fn saved_icon_size(data: &str) -> Option<u32> {
    let prefix = data.strip_prefix("data:image/png;base64,")?.get(..32)?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(prefix).ok()?;
    png_size(&bytes)
}

pub fn valid_tile_icon(icon: &str) -> bool {
    let Some(encoded) = icon.strip_prefix("data:image/png;base64,") else { return false; };
    if encoded.len() > MAX_PNG_BYTES.div_ceil(3) * 4 { return false; }
    base64::engine::general_purpose::STANDARD.decode(encoded).is_ok_and(|png| {
        if png.len() > MAX_PNG_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n")
            || png.get(12..16) != Some(b"IHDR")
        {
            return false;
        }
        png.get(16..24).is_some_and(|dimensions| {
            dimensions.chunks_exact(4).all(|dimension| {
                let size = u32::from_be_bytes(dimension.try_into().unwrap());
                (1..=ICON_SIZE).contains(&size)
            })
        })
    })
}

pub fn normalize_saved_icon(icon: &str) -> Option<String> {
    if !valid_tile_icon(icon) { return None; }
    let bytes = base64::engine::general_purpose::STANDARD.decode(icon.split_once(',')?.1).ok()?;
    let bytes = normalize(&bytes)?;
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
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
    let image = reader.decode().ok()?.into_rgba8();
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    let first = image.get_pixel(0, 0);
    let mut varied = false;
    for (x, y, pixel) in image.enumerate_pixels() {
        varied |= pixel != first;
        if pixel[3] > 8 {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    if !varied || right <= left || bottom <= top { return None; }
    if left == 0 && top == 0 && right == width && bottom == height &&
        width <= ICON_SIZE && height <= ICON_SIZE && bytes.len() <= MAX_PNG_BYTES
    {
        return Some(bytes.to_vec());
    }
    let cropped = image::imageops::crop_imm(&image, left, top, right - left, bottom - top).to_image();
    if cropped.pixels().all(|pixel| pixel == cropped.get_pixel(0, 0)) { return None; }
    let cropped = image::DynamicImage::ImageRgba8(cropped);
    for size in [ICON_SIZE, 48] {
        let image = if cropped.width().max(cropped.height()) > size {
            cropped.resize(size, size, image::imageops::FilterType::Lanczos3)
        } else {
            cropped.clone()
        };
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).ok()?;
        if png.get_ref().len() <= MAX_PNG_BYTES { return Some(png.into_inner()); }
    }
    None
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
            size: png_size(&png)?,
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
                    s.detailed.borrow_mut().take();
                    s.request.borrow_mut().take();
                    s.requested.set(false);
                }
                Ok(())
            })),
            &mut navigation,
        )?;
        let detail_state = state.clone();
        let detail_cache = cache.clone();
        let detail_changed = changed.clone();
        core.add_WebMessageReceived(&WebMessageReceivedEventHandler::create(Box::new(move |sender, args| {
            let (Some(core), Some(args), Some(state), Some(cache)) =
                (sender, args, detail_state.upgrade(), detail_cache.upgrade()) else { return Ok(()); };
            if state.request.borrow().is_none() { return Ok(()); }
            let mut message = PWSTR::null();
            if args.TryGetWebMessageAsString(&mut message).is_err() { return Ok(()); }
            let message = take_pwstr(message);
            if message.len() > 90_000 { return Ok(()); }
            let Some((nonce, data)) = message.strip_prefix("lumen-favorite-icon:").and_then(|s| s.split_once(':')) else { return Ok(()); };
            let Some(data) = data.strip_prefix("data:image/png;base64,") else { return Ok(()); };
            let mut source = PWSTR::null();
            args.Source(&mut source)?;
            let mut current = PWSTR::null();
            core.Source(&mut current)?;
            let source = take_pwstr(source);
            let current = take_pwstr(current);
            if source != current { return Ok(()); }
            let Ok(url) = url::Url::parse(&source) else { return Ok(()); };
            let origin = url.origin();
            if !state.request.borrow().as_ref().is_some_and(|(expected, token)| *expected == origin && token == nonce) { return Ok(()); }
            state.request.borrow_mut().take();
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else { return Ok(()); };
            let Some(icon) = cache.borrow_mut().get(&bytes) else { return Ok(()); };
            if icon.size >= 32 && state.get().is_none_or(|old| icon.size > old.size) {
                *state.detailed.borrow_mut() = Some((origin, icon));
                detail_changed();
            }
            Ok(())
        })), &mut Default::default())?;
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
    fn png_with_size(width: u32, height: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut bitmap = image::RgbaImage::new(width, height);
        bitmap.put_pixel(0, 0, image::Rgba([255, 255, 255, 255]));
        bitmap.put_pixel(width - 1, height - 1, image::Rgba([0, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(bitmap)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }
    #[test]
    fn icons_are_bounded_and_shared() {
        let mut cache = Cache::default();
        let a = cache.get(&png_with_size(96, 96)).unwrap();
        let b = cache.get(&png_with_size(96, 96)).unwrap();
        assert!(Rc::ptr_eq(&a, &b));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(a.data.split_once(',').unwrap().1)
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (64, 64));
        assert!(valid_tile_icon(&a.data));
        let mut legacy = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(32, 32)
            .write_to(&mut legacy, image::ImageFormat::Png)
            .unwrap();
        let legacy = base64::engine::general_purpose::STANDARD.encode(legacy.into_inner());
        assert!(valid_tile_icon(&format!("data:image/png;base64,{legacy}")));
        let weak = Rc::downgrade(&a);
        drop(a);
        drop(b);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn small_icons_keep_their_native_dimensions() {
        let png = normalize(&png_with_size(16, 24)).unwrap();
        let image = image::load_from_memory(&png).unwrap();
        assert_eq!((image.width(), image.height()), (16, 24));
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        assert!(valid_tile_icon(&format!("data:image/png;base64,{encoded}")));
    }

    #[test]
    fn invalid_and_oversized_images_are_rejected() {
        assert!(normalize(b"not PNG").is_none());
        assert!(normalize(&vec![0; 65537]).is_none());
        let mut bytes = png_with_size(64, 64);
        bytes[16..20].copy_from_slice(&100000u32.to_be_bytes());
        assert!(normalize(&bytes).is_none());
    }

    #[test]
    fn saved_icons_trim_transparent_padding_and_reject_empty_tiles() {
        let mut pixels = image::RgbaImage::new(32, 32);
        let encode = |pixels: image::RgbaImage| {
            let mut out = Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(pixels).write_to(&mut out, image::ImageFormat::Png).unwrap();
            format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(out.into_inner()))
        };
        assert!(normalize_saved_icon(&encode(pixels.clone())).is_none());
        assert!(normalize_saved_icon(&encode(image::RgbaImage::from_pixel(32, 32, image::Rgba([255; 4])))).is_none());
        for y in 8..24 {
            for x in 8..24 { pixels.put_pixel(x, y, image::Rgba([255; 4])); }
        }
        pixels.put_pixel(8, 8, image::Rgba([0; 4]));
        let icon = normalize_saved_icon(&encode(pixels)).unwrap();
        let raw = base64::engine::general_purpose::STANDARD.decode(icon.split_once(',').unwrap().1).unwrap();
        let image = image::load_from_memory(&raw).unwrap();
        assert_eq!((image.width(), image.height()), (16, 16));
    }
}
