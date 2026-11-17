pub fn search_url(engine: &str) -> &'static str {
    match engine {
        "yandex" => "https://yandex.ru/search/?text=",
        "duckduckgo" => "https://duckduckgo.com/?q=",
        _ => "https://www.google.com/search?q=",
    }
}

pub fn saved_search_query(address: &str) -> Option<String> {
    let (page, query) = address.split_once('?')?;
    let key = match page {
        "https://www.google.com/search" | "https://duckduckgo.com/" => "q",
        "https://yandex.ru/search/" => "text",
        _ => return None,
    };
    let (_, value) = url::form_urlencoded::parse(query.as_bytes()).find(|(name, _)| name == key)?;
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= 180 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

pub fn recent_searches_json<'a>(addresses: impl Iterator<Item = &'a str>) -> String {
    let mut seen = std::collections::HashSet::new();
    let queries: Vec<_> = addresses
        .take(300)
        .filter_map(saved_search_query)
        .filter(|query| seen.insert(query.to_lowercase()))
        .take(40)
        .collect();
    serde_json::to_string(&queries).unwrap().replace('<', "\\u003c")
}

#[test]
fn history_suggestions_deduplicate_and_escape_script_end_tags() {
    let urls = [
        "https://www.google.com/search?q=Linux",
        "https://duckduckgo.com/?q=linux",
        "https://yandex.ru/search/?text=%3C%2Fscript%3E",
        "https://example.org/?q=private",
    ];
    let result = recent_searches_json(urls.into_iter());
    assert!(!result.contains('<'));
    assert_eq!(serde_json::from_str::<Vec<String>>(&result).unwrap(), ["Linux", "</script>"]);
}
