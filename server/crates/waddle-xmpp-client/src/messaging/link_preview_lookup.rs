//! Waddle-private link-preview lookup over `urn:waddle:link-preview:0`.
//!
//! Composer-side IQ: `<iq type='get'><lookup><url/><scope/></lookup></iq>`
//! sent to the user's own server (no `to`), answered with
//! `<lookup status='ready|unsupported|blocked|failed'>` carrying an
//! optional `<preview token= original-url= normalized-url= expires-at=>`
//! whose `<title/>`, `<description/>`, `<image/>`, and `<player/>`
//! children mirror the server's `link_preview_lookup` handler. The
//! returned token is redeemed at send time via
//! [`super::types::SendMessageOptions::link_preview_token`].

use chrono::{DateTime, Utc};
use jid::BareJid;
use minidom::Element;
use url::Url;
use uuid::Uuid;

use super::namespaces::{NS_CLIENT, NS_WADDLE_LINK_PREVIEW};
use super::parsing::parse_cached_preview_image_url;
use super::types::{LinkPreviewImageData, LinkPreviewPlayer, LinkPreviewToken};
use waddle_xmpp_core::PreviewImageMediaType;

/// Reject `token` attributes larger than this many bytes (web
/// `MAX_LINK_PREVIEW_TOKEN_BYTES` parity) — the token is echoed back on
/// send, so an oversized value is either corrupt or hostile.
pub const MAX_LINK_PREVIEW_TOKEN_BYTES: usize = 4096;

/// A successful (`status='ready'`) lookup: the send-time token plus the
/// metadata the composer may render as a pre-send preview card.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkPreviewLookupReady {
    /// Server-minted token redeemed via the message send options.
    pub token: LinkPreviewToken,
    /// Echo of the requested URL (semantic match enforced on parse).
    pub original_url: Url,
    /// Server-normalized form of the URL.
    pub normalized_url: Url,
    /// Token expiry; stale tokens must not be attached to sends.
    pub expires_at: DateTime<Utc>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// Server-cached preview image (trusted-path check applied on parse).
    pub image: Option<LinkPreviewImageData>,
    /// `og:video` iframe player advertised by the page.
    pub player_embed: Option<LinkPreviewPlayer>,
}

/// Typed outcome of a link-preview lookup IQ.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkPreviewLookup {
    /// Preview resolved; token available until `expires_at`.
    Ready(Box<LinkPreviewLookupReady>),
    /// The URL cannot be previewed (unparsable, unsupported scheme, …).
    Unsupported,
    /// The server's resolver is disabled or refused the URL.
    Blocked,
    /// Resolution failed, or the response payload was malformed.
    Failed,
}

/// Build the lookup IQ. No `to` attribute: RFC 6120 §10.3 routes an
/// IQ without `to` to the user's own server, which is where the
/// resolver lives.
pub fn build_link_preview_lookup_iq(url: &Url, scope: &BareJid) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            Uuid::new_v4().to_string(),
        )
        .append(
            Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
                .append(
                    Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                        .append(url.as_str())
                        .build(),
                )
                .append(
                    Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                        .append(scope.as_str())
                        .build(),
                )
                .build(),
        )
        .build()
}

/// Parse the IQ result for a lookup of `requested_url`. Anything that
/// does not validate — missing/oversized token, URL that is not a
/// semantic match for the request, non-HTTPS preview URLs — collapses
/// to [`LinkPreviewLookup::Failed`] (web `parseLookupResponse` parity:
/// a malformed ready payload yields no preview).
pub fn parse_link_preview_lookup_response(iq: &Element, requested_url: &Url) -> LinkPreviewLookup {
    let Some(lookup) = find_lookup_element(iq) else {
        return LinkPreviewLookup::Failed;
    };
    match lookup.attr("status") {
        Some("unsupported") => LinkPreviewLookup::Unsupported,
        Some("blocked") => LinkPreviewLookup::Blocked,
        Some("ready") => parse_ready_preview(lookup, requested_url)
            .map(|ready| LinkPreviewLookup::Ready(Box::new(ready)))
            .unwrap_or(LinkPreviewLookup::Failed),
        _ => LinkPreviewLookup::Failed,
    }
}

fn find_lookup_element(iq: &Element) -> Option<&Element> {
    if iq.name() == "lookup" && iq.ns() == NS_WADDLE_LINK_PREVIEW {
        return Some(iq);
    }
    iq.get_child("lookup", NS_WADDLE_LINK_PREVIEW)
}

