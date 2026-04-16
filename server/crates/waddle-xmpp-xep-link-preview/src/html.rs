//! OpenGraph / Twitter Card / HTML metadata extraction.
//!
//! Given raw HTML + the post-redirect request URL, produces a sanitized
//! [`LinkPreview`]. Only `<head>` is inspected. Field selection order:
//!
//! - title: `og:title` → `twitter:title` → `<title>`
//! - description: `og:description` → `twitter:description` → `<meta name="description">`
//! - image: `og:image` → `og:image:url` → `twitter:image`, resolved against the base URL
//! - site_name: `og:site_name` → hostname (with leading `www.` stripped)
//! - canonical: `<link rel="canonical">` → request URL
//! - type: `og:type`
//!
//! All text fields are trimmed and length-capped
//! ([`crate::TITLE_MAX`], [`crate::DESCRIPTION_MAX`], [`crate::SITE_NAME_MAX`], [`crate::TYPE_MAX`]).
//! Image URLs that fail to parse, use a non-http(s) scheme, or resolve to
//! a private/reserved IP are dropped — but other fields are kept.

use scraper::{Html, Selector};
use url::Url;

use crate::ssrf::is_disallowed_ip;
use crate::{
    LinkPreview, LinkPreviewImage, DESCRIPTION_MAX, SITE_NAME_MAX, TITLE_MAX, TYPE_MAX,
};

pub fn parse_html(html: &str, base_url: &Url) -> LinkPreview {
    let doc = Html::parse_document(html);
    let head = find_head(&doc);

    let og = select_meta(&doc, &head, "property", "og:");
    let twitter = select_meta(&doc, &head, "name", "twitter:");
    let plain_description = select_meta(&doc, &head, "name", "description")
        .into_iter()
        .next()
        .map(|(_, v)| v);

    let og_get = |key: &str| first_content(&og, key);
    let tw_get = |key: &str| first_content(&twitter, key);

    let title = first_present([
        og_get("og:title"),
        tw_get("twitter:title"),
        title_tag(&doc, &head),
    ]);
    let description = first_present([
        og_get("og:description"),
        tw_get("twitter:description"),
        plain_description,
    ]);
    let site_name = og_get("og:site_name").or_else(|| Some(host_from(base_url)));
    let type_ = og_get("og:type");

    let image_src_raw = og_get("og:image")
        .or_else(|| og_get("og:image:url"))
        .or_else(|| tw_get("twitter:image"));
    let image = image_src_raw.and_then(|src| build_image(&src, &og, base_url));

    let canonical_url = canonical(&doc, &head, base_url);

    LinkPreview {
        url: base_url.to_string(),
        canonical_url: Some(canonical_url),
        title: cap(title, TITLE_MAX),
        description: cap(description, DESCRIPTION_MAX),
        site_name: cap(site_name, SITE_NAME_MAX),
        type_: cap(type_, TYPE_MAX),
        image,
    }
}

/// Restrict a selection to descendants of `<head>` only. scraper doesn't
/// have a `within` API so we return a cached `ElementRef` to walk.
fn find_head(doc: &Html) -> Option<scraper::ElementRef<'_>> {
    Selector::parse("head").ok().and_then(|sel| doc.select(&sel).next())
}

/// Pull out `(key, content)` pairs for every matching `<meta>` tag in
/// the head, keyed by its `property`/`name` attribute. First occurrence
/// wins when callers use [`first_content`] below.
fn select_meta(
    doc: &Html,
    head: &Option<scraper::ElementRef<'_>>,
    attr: &str,
    prefix_or_exact: &str,
) -> Vec<(String, String)> {
    let sel = match Selector::parse(&format!("meta[{attr}]")) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let iter: Box<dyn Iterator<Item = scraper::ElementRef<'_>>> = match head {
        Some(head) => Box::new(head.select(&sel)),
        None => Box::new(doc.select(&sel)),
    };

    iter.filter_map(|el| {
        let key = el.value().attr(attr)?.to_owned();
        let content = el.value().attr("content")?.to_owned();
        if prefix_or_exact.ends_with(':') {
            if !key.to_ascii_lowercase().starts_with(prefix_or_exact) {
                return None;
            }
        } else if key.to_ascii_lowercase() != prefix_or_exact {
            return None;
        }
        Some((key.to_ascii_lowercase(), content))
    })
    .collect()
}

