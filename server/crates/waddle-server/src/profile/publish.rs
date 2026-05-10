//! `ensure_pep_profile_published` — the publish-chain entry point.
//!
//! Dispatches on per-axis intents from [`ProfileSource`]:
//!
//! - [`PhotoIntent`]:
//!   - `Skip` — no PHOTO sync.
//!   - `SetFromUrl` — fetch + hash + publish data + metadata
//!     (XEP-0084 §4.1.1 / §4.1.2).
//!   - `RemoveIfOidcOwned` — publish empty `<metadata/>` at item
//!     id `current` (XEP-0084 §4.3) and strip vcard-temp `<PHOTO>` /
//!     vCard4 `<photo>`. Honored only when
//!     `users.avatar_source = 'oidc'` (the user-managed avatar
//!     guard); idempotent via a current-item inspection.
//! - [`NameIntent`]:
//!   - `Skip` — no FN sync.
//!   - `Set` — replace/insert vcard-temp `<FN>` / vCard4 `<fn>`.
//!   - `Remove` — strip those elements.
//!
//! vcard-temp + vCard4 mirror only runs when the chain actually
//! touches PHOTO or FN; idempotent re-removals never write.

use std::sync::Arc;

use jid::BareJid;
use tracing::{debug, info};
use waddle_xmpp::pubsub::{AccessModel, NodeConfig, PubSubItem};
use waddle_xmpp::xep::xep0084::{
    build_avatar_data, build_avatar_metadata, AvatarInfo, NODE_AVATAR_DATA, NODE_AVATAR_METADATA,
    NS_AVATAR_METADATA,
};
use waddle_xmpp::xep::xep0292::PEP_NODE_VCARD4;
use waddle_xmpp::xep::xep0300::{compute_hash, HashAlgo};
use xmpp_parsers::minidom::Element;

use super::avatar_source::{
    acquire_per_jid_lock, read_avatar_source, record_oidc_managed, AvatarSource,
};
use super::fetch::{fetch_avatar_bytes, AvatarBytes, FetchPolicy};
use super::source::{NameIntent, PhotoIntent, ProfileSource, ProfileSyncError};
use super::vcard_rmw::{
    apply_vcard4_update, apply_vcard_temp_update, remove_vcard4_fields, remove_vcard_temp_fields,
    PhotoUpdate, Vcard4FieldRemoval, Vcard4PhotoRef, VcardTempFieldRemoval,
};
use crate::server::routes::websocket::handlers::pubsub_fanout::{self, FanOutRequest};
use crate::server::routes::websocket::WebSocketState;
use crate::vcard::VCardStore;

/// XEP-0292 §4.1.1 / XEP-0084 §4.3: the vCard4 PEP node and the
/// avatar-removal metadata item are both addressed by the literal id
/// `current`. (Set publishes use the SHA-1-keyed item id; only the
/// removal shape is `current`.)
const VCARD4_ITEM_ID: &str = "current";
const AVATAR_METADATA_REMOVE_ITEM_ID: &str = "current";

/// `NodeConfig` for the OIDC-managed avatar/vCard4 PEP nodes.
///
/// Built off `pep_default()` so we inherit the canonical PEP shape
/// and override only:
/// - `access_model = Open` — any peer can resolve a user's avatar.
/// - `max_items = 1` — a new publish evicts the previous item.
fn oidc_pep_node_config() -> NodeConfig {
    NodeConfig {
        access_model: AccessModel::Open,
        max_items: 1,
        ..NodeConfig::pep_default()
    }
}

/// Dependencies passed to the publish helper.
pub struct ProfilePublishDeps {
    /// Shared WebSocket state — needed for both `pubsub_storage`
    /// (writes) and `pubsub_fanout::fan_out_publish` (XEP-0163 §3
    /// notifications to roster + multi-resource owner). Without
    /// fan-out, OIDC publishes silently update storage and
    /// subscribers never see `pubsub#event` notifications.
    pub state: Arc<WebSocketState>,
    pub vcard_store: VCardStore,
    pub fetch_policy: FetchPolicy,
}

