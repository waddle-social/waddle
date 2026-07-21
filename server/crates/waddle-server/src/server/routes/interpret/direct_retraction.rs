use super::groupchat_archive::scrub_unacked_for_tombstone;
use super::*;

pub(super) async fn apply_retraction_tombstone(
    mam_storage: &Arc<dyn MamStorage>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    pending_storage: Option<
        &Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage>,
    >,
    archive: &jid::BareJid,
    target_wire_id: &str,
    retraction_message: &Message,
) -> bool {
    // XEP-0424 §"Using the correct ID": a non-groupchat retraction
    // names the target by the SENDER's wire id, which is client-chosen
    // and unique only per author. The scrub identity therefore carries
    // the retraction author's bare JID: a cached message matches on
    // (wire id, from-bare == author) — never on a colliding wire id
    // minted by a different sender in the same conversation.
    // The dispatcher's `RichTargetValidationHandler` has already
    // enforced the same-author rule for the archive tombstone, so
    // `retraction_message.from` IS the original message's author. A
    // from-less retraction (defensive; canonicalization stamps `from`)
    // still tombstones the archive but skips the cache scrub — without
    // an author the wire-id match cannot be made precise, and a
    // cross-sender scrub is the worse failure.
    let scrub_target = match retraction_message.from.as_ref().map(|from| from.to_bare()) {
        Some(author) => Some(waddle_xmpp::tombstone::TombstoneTarget::Direct {
            wire_id: target_wire_id.to_string(),
            author,
            archive: archive.clone(),
        }),
        None => {
            warn!(
                archive = %archive,
                target = target_wire_id,
                "ApplyRetractionTombstone: retraction stanza missing from JID; \
                 the unacked/pending cache scrub is skipped (no author scope)"
            );
            None
        }
    };
    let original = match mam_storage
        .get_message_by_message_id(archive, target_wire_id)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            debug!(
                archive = %archive,
                target = target_wire_id,
                "ApplyRetractionTombstone: target not found in archive; skipping"
            );
            return false;
        }
        Err(error) => {
            warn!(
                archive = %archive,
                target = target_wire_id,
                %error,
                "ApplyRetractionTombstone: archive lookup failed; skipping"
            );
            return false;
        }
    };
    let Some(retraction_id) = retraction_message
        .id
        .as_ref()
        .and_then(|id| waddle_xmpp::mam::RichMessageId::new(id.0.clone()))
    else {
        warn!(
            archive = %archive,
            target = target_wire_id,
            "ApplyRetractionTombstone: retraction stanza missing valid message id; skipping"
        );
        return false;
    };
    let tombstone = waddle_xmpp::mam::ArchivedTombstone {
        retraction_id: Some(retraction_id),
        stamp: chrono::Utc::now(),
        moderation: None,
        // 1:1 tombstones never enter the groupchat tombstone-retry
        // match; there is no occupant identity to retain.
        sender_scope: None,
    };
    match mam_storage
        .replace_with_tombstone(&original.id, tombstone)
        .await
    {
        Ok(true) => {
            debug!(
                archive = %archive,
                original_id = %original.id,
                "ApplyRetractionTombstone: replaced row with tombstone"
            );
        }
        Ok(false) => {
            warn!(
                archive = %archive,
                original_id = %original.id,
                "ApplyRetractionTombstone: target row not found at replace time"
            );
            if let Some(scrub_target) = scrub_target.as_ref() {
                scrub_unacked_for_tombstone(
                    sm_session_registry,
                    pending_storage,
                    scrub_target,
                    "ApplyRetractionTombstone",
                )
                .await;
            }
            return false;
        }
        Err(error) => {
            warn!(
                archive = %archive,
                original_id = %original.id,
                %error,
                "ApplyRetractionTombstone: replace_with_tombstone failed"
            );
            if let Some(scrub_target) = scrub_target.as_ref() {
                scrub_unacked_for_tombstone(
                    sm_session_registry,
                    pending_storage,
                    scrub_target,
                    "ApplyRetractionTombstone",
                )
                .await;
            }
            return false;
        }
    }
    // Drop matching unacked outbound copies from any detached XEP-0198
    // session queues so a recipient mid-resume does not replay the
    // pre-scrub stanza on the wire. XEP-0424 §"prevent further
    // distribution" applies to in-flight as well as archived copies.
    // Scope by the recipient archive's bare JID so a colliding wire id
    // in another conversation is not accidentally scrubbed (Codex P1),
    // and by the retraction author so a colliding wire id from a
    // DIFFERENT sender in the same conversation survives.
    if let Some(scrub_target) = scrub_target.as_ref() {
        scrub_unacked_for_tombstone(
            sm_session_registry,
            pending_storage,
            scrub_target,
            "ApplyRetractionTombstone",
        )
        .await;
    }
    true
}
