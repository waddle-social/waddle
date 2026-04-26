//! XEP-0084: User Avatar — PEP-based avatar fetch.
//!
//! Implements a request-based client flow: given a bare JID, issue a
//! `pubsub#items` IQ for the `urn:xmpp:avatar:metadata` node to learn the
//! current avatar hash and MIME type, then fetch the matching item from the
//! `urn:xmpp:avatar:data` node and base64-decode its payload.
//!
//! Typed payloads only — JIDs use [`BareJid`], errors use [`ClientError`], and
//! the raw image bytes live in [`Avatar`].

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use jid::BareJid;
use minidom::Element;
use uuid::Uuid;

use crate::client::ClientHandle;
use crate::error::{ClientError, ClientResult};
use tracing::warn;

pub const NS_AVATAR_DATA: &str = "urn:xmpp:avatar:data";
pub const NS_AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";
pub const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

const NS_CLIENT: &str = "jabber:client";

/// Metadata advertised on the `urn:xmpp:avatar:metadata` PEP node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarInfo {
    /// SHA-1 hash of the image data, hex-encoded — also the pubsub item id.
    pub id: String,
    /// MIME type of the image (e.g. `image/png`).
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bytes: Option<u64>,
    pub url: Option<String>,
}

/// A fetched user avatar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avatar {
    /// The JID whose avatar this is.
    pub jid: BareJid,
    /// SHA-1 hash of the bytes (as published on the metadata node).
    pub id: String,
    /// MIME type (e.g. `image/png`).
    pub mime_type: String,
    /// Raw image bytes (base64-decoded).
    pub data: Vec<u8>,
}

// ── IQ builders ──────────────────────────────────────────────────────────────

/// Build a pubsub `items` IQ requesting the latest avatar metadata item.
fn build_metadata_request_iq(to: &BareJid) -> Element {
    let id = format!("avatar-meta-{}", Uuid::new_v4());
    let items = Element::builder("items", NS_PUBSUB)
        .attr("node", NS_AVATAR_METADATA)
        .attr("max_items", "1")
        .build();

    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();

    Element::builder("iq", NS_CLIENT)
        .attr("type", "get")
        .attr("to", to.to_string())
        .attr("id", id)
        .append(pubsub)
        .build()
}

/// Build a pubsub `items` IQ requesting a specific avatar-data item by id.
fn build_data_request_iq(to: &BareJid, item_id: &str) -> Element {
    let id = format!("avatar-data-{}", Uuid::new_v4());
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", item_id)
        .build();
    let items = Element::builder("items", NS_PUBSUB)
        .attr("node", NS_AVATAR_DATA)
        .append(item)
        .build();

    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();

    Element::builder("iq", NS_CLIENT)
        .attr("type", "get")
        .attr("to", to.to_string())
        .attr("id", id)
        .append(pubsub)
        .build()
}

// ── Response parsers ─────────────────────────────────────────────────────────

/// Parse a pubsub-items IQ result carrying an avatar-metadata payload.
/// Returns `None` if the node is empty (no avatar published).
fn parse_metadata_response(iq: &Element) -> Option<AvatarInfo> {
    let pubsub = iq.get_child("pubsub", NS_PUBSUB)?;
    let items = pubsub.get_child("items", NS_PUBSUB)?;
    if items.attr("node")? != NS_AVATAR_METADATA {
        return None;
    }
    let item = items.get_child("item", NS_PUBSUB)?;
    let metadata = item.get_child("metadata", NS_AVATAR_METADATA)?;
    let info = metadata
        .children()
        .find(|c| c.name() == "info" && c.ns() == NS_AVATAR_METADATA)?;

    let id = info.attr("id")?.to_string();
    let mime_type = info.attr("type").unwrap_or("image/png").to_string();
    let width = info.attr("width").and_then(|v| v.parse().ok());
    let height = info.attr("height").and_then(|v| v.parse().ok());
    let bytes = info.attr("bytes").and_then(|v| v.parse().ok());
    let url = info.attr("url").map(str::to_string);

    Some(AvatarInfo {
        id,
        mime_type,
        width,
        height,
        bytes,
        url,
    })
}

