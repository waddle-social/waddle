//! Sticker verb exports (XEP-0449): fetch the account's (or another
//! user's) published sticker packs, fetch one pack by id, publish a
//! pack to the own PEP `urn:xmpp:stickers:0` node, and retract one.
//!
//! Every method parses its untyped FFI inputs (JID strings, pack ids,
//! pack records) into typed values immediately, then delegates to the
//! shared `waddle-xmpp-client` builders. Fetch/publish are
//! `Result<_, WaddleError>` surfaces (the picker needs the stanza
//! error to explain a failed fetch); retract keeps the crate's
//! fire-and-forget `bool` shape like the other IQ verbs.

use jid::BareJid;
use minidom::Element;
use uuid::Uuid;

use waddle_xmpp_client::error::STANZA_CONDITION_ITEM_NOT_FOUND;
use waddle_xmpp_client::stickers::{
    build_pack_publish_iq, build_pack_retract_iq, build_sticker_pack_items_iq,
    build_sticker_packs_items_iq, parse_sticker_packs_result, PackId, StickerPackDoc,
    STICKERS_NODE,
};
use waddle_xmpp_client::ClientError;

use crate::convert::{sticker_pack_from_ffi, sticker_pack_to_ffi};
use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError, WaddleStickerPack};

// ── FFI → wire stanza factories ─────────────────────────────────────────────
//
// Pure functions between the FFI argument shapes and the shared
// builders; the dedicated test suite pins their output against the
// XEP wire shapes without a live connection. Every IQ id is a fresh
// UUID — never a fixed string.

/// XEP-0060 items GET over the whole `urn:xmpp:stickers:0` node.
/// `owner: None` routes to the caller's own PEP service.
pub(crate) fn sticker_packs_fetch_stanza(owner: Option<&BareJid>) -> Element {
    build_sticker_packs_items_iq(&Uuid::new_v4().to_string(), owner)
}

/// XEP-0060 items-by-id GET for one pack on `owner`'s `node`.
pub(crate) fn sticker_pack_fetch_stanza(owner: &BareJid, node: &str, pack_id: &PackId) -> Element {
    build_sticker_pack_items_iq(&Uuid::new_v4().to_string(), owner, node, pack_id)
}

/// XEP-0449 publish IQ to the own PEP node. Returns the pack id
/// recomputed from the pack content — the pubsub item id on the wire.
pub(crate) fn sticker_pack_publish_stanza(doc: &StickerPackDoc) -> (PackId, Element) {
    build_pack_publish_iq(&Uuid::new_v4().to_string(), doc)
}

/// XEP-0060 §7.2 retract IQ for one pack on the own PEP node.
pub(crate) fn sticker_pack_retract_stanza(pack_id: &PackId) -> Element {
    build_pack_retract_iq(&Uuid::new_v4().to_string(), pack_id)
}

/// Map a whole-node fetch reply. `item-not-found` (the node was never
/// auto-created because nothing was published) is the normal first-run
/// state and yields an empty list rather than an error.
pub(crate) fn map_fetch_packs_reply(
    reply: Result<Element, ClientError>,
    node: &str,
) -> Result<Vec<WaddleStickerPack>, WaddleError> {
    match reply {
        Ok(result) => Ok(parse_sticker_packs_result(&result, node)
            .into_iter()
            .map(sticker_pack_to_ffi)
            .collect()),
        Err(ClientError::StanzaError(err)) if err.condition == STANZA_CONDITION_ITEM_NOT_FOUND => {
            Ok(Vec::new())
        }
        Err(e) => Err(client_error_to_waddle(&e)),
    }
}

