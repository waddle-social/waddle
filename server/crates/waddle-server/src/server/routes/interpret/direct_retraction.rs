use super::groupchat_archive::scrub_unacked_for_tombstone;
use super::*;

pub(super) async fn apply_retraction_tombstone(
    mam_storage: &Arc<dyn MamStorage>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    archive: &jid::BareJid,
    target_wire_id: &str,
    retraction_message: &Message,
) -> bool {
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
            return false;
        }
        Err(error) => {
            warn!(
                archive = %archive,
                original_id = %original.id,
                %error,
                "ApplyRetractionTombstone: replace_with_tombstone failed"
            );
            return false;
        }
    }
    // Drop matching unacked outbound copies from any detached XEP-0198
    // session queues so a recipient mid-resume does not replay the
    // pre-scrub stanza on the wire. XEP-0424 §"prevent further
    // distribution" applies to in-flight as well as archived copies.
    // Scope by the recipient archive's bare JID so a colliding wire id
    // in another conversation is not accidentally scrubbed (Codex P1).
    scrub_unacked_for_tombstone(
        sm_session_registry,
        target_wire_id,
        &archive.to_string(),
        "ApplyRetractionTombstone",
    )
    .await;
    true
}