/// Result of a publish-chain run, useful for tests + telemetry.
#[derive(Debug, Default, Clone)]
pub struct ProfilePublishOutcome {
    /// Hex-encoded SHA-1 of the published bytes, if `SetFromUrl` ran.
    pub photo_sha1_hex: Option<String>,
    /// MIME of the published photo, if `SetFromUrl` ran.
    pub photo_mime: Option<String>,
    pub photo_bytes_len: Option<usize>,
    pub published_avatar_data: bool,
    pub published_avatar_metadata: bool,
    /// XEP-0084 §4.3 empty `<metadata/>` was published.
    pub published_avatar_removal: bool,
    pub mirrored_vcard_temp: bool,
    pub mirrored_vcard4: bool,
    /// PHOTO/FN explicitly removed from the vCard surfaces.
    pub removed_vcard_temp_photo: bool,
    pub removed_vcard_temp_fn: bool,
    pub removed_vcard4_photo: bool,
    pub removed_vcard4_fn: bool,
    /// `users.avatar_source = 'user'`, so `RemoveIfOidcOwned` was
    /// suppressed. Visible to tests + telemetry.
    pub photo_axis_guarded_by_user_managed: bool,
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

    // Hold the per-(BareJid) avatar-source mutex across the entire
    // publish chain. The wire avatar-publish hook in `pubsub_dispatch`
    // acquires the same mutex around its `record_self_published`
    // call — together this serializes "user wire publishes their
    // avatar then flips provenance to 'user'" against "OIDC reconcile
    // reads provenance then publishes empty `<metadata/>`", closing
    // the TOCTOU race that would otherwise let OIDC wipe a freshly-
    // published user avatar.
    let _guard = acquire_per_jid_lock(&deps.state, jid).await;

    let mut outcome = ProfilePublishOutcome::default();
    let (photo_intent, name_intent) = match source {
        ProfileSource::Oidc { photo, name } => (photo, name),
    };

    // ---- PHOTO chain (XEP-0084 §4.1 set / §4.3 remove) ----
    let photo_op = resolve_photo_op(deps, jid, photo_intent, &mut outcome).await?;

    // ---- vcard surfaces (XEP-0054 / XEP-0292 / XEP-0398 §3) ----
    let touches_vcard =
        !matches!(photo_op, PhotoOp::None) || !matches!(name_intent, NameIntent::Skip);
    if touches_vcard {
        mirror_vcard_temp(deps, jid, &photo_op, &name_intent, &mut outcome).await?;
        mirror_vcard4(deps, jid, &photo_op, &name_intent, &mut outcome).await?;
    }

    info!(
        jid = %jid,
        set_photo = outcome.photo_sha1_hex.is_some(),
        removed_photo = outcome.published_avatar_removal,
        guard_user_managed = outcome.photo_axis_guarded_by_user_managed,
        set_fn = matches!(name_intent, NameIntent::Set(_)),
        removed_fn = matches!(name_intent, NameIntent::Remove)
            && (outcome.removed_vcard_temp_fn || outcome.removed_vcard4_fn),
        "ensure_pep_profile_published completed"
    );

    Ok(outcome)
}

/// What the PHOTO branch decided to do, threaded into the vcard
/// surfaces so they apply consistent set / strip behavior.
enum PhotoOp {
    /// Skip / guarded / idempotent no-op. Do not touch vcard PHOTO.
    None,
    /// New bytes were published; mirror PHOTO into vcard surfaces.
    Set(AvatarBytes, String),
    /// Empty `<metadata/>` was published; strip PHOTO from vcards.
    RemovePublished,
}

