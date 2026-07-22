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
//!
//! Publishing (XEP-0084 §3) is builder-only here: callers compute the SHA-1
//! item id with [`compute_avatar_item_id`], publish the data item first via
//! [`build_publish_avatar_data_iq`], and only then the metadata item via
//! [`build_publish_avatar_metadata_iq`] — the §3.2 data-before-metadata order
//! is enforced by the calling boundary (FFI/wasm), which owns the IQ acks.

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use jid::BareJid;
use minidom::Element;
use sha1::{Digest, Sha1};
use std::future::Future;
use url::Url;
use uuid::Uuid;

use crate::pep::build_pep_publish_iq_with_item_id;

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

/// XEP-0084 §4.3 removal convention: the empty `<metadata/>` "no avatar"
/// item is published under this fixed item id (mirrors the server's
/// `AVATAR_METADATA_REMOVE_ITEM_ID`).
pub const AVATAR_REMOVE_ITEM_ID: &str = "current";

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
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            NS_AVATAR_METADATA,
        )
        .attr(minidom::rxml::xml_ncname!("max_items").to_owned(), "1")
        .build();

    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();

    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.to_string())
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(pubsub)
        .build()
}

/// Build a pubsub `items` IQ requesting a specific avatar-data item by id.
pub fn build_data_request_iq(to: &BareJid, item_id: &str) -> Element {
    let id = format!("avatar-data-{}", Uuid::new_v4());
    let item = Element::builder("item", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id)
        .build();
    let items = Element::builder("items", NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            NS_AVATAR_DATA,
        )
        .append(item)
        .build();

    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();

    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.to_string())
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(pubsub)
        .build()
}

/// Build a vCard request IQ for XEP-0054 `PHOTO` fallback.
pub fn build_vcard_request_iq(to: &BareJid) -> Element {
    let id = format!("avatar-vcard-{}", Uuid::new_v4());
    let vcard = Element::builder("vCard", NS_VCARD_TEMP).build();

    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.to_string())
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(vcard)
        .build()
}

// ── Publish builders (XEP-0084 §3) ───────────────────────────────────────────

