//! Усиленный блокировщик: движок Brave `adblock` (синтаксис EasyList / uBlock / AdGuard).
//! Списки скачиваются в фоне и кэшируются на диске.

use std::{
    collections::HashSet,
    fs,
    path::Path,
    time::Duration,
};

use adblock::{
    lists::{FilterSet, ParseOptions},
    request::Request,
    Engine,
};

/// Имя файла и адрес списка. RU AdList и AdGuard Russian нужны для российских сайтов
/// (Яндекс, VK, mail.ru и др.).
pub const LISTS: &[(&str, &str)] = &[
    ("easylist", "https://easylist.to/easylist/easylist.txt"),
    ("ruadlist", "https://easylist-downloads.adblockplus.org/advblock.txt"),
    ("adguard-ru", "https://filters.adtidy.org/extension/ublock/filters/1.txt"),
    ("ublock", "https://ublockorigin.github.io/uAssets/filters/filters.txt"),
];

const REFRESH_AFTER: Duration = Duration::from_secs(3 * 24 * 3600);

pub struct Filters {
    engine: Engine,
    pub rules: usize,
}

pub struct Cosmetic {
    pub hide: HashSet<String>,
    pub exceptions: HashSet<String>,
    pub generichide: bool,
}

impl Filters {
    /// Собирает движок из скачанных списков и пользовательских `*.txt`.
    pub fn build(lists_dir: &Path, user_dir: &Path) -> Option<Filters> {
        let mut set = FilterSet::new(false);
        let mut rules = 0usize;
        let mut any = false;
        let mut add = |path: &Path| {
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
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("txt") {
                    add(&p);
                }
            }
        }
        if !any {
            return None;
        }
        Some(Filters { engine: Engine::new_with_filter_set(set), rules })
    }

    pub fn should_block(&self, url: &str, source: &str, rtype: &str) -> bool {
        let Ok(req) = Request::new(url, source, rtype, "get") else { return false };
        let r = self.engine.check_network_request(&req);
        r.filter.is_some() && (r.exception.is_none() || r.important)
    }

    pub fn cosmetic(&self, url: &str) -> Cosmetic {
        let r = self.engine.url_cosmetic_resources(url);
        Cosmetic { hide: r.hide_selectors, exceptions: r.exceptions, generichide: r.generichide }
    }

    pub fn generic(&self, classes: &[String], ids: &[String], exceptions: &HashSet<String>) -> Vec<String> {
        self.engine.hidden_class_id_selectors(classes, ids, exceptions)
    }
}

/// Скачивает устаревшие списки. Возвращает число обновлённых.
pub fn update_lists(dir: &Path, force: bool) -> usize {
    let _ = fs::create_dir_all(dir);
    let mut n = 0;
    for (name, url) in LISTS {
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
        let ok = crate::sys::curl_download(url, &tmp)
            && fs::metadata(&tmp).map(|m| m.len() > 10_000).unwrap_or(false);
        if ok {
            let _ = fs::rename(&tmp, &path);
            n += 1;
        } else {
            let _ = fs::remove_file(&tmp);
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