async fn resolve_photo_op(
    deps: &ProfilePublishDeps,
    jid: &BareJid,
    intent: PhotoIntent,
    outcome: &mut ProfilePublishOutcome,
) -> Result<PhotoOp, ProfileSyncError> {
    match intent {
        PhotoIntent::Skip => Ok(PhotoOp::None),
        PhotoIntent::SetFromUrl(url) => {
            // User-managed guard also applies to the SET path. The
            // outer per-(BareJid) lock (acquired in
            // `ensure_pep_profile_published`) makes this read +
            // subsequent publish atomic against a concurrent wire
            // publish that would flip provenance to `'user'`.
            // Without this branch, OIDC reconcile after a user wire
            // publish would silently overwrite their avatar.
            let db_actor = deps.state.deps.app_state.db_pool.global_actor();
            let source = read_avatar_source(db_actor, jid).await?;
            if source == AvatarSource::User {
                outcome.photo_axis_guarded_by_user_managed = true;
                return Ok(PhotoOp::None);
            }
            let bytes = fetch_avatar_bytes(&url, &deps.fetch_policy).await?;
            let hash = compute_hash(HashAlgo::Sha1, &bytes.bytes);
            let id = hash.to_hex();
            outcome.photo_sha1_hex = Some(id.clone());
            outcome.photo_mime = Some(bytes.mime.clone());
            outcome.photo_bytes_len = Some(bytes.bytes.len());
            publish_avatar_data(&deps.state, jid, &id, &bytes).await?;
            outcome.published_avatar_data = true;
            publish_avatar_metadata(&deps.state, jid, &id, &bytes).await?;
            outcome.published_avatar_metadata = true;
            // Record OIDC ownership of the freshly-published avatar
            // so the provenance row exists from the very first OIDC
            // publish (otherwise `read_avatar_source` returns
            // `Unknown` for users who never wire-published, which
            // would defeat any future "did the user override?" probe
            // that relies on a non-Unknown signal).
            record_oidc_managed(db_actor, jid).await;
            Ok(PhotoOp::Set(bytes, id))
        }
        PhotoIntent::RemoveIfOidcOwned => {
            // Guard 1: only act when avatar_source = 'oidc'. A user
            // who self-published via wire XEP-0084 keeps their
            // picture (the user-managed avatar guard).
            let db_actor = deps.state.deps.app_state.db_pool.global_actor();
            let source = read_avatar_source(db_actor, jid).await?;
            if source == AvatarSource::User {
                outcome.photo_axis_guarded_by_user_managed = true;
                return Ok(PhotoOp::None);
            }
            // Guard 2: idempotence — if no prior metadata item or
            // the current item is already an empty `<metadata/>`,
            // there's nothing to remove. Repeated re-logins after
            // removal are wire-no-ops.
            if !avatar_metadata_present(deps, jid).await? {
                return Ok(PhotoOp::None);
            }
            publish_empty_avatar_metadata(&deps.state, jid).await?;
            outcome.published_avatar_removal = true;
            Ok(PhotoOp::RemovePublished)
        }
    }
}

async fn mirror_vcard_temp(
    deps: &ProfilePublishDeps,
    jid: &BareJid,
    photo_op: &PhotoOp,
    name_intent: &NameIntent,
    outcome: &mut ProfilePublishOutcome,
) -> Result<(), ProfileSyncError> {
    let existing = deps.vcard_store.get(jid).await?;

    let photo_set: Option<PhotoUpdate<'_>> = match photo_op {
        PhotoOp::Set(bytes, _) => Some(PhotoUpdate {
            bytes: bytes.bytes.as_slice(),
            mime: bytes.mime.as_str(),
        }),
        _ => None,
    };
    let name_set: Option<&str> = match name_intent {
        NameIntent::Set(s) => Some(s.as_str()),
        _ => None,
    };

    let after_set = if photo_set.is_some() || name_set.is_some() {
        Some(apply_vcard_temp_update(
            existing.as_ref(),
            photo_set,
            name_set,
        ))
    } else {
        None
    };

    let removal = VcardTempFieldRemoval {
        remove_photo: matches!(photo_op, PhotoOp::RemovePublished),
        remove_fn: matches!(name_intent, NameIntent::Remove),
    };

    let final_vcard = if removal.remove_photo || removal.remove_fn {
        after_set
            .as_ref()
            .or(existing.as_ref())
            .map(|base| remove_vcard_temp_fields(base, &removal))
    } else {
        after_set
    };

    if let Some(vcard) = final_vcard {
        // Idempotence guard — same rationale as `mirror_vcard4`'s
        // guard below: skip the write when the vcard is unchanged
        // so we don't bump storage timestamps and (more importantly,
        // for any subscriber to vcard-temp via legacy IQ result
        // delivery) re-emit the result for a no-op login.
        if let Some(existing) = existing.as_ref() {
            if String::from(existing) == String::from(&vcard) {
                return Ok(());
            }
        }
        deps.vcard_store.set(jid, &vcard).await?;
        outcome.mirrored_vcard_temp = true;
        outcome.removed_vcard_temp_photo = removal.remove_photo;
        outcome.removed_vcard_temp_fn = removal.remove_fn;
    }
    Ok(())
}

