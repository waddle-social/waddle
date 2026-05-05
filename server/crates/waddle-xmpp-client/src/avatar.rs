//! XEP-0084: User Avatar and XEP-0054 vCard PHOTO avatar fetch.
//!
//! Implements a request-based client flow: given a bare JID, issue a
//! `pubsub#items` IQ for the `urn:xmpp:avatar:metadata` node to learn the
//! current avatar hash and MIME type, then fetch the matching item from the
//! `urn:xmpp:avatar:data` node and base64-decode its payload. If XEP-0084 is
//! unavailable or unusable, fall back to the XEP-0054 `vcard-temp` `PHOTO`
//! shape, supporting both `BINVAL` bytes and `EXTVAL` URLs.
//!
//! Typed payloads only — JIDs use [`BareJid`], errors use [`ClientError`], and
//! the raw image bytes / URL live in [`Avatar`].

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use jid::BareJid;
use minidom::Element;
use std::future::Future;
use uuid::Uuid;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::client::ClientHandle;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::error::{ClientError, ClientResult};
use tracing::warn;

pub const NS_AVATAR_DATA: &str = "urn:xmpp:avatar:data";
pub const NS_AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";
pub const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
pub const NS_VCARD_TEMP: &str = "vcard-temp";

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
    /// Raw image bytes (base64-decoded), if carried by XMPP.
    pub data: Vec<u8>,
    /// HTTP(S) avatar URL, if the avatar is externally hosted.
    pub url: Option<String>,
}

/// vCard `PHOTO` payload from XEP-0054.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcardPhoto {
    pub mime_type: Option<String>,
    pub data: Option<Vec<u8>>,
    pub url: Option<String>,
}

/// Error classification for request flows that distinguish stanza-level
/// "not available" failures from transport/runtime failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvatarRequestFailure<E> {
    StanzaError,
    Other(E),
}

// ── IQ builders ──────────────────────────────────────────────────────────────

