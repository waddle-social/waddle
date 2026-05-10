//! `ensure_pep_profile_published` — the publish-chain entry point.
//!
//! Steps execute conditionally based on which subset of `ProfileSource`
//! is set:
//!
//! - PHOTO chain (XEP-0084 §4.1.1 / §4.1.2): runs only if `avatar_url`
//!   is present. Bytes are fetched once, hashed, and the chain
//!   publishes data → metadata.
//! - FN chain (XEP-0398 §3): runs whenever `display_name` is present.
//! - vcard-temp + vCard4 mirror: applies to whichever subset ran.

use std::sync::Arc;

use jid::BareJid;
use tracing::{debug, info};
use waddle_xmpp::pubsub::{NodeConfig, PubSubItem, PubSubStorage};
use waddle_xmpp::xep::xep0084::{
    build_avatar_data, build_avatar_metadata, AvatarInfo, NODE_AVATAR_DATA, NODE_AVATAR_METADATA,
};
use waddle_xmpp::xep::xep0292::PEP_NODE_VCARD4;
use waddle_xmpp::xep::xep0300::{compute_hash, HashAlgo};
use xmpp_parsers::minidom::Element;

use super::fetch::{fetch_avatar_bytes, AvatarBytes, FetchPolicy};
use super::source::{ProfileSource, ProfileSyncError};
use super::vcard_rmw::{apply_vcard4_update, apply_vcard_temp_update, PhotoUpdate};
use crate::vcard::VCardStore;

/// XEP-0292 §4.1.1: a vCard4 PEP node holds a single item with id
/// `current`.
const VCARD4_ITEM_ID: &str = "current";

/// `NodeConfig` for the OIDC-managed avatar/vCard4 PEP nodes:
///
/// - `AccessModel::Open` so any peer (not just roster contacts) can
///   resolve a user's avatar — typical web-app expectation.
/// - `max_items = 1` so a new avatar/vCard publish evicts the
///   previous item rather than accumulating one row per unique
///   payload. XEP-0084 metadata + XEP-0292 vCard4 are last-writer-
///   wins; avatar data is keyed on its SHA-1 so the old data row
///   would never be re-fetched anyway, but evicting it removes a
///   storage-bloat / past-avatar-leak surface.
fn oidc_pep_node_config() -> NodeConfig {
    NodeConfig {
        max_items: 1,
        ..NodeConfig::public()
    }
}

/// Dependencies passed to the publish helper.
pub struct ProfilePublishDeps {
    pub pubsub_storage: Arc<dyn PubSubStorage>,
    pub vcard_store: VCardStore,
    pub fetch_policy: FetchPolicy,
}

/// Result of a publish-chain run, useful for tests + telemetry.
#[derive(Debug, Default, Clone)]
pub struct ProfilePublishOutcome {
    /// Hex-encoded SHA-1 of the published bytes, if PHOTO ran.
    pub photo_sha1_hex: Option<String>,
    /// MIME of the published photo, if PHOTO ran.
    pub photo_mime: Option<String>,
    pub photo_bytes_len: Option<usize>,
    pub published_avatar_data: bool,
    pub published_avatar_metadata: bool,
    pub mirrored_vcard_temp: bool,
    pub mirrored_vcard4: bool,
}

/// Materialize a conformant PEP avatar + vCard set for `jid` from
/// `source`. Idempotent. No-op when `source.is_no_op()`.
pub async fn ensure_pep_profile_published(
    deps: &ProfilePublishDeps,
    jid: &BareJid,
    source: ProfileSource,
) -> Result<ProfilePublishOutcome, ProfileSyncError> {
    if source.is_no_op() {
        debug!(jid = %jid, "ensure_pep_profile_published called with empty source; no-op");
        return Ok(ProfilePublishOutcome::default());
    }

    let mut outcome = ProfilePublishOutcome::default();
    let (avatar_url, display_name) = match source {
        ProfileSource::Oidc {
            avatar_url,
            display_name,
        } => (avatar_url, display_name),
    };

    // ---- PHOTO chain (XEP-0084) ----
    let avatar_bytes_opt = if let Some(url) = avatar_url.as_ref() {
        let bytes = fetch_avatar_bytes(url, &deps.fetch_policy).await?;
        let hash = compute_hash(HashAlgo::Sha1, &bytes.bytes);
        let id = hash.to_hex();
        outcome.photo_sha1_hex = Some(id.clone());
        outcome.photo_mime = Some(bytes.mime.clone());
        outcome.photo_bytes_len = Some(bytes.bytes.len());

        publish_avatar_data(&deps.pubsub_storage, jid, &id, &bytes).await?;
        outcome.published_avatar_data = true;
        publish_avatar_metadata(&deps.pubsub_storage, jid, &id, &bytes).await?;
        outcome.published_avatar_metadata = true;
        Some(bytes)
    } else {
        None
    };

    let photo_update = avatar_bytes_opt.as_ref().map(|b| PhotoUpdate {
        bytes: b.bytes.as_slice(),
        mime: b.mime.as_str(),
    });

    // ---- vcard-temp mirror (XEP-0054 / XEP-0398 §3) ----
    let need_vcard_update = photo_update.is_some() || display_name.is_some();
    if need_vcard_update {
        let existing = deps.vcard_store.get(jid).await?;
        let updated =
            apply_vcard_temp_update(existing.as_ref(), photo_update, display_name.as_deref());
        deps.vcard_store.set(jid, &updated).await?;
        outcome.mirrored_vcard_temp = true;
    }

    // ---- vCard4 PEP mirror (XEP-0292) ----
    if need_vcard_update {
        let existing_vcard4 = read_existing_vcard4(deps, jid).await?;
        let updated_vcard4 = apply_vcard4_update(
            existing_vcard4.as_ref(),
            photo_update,
            display_name.as_deref(),
        );
        publish_vcard4(&deps.pubsub_storage, jid, &updated_vcard4).await?;
        outcome.mirrored_vcard4 = true;
    }

    info!(
        jid = %jid,
        photo = outcome.photo_sha1_hex.is_some(),
        fn_set = display_name.is_some(),
        "ensure_pep_profile_published completed"
    );

    Ok(outcome)
}

