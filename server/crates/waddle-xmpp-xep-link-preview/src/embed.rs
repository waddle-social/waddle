//! XEP-0372 reference + nested `<preview>` XML serializer.
//!
//! Produces the exact wire format the receiver TypeScript stack (in
//! `chat/src/lib/xmpp/extensions/preview.ts`) already parses, so we do
//! not need a Waddle-specific protocol change on the client.

use minidom::Element;
use xmpp_parsers::message::Message;

use crate::detect::DetectedUrl;
use crate::{
    LinkPreview, LinkPreviewImage, DESCRIPTION_MAX, NS_REFERENCE, NS_WADDLE_PREVIEW,
    SITE_NAME_MAX, TITLE_MAX, TYPE_MAX,
};

/// Build a `<reference type='data' begin='…' end='…' uri='…'>` element
/// carrying a nested `<preview xmlns='urn:waddle:link-preview:0'>` child
/// populated from `preview`.
pub fn build_reference(detected: &DetectedUrl, preview: &LinkPreview) -> Element {
    let mut reference = Element::builder("reference", NS_REFERENCE)
        .attr("type", "data")
        .attr("begin", detected.utf16_begin.to_string())
        .attr("end", detected.utf16_end.to_string())
        .attr("uri", detected.url.as_str())
        .build();

    let preview_el = build_preview(&detected.url, preview);
    reference.append_child(preview_el);
    reference
}

fn build_preview(url: &str, preview: &LinkPreview) -> Element {
    let mut builder = Element::builder("preview", NS_WADDLE_PREVIEW).attr("url", url);

    if let Some(ref title) = preview.title {
        builder = builder.append(text_child("title", cap(title, TITLE_MAX)));
    }
    if let Some(ref desc) = preview.description {
        builder = builder.append(text_child("description", cap(desc, DESCRIPTION_MAX)));
    }
    if let Some(ref site) = preview.site_name {
        builder = builder.append(text_child("site-name", cap(site, SITE_NAME_MAX)));
    }
    if let Some(ref type_) = preview.type_ {
        builder = builder.append(text_child("type", cap(type_, TYPE_MAX)));
    }
    if let Some(ref img) = preview.image {
        builder = builder.append(build_image(img));
    }
    builder.build()
}

fn text_child(name: &str, text: String) -> Element {
    Element::builder(name, NS_WADDLE_PREVIEW)
        .append(text.as_str())
        .build()
}

fn build_image(img: &LinkPreviewImage) -> Element {
    let mut builder = Element::builder("image", NS_WADDLE_PREVIEW).attr("src", img.src.as_str());
    if let Some(ref w) = img.width {
        builder = builder.attr("width", w.as_str());
    }
    if let Some(ref h) = img.height {
        builder = builder.attr("height", h.as_str());
    }
    builder.build()
}

fn cap(raw: &str, max: usize) -> String {
    let mut s = String::new();
    for (i, c) in raw.trim().chars().enumerate() {
        if i >= max {
            break;
        }
        s.push(c);
    }
    s
}

/// Is this element the server's own preview-carrying reference? Used by
/// the sender-authoritative strip pass to remove any client-authored
/// copy before enrichment runs.
pub fn is_preview_reference(element: &Element) -> bool {
    if element.ns() != NS_REFERENCE || element.name() != "reference" {
        return false;
    }
    if element.attr("type") != Some("data") {
        return false;
    }
    element
        .children()
        .any(|child| child.ns() == NS_WADDLE_PREVIEW && child.name() == "preview")
}

/// Does this message carry a top-level `<no-preview xmlns='urn:waddle:link-preview:0'/>`
/// hint from the sender opting out of enrichment for this message?
pub fn has_no_preview_hint(msg: &Message) -> bool {
    msg.payloads
        .iter()
        .any(|el| el.ns() == NS_WADDLE_PREVIEW && el.name() == "no-preview")
}

/// Remove any client-authored preview references from `msg.payloads`,
/// returning the number stripped. Always called before enrichment so
/// receivers trust the server-injected preview exclusively.
pub fn strip_client_preview_references(msg: &mut Message) -> usize {
    let before = msg.payloads.len();
    msg.payloads.retain(|el| !is_preview_reference(el));
    before - msg.payloads.len()
}