/// Build a pubsub `items` IQ requesting the latest avatar metadata item.
pub fn build_metadata_request_iq(to: &BareJid) -> Element {
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
pub fn build_data_request_iq(to: &BareJid, item_id: &str) -> Element {
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

/// Build a vCard request IQ for XEP-0054 `PHOTO` fallback.
pub fn build_vcard_request_iq(to: &BareJid) -> Element {
    let id = format!("avatar-vcard-{}", Uuid::new_v4());
    let vcard = Element::builder("vCard", NS_VCARD_TEMP).build();

    Element::builder("iq", NS_CLIENT)
        .attr("type", "get")
        .attr("to", to.to_string())
        .attr("id", id)
        .append(vcard)
        .build()
}

// ── Response parsers ─────────────────────────────────────────────────────────

/// Parse a pubsub-items IQ result carrying an avatar-metadata payload.
/// Returns `None` if the node is empty (no avatar published).
pub fn parse_metadata_response(iq: &Element) -> Option<AvatarInfo> {
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
pub fn parse_data_response(iq: &Element) -> Option<String> {
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

/// Parse a XEP-0054 vCard `PHOTO` fallback payload.
pub fn parse_vcard_photo_response(iq: &Element) -> Option<VcardPhoto> {
    let vcard = iq.get_child("vCard", NS_VCARD_TEMP)?;
    let photo = vcard.get_child("PHOTO", NS_VCARD_TEMP)?;

    if let Some(url) = photo
        .get_child("EXTVAL", NS_VCARD_TEMP)
        .map(Element::text)
        .filter(|text| !text.trim().is_empty())
    {
        return Some(VcardPhoto {
            mime_type: None,
            data: None,
            url: Some(url.trim().to_string()),
        });
    }

    let base64_text = photo.get_child("BINVAL", NS_VCARD_TEMP)?.text();
    let data = decode_base64_bytes(&base64_text)?;
    let mime_type = photo
        .get_child("TYPE", NS_VCARD_TEMP)
        .map(Element::text)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());

    Some(VcardPhoto {
        mime_type,
        data: Some(data),
        url: None,
    })
}

/// Request an avatar using XEP-0084 first, then XEP-0054 vCard PHOTO fallback.
pub async fn request_avatar_with_iq<F, Fut, E>(
    jid: &BareJid,
    mut send_iq: F,
) -> Result<Option<Avatar>, E>
where
    F: FnMut(Element) -> Fut,
    Fut: Future<Output = Result<Element, AvatarRequestFailure<E>>>,
{
    let meta_iq = build_metadata_request_iq(jid);
    let meta_response = match send_iq(meta_iq).await {
        Ok(elem) => Some(elem),
        Err(AvatarRequestFailure::StanzaError) => None,
        Err(AvatarRequestFailure::Other(e)) => return Err(e),
    };

    if let Some(meta_response) = meta_response {
        if let Some(info) = parse_metadata_response(&meta_response) {
            if let Some(url) = info.url.as_deref().filter(|url| !url.trim().is_empty()) {
                return Ok(Some(Avatar {
                    jid: jid.clone(),
                    id: info.id,
                    mime_type: info.mime_type,
                    data: Vec::new(),
                    url: Some(url.trim().to_string()),
                }));
            }

            let data_iq = build_data_request_iq(jid, &info.id);
            match send_iq(data_iq).await {
                Ok(data_response) => {
                    if let Some(base64_text) = parse_data_response(&data_response) {
                        if let Some(data) = decode_base64_bytes(&base64_text) {
                            return Ok(Some(Avatar {
                                jid: jid.clone(),
                                id: info.id,
                                mime_type: info.mime_type,
                                data,
                                url: None,
                            }));
                        }
                        warn!(jid = %jid, "avatar data base64 decode failed");
                    }
                }
                Err(AvatarRequestFailure::StanzaError) => {}
                Err(AvatarRequestFailure::Other(e)) => return Err(e),
            }
        }
    }

    request_vcard_avatar(jid, send_iq).await
}

async fn request_vcard_avatar<F, Fut, E>(jid: &BareJid, mut send_iq: F) -> Result<Option<Avatar>, E>
where
    F: FnMut(Element) -> Fut,
    Fut: Future<Output = Result<Element, AvatarRequestFailure<E>>>,
{
    let vcard_iq = build_vcard_request_iq(jid);
    let vcard_response = match send_iq(vcard_iq).await {
        Ok(elem) => elem,
        Err(AvatarRequestFailure::StanzaError) => return Ok(None),
        Err(AvatarRequestFailure::Other(e)) => return Err(e),
    };

    let Some(photo) = parse_vcard_photo_response(&vcard_response) else {
        return Ok(None);
    };

    Ok(vcard_photo_to_avatar(jid, photo))
}

fn vcard_photo_to_avatar(jid: &BareJid, photo: VcardPhoto) -> Option<Avatar> {
    if let Some(url) = photo.url.filter(|url| !url.trim().is_empty()) {
        let id = url.trim().to_string();
        return Some(Avatar {
            jid: jid.clone(),
            id,
            mime_type: photo.mime_type.unwrap_or_else(|| "image/png".to_string()),
            data: Vec::new(),
            url: Some(url.trim().to_string()),
        });
    }

    photo.data.map(|data| Avatar {
        jid: jid.clone(),
        id: "vcard-photo".to_string(),
        mime_type: photo.mime_type.unwrap_or_else(|| "image/png".to_string()),
        data,
        url: None,
    })
}

fn decode_base64_bytes(base64_text: &str) -> Option<Vec<u8>> {
    let cleaned: String = base64_text.chars().filter(|c| !c.is_whitespace()).collect();
    BASE64_STANDARD.decode(cleaned.as_bytes()).ok()
}

// ── AvatarExt trait ──────────────────────────────────────────────────────────

/// High-level avatar operations on a connected client.
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub trait AvatarExt {
    /// Fetch the published avatar for the given JID, if any.
    ///
    /// Issues XEP-0084 IQ round-trips first and falls back to XEP-0054 vCard
    /// PHOTO when avatar metadata or data is unavailable.
    fn request_avatar<'a>(
        &'a self,
        jid: &'a BareJid,
    ) -> impl std::future::Future<Output = ClientResult<Option<Avatar>>> + Send + 'a;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl AvatarExt for ClientHandle {
    async fn request_avatar(&self, jid: &BareJid) -> ClientResult<Option<Avatar>> {
        request_avatar_with_iq(jid, |stanza| async move {
            self.send_iq(stanza).await.map_err(|error| match error {
                ClientError::StanzaError(_) => AvatarRequestFailure::StanzaError,
                other => AvatarRequestFailure::Other(other),
            })
        })
        .await
    }
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

    fn make_metadata_url_iq(id: &str, mime: &str, url: &str) -> Element {
        let info = Element::builder("info", NS_AVATAR_METADATA)
            .attr("id", id)
            .attr("type", mime)
            .attr("bytes", "42")
            .attr("url", url)
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

    fn make_vcard_binval_iq(mime: &str, base64_data: &str) -> Element {
        let photo = Element::builder("PHOTO", NS_VCARD_TEMP)
            .append(Element::builder("TYPE", NS_VCARD_TEMP).append(mime).build())
            .append(
                Element::builder("BINVAL", NS_VCARD_TEMP)
                    .append(base64_data)
                    .build(),
            )
            .build();
        let vcard = Element::builder("vCard", NS_VCARD_TEMP)
            .append(photo)
            .build();
        Element::builder("iq", NS_CLIENT)
            .attr("type", "result")
            .attr("id", "abc")
            .append(vcard)
            .build()
    }

    fn make_vcard_extval_iq(url: &str) -> Element {
        let photo = Element::builder("PHOTO", NS_VCARD_TEMP)
            .append(
                Element::builder("EXTVAL", NS_VCARD_TEMP)
                    .append(url)
                    .build(),
            )
            .build();
        let vcard = Element::builder("vCard", NS_VCARD_TEMP)
            .append(photo)
            .build();
        Element::builder("iq", NS_CLIENT)
            .attr("type", "result")
            .attr("id", "abc")
            .append(vcard)
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
        assert_eq!(info.url, None);
    }

    #[test]
    fn parse_metadata_extracts_url() {
        let iq = make_metadata_url_iq("deadbeef", "image/png", "https://example.test/a.png");
        let info = parse_metadata_response(&iq).expect("info");
        assert_eq!(info.url.as_deref(), Some("https://example.test/a.png"));
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
    fn parse_vcard_photo_extracts_binval_bytes() {
        let iq = make_vcard_binval_iq("image/jpeg", "aG Vs\n bG8=");
        let photo = parse_vcard_photo_response(&iq).expect("photo");
        assert_eq!(photo.mime_type.as_deref(), Some("image/jpeg"));
        assert_eq!(photo.data.as_deref(), Some(b"hello".as_slice()));
        assert_eq!(photo.url, None);
    }

    #[test]
    fn parse_vcard_photo_extracts_extval_url() {
        let iq = make_vcard_extval_iq("https://example.test/avatar.png");
        let photo = parse_vcard_photo_response(&iq).expect("photo");
        assert_eq!(photo.data, None);
        assert_eq!(
            photo.url.as_deref(),
            Some("https://example.test/avatar.png")
        );
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

    #[test]
    fn build_vcard_request_has_correct_shape() {
        let jid: BareJid = "bob@example.com".parse().unwrap();
        let iq = build_vcard_request_iq(&jid);
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("bob@example.com"));
        assert!(iq.get_child("vCard", NS_VCARD_TEMP).is_some());
    }

    #[test]
    fn request_avatar_prefers_xep_0084_data() {
        let jid: BareJid = "alice@example.com".parse().unwrap();
        let responses = std::cell::RefCell::new(vec![
            make_metadata_iq("deadbeef", "image/png"),
            make_data_iq("deadbeef", "aGVsbG8="),
            make_vcard_binval_iq("image/jpeg", "d29ybGQ="),
        ]);

        let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |stanza| {
            let response = responses.borrow_mut().remove(0);
            async move {
                assert_eq!(stanza.name(), "iq");
                Ok::<_, AvatarRequestFailure<()>>(response)
            }
        }))
        .unwrap()
        .expect("avatar");

        assert_eq!(avatar.id, "deadbeef");
        assert_eq!(avatar.mime_type, "image/png");
        assert_eq!(avatar.data, b"hello");
        assert_eq!(avatar.url, None);
        assert_eq!(responses.borrow().len(), 1);
    }

    #[test]
    fn request_avatar_returns_xep_0084_url_without_data_request() {
        let jid: BareJid = "alice@example.com".parse().unwrap();
        let responses = std::cell::RefCell::new(vec![make_metadata_url_iq(
            "deadbeef",
            "image/png",
            "https://example.test/a.png",
        )]);

        let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |_stanza| {
            let response = responses.borrow_mut().remove(0);
            async move { Ok::<_, AvatarRequestFailure<()>>(response) }
        }))
        .unwrap()
        .expect("avatar");

        assert_eq!(avatar.id, "deadbeef");
        assert_eq!(avatar.data, Vec::<u8>::new());
        assert_eq!(avatar.url.as_deref(), Some("https://example.test/a.png"));
        assert!(responses.borrow().is_empty());
    }

    #[test]
    fn request_avatar_falls_back_to_vcard_binval() {
        let jid: BareJid = "alice@example.com".parse().unwrap();
        let responses =
            std::cell::RefCell::new(vec![make_vcard_binval_iq("image/jpeg", "d29ybGQ=")]);

        let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |stanza| {
            let is_metadata = stanza
                .get_child("pubsub", NS_PUBSUB)
                .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
                .is_some_and(|items| items.attr("node") == Some(NS_AVATAR_METADATA));
            let response = (!is_metadata).then(|| responses.borrow_mut().remove(0));
            async move {
                match response {
                    Some(response) => Ok::<_, AvatarRequestFailure<()>>(response),
                    None => Err(AvatarRequestFailure::StanzaError),
                }
            }
        }))
        .unwrap()
        .expect("avatar");

        assert_eq!(avatar.id, "vcard-photo");
        assert_eq!(avatar.mime_type, "image/jpeg");
        assert_eq!(avatar.data, b"world");
        assert_eq!(avatar.url, None);
    }

    #[test]
    fn request_avatar_falls_back_to_vcard_extval() {
        let jid: BareJid = "alice@example.com".parse().unwrap();
        let responses =
            std::cell::RefCell::new(vec![make_vcard_extval_iq("https://example.test/vcard.png")]);

        let avatar = futures::executor::block_on(request_avatar_with_iq(&jid, |stanza| {
            let is_metadata = stanza
                .get_child("pubsub", NS_PUBSUB)
                .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
                .is_some_and(|items| items.attr("node") == Some(NS_AVATAR_METADATA));
            let response = (!is_metadata).then(|| responses.borrow_mut().remove(0));
            async move {
                match response {
                    Some(response) => Ok::<_, AvatarRequestFailure<()>>(response),
                    None => Err(AvatarRequestFailure::StanzaError),
                }
            }
        }))
        .unwrap()
        .expect("avatar");

        assert_eq!(avatar.data, Vec::<u8>::new());
        assert_eq!(
            avatar.url.as_deref(),
            Some("https://example.test/vcard.png")
        );
    }
}
