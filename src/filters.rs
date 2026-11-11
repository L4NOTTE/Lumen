//! Усиленный блокировщик: движок Brave `adblock` (синтаксис EasyList / uBlock / AdGuard).
//! Списки скачиваются в фоне и кэшируются на диске.

use std::{collections::HashSet, fs, path::Path, time::Duration};

use adblock::{
    lists::{FilterSet, ParseOptions},
    request::Request,
    Engine,
};

/// Имя файла и запасные адреса списка (пробуем по очереди).
/// RU AdList и AdGuard Russian нужны для российских сайтов (Яндекс, VK, mail.ru и др.).
pub const LISTS: &[(&str, &[&str])] = &[
    (
        "easylist",
        &[
            "https://easylist.to/easylist/easylist.txt",
            "https://raw.githubusercontent.com/easylist/easylist/gh-pages/easylist.txt",
        ],
    ),
    (
        "ruadlist",
        &[
            "https://easylist-downloads.adblockplus.org/advblock.txt",
            "https://raw.githubusercontent.com/easylist/ruadlist/master/advblock.txt",
        ],
    ),
    (
        "adguard-ru",
        &[
            "https://filters.adtidy.org/extension/ublock/filters/1.txt",
            "https://raw.githubusercontent.com/AdguardTeam/FiltersRegistry/master/filters/filter_1_Russian/filter.txt",
        ],
    ),
    (
        "ublock",
        &[
            "https://ublockorigin.github.io/uAssets/filters/filters.txt",
            "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters.txt",
        ],
    ),
];

const MAX_LIST: usize = 8 * 1024 * 1024;
const MAX_COMBINED: u64 = 32 * 1024 * 1024;
const REFRESH_AFTER: Duration = Duration::from_secs(3 * 24 * 3600);

pub struct Filters {
    engine: Engine,
    pub rules: usize,
}

pub struct Cosmetic {
    pub hide: HashSet<String>,
    pub procedural: HashSet<String>,
    pub exceptions: HashSet<String>,
    pub generichide: bool,
}

impl Filters {
    /// Собирает движок из скачанных списков и пользовательских `*.txt`.
    pub fn build(lists_dir: &Path, user_dir: &Path) -> Option<Filters> {
        let mut set = FilterSet::new(false);
        let mut rules = 0usize;
        let mut any = false;
        let mut combined = 0u64;
        let mut add = |path: &Path| {
            let Ok(size) = fs::metadata(path).map(|m| m.len()) else {
                return;
            };
            if size > MAX_LIST as u64 || combined.saturating_add(size) > MAX_COMBINED {
                return;
            }
            combined += size;
            if let Ok(text) = fs::read_to_string(path) {
                rules += text
                    .lines()
                    .filter(|l| !l.is_empty() && !l.starts_with('!') && !l.starts_with('['))
                    .count();
                set.add_filter_list(text, ParseOptions::default());
                any = true;
            }
        };
        for (name, _) in LISTS {
            add(&lists_dir.join(format!("{name}.txt")));
        }
        if let Ok(rd) = fs::read_dir(user_dir) {
            for e in rd.flatten().take(32) {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("txt") {
                    add(&p);
                }
            }
        }
        if !any {
            return None;
        }
        Some(Filters {
            engine: Engine::new_with_filter_set(set),
            rules,
        })
    }

    pub fn should_block(&self, url: &str, source: &str, rtype: &str) -> bool {
        let Ok(req) = Request::new(url, source, rtype, "get") else {
            return false;
        };
        let r = self.engine.check_network_request(&req);
        r.filter.is_some() && (r.exception.is_none() || r.important)
    }

    pub fn cosmetic(&self, url: &str) -> Cosmetic {
        let r = self.engine.url_cosmetic_resources(url);
        Cosmetic {
            hide: r.hide_selectors,
            procedural: r.procedural_actions,
            exceptions: r.exceptions,
            generichide: r.generichide,
        }
    }

    pub fn generic(
        &self,
        classes: &[String],
        ids: &[String],
        exceptions: &HashSet<String>,
    ) -> Vec<String> {
        self.engine
            .hidden_class_id_selectors(classes, ids, exceptions)
    }
}

fn fetch(url: &str, tmp: &Path) -> Option<String> {
    let ok = crate::sys::curl_download(url, tmp);
    let text = if ok && fs::metadata(tmp).is_ok_and(|m| m.len() <= MAX_LIST as u64) {
        fs::read(tmp)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    } else {
        None
    };
    let _ = fs::remove_file(tmp);
    text
}

/// Разворачивает строки `%include файл%` (так устроен RU AdList на GitHub).
fn expand_includes(
    text: String,
    base: &str,
    tmp: &Path,
    depth: u8,
    budget: &mut usize,
) -> Option<String> {
    if text.len() > MAX_LIST {
        return None;
    }
    if depth == 0 || !text.contains("%include ") {
        return Some(text);
    }
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if let Some(name) = line
            .trim()
            .strip_prefix("%include ")
            .and_then(|r| r.strip_suffix('%'))
        {
            if *budget == 0 {
                return None;
            }
            *budget -= 1;
            let inc = fetch(&format!("{base}{}", name.trim()), tmp)?;
            let inc = expand_includes(inc, base, tmp, depth - 1, budget)?;
            if out.len().saturating_add(inc.len() + 1) > MAX_LIST {
                return None;
            }
            out.push_str(&inc);
            out.push('\n');
        } else {
            if out.len().saturating_add(line.len() + 1) > MAX_LIST {
                return None;
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    Some(out)
}
/// Скачивает устаревшие списки. Возвращает число обновлённых.
pub fn update_lists(dir: &Path, force: bool) -> usize {
    let _ = fs::create_dir_all(dir);
    let mut n = 0;
    for (name, urls) in LISTS {
        let path = dir.join(format!("{name}.txt"));
        let fresh = !force
            && fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d < REFRESH_AFTER)
                .unwrap_or(false);
        if fresh {
            continue;
        }
        let tmp = dir.join(format!("{name}.tmp"));
        for url in *urls {
            let Some(text) = fetch(url, &tmp) else {
                continue;
            };
            let base = &url[..url.rfind('/').map(|i| i + 1).unwrap_or(url.len())];
            let Some(text) = expand_includes(text, base, &tmp, 2, &mut 16) else {
                continue;
            };
            if text.len() > 10_000 {
                let part = dir.join(format!("{name}.part"));
                if fs::write(&part, text).is_ok() && fs::rename(&part, &path).is_ok() {
                    n += 1;
                    break;
                }
            }
        }
    }
    n
}

/// CSS из набора селекторов: каждое правило отдельно, чтобы один плохой селектор не ломал остальные.
pub fn selectors_to_css<'a>(sels: impl IntoIterator<Item = &'a String>) -> String {
    let mut css = String::new();
    for s in sels {
        if s.contains('{') || s.contains('}') {
            continue;
        }
        css.push_str(s);
        css.push_str("{display:none!important}\n");
    }
    css
}

#[cfg(test)]
mod security_tests {
    use super::*;
    #[test]
    fn oversized_lists_and_exhausted_includes_fail_closed() {
        let path = Path::new("unused");
        assert!(expand_includes(
            "a".repeat(MAX_LIST + 1),
            "https://example.com/",
            path,
            2,
            &mut 16
        )
        .is_none());
        assert!(expand_includes(
            "%include file%".into(),
            "https://example.com/",
            path,
            2,
            &mut 0
        )
        .is_none());
    }
}
