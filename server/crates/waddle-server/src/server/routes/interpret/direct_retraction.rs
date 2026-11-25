use super::groupchat_archive::scrub_unacked_for_tombstone;
use super::{debug, warn, Deps, Message};
use waddle_xmpp_core::xep0359::StanzaId;

pub(super) async fn apply_retraction_tombstone(
    deps: &Deps<'_>,
    archive: &jid::BareJid,
    target_wire_id: &str,
    retraction_message: &Message,
) -> Option<StanzaId> {
    let mam_storage = deps.mam_storage?;
    let sm_session_registry = deps.sm_session_registry;
    let pending_storage = deps.pending_delivery_storage;
    let effects = deps.effects;
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
            return None;
        }
        Err(error) => {
            effects.fail_plan(super::effects::PlanFailure::RetractionTargetRead);
            warn!(
                archive = %archive,
                target = target_wire_id,
                %error,
                "ApplyRetractionTombstone: archive lookup failed; skipping"
            );
            return None;
        }
    };
    // XEP-0424 §Business rules: a retraction naming a correction retracts the
    // original message and all of its corrections.
    let original =
        match super::correction_target::resolve_correction_original(mam_storage, archive, original)
            .await
        {
            Ok(original) => original,
            Err(error) => {
                effects.fail_plan(super::effects::PlanFailure::RetractionTargetRead);
                warn!(
                    archive = %archive,
                    target = target_wire_id,
                    %error,
                    "ApplyRetractionTombstone: original lookup failed; skipping"
                );
                return None;
            }
        };
    let target_wire_id = original
        .stanza_id
        .as_ref()
        .map_or(target_wire_id, |id| id.id.as_str());
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
    // XEP-0424 §Tombstones: a retraction applies to the original and each
    // of its XEP-0308 corrections. A correction is cached with
    // `<replace id=…/>` naming the original's wire id or, for clients that
    // reference it, its origin-id (the same identities the archive cascade
    // uses), so one scrub target per distinct id; the matcher scopes
    // `<replace/>` matches to the same author.
    let scrub_targets: Vec<waddle_xmpp::tombstone::TombstoneTarget> =
        match retraction_message.from.as_ref().map(|from| from.to_bare()) {
            Some(author) => {
                let mut ids = vec![target_wire_id.to_string()];
                if let Some(origin) = original.origin_id.as_ref() {
                    if !ids.iter().any(|id| id == origin.as_str()) {
                        ids.push(origin.as_str().to_string());
                    }
                }
                ids.into_iter()
                    .map(|wire_id| waddle_xmpp::tombstone::TombstoneTarget::Direct {
                        wire_id,
                        author: author.clone(),
                        archive: archive.clone(),
                    })
                    .collect()
            }
            None => {
                warn!(
                    archive = %archive,
                    target = target_wire_id,
                    "ApplyRetractionTombstone: retraction stanza missing from JID; \
                     the unacked/pending cache scrub is skipped (no author scope)"
                );
                Vec::new()
            }
        };
    let Some(retraction_id) = retraction_message
        .id
        .as_ref()
        .and_then(|id| waddle_xmpp::mam::RichMessageId::try_new(id.0.clone()))
    else {
        warn!(
            archive = %archive,
            target = target_wire_id,
            "ApplyRetractionTombstone: retraction stanza missing valid message id; skipping"
        );
        return None;
    };
    let tombstone = waddle_xmpp::mam::ArchivedTombstone {
        retraction_id: Some(retraction_id),
        stamp: chrono::Utc::now(),
        moderation: None,
        reason: super::groupchat_archive::retraction_reason(retraction_message),
        // XEP-0425 §Tombstones: keep the <replace/> of a retracted correction.
        replaces_id: original
            .rich
            .as_ref()
            .and_then(|rich| rich.correction_replaces_id())
            .cloned(),
        // 1:1 tombstones never enter the groupchat tombstone-retry
        // match; there is no occupant identity to retain.
        sender_scope: None,
    };
    if effects.is_planning() {
        use super::effects::direct::DurableDirectEffect;
        use super::effects::{DurableEffect, Effect, PlannedEffect};
        let target = StanzaId::new(original.id, jid::Jid::from(archive.clone()));
        effects.record(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::RetractionTombstone {
                archive: archive.clone(),
                target: target.clone(),
                tombstone,
            },
        ))));
        for scrub_target in scrub_targets {
            if !super::groupchat_archive::plan_tombstone_replay(deps, scrub_target).await {
                return None;
            }
        }
        return Some(target);
    }
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
            for scrub_target in &scrub_targets {
                scrub_unacked_for_tombstone(
                    sm_session_registry,
                    pending_storage,
                    scrub_target,
                    "ApplyRetractionTombstone",
                )
                .await;
            }
            return None;
        }
        Err(error) => {
            warn!(
                archive = %archive,
                original_id = %original.id,
                %error,
                "ApplyRetractionTombstone: replace_with_tombstone failed"
            );
            for scrub_target in &scrub_targets {
                scrub_unacked_for_tombstone(
                    sm_session_registry,
                    pending_storage,
                    scrub_target,
                    "ApplyRetractionTombstone",
                )
                .await;
            }
            return None;
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
    for scrub_target in &scrub_targets {
        scrub_unacked_for_tombstone(
            sm_session_registry,
            pending_storage,
            scrub_target,
            "ApplyRetractionTombstone",
        )
        .await;
    }
    Some(StanzaId::new(original.id, jid::Jid::from(archive.clone())))
}