/// Metadata for an avatar about to be published. Unlike the fetch-side
/// [`AvatarInfo`], the attributes XEP-0084 §4.2.1 marks REQUIRED on
/// `<info/>` (`bytes`, `id`, `type`) are non-optional here so a publish
/// can never omit them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarPublishInfo {
    /// Image size in bytes (REQUIRED).
    pub bytes: u32,
    /// SHA-1 hash of the image bytes, hex-encoded — also the item id (REQUIRED).
    pub id: String,
    /// MIME type, e.g. `image/png` (REQUIRED).
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Compute the XEP-0084 §3.1 pubsub item id: the SHA-1 hash of the raw
/// image bytes, lowercase hex-encoded.
pub fn compute_avatar_item_id(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(40), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Build the §3.2 avatar-data publish IQ: `<data/>` carries the RFC 4648
/// §4 base64 of the raw image bytes, published at `item_id` (the SHA-1
/// hex of those bytes, from [`compute_avatar_item_id`]).
pub fn build_publish_avatar_data_iq(item_id: &str, data: &[u8]) -> Element {
    let id = format!("avatar-publish-data-{}", Uuid::new_v4());
    let payload = Element::builder("data", NS_AVATAR_DATA)
        .append(BASE64_STANDARD.encode(data))
        .build();
    build_pep_publish_iq_with_item_id(&id, NS_AVATAR_DATA, item_id, payload)
}

/// Build the §3.3 avatar-metadata publish IQ: `<metadata><info/></metadata>`
/// with the REQUIRED `bytes`/`id`/`type` attributes plus `width`/`height`
/// when known, published at the same SHA-1 item id as the data item.
pub fn build_publish_avatar_metadata_iq(info: &AvatarPublishInfo) -> Element {
    let id = format!("avatar-publish-meta-{}", Uuid::new_v4());
    let mut info_builder = Element::builder("info", NS_AVATAR_METADATA)
        .attr(
            minidom::rxml::xml_ncname!("bytes").to_owned(),
            info.bytes.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            info.id.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("type").to_owned(),
            info.mime_type.as_str(),
        );
    if let Some(width) = info.width {
        info_builder = info_builder.attr(
            minidom::rxml::xml_ncname!("width").to_owned(),
            width.to_string(),
        );
    }
    if let Some(height) = info.height {
        info_builder = info_builder.attr(
            minidom::rxml::xml_ncname!("height").to_owned(),
            height.to_string(),
        );
    }
    let payload = Element::builder("metadata", NS_AVATAR_METADATA)
        .append(info_builder.build())
        .build();
    build_pep_publish_iq_with_item_id(&id, NS_AVATAR_METADATA, &info.id, payload)
}

/// Build the §4.3 "no avatar" IQ: publish an EMPTY `<metadata/>` at the
/// fixed [`AVATAR_REMOVE_ITEM_ID`] so subscribers drop their cached avatar.
pub fn build_disable_avatar_iq() -> Element {
    let id = format!("avatar-disable-{}", Uuid::new_v4());
    let payload = Element::builder("metadata", NS_AVATAR_METADATA).build();
    build_pep_publish_iq_with_item_id(&id, NS_AVATAR_METADATA, AVATAR_REMOVE_ITEM_ID, payload)
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

/// Outcome of a §4.2-aware avatar fetch: `id` is the item id the fetch
/// resolved (the advertised XEP-0084 metadata id, or the synthetic
/// vCard fallback id). `avatar` is `None` exactly when `id` was in the
/// caller's known set — XEP-0084 §4.2 forbids re-retrieving image data
/// the client already holds, so the data IQ was skipped and the caller
/// serves the bytes from its own cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarFetch {
    pub id: String,
    pub avatar: Option<Avatar>,
}

/// Request an avatar using XEP-0084 first, then XEP-0054 vCard PHOTO fallback.
pub async fn request_avatar_with_iq<F, Fut, E>(
    jid: &BareJid,
    send_iq: F,
) -> Result<Option<Avatar>, E>
where
    F: FnMut(Element) -> Fut,
    Fut: Future<Output = Result<Element, AvatarRequestFailure<E>>>,
{
    request_avatar_with_iq_skipping(jid, &[], send_iq)
        .await
        .map(|fetch| fetch.and_then(|fetch| fetch.avatar))
}

/// XEP-0084 fetch honoring the §4.2 no-refetch rule: query the metadata
/// node, and if the advertised item id is in `known_ids` answer with an
/// id-only [`AvatarFetch`] WITHOUT issuing the data IQ. Unknown ids
/// fetch data as usual; when the metadata path yields nothing the
/// XEP-0054 vCard PHOTO fallback runs (never skipped — its synthetic id
/// does not address a pubsub data item).
pub async fn request_avatar_with_iq_skipping<F, Fut, E>(
    jid: &BareJid,
    known_ids: &[String],
    mut send_iq: F,
) -> Result<Option<AvatarFetch>, E>
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
            if known_ids.iter().any(|known| known == &info.id) {
                return Ok(Some(AvatarFetch {
                    id: info.id,
                    avatar: None,
                }));
            }

            if let Some(url) = info.url.as_deref().and_then(normalize_https_avatar_url) {
                return Ok(Some(AvatarFetch {
                    id: info.id.clone(),
                    avatar: Some(Avatar {
                        jid: jid.clone(),
                        id: info.id,
                        mime_type: info.mime_type,
                        data: Vec::new(),
                        url: Some(url),
                    }),
                }));
            }

            let data_iq = build_data_request_iq(jid, &info.id);
            match send_iq(data_iq).await {
                Ok(data_response) => {
                    if let Some(base64_text) = parse_data_response(&data_response) {
                        if let Some(data) = decode_base64_bytes(&base64_text) {
                            return Ok(Some(AvatarFetch {
                                id: info.id.clone(),
                                avatar: Some(Avatar {
                                    jid: jid.clone(),
                                    id: info.id,
                                    mime_type: info.mime_type,
                                    data,
                                    url: None,
                                }),
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

    Ok(request_vcard_avatar(jid, send_iq)
        .await?
        .map(|avatar| AvatarFetch {
            id: avatar.id.clone(),
            avatar: Some(avatar),
        }))
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
    if let Some(url) = photo.url.as_deref().and_then(normalize_https_avatar_url) {
        let id = url.clone();
        return Some(Avatar {
            jid: jid.clone(),
            id,
            mime_type: photo.mime_type.unwrap_or_else(|| "image/png".to_string()),
            data: Vec::new(),
            url: Some(url),
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

fn normalize_https_avatar_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let parsed = Url::parse(trimmed).ok()?;
    if parsed.scheme() == "https" {
        Some(trimmed.to_string())
    } else {
        None
    }
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
    /// PHOTO when avatar metadata or data is unavailable. Item ids in
    /// `known_ids` honor the §4.2 no-refetch rule: an advertised id the
    /// caller already holds answers id-only, without the data IQ.
    fn request_avatar<'a>(
        &'a self,
        jid: &'a BareJid,
        known_ids: &'a [String],
    ) -> impl std::future::Future<Output = ClientResult<Option<AvatarFetch>>> + Send + 'a;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl AvatarExt for ClientHandle {
    async fn request_avatar(
        &self,
        jid: &BareJid,
        known_ids: &[String],
    ) -> ClientResult<Option<AvatarFetch>> {
        request_avatar_with_iq_skipping(jid, known_ids, |stanza| async move {
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
mod tests;
