//! XEP-0084: User Avatar and XEP-0054 vCard PHOTO avatar fetch.
//!
//! Implements a request-based client flow: given a bare JID, issue a
//! `pubsub#items` IQ for the `urn:xmpp:avatar:metadata` node to learn the
//! current avatar hash and MIME type, then fetch the matching item from the
//! `urn:xmpp:avatar:data` node and base64-decode its payload. If XEP-0084 is
//! unavailable or unusable, fall back to the XEP-0054 `vcard-temp` `PHOTO`
//! shape, accepting only in-band `BINVAL` bytes so resolving an avatar never
//! leaks the viewer's IP address to a third-party URL.
//!
//! Typed payloads only — JIDs use [`BareJid`], errors use [`ClientError`], and
//! the raw image bytes live in [`Avatar`].
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
use uuid::Uuid;

use crate::error::{StanzaError, StanzaErrorType};
use crate::pep::build_pep_publish_iq_with_item_id;
use crate::pubsub_event::{PubsubEvent, PubsubEventPayload};

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
}

/// vCard `PHOTO` payload from XEP-0054.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcardPhoto {
    pub mime_type: Option<String>,
    pub data: Option<Vec<u8>>,
}

/// Error classification for request flows that distinguish stanza-level
/// "not available" failures from transport/runtime failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvatarRequestFailure<E> {
    StanzaError,
    Other(E),
}

impl<E> AvatarRequestFailure<E> {
    /// Classify a stanza error answering an avatar IQ. Definitive answers
    /// (no node, no vCard, not allowed) mean the peer has no readable
    /// avatar. Transient ones (`type='wait'`, unreachable or failing
    /// remote server) are failures, so callers keep the avatar they
    /// already show instead of clearing it.
    pub fn from_stanza_error(error: StanzaError, transient: impl FnOnce(StanzaError) -> E) -> Self {
        let is_transient = matches!(error.error_type, StanzaErrorType::Wait)
            || matches!(
                error.condition.as_str(),
                "remote-server-not-found"
                    | "remote-server-timeout"
                    | "internal-server-error"
                    | "resource-constraint"
            );
        if is_transient {
            Self::Other(transient(error))
        } else {
            Self::StanzaError
        }
    }
}

/// A XEP-0084 metadata transition announced by a peer's PEP service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarChanged {
    /// Bare JID of the peer that published the metadata transition.
    pub jid: BareJid,
    /// The advertised in-band avatar item id, or `None` when the peer
    /// disabled its avatar or retracted the metadata item.
    pub avatar_id: Option<String>,
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
    parse_metadata_info(item.get_child("metadata", NS_AVATAR_METADATA)?)
}

/// Whether a metadata items result is the XEP-0084 §4.3 "disable" shape:
/// the current item is an empty `<metadata/>`. The owner has explicitly
/// switched avatars off, so no other source (e.g. an old vCard PHOTO)
/// may resurrect one.
fn is_metadata_disabled(iq: &Element) -> bool {
    iq.get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .filter(|items| items.attr("node") == Some(NS_AVATAR_METADATA))
        .and_then(|items| items.get_child("item", NS_PUBSUB))
        .and_then(|item| item.get_child("metadata", NS_AVATAR_METADATA))
        .is_some_and(|metadata| metadata_info_elements(metadata).next().is_none())
}

/// Parse an XEP-0084 metadata PEP event into one typed avatar transition.
///
/// XEP-0084 metadata is a singleton node in normal operation. A retraction
/// Only an empty `<metadata/>` means the peer disabled its avatar; a
/// retract-only notification produces no event.
pub fn parse_metadata_event(event: &PubsubEvent) -> Option<AvatarChanged> {
    if event.node != NS_AVATAR_METADATA {
        return None;
    }
    // PEP notifications come from the owner's bare JID (XEP-0163 §4.3);
    // a full-JID sender is a peer's client or a MUC occupant, not a PEP
    // service, and must not drive avatar state.
    let from = event.from.as_ref()?;
    if from.resource().is_some() {
        return None;
    }
    let jid = from.to_bare();
    // A retract says an item went away, not that the avatar is disabled:
    // a node may retract an OLD item after publishing a new one. Only the
    // XEP-0084 §4.3 empty `<metadata/>` publication clears the avatar;
    // retract-only notifications are left to revalidation.
    let item = event.items.iter().find(|item| !item.retracted)?;
    let PubsubEventPayload::Opaque { element } = &item.payload else {
        return None;
    };
    if !element.is("metadata", NS_AVATAR_METADATA) {
        return None;
    }

    Some(AvatarChanged {
        jid,
        avatar_id: metadata_avatar_id(element),
    })
}

fn metadata_avatar_id(metadata: &Element) -> Option<String> {
    let mut external_id = None;
    for info in metadata_info_elements(metadata) {
        let Some(id) = info.attr("id") else {
            continue;
        };
        if info.attr("url").is_none() {
            return Some(id.to_string());
        }
        external_id.get_or_insert_with(|| id.to_string());
    }
    external_id
}

/// Select an in-band metadata representation. URL-only `<info/>` elements
/// are deliberately ignored: resolving them would disclose the viewer's IP
/// address to an arbitrary third party.
fn parse_metadata_info(metadata: &Element) -> Option<AvatarInfo> {
    let info = metadata_info_elements(metadata).find(|child| child.attr("url").is_none())?;

    Some(AvatarInfo {
        id: info.attr("id")?.to_string(),
        mime_type: info.attr("type").unwrap_or("image/png").to_string(),
        width: info.attr("width").and_then(|value| value.parse().ok()),
        height: info.attr("height").and_then(|value| value.parse().ok()),
        bytes: info.attr("bytes").and_then(|value| value.parse().ok()),
    })
}

fn metadata_info_elements(metadata: &Element) -> impl Iterator<Item = &Element> {
    metadata
        .children()
        .filter(|child| child.name() == "info" && child.ns() == NS_AVATAR_METADATA)
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
        if is_metadata_disabled(&meta_response) {
            return Ok(None);
        }
        if let Some(info) = parse_metadata_response(&meta_response) {
            if known_ids.iter().any(|known| known == &info.id) {
                return Ok(Some(AvatarFetch {
                    id: info.id,
                    avatar: None,
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
    photo.data.map(|data| Avatar {
        jid: jid.clone(),
        id: "vcard-photo".to_string(),
        mime_type: photo.mime_type.unwrap_or_else(|| "image/png".to_string()),
        data,
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
                ClientError::StanzaError(error) => {
                    AvatarRequestFailure::from_stanza_error(error, ClientError::StanzaError)
                }
                other => AvatarRequestFailure::Other(other),
            })
        })
        .await
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
