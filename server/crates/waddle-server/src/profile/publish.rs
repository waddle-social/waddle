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
//! - XEP-0153 self-presence: runs only if PHOTO ran (the hash that
//!   travels in `<x xmlns="vcard-temp:x:update">` is undefined when
//!   no PHOTO change happened).

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use jid::BareJid;
use tracing::{debug, info, warn};
use waddle_xmpp::pubsub::{NodeConfig, PubSubItem, PubSubStorage};
use waddle_xmpp::xep::xep0300::{compute_hash, HashAlgo};
use xmpp_parsers::minidom::Element;

use super::fetch::{fetch_avatar_bytes, AvatarBytes, FetchPolicy};
use super::source::{ProfileSource, ProfileSyncError};
use super::vcard_rmw::{apply_vcard4_update, apply_vcard_temp_update};
use crate::vcard::VCardStore;

pub const PEP_NODE_AVATAR_DATA: &str = "urn:xmpp:avatar:data";
pub const PEP_NODE_AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";
pub const PEP_NODE_VCARD4: &str = "urn:xmpp:vcard4";
pub const NS_AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";
pub const NS_AVATAR_DATA: &str = "urn:xmpp:avatar:data";

/// Dependencies passed to the publish helper. Bundled so the OIDC
/// bridge and the future startup backfill share a single shape.
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

    // ---- vcard-temp mirror (XEP-0054 / XEP-0398 §3) ----
    let need_vcard_update = avatar_bytes_opt.is_some() || display_name.is_some();
    if need_vcard_update {
        let existing = deps
            .vcard_store
            .get(jid)
            .await
            .map_err(|e| ProfileSyncError::VCardTemp(e.to_string()))?;
        let updated = apply_vcard_temp_update(
            existing.as_ref(),
            avatar_bytes_opt.as_ref().map(|b| b.bytes.as_slice()),
            avatar_bytes_opt.as_ref().map(|b| b.mime.as_str()),
            display_name.as_deref(),
        );
        deps.vcard_store
            .set(jid, &updated)
            .await
            .map_err(|e| ProfileSyncError::VCardTemp(e.to_string()))?;
        outcome.mirrored_vcard_temp = true;
    }

    // ---- vCard4 PEP mirror (XEP-0292) ----
    if need_vcard_update {
        let existing_vcard4 = read_existing_vcard4(deps, jid).await?;
        let updated_vcard4 = apply_vcard4_update(
            existing_vcard4.as_ref(),
            avatar_bytes_opt.as_ref().map(|b| b.bytes.as_slice()),
            avatar_bytes_opt.as_ref().map(|b| b.mime.as_str()),
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
    let data_element = Element::builder("data", NS_AVATAR_DATA)
        .append(BASE64.encode(&bytes.bytes).as_str())
        .build();

    let item = PubSubItem {
        id: Some(item_id.to_string()),
        publisher: Some(jid.clone()),
        payload: Some(data_element),
    };

    storage
        .publish_item(jid, PEP_NODE_AVATAR_DATA, &item, Some(jid), true)
        .await
        .map_err(|e| ProfileSyncError::PubSubPublish(e.to_string()))?;
    set_public_access(storage, jid, PEP_NODE_AVATAR_DATA).await?;
    Ok(())
}

async fn publish_avatar_metadata(
    storage: &Arc<dyn PubSubStorage>,
    jid: &BareJid,
    item_id: &str,
    bytes: &AvatarBytes,
) -> Result<(), ProfileSyncError> {
    let info = Element::builder("info", NS_AVATAR_METADATA)
        .attr("id", item_id)
        .attr("type", &bytes.mime)
        .attr("bytes", bytes.bytes.len().to_string())
        .build();
    let metadata = Element::builder("metadata", NS_AVATAR_METADATA)
        .append(info)
        .build();

    let item = PubSubItem {
        id: Some(item_id.to_string()),
        publisher: Some(jid.clone()),
        payload: Some(metadata),
    };

    storage
        .publish_item(jid, PEP_NODE_AVATAR_METADATA, &item, Some(jid), true)
        .await
        .map_err(|e| ProfileSyncError::PubSubPublish(e.to_string()))?;
    set_public_access(storage, jid, PEP_NODE_AVATAR_METADATA).await?;
    Ok(())
}

async fn publish_vcard4(
    storage: &Arc<dyn PubSubStorage>,
    jid: &BareJid,
    vcard: &Element,
) -> Result<(), ProfileSyncError> {
    let item = PubSubItem {
        id: Some("current".to_string()),
        publisher: Some(jid.clone()),
        payload: Some(vcard.clone()),
    };

    storage
        .publish_item(jid, PEP_NODE_VCARD4, &item, Some(jid), true)
        .await
        .map_err(|e| ProfileSyncError::VCard4(e.to_string()))?;
    set_public_access(storage, jid, PEP_NODE_VCARD4).await?;
    Ok(())
}

async fn read_existing_vcard4(
    deps: &ProfilePublishDeps,
    jid: &BareJid,
) -> Result<Option<Element>, ProfileSyncError> {
    let items = deps
        .pubsub_storage
        .get_items(jid, PEP_NODE_VCARD4, Some(1), &[])
        .await
        .map_err(|e| ProfileSyncError::VCard4(e.to_string()))?;
    let Some(item) = items.into_iter().next() else {
        return Ok(None);
    };
    let Some(xml) = item.payload_xml else {
        return Ok(None);
    };
    xml.parse::<Element>()
        .map(Some)
        .map_err(|e| ProfileSyncError::VCard4(format!("malformed stored vCard4: {e}")))
}

/// Set `NodeConfig::public()` on the avatar/vCard4 nodes. Called
/// after each publish — idempotent — so that any authenticated peer
/// can read the bytes/metadata/vCard4 (XEP-0084 + XEP-0292 design
/// intent: avatars and profile data are semi-public).
async fn set_public_access(
    storage: &Arc<dyn PubSubStorage>,
    jid: &BareJid,
    node: &str,
) -> Result<(), ProfileSyncError> {
    if let Err(error) = storage
        .update_node_config(jid, node, &NodeConfig::public())
        .await
    {
        warn!(
            jid = %jid,
            node,
            error = %error,
            "Failed to set NodeConfig::public on PEP node; subsequent reads from non-roster peers may be denied"
        );
        return Err(ProfileSyncError::PubSubPublish(error.to_string()));
    }
    Ok(())
}
