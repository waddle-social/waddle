//! GIF search through the Waddle web app's `/api/giphy` proxy.
//!
//! The proxy holds the GIPHY key; this client never sees it. It answers
//! `{"data": [{id, title, images: {fixed_height_small: {url}, original:
//! {url}}}]}` with 200, 503 when no key is configured, 429 when rate
//! limited and 502 when GIPHY fails, the last three with `{"error": …}`.
//! The Apple client reads exactly this. A picked GIF is sent as its
//! original's address in the message body, as the web app does.
//!
//! The proxy is part of the web app, which is not the XMPP server this
//! account signs in to, so its origin is `https://waddle.chat` unless
//! `WADDLE_GIF_PROXY_URL` names another.

use std::time::Duration;

use serde_json::Value;
use url::Url;

pub const DEFAULT_BASE: &str = "https://waddle.chat";
/// The environment variable that points GIF search at another web app.
pub const BASE_ENV: &str = "WADDLE_GIF_PROXY_URL";
pub const DEFAULT_LIMIT: u32 = 24;
const MAX_LIMIT: u32 = 50;
const MAX_QUERY_CHARS: usize = 100;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const UNAVAILABLE: &str = "GIF search is unavailable right now.";
const RATE_LIMITED: &str = "Too many GIF searches. Try again shortly.";

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gif {
    pub id: String,
    pub title: String,
    /// The small rendition for the picker grid.
    pub preview: Url,
    /// The full GIF that gets shared.
    pub original: Url,
}

/// What a search produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Results(Vec<Gif>),
    /// The proxy has no GIPHY key.
    NotConfigured,
    Unavailable(String),
}

/// The proxy's origin: `override_url` when it is a usable origin (https,
/// or http on loopback), else the default.
pub fn base_url(override_url: Option<&str>) -> Url {
    override_url
        .and_then(|raw| crate::auth::device::ServerUrl::parse(raw).ok())
        .map(|server| server.as_url().clone())
        .unwrap_or_else(|| Url::parse(DEFAULT_BASE).expect("default base is a URL"))
}

/// `<base>/api/giphy?limit=N&q=…`. An empty query asks for trending GIFs
/// and leaves out `q`.
pub fn request_url(base: &Url, query: &str, limit: u32) -> Url {
    let mut url = base.clone();
    url.set_path("api/giphy");
    let limit = limit.clamp(1, MAX_LIMIT);
    let query: String = query.trim().chars().take(MAX_QUERY_CHARS).collect();
    let query = query.trim();
    let mut encoded = format!("limit={limit}");
    if !query.is_empty() {
        // `+` is only ever a space here: a literal one is `%2B`.
        let value = url::form_urlencoded::byte_serialize(query.as_bytes())
            .collect::<String>()
            .replace('+', "%20");
        encoded.push_str("&q=");
        encoded.push_str(&value);
    }
    url.set_query(Some(&encoded));
    url
}

fn https(value: Option<&Value>) -> Option<Url> {
    let url = Url::parse(value?.as_str()?).ok()?;
    (url.scheme() == "https" && url.host_str().is_some()).then_some(url)
}

/// An entry with any malformed field is skipped, not fatal to the page.
fn gif(entry: &Value) -> Option<Gif> {
    let images = entry.get("images")?;
    Some(Gif {
        id: entry.get("id")?.as_str()?.to_string(),
        title: entry
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        preview: https(images.get("fixed_height_small")?.get("url"))?,
        original: https(images.get("original")?.get("url"))?,
    })
}

/// Classify the proxy's answer.
pub fn classify(status: u16, body: &[u8]) -> Outcome {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let unavailable = || Outcome::Unavailable(UNAVAILABLE.to_string());
    match status {
        200 => match parsed.as_ref().and_then(|value| value.get("data")) {
            Some(Value::Array(entries)) => {
                Outcome::Results(entries.iter().filter_map(gif).collect())
            }
            _ => unavailable(),
        },
        503 => Outcome::NotConfigured,
        429 => Outcome::Unavailable(RATE_LIMITED.to_string()),
        _ => parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(Value::as_str)
            .filter(|message| !message.is_empty() && message.chars().count() <= 200)
            .map_or_else(unavailable, |message| {
                Outcome::Unavailable(message.to_string())
            }),
    }
}