async fn publish_avatar_data(
    storage: &Arc<dyn PubSubStorage>,
    jid: &BareJid,
    item_id: &str,
    bytes: &AvatarBytes,
) -> Result<(), ProfileSyncError> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

    ensure_node_with_oidc_config(storage, jid, NODE_AVATAR_DATA).await?;

    let payload = build_avatar_data(&BASE64.encode(&bytes.bytes));
    let item = PubSubItem {
        id: Some(item_id.to_string()),
        publisher: Some(jid.clone()),
        payload: Some(payload),
    };
    storage
        .publish_item(jid, NODE_AVATAR_DATA, &item, Some(jid), false)
        .await?;
    Ok(())
}

async fn publish_avatar_metadata(
    storage: &Arc<dyn PubSubStorage>,
    jid: &BareJid,
    item_id: &str,
    bytes: &AvatarBytes,
) -> Result<(), ProfileSyncError> {
    ensure_node_with_oidc_config(storage, jid, NODE_AVATAR_METADATA).await?;

    let info = AvatarInfo {
        id: item_id.to_string(),
        mime_type: bytes.mime.clone(),
        width: None,
        height: None,
        bytes: Some(bytes.bytes.len() as u64),
        url: None,
    };
    let payload = build_avatar_metadata(&info);
    let item = PubSubItem {
        id: Some(item_id.to_string()),
        publisher: Some(jid.clone()),
        payload: Some(payload),
    };
    storage
        .publish_item(jid, NODE_AVATAR_METADATA, &item, Some(jid), false)
        .await?;
    Ok(())
}

async fn publish_vcard4(
    storage: &Arc<dyn PubSubStorage>,
    jid: &BareJid,
    vcard: &Element,
) -> Result<(), ProfileSyncError> {
    ensure_node_with_oidc_config(storage, jid, PEP_NODE_VCARD4).await?;

    let item = PubSubItem {
        id: Some(VCARD4_ITEM_ID.to_string()),
        publisher: Some(jid.clone()),
        payload: Some(vcard.clone()),
    };
    storage
        .publish_item(jid, PEP_NODE_VCARD4, &item, Some(jid), false)
        .await?;
    Ok(())
}

/// Read the existing `current` vCard4 item if any. Reading by id
/// (rather than "latest item") is what makes the
/// publish-as-`current` flow robust against legacy items written
/// under different ids — those would not match here, so we treat
/// them as "no existing vCard4" and the publish overwrites the
/// `current` slot directly. With `max_items = 1` on the node the
/// stale-id row is then evicted on the next publish.
async fn read_existing_vcard4(
    deps: &ProfilePublishDeps,
    jid: &BareJid,
) -> Result<Option<Element>, ProfileSyncError> {
    let items = deps
        .pubsub_storage
        .get_items(jid, PEP_NODE_VCARD4, Some(1), &[VCARD4_ITEM_ID.to_string()])
        .await?;
    let Some(item) = items.into_iter().next() else {
        return Ok(None);
    };
    let Some(xml) = item.payload_xml else {
        return Ok(None);
    };
    xml.parse::<Element>()
        .map(Some)
        .map_err(|e| ProfileSyncError::VCard4Malformed(e.to_string()))
}

/// Pre-create the PEP node with the OIDC-managed config (Open
/// access, `max_items=1`) BEFORE the first publish. If the node
/// already exists with a different config, flip it. Either way the
/// node is at the desired shape by the time `publish_item` lands —
/// closing the auto-create-then-flip race where a peer fetching
/// between the two would have been denied by `pep_default()`'s
/// Presence access.
async fn ensure_node_with_oidc_config(
    storage: &Arc<dyn PubSubStorage>,
    jid: &BareJid,
    node: &str,
) -> Result<(), ProfileSyncError> {
    let _ = storage.get_or_create_node(jid, node).await?;
    storage
        .update_node_config(jid, node, &oidc_pep_node_config())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oidc_pep_node_config_is_public_with_one_item_cap() {
        let cfg = oidc_pep_node_config();
        assert_eq!(cfg.max_items, 1);
        assert_eq!(
            cfg.access_model,
            waddle_xmpp::pubsub::AccessModel::Open,
            "OIDC-managed PEP nodes are semi-public so non-roster peers can resolve avatars"
        );
    }

    #[test]
    fn xep0084_namespace_constants_are_reused_not_redeclared() {
        // Sanity check that our publish chain talks to the same node
        // names the XEP-0084 / XEP-0292 modules export — guarding
        // against drift if either side is moved later.
        assert_eq!(NODE_AVATAR_DATA, "urn:xmpp:avatar:data");
        assert_eq!(NODE_AVATAR_METADATA, "urn:xmpp:avatar:metadata");
        assert_eq!(PEP_NODE_VCARD4, "urn:xmpp:vcard4");
    }
}