fn parse_ready_preview(lookup: &Element, requested_url: &Url) -> Option<LinkPreviewLookupReady> {
    let preview = lookup.get_child("preview", NS_WADDLE_LINK_PREVIEW)?;
    let token = preview.attr("token").map(str::trim)?;
    if token.len() > MAX_LINK_PREVIEW_TOKEN_BYTES {
        return None;
    }
    let token = LinkPreviewToken::new(token).ok()?;
    let original_url = eligible_preview_url(preview.attr("original-url")?)?;
    if original_url != *requested_url {
        return None;
    }
    let normalized_url = eligible_preview_url(preview.attr("normalized-url")?)?;
    let expires_at = DateTime::parse_from_rfc3339(preview.attr("expires-at")?.trim())
        .ok()?
        .with_timezone(&Utc);
    Some(LinkPreviewLookupReady {
        token,
        original_url,
        normalized_url,
        expires_at,
        title: child_text(preview, "title"),
        description: child_text(preview, "description"),
        image: preview
            .get_child("image", NS_WADDLE_LINK_PREVIEW)
            .and_then(parse_lookup_image),
        player_embed: preview
            .get_child("player", NS_WADDLE_LINK_PREVIEW)
            .and_then(parse_lookup_player),
    })
}

/// Web `isEligiblePreviewUrl` parity: HTTPS with a dotted hostname.
/// Also the pre-flight gate callers apply before issuing a lookup.
pub fn eligible_preview_url(value: &str) -> Option<Url> {
    let url = Url::parse(value.trim()).ok()?;
    (url.scheme() == "https" && url.host_str().is_some_and(|host| host.contains('.')))
        .then_some(url)
}

fn child_text(preview: &Element, name: &str) -> Option<String> {
    preview
        .get_child(name, NS_WADDLE_LINK_PREVIEW)
        .map(|child| child.text().trim().to_string())
        .filter(|text| !text.is_empty())
}

/// `<image url media-type [width] [height] [alt]/>` — the URL must be a
/// server-cached XEP-0363 `link-preview-…` path (the same rule inbound
/// message previews enforce), never a third-party origin.
fn parse_lookup_image(el: &Element) -> Option<LinkPreviewImageData> {
    let url = parse_cached_preview_image_url(el.attr("url")?)?;
    let media_type = el
        .attr("media-type")?
        .parse::<PreviewImageMediaType>()
        .ok()?;
    Some(LinkPreviewImageData {
        url,
        media_type,
        width: el.attr("width").and_then(|value| value.parse().ok()),
        height: el.attr("height").and_then(|value| value.parse().ok()),
        alt: el
            .attr("alt")
            .map(str::trim)
            .filter(|alt| !alt.is_empty())
            .map(str::to_owned),
    })
}