async fn mirror_vcard4(
    deps: &ProfilePublishDeps,
    jid: &BareJid,
    photo_op: &PhotoOp,
    name_intent: &NameIntent,
    outcome: &mut ProfilePublishOutcome,
) -> Result<(), ProfileSyncError> {
    let existing_vcard4 = read_existing_vcard4(deps, jid).await?;

    let photo_uri_pair: Option<(String, String)> = match photo_op {
        PhotoOp::Set(bytes, sha1) => Some((vcard4_photo_pep_uri(jid, sha1), bytes.mime.clone())),
        _ => None,
    };
    let photo_set: Option<Vcard4PhotoRef<'_>> =
        photo_uri_pair.as_ref().map(|(uri, mime)| Vcard4PhotoRef {
            uri: uri.as_str(),
            mime: mime.as_str(),
        });
    let name_set: Option<&str> = match name_intent {
        NameIntent::Set(s) => Some(s.as_str()),
        _ => None,
    };

    let after_set = if photo_set.is_some() || name_set.is_some() {
        Some(apply_vcard4_update(
            existing_vcard4.as_ref(),
            photo_set,
            name_set,
        ))
    } else {
        None
    };

    let removal = Vcard4FieldRemoval {
        remove_photo: matches!(photo_op, PhotoOp::RemovePublished),
        remove_fn: matches!(name_intent, NameIntent::Remove),
    };

    let final_vcard4 = if removal.remove_photo || removal.remove_fn {
        after_set
            .as_ref()
            .or(existing_vcard4.as_ref())
            .map(|base| remove_vcard4_fields(base, &removal))
    } else {
        after_set
    };

    if let Some(vcard) = final_vcard4 {
        // Idempotence guard: skip the publish (and the resulting
        // XEP-0163 §3 fan-out event) when the serialized vCard4 hasn't
        // changed. Without this every login that doesn't actually
        // mutate a field still bumps `published_at` on `current` and
        // emits a redundant `<message><event>` to every subscriber —
        // measurable noise for IDPs that don't supply `name`.
        if let Some(existing) = existing_vcard4.as_ref() {
            if String::from(existing) == String::from(&vcard) {
                return Ok(());
            }
        }
        publish_vcard4(&deps.state, jid, &vcard).await?;
        outcome.mirrored_vcard4 = true;
        outcome.removed_vcard4_photo = removal.remove_photo;
        outcome.removed_vcard4_fn = removal.remove_fn;
    }
    Ok(())
}

async fn publish_avatar_data(
    state: &Arc<WebSocketState>,
    jid: &BareJid,
    item_id: &str,
    bytes: &AvatarBytes,
) -> Result<(), ProfileSyncError> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

    ensure_node_with_oidc_config(state, jid, NODE_AVATAR_DATA).await?;

    let payload = build_avatar_data(&BASE64.encode(&bytes.bytes));
    let item = PubSubItem {
        id: Some(item_id.to_string()),
        publisher: Some(jid.clone()),
        payload: Some(payload),
    };
    publish_and_fan_out(state, jid, NODE_AVATAR_DATA, &item, item_id).await
}

async fn publish_avatar_metadata(
    state: &Arc<WebSocketState>,
    jid: &BareJid,
    item_id: &str,
    bytes: &AvatarBytes,
) -> Result<(), ProfileSyncError> {
    ensure_node_with_oidc_config(state, jid, NODE_AVATAR_METADATA).await?;

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
    publish_and_fan_out(state, jid, NODE_AVATAR_METADATA, &item, item_id).await
}

/// XEP-0084 §4.3: publish an empty `<metadata/>` element at item id
/// `current` to signal "no avatar". Subscribers receive the
/// `pubsub#event` and drop their cached avatar.
async fn publish_empty_avatar_metadata(
    state: &Arc<WebSocketState>,
    jid: &BareJid,
) -> Result<(), ProfileSyncError> {
    ensure_node_with_oidc_config(state, jid, NODE_AVATAR_METADATA).await?;

    let payload = Element::builder("metadata", NS_AVATAR_METADATA).build();
    let item = PubSubItem {
        id: Some(AVATAR_METADATA_REMOVE_ITEM_ID.to_string()),
        publisher: Some(jid.clone()),
        payload: Some(payload),
    };
    publish_and_fan_out(
        state,
        jid,
        NODE_AVATAR_METADATA,
        &item,
        AVATAR_METADATA_REMOVE_ITEM_ID,
    )
    .await
}

/// Idempotence probe — returns `false` when the avatar-metadata
/// node is empty OR the latest item is already the empty-`<metadata/>`
/// removal shape. Saves an extra wire publish (and fan-out) on
/// repeated re-logins after a removal.
///
/// Probes only the single latest item (`max_items=1` on the OIDC-managed
/// node means there's only ever one anyway). Storage backends with
/// different eviction order can't fool the probe by leaving an old
/// SHA-1 item alongside the new `current` empty one.
async fn avatar_metadata_present(
    deps: &ProfilePublishDeps,
    jid: &BareJid,
) -> Result<bool, ProfileSyncError> {
    let items = deps
        .state
        .deps
        .protocol
        .pubsub_storage
        .get_items(jid, NODE_AVATAR_METADATA, Some(1), &[])
        .await?;
    let Some(item) = items.into_iter().next() else {
        return Ok(false);
    };
    let Some(xml) = item.payload_xml.as_deref() else {
        return Ok(false);
    };
    let Ok(el) = xml.parse::<Element>() else {
        // Malformed stored payload — treat as "no avatar present" so
        // a removal request is safely a no-op rather than re-firing
        // an event over a corrupt item we can't reason about.
        return Ok(false);
    };
    Ok(el.children().next().is_some())
}