/// Is this element a GitHub enrichment child (`<repo|issue|pr>` in
/// `urn:waddle:github:0`) whose `url` attribute matches `target`? Used
/// to avoid duplicating a preview for URLs already expanded by the
/// GitHub enricher which ran earlier in the pipeline.
pub fn is_github_embed_for(element: &Element, target: &str) -> bool {
    if element.ns() != "urn:waddle:github:0" {
        return false;
    }
    if !matches!(element.name(), "repo" | "issue" | "pr") {
        return false;
    }
    element.attr("url") == Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::DetectedUrl;
    use crate::LinkPreview;

    fn detected(url: &str, begin: usize, end: usize) -> DetectedUrl {
        DetectedUrl {
            url: url.to_owned(),
            utf16_begin: begin,
            utf16_end: end,
        }
    }

    fn full_preview() -> LinkPreview {
        LinkPreview {
            url: "https://example.com/a".to_owned(),
            canonical_url: Some("https://example.com/a".to_owned()),
            title: Some("T".to_owned()),
            description: Some("D".to_owned()),
            site_name: Some("Ex".to_owned()),
            type_: Some("article".to_owned()),
            image: Some(LinkPreviewImage {
                src: "https://cdn.example.com/a.png".to_owned(),
                width: Some("1200".to_owned()),
                height: Some("630".to_owned()),
            }),
        }
    }

    #[test]
    fn build_reference_has_expected_attrs() {
        let d = detected("https://example.com/a", 4, 25);
        let el = build_reference(&d, &full_preview());

        assert_eq!(el.name(), "reference");
        assert_eq!(el.ns(), NS_REFERENCE);
        assert_eq!(el.attr("type"), Some("data"));
        assert_eq!(el.attr("begin"), Some("4"));
        assert_eq!(el.attr("end"), Some("25"));
        assert_eq!(el.attr("uri"), Some("https://example.com/a"));
    }

    #[test]
    fn build_reference_has_nested_preview_with_fields() {
        let d = detected("https://example.com/a", 0, 21);
        let el = build_reference(&d, &full_preview());
        let preview = el
            .get_child("preview", NS_WADDLE_PREVIEW)
            .expect("preview child");
        assert_eq!(preview.attr("url"), Some("https://example.com/a"));
        assert_eq!(
            preview.get_child("title", NS_WADDLE_PREVIEW).map(|c| c.text()),
            Some("T".to_owned())
        );
        assert_eq!(
            preview.get_child("description", NS_WADDLE_PREVIEW).map(|c| c.text()),
            Some("D".to_owned())
        );
        assert_eq!(
            preview.get_child("site-name", NS_WADDLE_PREVIEW).map(|c| c.text()),
            Some("Ex".to_owned())
        );
        assert_eq!(
            preview.get_child("type", NS_WADDLE_PREVIEW).map(|c| c.text()),
            Some("article".to_owned())
        );
        let image = preview.get_child("image", NS_WADDLE_PREVIEW).expect("image");
        assert_eq!(image.attr("src"), Some("https://cdn.example.com/a.png"));
        assert_eq!(image.attr("width"), Some("1200"));
        assert_eq!(image.attr("height"), Some("630"));
    }

    #[test]
    fn build_reference_omits_missing_optional_fields() {
        let preview = LinkPreview {
            url: "https://example.com/a".to_owned(),
            canonical_url: None,
            title: Some("T".to_owned()),
            description: None,
            site_name: None,
            type_: None,
            image: None,
        };
        let el = build_reference(&detected("https://example.com/a", 0, 21), &preview);
        let inner = el.get_child("preview", NS_WADDLE_PREVIEW).unwrap();
        assert!(inner.get_child("title", NS_WADDLE_PREVIEW).is_some());
        assert!(inner.get_child("description", NS_WADDLE_PREVIEW).is_none());
        assert!(inner.get_child("site-name", NS_WADDLE_PREVIEW).is_none());
        assert!(inner.get_child("image", NS_WADDLE_PREVIEW).is_none());
    }

    #[test]
    fn build_reference_truncates_overlong_fields() {
        let mut p = full_preview();
        p.title = Some("x".repeat(500));
        let el = build_reference(&detected("https://example.com/a", 0, 21), &p);
        let title = el
            .get_child("preview", NS_WADDLE_PREVIEW)
            .and_then(|pr| pr.get_child("title", NS_WADDLE_PREVIEW))
            .expect("title")
            .text();
        assert_eq!(title.chars().count(), TITLE_MAX);
    }

    #[test]
    fn detects_own_preview_reference() {
        let el = build_reference(&detected("https://example.com/a", 0, 21), &full_preview());
        assert!(is_preview_reference(&el));
    }

    #[test]
    fn is_preview_reference_rejects_mention_type() {
        let el = Element::builder("reference", NS_REFERENCE)
            .attr("type", "mention")
            .attr("uri", "xmpp:alice@example.com")
            .build();
        assert!(!is_preview_reference(&el));
    }

    #[test]
    fn is_preview_reference_rejects_data_ref_without_preview_child() {
        let el = Element::builder("reference", NS_REFERENCE)
            .attr("type", "data")
            .attr("uri", "https://example.com/file.jpg")
            .build();
        assert!(!is_preview_reference(&el));
    }

    #[test]
    fn is_preview_reference_rejects_wrong_namespace_on_child() {
        let mut el = Element::builder("reference", NS_REFERENCE)
            .attr("type", "data")
            .attr("uri", "https://example.com/a")
            .build();
        el.append_child(
            Element::builder("preview", "urn:bogus:link-preview:99").build(),
        );
        assert!(!is_preview_reference(&el));
    }

    #[test]
    fn strip_client_preview_references_removes_only_preview_refs() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
            <body>hi</body>\
            <reference xmlns='urn:xmpp:reference:0' type='mention' uri='xmpp:alice@example.com'/>\
            <reference xmlns='urn:xmpp:reference:0' type='data' uri='https://example.com/file.jpg'/>\
            <reference xmlns='urn:xmpp:reference:0' type='data' uri='https://example.com/a'>\
                <preview xmlns='urn:waddle:link-preview:0' url='https://example.com/a'><title>forged</title></preview>\
            </reference>\
        </message>";
        let root: Element = xml.parse().unwrap();
        let mut msg = Message::try_from(root).unwrap();
        let stripped = strip_client_preview_references(&mut msg);
        assert_eq!(stripped, 1);
        // Mention + file-data references survive.
        let remaining_refs = msg
            .payloads
            .iter()
            .filter(|el| el.ns() == NS_REFERENCE && el.name() == "reference")
            .count();
        assert_eq!(remaining_refs, 2);
    }

    #[test]
    fn has_no_preview_hint_detects_top_level_marker() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
            <body>hi</body>\
            <no-preview xmlns='urn:waddle:link-preview:0'/>\
        </message>";
        let msg = Message::try_from(xml.parse::<Element>().unwrap()).unwrap();
        assert!(has_no_preview_hint(&msg));
    }

    #[test]
    fn has_no_preview_hint_returns_false_without_marker() {
        let xml =
            "<message xmlns='jabber:client' type='chat'><body>hi</body></message>";
        let msg = Message::try_from(xml.parse::<Element>().unwrap()).unwrap();
        assert!(!has_no_preview_hint(&msg));
    }

    #[test]
    fn is_github_embed_for_recognises_repo_issue_pr() {
        let url = "https://github.com/rust-lang/rust";
        for name in ["repo", "issue", "pr"] {
            let el = Element::builder(name, "urn:waddle:github:0")
                .attr("url", url)
                .build();
            assert!(is_github_embed_for(&el, url));
        }
    }

    #[test]
    fn is_github_embed_for_requires_matching_url() {
        let el = Element::builder("repo", "urn:waddle:github:0")
            .attr("url", "https://github.com/other/repo")
            .build();
        assert!(!is_github_embed_for(&el, "https://github.com/rust-lang/rust"));
    }

    #[test]
    fn is_github_embed_for_rejects_wrong_namespace() {
        let el = Element::builder("repo", "urn:unrelated:0")
            .attr("url", "https://github.com/rust-lang/rust")
            .build();
        assert!(!is_github_embed_for(&el, "https://github.com/rust-lang/rust"));
    }
}