/// `<player url [width] [height]/>` — HTTPS only; whether the origin is
/// allowed to embed stays a UI-layer decision (web allowlist parity).
fn parse_lookup_player(el: &Element) -> Option<LinkPreviewPlayer> {
    let url = eligible_preview_url(el.attr("url")?)?;
    Some(LinkPreviewPlayer {
        url,
        width: el.attr("width").and_then(|value| value.parse().ok()),
        height: el.attr("height").and_then(|value| value.parse().ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requested() -> Url {
        Url::parse("https://example.com/article").expect("test url")
    }

    fn preview_builder() -> minidom::ElementBuilder {
        Element::builder("preview", NS_WADDLE_LINK_PREVIEW)
            .attr(minidom::rxml::xml_ncname!("token").to_owned(), "tok-1")
            .attr(
                minidom::rxml::xml_ncname!("original-url").to_owned(),
                "https://example.com/article",
            )
            .attr(
                minidom::rxml::xml_ncname!("normalized-url").to_owned(),
                "https://example.com/article",
            )
            .attr(
                minidom::rxml::xml_ncname!("expires-at").to_owned(),
                "2026-07-16T12:00:00Z",
            )
    }

    fn lookup_result(preview: Option<Element>) -> Element {
        let mut lookup = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .attr(minidom::rxml::xml_ncname!("status").to_owned(), "ready");
        if let Some(preview) = preview {
            lookup = lookup.append(preview);
        }
        Element::builder("iq", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "lookup-1")
            .append(lookup.build())
            .build()
    }

    fn status_result(status: &str) -> Element {
        Element::builder("iq", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "lookup-1")
            .append(
                Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
                    .attr(minidom::rxml::xml_ncname!("status").to_owned(), status)
                    .build(),
            )
            .build()
    }

    #[test]
    fn build_iq_has_get_type_id_and_no_to() {
        let scope: BareJid = "room@conference.waddle.social".parse().expect("jid");
        let iq = build_link_preview_lookup_iq(&requested(), &scope);
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.attr("type"), Some("get"));
        assert!(iq.attr("id").is_some_and(|id| !id.is_empty()));
        assert_eq!(iq.attr("to"), None);
    }

    #[test]
    fn build_iq_carries_url_and_scope_children() {
        let scope: BareJid = "peer@waddle.social".parse().expect("jid");
        let iq = build_link_preview_lookup_iq(&requested(), &scope);
        let lookup = iq
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup child");
        let url = lookup
            .get_child("url", NS_WADDLE_LINK_PREVIEW)
            .expect("url child");
        assert_eq!(url.text(), "https://example.com/article");
        let scope_el = lookup
            .get_child("scope", NS_WADDLE_LINK_PREVIEW)
            .expect("scope child");
        assert_eq!(scope_el.text(), "peer@waddle.social");
    }

    #[test]
    fn parses_ready_preview_with_metadata() {
        let preview = preview_builder()
            .append(
                Element::builder("title", NS_WADDLE_LINK_PREVIEW)
                    .append("An Article")
                    .build(),
            )
            .append(
                Element::builder("description", NS_WADDLE_LINK_PREVIEW)
                    .append("Summary text")
                    .build(),
            )
            .append(
                Element::builder("image", NS_WADDLE_LINK_PREVIEW)
                    .attr(
                        minidom::rxml::xml_ncname!("url").to_owned(),
                        "https://xmpp.waddle.social/api/files/6a1f0f4e-8d24-45a3-9f7a-0b3c8f0d9e2a/link-preview-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.png",
                    )
                    .attr(
                        minidom::rxml::xml_ncname!("media-type").to_owned(),
                        "image/png",
                    )
                    .attr(minidom::rxml::xml_ncname!("width").to_owned(), "640")
                    .attr(minidom::rxml::xml_ncname!("height").to_owned(), "480")
                    .attr(minidom::rxml::xml_ncname!("alt").to_owned(), "Alt text")
                    .build(),
            )
            .append(
                Element::builder("player", NS_WADDLE_LINK_PREVIEW)
                    .attr(
                        minidom::rxml::xml_ncname!("url").to_owned(),
                        "https://www.youtube-nocookie.com/embed/abc123",
                    )
                    .attr(minidom::rxml::xml_ncname!("width").to_owned(), "1280")
                    .attr(minidom::rxml::xml_ncname!("height").to_owned(), "720")
                    .build(),
            )
            .build();
        let LinkPreviewLookup::Ready(ready) =
            parse_link_preview_lookup_response(&lookup_result(Some(preview)), &requested())
        else {
            panic!("expected ready lookup");
        };
        assert_eq!(ready.token.as_str(), "tok-1");
        assert_eq!(ready.original_url, requested());
        assert_eq!(ready.title.as_deref(), Some("An Article"));
        assert_eq!(ready.description.as_deref(), Some("Summary text"));
        let image = ready.image.expect("image");
        assert_eq!(image.width, Some(640));
        assert_eq!(image.height, Some(480));
        assert_eq!(image.alt.as_deref(), Some("Alt text"));
        let player = ready.player_embed.expect("player");
        assert_eq!(player.width, Some(1280));
        assert_eq!(player.height, Some(720));
        assert_eq!(
            ready.expires_at,
            DateTime::parse_from_rfc3339("2026-07-16T12:00:00Z")
                .expect("ts")
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn parses_terminal_statuses() {
        assert_eq!(
            parse_link_preview_lookup_response(&status_result("unsupported"), &requested()),
            LinkPreviewLookup::Unsupported
        );
        assert_eq!(
            parse_link_preview_lookup_response(&status_result("blocked"), &requested()),
            LinkPreviewLookup::Blocked
        );
        assert_eq!(
            parse_link_preview_lookup_response(&status_result("failed"), &requested()),
            LinkPreviewLookup::Failed
        );
        assert_eq!(
            parse_link_preview_lookup_response(&status_result("bogus"), &requested()),
            LinkPreviewLookup::Failed
        );
    }

    #[test]
    fn ready_without_preview_child_fails() {
        assert_eq!(
            parse_link_preview_lookup_response(&lookup_result(None), &requested()),
            LinkPreviewLookup::Failed
        );
    }

    #[test]
    fn oversized_token_fails() {
        let oversized = "x".repeat(MAX_LINK_PREVIEW_TOKEN_BYTES + 1);
        let preview = Element::builder("preview", NS_WADDLE_LINK_PREVIEW)
            .attr(minidom::rxml::xml_ncname!("token").to_owned(), oversized)
            .attr(
                minidom::rxml::xml_ncname!("original-url").to_owned(),
                "https://example.com/article",
            )
            .attr(
                minidom::rxml::xml_ncname!("normalized-url").to_owned(),
                "https://example.com/article",
            )
            .attr(
                minidom::rxml::xml_ncname!("expires-at").to_owned(),
                "2026-07-16T12:00:00Z",
            )
            .build();
        assert_eq!(
            parse_link_preview_lookup_response(&lookup_result(Some(preview)), &requested()),
            LinkPreviewLookup::Failed
        );
    }

    #[test]
    fn token_at_cap_still_parses() {
        let max_token = "x".repeat(MAX_LINK_PREVIEW_TOKEN_BYTES);
        let preview = preview_builder()
            .attr(minidom::rxml::xml_ncname!("token").to_owned(), max_token)
            .build();
        assert!(matches!(
            parse_link_preview_lookup_response(&lookup_result(Some(preview)), &requested()),
            LinkPreviewLookup::Ready(_)
        ));
    }

    #[test]
    fn url_semantic_mismatch_fails() {
        let other = Url::parse("https://example.com/other").expect("url");
        let preview = preview_builder().build();
        assert_eq!(
            parse_link_preview_lookup_response(&lookup_result(Some(preview)), &other),
            LinkPreviewLookup::Failed
        );
    }

    #[test]
    fn url_semantic_match_survives_normalization() {
        // Url::parse normalizes the default port away on both sides.
        let requested = Url::parse("https://example.com:443/article").expect("url");
        let preview = preview_builder().build();
        assert!(matches!(
            parse_link_preview_lookup_response(&lookup_result(Some(preview)), &requested),
            LinkPreviewLookup::Ready(_)
        ));
    }

    #[test]
    fn non_https_urls_fail() {
        let preview = preview_builder()
            .attr(
                minidom::rxml::xml_ncname!("normalized-url").to_owned(),
                "http://example.com/article",
            )
            .build();
        assert_eq!(
            parse_link_preview_lookup_response(&lookup_result(Some(preview)), &requested()),
            LinkPreviewLookup::Failed
        );
    }

    #[test]
    fn untrusted_image_path_is_dropped_but_lookup_stays_ready() {
        let preview = preview_builder()
            .append(
                Element::builder("image", NS_WADDLE_LINK_PREVIEW)
                    .attr(
                        minidom::rxml::xml_ncname!("url").to_owned(),
                        "https://evil.example/track.png",
                    )
                    .attr(
                        minidom::rxml::xml_ncname!("media-type").to_owned(),
                        "image/png",
                    )
                    .build(),
            )
            .build();
        let LinkPreviewLookup::Ready(ready) =
            parse_link_preview_lookup_response(&lookup_result(Some(preview)), &requested())
        else {
            panic!("expected ready lookup");
        };
        assert_eq!(ready.image, None);
    }

    #[test]
    fn unsupported_image_media_type_is_dropped() {
        let preview = preview_builder()
            .append(
                Element::builder("image", NS_WADDLE_LINK_PREVIEW)
                    .attr(
                        minidom::rxml::xml_ncname!("url").to_owned(),
                        "https://xmpp.waddle.social/api/files/6a1f0f4e-8d24-45a3-9f7a-0b3c8f0d9e2a/link-preview-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.png",
                    )
                    .attr(
                        minidom::rxml::xml_ncname!("media-type").to_owned(),
                        "image/svg+xml",
                    )
                    .build(),
            )
            .build();
        let LinkPreviewLookup::Ready(ready) =
            parse_link_preview_lookup_response(&lookup_result(Some(preview)), &requested())
        else {
            panic!("expected ready lookup");
        };
        assert_eq!(ready.image, None);
    }

    #[test]
    fn missing_expiry_fails() {
        let preview = Element::builder("preview", NS_WADDLE_LINK_PREVIEW)
            .attr(minidom::rxml::xml_ncname!("token").to_owned(), "tok-1")
            .attr(
                minidom::rxml::xml_ncname!("original-url").to_owned(),
                "https://example.com/article",
            )
            .attr(
                minidom::rxml::xml_ncname!("normalized-url").to_owned(),
                "https://example.com/article",
            )
            .build();
        assert_eq!(
            parse_link_preview_lookup_response(&lookup_result(Some(preview)), &requested()),
            LinkPreviewLookup::Failed
        );
    }
}