/// Parse a pubsub-items IQ result carrying an avatar-data payload.
/// Returns the base64 text content of the `<data>` child.
fn parse_data_response(iq: &Element) -> Option<String> {
    let pubsub = iq.get_child("pubsub", NS_PUBSUB)?;
    let items = pubsub.get_child("items", NS_PUBSUB)?;
    if items.attr("node")? != NS_AVATAR_DATA {
        return None;
    }
    let item = items.get_child("item", NS_PUBSUB)?;
    let data = item.get_child("data", NS_AVATAR_DATA)?;
    let text = data.text();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

// ── AvatarExt trait ──────────────────────────────────────────────────────────

/// High-level avatar operations on a connected client.
pub trait AvatarExt {
    /// Fetch the published avatar for the given JID, if any.
    ///
    /// Issues two IQ round-trips: one for metadata, one for the image bytes.
    /// Returns `Ok(None)` when the JID has not published a `urn:xmpp:avatar`
    /// metadata item. Any transport / protocol error surfaces as `Err`.
    fn request_avatar<'a>(
        &'a self,
        jid: &'a BareJid,
    ) -> impl std::future::Future<Output = ClientResult<Option<Avatar>>> + Send + 'a;
}

impl AvatarExt for ClientHandle {
    async fn request_avatar(&self, jid: &BareJid) -> ClientResult<Option<Avatar>> {
        let meta_iq = build_metadata_request_iq(jid);
        let meta_response = match self.send_iq(meta_iq).await {
            Ok(elem) => elem,
            // A JID with no published avatar typically returns
            // `item-not-found`; treat any stanza-level error as "no avatar"
            // rather than bubbling up a hard failure.
            Err(ClientError::StanzaError(_)) => return Ok(None),
            Err(e) => return Err(e),
        };

        let Some(info) = parse_metadata_response(&meta_response) else {
            return Ok(None);
        };

        // XEP-0084 §4.2: `<info url=".."/>` advertises an externally-hosted
        // avatar. When present, fetch bytes over HTTPS instead of querying
        // the data node — publishers that use `url` typically don't populate
        // `urn:xmpp:avatar:data` at all. Transport failures surface as
        // `Ok(None)` so a flaky avatar CDN doesn't break the chat UI.
        if let Some(url) = info.url.as_deref() {
            return Ok(fetch_avatar_url(jid, &info, url).await);
        }

        let data_iq = build_data_request_iq(jid, &info.id);
        let data_response = match self.send_iq(data_iq).await {
            Ok(elem) => elem,
            Err(ClientError::StanzaError(_)) => return Ok(None),
            Err(e) => return Err(e),
        };

        let Some(base64_text) = parse_data_response(&data_response) else {
            return Ok(None);
        };

        // Strip whitespace that servers sometimes interleave in long base64
        // payloads before decoding. Malformed base64 is treated as "no
        // avatar" (the data is unusable) rather than propagating the error —
        // we log and move on so a single bad publisher cannot block the
        // rest of the UI.
        let cleaned: String = base64_text.chars().filter(|c| !c.is_whitespace()).collect();
        let data = match BASE64_STANDARD.decode(cleaned.as_bytes()) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!(jid = %jid, error = %e, "avatar data base64 decode failed");
                return Ok(None);
            }
        };

        Ok(Some(Avatar {
            jid: jid.clone(),
            id: info.id,
            mime_type: info.mime_type,
            data,
        }))
    }
}