fn first_content(pairs: &[(String, String)], key: &str) -> Option<String> {
    pairs
        .iter()
        .find(|(k, _)| k == &key.to_ascii_lowercase())
        .map(|(_, v)| v.clone())
}

fn title_tag(doc: &Html, head: &Option<scraper::ElementRef<'_>>) -> Option<String> {
    let sel = Selector::parse("title").ok()?;
    let node = match head {
        Some(head) => head.select(&sel).next()?,
        None => doc.select(&sel).next()?,
    };
    let text: String = node.text().collect();
    Some(text)
}

fn canonical(doc: &Html, head: &Option<scraper::ElementRef<'_>>, base: &Url) -> String {
    let sel = match Selector::parse("link[rel='canonical']") {
        Ok(s) => s,
        Err(_) => return base.to_string(),
    };
    let iter: Box<dyn Iterator<Item = scraper::ElementRef<'_>>> = match head {
        Some(head) => Box::new(head.select(&sel)),
        None => Box::new(doc.select(&sel)),
    };
    for el in iter {
        if let Some(href) = el.value().attr("href") {
            if let Ok(resolved) = base.join(href) {
                return resolved.to_string();
            }
        }
    }
    base.to_string()
}

fn build_image(
    raw: &str,
    og_pairs: &[(String, String)],
    base: &Url,
) -> Option<LinkPreviewImage> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let resolved = base.join(trimmed).ok()?;
    if !matches!(resolved.scheme(), "http" | "https") {
        return None;
    }

    if let Some(host) = resolved.host() {
        if let url::Host::Ipv4(ip) = host {
            if is_disallowed_ip(ip.into()) {
                return None;
            }
        }
        if let url::Host::Ipv6(ip) = host {
            if is_disallowed_ip(ip.into()) {
                return None;
            }
        }
    } else {
        return None;
    }

    let width = first_content(og_pairs, "og:image:width").and_then(non_empty);
    let height = first_content(og_pairs, "og:image:height").and_then(non_empty);

    Some(LinkPreviewImage {
        src: resolved.to_string(),
        width,
        height,
    })
}

fn host_from(url: &Url) -> String {
    url.host_str()
        .map(|h| h.strip_prefix("www.").unwrap_or(h).to_owned())
        .unwrap_or_default()
}

fn cap(raw: Option<String>, max: usize) -> Option<String> {
    let trimmed = raw?.trim().to_owned();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() > max {
        let mut s = String::with_capacity(max);
        for (i, c) in trimmed.chars().enumerate() {
            if i >= max {
                break;
            }
            s.push(c);
        }
        Some(s)
    } else {
        Some(trimmed)
    }
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_owned())
    }
}