/// Map a retract reply. `item-not-found` — the pack is already gone,
/// e.g. a retry after a retract whose reply was lost mid-flight —
/// counts as success: the delete verb is idempotent (mirroring the
/// fetch path, where the never-created node is the normal first-run
/// state), so the caller can reconcile its cache and self-heal.
pub(crate) fn map_retract_reply(reply: Result<Element, ClientError>) -> Result<(), ClientError> {
    match reply {
        Ok(_) => Ok(()),
        Err(ClientError::StanzaError(err)) if err.condition == STANZA_CONDITION_ITEM_NOT_FOUND => {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Map a single-pack fetch reply. Both `item-not-found` and a result
/// without the requested pack yield `Ok(None)`.
pub(crate) fn map_fetch_pack_reply(
    reply: Result<Element, ClientError>,
    node: &str,
    pack_id: &PackId,
) -> Result<Option<WaddleStickerPack>, WaddleError> {
    let packs = map_fetch_packs_reply(reply, node)?;
    Ok(packs.into_iter().find(|pack| pack.id == pack_id.as_str()))
}

// ── Exported verbs ───────────────────────────────────────────────────────────

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// XEP-0449: fetch every sticker pack published on the
    /// `urn:xmpp:stickers:0` PEP node. `owner_jid: None` fetches the
    /// account's own packs; `Some(jid)` fetches another user's
    /// (readable because the node's access model is `open`). An
    /// `item-not-found` reply — no pack ever published — is the normal
    /// first-run state and returns an empty list.
    pub async fn fetch_sticker_packs(
        &self,
        owner_jid: Option<String>,
    ) -> Result<Vec<WaddleStickerPack>, WaddleError> {
        let owner = owner_jid
            .map(|jid| jid.parse::<BareJid>())
            .transpose()
            .map_err(|_| WaddleError::InvalidJid)?;
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = sticker_packs_fetch_stanza(owner.as_ref());
        map_fetch_packs_reply(send_iq_with_timeout(&handle, iq).await, STICKERS_NODE)
    }

    /// XEP-0449: fetch one sticker pack by id from `owner_jid`'s node
    /// (XEP-0060 items-by-id). `node: None` defaults to the PEP
    /// `urn:xmpp:stickers:0` node; `Some(node)` targets a generic
    /// pubsub node per the `<sticker jid/node>` reference attributes.
    /// A missing pack (or empty node) yields `Ok(None)`.
    pub async fn fetch_sticker_pack(
        &self,
        owner_jid: String,
        node: Option<String>,
        pack_id: String,
    ) -> Result<Option<WaddleStickerPack>, WaddleError> {
        let owner = owner_jid
            .parse::<BareJid>()
            .map_err(|_| WaddleError::InvalidJid)?;
        let pack_id = PackId::new(pack_id).ok_or(WaddleError::InvalidArgument)?;
        let node = node
            .filter(|node| !node.trim().is_empty())
            .unwrap_or_else(|| STICKERS_NODE.to_string());
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = sticker_pack_fetch_stanza(&owner, &node, &pack_id);
        map_fetch_pack_reply(send_iq_with_timeout(&handle, iq).await, &node, &pack_id)
    }

    /// XEP-0449: publish (create or update/import) a sticker pack on
    /// the account's own PEP node. The pubsub item id is recomputed
    /// from the pack content per §"Sticker pack hash calculation" —
    /// the caller-supplied `pack.id` is ignored — and the id actually
    /// published is returned.
    pub async fn publish_sticker_pack(
        &self,
        pack: WaddleStickerPack,
    ) -> Result<String, WaddleError> {
        let doc = match sticker_pack_from_ffi(pack) {
            Ok(doc) => doc,
            Err(e) => {
                self.emit_error(format!("publish_sticker_pack failed: {e}"));
                return Err(WaddleError::InvalidArgument);
            }
        };
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let (id, iq) = sticker_pack_publish_stanza(&doc);
        match send_iq_with_timeout(&handle, iq).await {
            Ok(_) => Ok(id.to_string()),
            Err(e) => {
                self.emit_error(format!("publish_sticker_pack failed: {e}"));
                Err(client_error_to_waddle(&e))
            }
        }
    }

    /// XEP-0449: retract (delete) one pack from the account's own PEP
    /// node. Idempotent: an `item-not-found` reply (the pack is
    /// already gone) is success, so a retry after an interrupted
    /// retract self-heals. Returns `false` on invalid input, no live
    /// session, or any other stanza error (with an `Error` event
    /// carrying the diagnostic).
    pub async fn retract_sticker_pack(&self, pack_id: String) -> bool {
        let Some(pack_id) = PackId::new(pack_id) else {
            self.emit_error("retract_sticker_pack failed: pack id must not be empty".to_string());
            return false;
        };
        let Some(handle) = self.clone_handle().await else {
            return false;
        };
        let iq = sticker_pack_retract_stanza(&pack_id);
        match map_retract_reply(send_iq_with_timeout(&handle, iq).await) {
            Ok(()) => true,
            Err(e) => {
                self.emit_error(format!("retract_sticker_pack failed: {e}"));
                false
            }
        }
    }
}