async fn publish_vcard4(
    state: &Arc<WebSocketState>,
    jid: &BareJid,
    vcard: &Element,
) -> Result<(), ProfileSyncError> {
    ensure_node_with_oidc_config(state, jid, PEP_NODE_VCARD4).await?;

    let item = PubSubItem {
        id: Some(VCARD4_ITEM_ID.to_string()),
        publisher: Some(jid.clone()),
        payload: Some(vcard.clone()),
    };
    publish_and_fan_out(state, jid, PEP_NODE_VCARD4, &item, VCARD4_ITEM_ID).await
}

/// Read the existing `current` vCard4 item if any. Reading by id
/// rather than "latest item" makes the publish-as-`current` flow
/// robust against legacy items written under different ids — those
/// would not match here, so we treat them as "no existing vCard4"
/// and the publish overwrites the `current` slot directly. With
/// `max_items = 1` on the node the stale-id row is then evicted on
/// the next publish.
async fn read_existing_vcard4(
    deps: &ProfilePublishDeps,
    jid: &BareJid,
) -> Result<Option<Element>, ProfileSyncError> {
    let items = deps
        .state
        .deps
        .protocol
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

/// Build the canonical vCard4 photo URI for `jid` referencing the
/// `urn:xmpp:avatar:data` PEP item with the given SHA-1 hex. Uses
/// the XEP-0147 `xmpp:` URI scheme with a `?pubsub` query so a
/// dereferencing client can pull the bytes via PEP rather than a
/// `data:` URI inflating every fan-out stanza.
fn vcard4_photo_pep_uri(jid: &BareJid, sha1_hex: &str) -> String {
    format!("xmpp:{jid}?pubsub;node={NODE_AVATAR_DATA};item={sha1_hex}")
}

/// Pre-create the PEP node with the OIDC-managed config (Open
/// access, `max_items=1`) BEFORE the first publish. Closing the
/// auto-create-then-flip race where a peer fetching between the two
/// would have been denied by `pep_default()`'s Presence access.
async fn ensure_node_with_oidc_config(
    state: &Arc<WebSocketState>,
    jid: &BareJid,
    node: &str,
) -> Result<(), ProfileSyncError> {
    let storage = &state.deps.protocol.pubsub_storage;
    let _ = storage.get_or_create_node(jid, node).await?;
    storage
        .update_node_config(jid, node, &oidc_pep_node_config())
        .await?;
    Ok(())
}

/// Persist the item via `PubSubStorage::publish_item` AND drive the
/// XEP-0163 §3 fan-out so subscribers (roster contacts with
/// `+notify`, multi-resource owner) actually receive the
/// `pubsub#event` notification — including for the §4.3 empty
/// `<metadata/>` removal shape.
async fn publish_and_fan_out(
    state: &Arc<WebSocketState>,
    jid: &BareJid,
    node: &str,
    item: &PubSubItem,
    item_id: &str,
) -> Result<(), ProfileSyncError> {
    state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(jid, node, item, Some(jid), false)
        .await?;

    pubsub_fanout::fan_out_publish(
        state,
        FanOutRequest {
            owner: jid,
            node,
            published_item: item,
            item_id,
            publisher: Some(jid),
            publisher_full: None,
            is_pep: true,
        },
    )
    .await;

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
            AccessModel::Open,
            "OIDC-managed PEP nodes are semi-public so non-roster peers can resolve avatars"
        );
    }

    #[test]
    fn xep0084_namespace_constants_are_reused_not_redeclared() {
        assert_eq!(NODE_AVATAR_DATA, "urn:xmpp:avatar:data");
        assert_eq!(NODE_AVATAR_METADATA, "urn:xmpp:avatar:metadata");
        assert_eq!(PEP_NODE_VCARD4, "urn:xmpp:vcard4");
    }

    #[test]
    fn vcard4_photo_uri_points_at_pep_avatar_item() {
        let jid: BareJid = "alice@example.com".parse().unwrap();
        let uri = vcard4_photo_pep_uri(&jid, "abc123");
        assert_eq!(
            uri,
            "xmpp:alice@example.com?pubsub;node=urn:xmpp:avatar:data;item=abc123"
        );
    }
}