fn first_present<const N: usize>(options: [Option<String>; N]) -> Option<String> {
    for opt in options {
        if let Some(v) = opt {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.com/article?q=1").unwrap()
    }

    #[test]
    fn extracts_og_title_description_image_site_name_type() {
        let html = r#"
            <html><head>
              <meta property="og:title" content="Title">
              <meta property="og:description" content="Desc">
              <meta property="og:image" content="https://cdn.example.com/a.png">
              <meta property="og:image:width" content="1200">
              <meta property="og:image:height" content="630">
              <meta property="og:site_name" content="Example">
              <meta property="og:type" content="article">
            </head></html>
        "#;
        let p = parse_html(html, &base());
        assert_eq!(p.title.as_deref(), Some("Title"));
        assert_eq!(p.description.as_deref(), Some("Desc"));
        assert_eq!(p.site_name.as_deref(), Some("Example"));
        assert_eq!(p.type_.as_deref(), Some("article"));
        let img = p.image.unwrap();
        assert_eq!(img.src, "https://cdn.example.com/a.png");
        assert_eq!(img.width.as_deref(), Some("1200"));
        assert_eq!(img.height.as_deref(), Some("630"));
    }

    #[test]
    fn falls_back_to_twitter_card() {
        let html = r#"
            <html><head>
              <meta name="twitter:title" content="TTitle">
              <meta name="twitter:description" content="TDesc">
              <meta name="twitter:image" content="https://cdn.example.com/t.png">
            </head></html>
        "#;
        let p = parse_html(html, &base());
        assert_eq!(p.title.as_deref(), Some("TTitle"));
        assert_eq!(p.description.as_deref(), Some("TDesc"));
        assert_eq!(p.image.unwrap().src, "https://cdn.example.com/t.png");
    }

    #[test]
    fn falls_back_to_title_tag_and_meta_description() {
        let html = r#"
            <html><head>
              <title>Plain Title</title>
              <meta name="description" content="Plain description">
            </head></html>
        "#;
        let p = parse_html(html, &base());
        assert_eq!(p.title.as_deref(), Some("Plain Title"));
        assert_eq!(p.description.as_deref(), Some("Plain description"));
    }

    #[test]
    fn og_wins_over_twitter_and_title() {
        let html = r#"
            <html><head>
              <title>Plain</title>
              <meta property="og:title" content="OG">
              <meta name="twitter:title" content="TW">
            </head></html>
        "#;
        let p = parse_html(html, &base());
        assert_eq!(p.title.as_deref(), Some("OG"));
    }

    #[test]
    fn site_name_falls_back_to_hostname_without_www() {
        let p = parse_html("<html></html>", &Url::parse("https://www.example.com/").unwrap());
        assert_eq!(p.site_name.as_deref(), Some("example.com"));
    }

    #[test]
    fn canonical_url_from_link_tag() {
        let html = r#"<html><head><link rel="canonical" href="https://example.com/canonical"></head></html>"#;
        let p = parse_html(html, &base());
        assert_eq!(p.canonical_url.as_deref(), Some("https://example.com/canonical"));
    }

    #[test]
    fn canonical_url_falls_back_to_base() {
        let p = parse_html("<html></html>", &base());
        assert_eq!(p.canonical_url.as_deref(), Some("https://example.com/article?q=1"));
    }

    #[test]
    fn resolves_protocol_relative_image() {
        let html = r#"<html><head><meta property="og:image" content="//cdn.example.com/a.png"></head></html>"#;
        let p = parse_html(html, &base());
        assert_eq!(p.image.unwrap().src, "https://cdn.example.com/a.png");
    }

    #[test]
    fn resolves_root_relative_image() {
        let html = r#"<html><head><meta property="og:image" content="/a.png"></head></html>"#;
        let p = parse_html(html, &base());
        assert_eq!(p.image.unwrap().src, "https://example.com/a.png");
    }

    #[test]
    fn drops_image_with_non_http_scheme() {
        let html = r#"<html><head><meta property="og:image" content="javascript:alert(1)"></head></html>"#;
        let p = parse_html(html, &base());
        assert!(p.image.is_none());
    }

    #[test]
    fn drops_image_with_data_url() {
        let html = r#"<html><head><meta property="og:image" content="data:image/png;base64,xx"></head></html>"#;
        let p = parse_html(html, &base());
        assert!(p.image.is_none());
    }

    #[test]
    fn drops_image_hosted_on_private_ip() {
        let html = r#"<html><head><meta property="og:image" content="http://192.168.1.1/og.png"></head></html>"#;
        let p = parse_html(html, &base());
        assert!(p.image.is_none());
    }

    #[test]
    fn truncates_long_title() {
        let long = "x".repeat(500);
        let html = format!(
            r#"<html><head><meta property="og:title" content="{long}"></head></html>"#
        );
        let p = parse_html(&html, &base());
        assert_eq!(p.title.as_ref().map(|s| s.chars().count()), Some(TITLE_MAX));
    }

    #[test]
    fn truncates_long_description() {
        let long = "y".repeat(1000);
        let html = format!(
            r#"<html><head><meta property="og:description" content="{long}"></head></html>"#
        );
        let p = parse_html(&html, &base());
        assert_eq!(p.description.as_ref().map(|s| s.chars().count()), Some(DESCRIPTION_MAX));
    }

    #[test]
    fn trims_whitespace_and_drops_empty_fields() {
        let html = r#"<html><head><meta property="og:title" content="   "><meta property="og:description" content=""></head></html>"#;
        let p = parse_html(html, &base());
        assert!(p.title.is_none());
        assert!(p.description.is_none());
    }

    #[test]
    fn first_duplicate_og_tag_wins() {
        let html = r#"
            <html><head>
              <meta property="og:title" content="first">
              <meta property="og:title" content="second">
            </head></html>
        "#;
        let p = parse_html(html, &base());
        assert_eq!(p.title.as_deref(), Some("first"));
    }

    #[test]
    fn handles_html_with_no_metadata() {
        let p = parse_html("<html><head></head><body>x</body></html>", &base());
        assert!(p.title.is_none());
        assert!(p.description.is_none());
        assert!(p.image.is_none());
        // site_name still falls back to hostname.
        assert_eq!(p.site_name.as_deref(), Some("example.com"));
    }
}