/// HTTP-fetch the bytes advertised by a XEP-0084 `url=` metadata entry.
///
/// Only `https://` URLs are honoured — `http://` entries are skipped so we
/// never downgrade user avatars to plaintext transport. Any error (scheme,
/// network, HTTP status, body read) is logged and collapsed to `None` so the
/// UI falls back to initials rather than surfacing a hard error for what is
/// cosmetic data.
async fn fetch_avatar_url(jid: &BareJid, info: &AvatarInfo, url: &str) -> Option<Avatar> {
    if !url.starts_with("https://") {
        warn!(jid = %jid, url, "avatar url is not https, skipping");
        return None;
    }

    let response = match reqwest::get(url).await {
        Ok(r) => r,
        Err(e) => {
            warn!(jid = %jid, url, error = %e, "avatar url fetch failed");
            return None;
        }
    };

    if !response.status().is_success() {
        warn!(
            jid = %jid,
            url,
            status = %response.status(),
            "avatar url fetch returned non-success"
        );
        return None;
    }

    let bytes = match response.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            warn!(jid = %jid, url, error = %e, "avatar url body read failed");
            return None;
        }
    };

    Some(Avatar {
        jid: jid.clone(),
        id: info.id.clone(),
        mime_type: info.mime_type.clone(),
        data: bytes,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_metadata_iq(id: &str, mime: &str) -> Element {
        let info = Element::builder("info", NS_AVATAR_METADATA)
            .attr("id", id)
            .attr("type", mime)
            .attr("bytes", "42")
            .attr("width", "64")
            .attr("height", "64")
            .build();
        let metadata = Element::builder("metadata", NS_AVATAR_METADATA)
            .append(info)
            .build();
        let item = Element::builder("item", NS_PUBSUB)
            .attr("id", id)
            .append(metadata)
            .build();
        let items = Element::builder("items", NS_PUBSUB)
            .attr("node", NS_AVATAR_METADATA)
            .append(item)
            .build();
        let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
        Element::builder("iq", NS_CLIENT)
            .attr("type", "result")
            .attr("id", "abc")
            .append(pubsub)
            .build()
    }

    fn make_data_iq(id: &str, base64_data: &str) -> Element {
        let data = Element::builder("data", NS_AVATAR_DATA)
            .append(minidom::Node::Text(base64_data.to_string()))
            .build();
        let item = Element::builder("item", NS_PUBSUB)
            .attr("id", id)
            .append(data)
            .build();
        let items = Element::builder("items", NS_PUBSUB)
            .attr("node", NS_AVATAR_DATA)
            .append(item)
            .build();
        let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
        Element::builder("iq", NS_CLIENT)
            .attr("type", "result")
            .attr("id", "abc")
            .append(pubsub)
            .build()
    }

    #[test]
    fn parse_metadata_extracts_info() {
        let iq = make_metadata_iq("deadbeef", "image/png");
        let info = parse_metadata_response(&iq).expect("info");
        assert_eq!(info.id, "deadbeef");
        assert_eq!(info.mime_type, "image/png");
        assert_eq!(info.width, Some(64));
        assert_eq!(info.height, Some(64));
        assert_eq!(info.bytes, Some(42));
    }

    #[test]
    fn parse_metadata_returns_none_without_info() {
        let empty_items = Element::builder("items", NS_PUBSUB)
            .attr("node", NS_AVATAR_METADATA)
            .build();
        let pubsub = Element::builder("pubsub", NS_PUBSUB)
            .append(empty_items)
            .build();
        let iq = Element::builder("iq", NS_CLIENT)
            .attr("type", "result")
            .attr("id", "x")
            .append(pubsub)
            .build();
        assert!(parse_metadata_response(&iq).is_none());
    }

    #[test]
    fn parse_metadata_rejects_wrong_node() {
        let items = Element::builder("items", NS_PUBSUB)
            .attr("node", "some:other:node")
            .build();
        let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
        let iq = Element::builder("iq", NS_CLIENT).append(pubsub).build();
        assert!(parse_metadata_response(&iq).is_none());
    }

    #[test]
    fn parse_data_extracts_base64() {
        let iq = make_data_iq("deadbeef", "aGVsbG8=");
        let text = parse_data_response(&iq).expect("text");
        assert_eq!(text, "aGVsbG8=");
    }

    #[test]
    fn parse_data_returns_none_for_empty() {
        let iq = make_data_iq("deadbeef", "");
        assert!(parse_data_response(&iq).is_none());
    }

    #[test]
    fn build_metadata_request_has_correct_shape() {
        let jid: BareJid = "alice@example.com".parse().unwrap();
        let iq = build_metadata_request_iq(&jid);
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("alice@example.com"));
        let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub");
        let items = pubsub.get_child("items", NS_PUBSUB).expect("items");
        assert_eq!(items.attr("node"), Some(NS_AVATAR_METADATA));
        assert_eq!(items.attr("max_items"), Some("1"));
    }

    #[test]
    fn build_data_request_includes_item_id() {
        let jid: BareJid = "bob@example.com".parse().unwrap();
        let iq = build_data_request_iq(&jid, "cafef00d");
        let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub");
        let items = pubsub.get_child("items", NS_PUBSUB).expect("items");
        assert_eq!(items.attr("node"), Some(NS_AVATAR_DATA));
        let item = items.get_child("item", NS_PUBSUB).expect("item");
        assert_eq!(item.attr("id"), Some("cafef00d"));
    }
}