/// Search (or, for an empty query, list trending GIFs).
pub async fn search(base: Url, query: String) -> Outcome {
    let unavailable = || Outcome::Unavailable(UNAVAILABLE.to_string());
    let Ok(client) = reqwest::Client::builder()
        .user_agent(concat!("Waddle-Linux/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return unavailable();
    };
    let Ok(mut response) = client
        .get(request_url(&base, &query, DEFAULT_LIMIT))
        .send()
        .await
    else {
        return unavailable();
    };
    let status = response.status().as_u16();
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if body.len() + chunk.len() <= MAX_RESPONSE_BYTES => {
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            _ => return unavailable(),
        }
    }
    classify(status, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, preview: &str, original: &str) -> String {
        format!(
            r#"{{"id":"{id}","title":"T {id}","images":{{"fixed_height_small":{{"url":"{preview}"}},"original":{{"url":"{original}"}}}}}}"#
        )
    }

    #[test]
    fn requests_match_the_apple_client() {
        let base = base_url(None);
        assert_eq!(
            request_url(&base, "", 24).as_str(),
            "https://waddle.chat/api/giphy?limit=24"
        );
        assert_eq!(
            request_url(&base, "  happy cat ", 24).as_str(),
            "https://waddle.chat/api/giphy?limit=24&q=happy%20cat"
        );
        assert_eq!(
            request_url(&base, "c++ & a=b #1", 24).as_str(),
            "https://waddle.chat/api/giphy?limit=24&q=c%2B%2B%20%26%20a%3Db%20%231"
        );
        let other = Url::parse("https://example.test").unwrap();
        assert_eq!(
            request_url(&other, "", 0).as_str(),
            "https://example.test/api/giphy?limit=1"
        );
        assert_eq!(
            request_url(&other, "", 500).as_str(),
            "https://example.test/api/giphy?limit=50"
        );
        let long = request_url(&other, &"a".repeat(300), 10);
        assert_eq!(
            long.as_str(),
            format!(
                "https://example.test/api/giphy?limit=10&q={}",
                "a".repeat(100)
            )
        );
    }

    #[test]
    fn the_base_is_an_origin_or_the_default() {
        assert_eq!(base_url(None).as_str(), "https://waddle.chat/");
        assert_eq!(
            base_url(Some("web.example.org/ignored/path")).as_str(),
            "https://web.example.org/"
        );
        assert_eq!(
            base_url(Some("http://localhost:4321")).as_str(),
            "http://localhost:4321/"
        );
        assert_eq!(
            base_url(Some("http://evil.example")).as_str(),
            "https://waddle.chat/"
        );
        assert_eq!(base_url(Some("")).as_str(), "https://waddle.chat/");
    }

    #[test]
    fn results_skip_malformed_entries() {
        let body = format!(
            r#"{{"data":[{},{},{},{{"id":"x"}}]}}"#,
            entry(
                "a",
                "https://media.giphy.com/a/s.gif",
                "https://media.giphy.com/a/g.gif"
            ),
            entry(
                "b",
                "http://media.giphy.com/b/s.gif",
                "https://media.giphy.com/b/g.gif"
            ),
            entry(
                "c",
                "https://media.giphy.com/c/s.gif",
                "https://media.giphy.com/c/g.gif"
            ),
        );
        let Outcome::Results(gifs) = classify(200, body.as_bytes()) else {
            panic!("results");
        };
        assert_eq!(gifs.len(), 2);
        assert_eq!(gifs[0].id, "a");
        assert_eq!(gifs[0].title, "T a");
        assert_eq!(gifs[0].original.as_str(), "https://media.giphy.com/a/g.gif");
        assert_eq!(gifs[1].id, "c");
    }

    #[test]
    fn statuses_map_to_outcomes() {
        let unavailable = Outcome::Unavailable(UNAVAILABLE.into());
        assert_eq!(
            classify(200, br#"{"data":[]}"#),
            Outcome::Results(Vec::new())
        );
        assert_eq!(classify(200, br"{}"), unavailable);
        assert_eq!(classify(200, b"nope"), unavailable);
        assert_eq!(
            classify(503, br#"{"error":"no key"}"#),
            Outcome::NotConfigured
        );
        assert_eq!(
            classify(429, b"{}"),
            Outcome::Unavailable(RATE_LIMITED.into())
        );
        assert_eq!(
            classify(502, br#"{"error":"Giphy failed"}"#),
            Outcome::Unavailable("Giphy failed".into())
        );
        assert_eq!(classify(500, b"<html>"), unavailable);
    }
}
