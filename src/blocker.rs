use std::{collections::HashSet, fs, path::Path};

/// Встроенный список рекламных и трекерных доменов (блокируются вместе с поддоменами).
const DEFAULT_HOSTS: &str = "
doubleclick.net googlesyndication.com googleadservices.com google-analytics.com
adservice.google.com pagead2.googlesyndication.com adnxs.com adsrvr.org adform.net
taboola.com outbrain.com criteo.com criteo.net rubiconproject.com pubmatic.com
openx.net amazon-adsystem.com scorecardresearch.com quantserve.com moatads.com
smartadserver.com casalemedia.com teads.tv advertising.com ads.yahoo.com
bat.bing.com hotjar.com mixpanel.com
mc.yandex.ru an.yandex.ru yandexadexchange.net adfox.ru adriver.ru begun.ru
yabs.yandex.ru an.yandex.com an.yandex.kz an.yandex.by ads.adfox.ru
ad.mail.ru tns-counter.ru mediametrics.ru ads.vk.com top-fwz1.mail.ru
";

/// Косметическая фильтрация: скрываем типичные рекламные блоки.
const DEFAULT_CSS: &str = "
ins.adsbygoogle,.adsbygoogle,[id^=\"google_ads\"],[id^=\"div-gpt-ad\"],
[id*=\"yandex_rtb\"],[id^=\"yandex_ad\"],[class*=\"adfox\"],[id*=\"adfox\"],
.advert,.advertisement,[data-ad-slot],iframe[src*=\"doubleclick.net\"],
iframe[src*=\"googlesyndication.com\"]{display:none!important}
";

pub struct Blocker {
    hosts: HashSet<String>,
    script: String,
}

impl Blocker {
    /// Загружает встроенные правила + все *.txt из `dir`
    /// (поддерживаются `||домен^`, `##селектор` и формат hosts).
    pub fn load(dir: &Path) -> Self {
        let mut hosts: HashSet<String> =
            DEFAULT_HOSTS.split_whitespace().map(String::from).collect();
        let mut css = String::from(DEFAULT_CSS);

        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().and_then(|x| x.to_str()) != Some("txt") {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&p) else { continue };
                for line in text.lines() {
                    parse_line(line.trim(), &mut hosts, &mut css);
                }
            }
        }

        let mut list: Vec<&String> = hosts.iter().collect();
        list.sort();
        let script = include_str!("inject.js")
            .replace("__HOSTS__", &serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()))
            .replace("__CSS__", &serde_json::to_string(&css).unwrap_or_else(|_| "\"\"".into()));

        Self { hosts, script }
    }

    /// Скрипт, который внедряется в каждую страницу.
    pub fn script(&self) -> &str {
        &self.script
    }

    pub fn is_blocked(&self, url: &str) -> bool {
        let Ok(u) = url::Url::parse(url) else { return false };
        let Some(mut h) = u.host_str() else { return false };
        loop {
            if self.hosts.contains(h) {
                return true;
            }
            match h.find('.') {
                Some(i) => h = &h[i + 1..],
                None => return false,
            }
        }
    }
}

fn parse_line(l: &str, hosts: &mut HashSet<String>, css: &mut String) {
    if l.is_empty() || l.starts_with('!') || l.starts_with("@@") {
        return;
    }
    if let Some(sel) = l.strip_prefix("##") {
        if !sel.is_empty() {
            css.push_str(&format!("{sel}{{display:none!important}}\n"));
        }
        return;
    }
    if l.starts_with('#') {
        return;
    }
    if let Some(rest) = l.strip_prefix("||") {
        let end = rest
            .find(|c| matches!(c, '^' | '$' | '/' | '*' | ':'))
            .unwrap_or(rest.len());
        let (host, tail) = rest.split_at(end);
        if tail.starts_with('/') || tail.starts_with('*') {
            return; // правила с путями здесь не поддерживаются
        }
        if host.contains('.') {
            hosts.insert(host.to_lowercase());
        }
        return;
    }
    let mut it = l.split_whitespace();
    if let (Some(ip), Some(h)) = (it.next(), it.next()) {
        if (ip == "0.0.0.0" || ip == "127.0.0.1") && h != "localhost" {
            hosts.insert(h.to_lowercase());
        }
    }
}

#[cfg(test)]
mod yandex_tests {
    #[test]
    fn ads_are_blocked_without_blocking_weather_or_static_assets() {
        let blocker=super::Blocker::load(std::path::Path::new("__no_custom_filters__"));
        assert!(blocker.is_blocked("https://yabs.yandex.ru/count/example"));
        assert!(blocker.is_blocked("https://an.yandex.ru/meta/example"));
        assert!(!blocker.is_blocked("https://yandex.ru/pogoda/"));
        assert!(!blocker.is_blocked("https://yastatic.net/weather/icon.svg"));
        assert!(!blocker.is_blocked("https://yabs.yandex.ru.example.com/"));
    }
}
