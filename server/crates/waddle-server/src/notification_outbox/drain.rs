//! T1 candidate drain: gate evaluation per candidate, coalescing insert
//! and merge of outbox jobs, and typed suppression audit writes.

use super::{
    active_mention_ttl_ms_from_env, build_waddle_context, evaluate_push_gate_at_dispatch,
    target_from_subscription, xep0191_blocks_notification_candidate, BareJid, BlockingStorage,
    DndState, NotificationActivity, NotificationCandidate, NotificationDrainDeps,
    NotificationOutboxError, NotificationOutboxStore, NotificationOutboxTarget, PushEvalCaches,
    PushEvalDeps, PushEvalStage, PushSubscriptionStore, RoomPolicyCacheEntry, SuppressedReason,
    T1PushDispatchOutcome,
};
use crate::notification_settings_projection::NotificationSettingsProjectionStore;
use std::collections::BTreeMap;

/// Maximum T1 policy-deferral attempts before a candidate is
/// terminally suppressed. The bound is deliberately generous:
/// 48 attempts with the saturated retry delay is roughly four hours,
/// so transient blocklist, gate, or room-policy infrastructure faults
/// keep retrying while permanently unresolvable candidates stop
/// starving fresher rows.
const MAX_CANDIDATE_POLICY_ATTEMPTS: i64 = 48;

impl NotificationOutboxStore {
    pub async fn drain_pending_candidates_into_outbox(
        &self,
        push_store: &dyn PushSubscriptionStore,
        blocking_storage: &dyn BlockingStorage,
        settings_projection: &NotificationSettingsProjectionStore,
        deps: NotificationDrainDeps<'_>,
        first_party_service_jid: &BareJid,
        batch_size: usize,
    ) -> Result<usize, NotificationOutboxError> {
        let NotificationDrainDeps {
            room_policy,
            dnd_reader,
            activity_reader,
        } = deps;
        let candidates = self.pending_candidates(batch_size).await?;
        let mut target_cache = BTreeMap::<BareJid, Vec<NotificationOutboxTarget>>::new();
        let mut room_policy_cache = BTreeMap::<BareJid, RoomPolicyCacheEntry>::new();
        let mut dnd_cache = BTreeMap::<BareJid, DndState>::new();
        let mut activity_cache =
            BTreeMap::<(BareJid, BareJid), Option<NotificationActivity>>::new();
        let active_mention_ttl_ms = active_mention_ttl_ms_from_env();
        let mut processed = 0usize;
        for candidate in candidates {
            // Self-DM filtering happens at the `NotificationCandidate`
            // constructor (`SelfDirectedNotificationCandidate` typed
            // error). A self-directed candidate is structurally
            // invalid and is rejected before it can be persisted, so
            // the T1 drain loop never observes one. See
            // `NotificationCandidate::direct_message` for the typed
            // boundary.
            match xep0191_blocks_notification_candidate(&candidate, blocking_storage).await {
                Ok(true) => {
                    let now_ms = crate::time::now_ms();
                    let claimed = self
                        .persist_suppressed_candidate(
                            &candidate,
                            SuppressedReason::Xep0191Blocked,
                            now_ms,
                        )
                        .await?;
                    if claimed {
                        tracing::info!(
                            notification_class = candidate.class().as_db_value(),
                            push_stage = "suppressed",
                            suppression_reason = SuppressedReason::Xep0191Blocked.as_db_value(),
                            "push pipeline transition"
                        );
                        waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                            SuppressedReason::Xep0191Blocked.telemetry_reason(),
                        );
                        processed += 1;
                    }
                    continue;
                }
                Ok(false) => {}
                Err(_) => {
                    tracing::warn!(

                        "XEP-0191 blocklist load failed; deferring notification candidate fail-closed"
                    );
                    if self.defer_candidate_policy_error(&candidate).await? {
                        processed += 1;
                    }
                    continue;
                }
            }
            // T1 push-gate re-evaluation — race-window guard,
            // defense-in-depth (XEP-0492 + XEP-0191 + XEP-0513 + XEP-0334 +
            // Waddle DnD).
            //
            // The same typed evaluator already ran at T0 (DM emission
            // in `offline_delivery.rs`, groupchat emission in
            // `groupchat_inbox.rs`) and a Suppressed outcome there
            // short-circuits the candidate insert entirely. Per the
            // compliance rule the common case is "no row in
            // `notification_candidates` for suppressed outcomes."
            //
            // This T1 invocation catches the race where recipient
            // state changed *between* the T0 emission and the T1
            // dispatch (e.g. the user flipped XEP-0492 to `<never/>`
            // mid-flight, or a groupchat config change toggled
            // members-only). If the projection has changed the drain
            // marks the candidate outboxed without enqueueing a job —
            // the row exists only briefly during the race window,
            // which is acceptable per the locked Q2 design (push
            // output is preserved).
            //
            // The class on the candidate is purely message-derived
            // from T0; combined with the recipient's effective
            // notification level (consulted fresh here against the
            // projection store) the typed reducer decides
            // publish-or-suppress. The room-policy lookup is cached
            // for the duration of this drain pass so a 100-member
            // groupchat does not produce 100 actor round-trips.
            let eval_deps = PushEvalDeps {
                settings_projection,
                room_policy,
                dnd_reader,
                activity_reader,
                active_mention_ttl_ms,
            };
            let mut eval_caches = PushEvalCaches {
                room_policy: &mut room_policy_cache,
                dnd: &mut dnd_cache,
                activity: &mut activity_cache,
            };
            let outcome = match evaluate_push_gate_at_dispatch(
                PushEvalStage::T1Drain,
                eval_deps,
                &candidate,
                &mut eval_caches,
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(_) => {
                    tracing::warn!("push gate evaluation failed at T1; deferring candidate");
                    if self.defer_candidate_policy_error(&candidate).await? {
                        processed += 1;
                    }
                    continue;
                }
            };
            let rich = match outcome {
                T1PushDispatchOutcome::Suppressed { reason } => {
                    let now_ms = crate::time::now_ms();
                    let claimed = self
                        .persist_suppressed_candidate(&candidate, reason, now_ms)
                        .await?;
                    if claimed {
                        tracing::info!(
                            notification_class = candidate.class().as_db_value(),
                            push_stage = "suppressed",
                            suppression_reason = reason.as_db_value(),
                            "T1 push gate suppressed XEP-0357 notification candidate"
                        );
                        waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                            reason.telemetry_reason(),
                        );
                        processed += 1;
                    }
                    continue;
                }
                T1PushDispatchOutcome::DeferUnknownRoomPolicy => {
                    // Actionable diagnostics for `Err(_)` lookups already
                    // fired exactly once per (drain batch, room) in
                    // `resolve_cached_room_policy`. The per-candidate
                    // deferral is `debug!` here so the cache-miss warn
                    // stays the single source-of-truth signal for
                    // operators triaging room-policy lookup failures.
                    tracing::debug!(


                        class = ?candidate.class(),
                        "MUC config unavailable at T1; deferring candidate (unknown room policy is not 'public')"
                    );
                    if self.defer_candidate_policy_error(&candidate).await? {
                        processed += 1;
                    }
                    continue;
                }
                T1PushDispatchOutcome::Deliver { rich } => rich,
            };
            let recipient_key = candidate.recipient_bare_jid.clone();
            if !target_cache.contains_key(&recipient_key) {
                let resolved = resolve_first_party_targets(
                    push_store,
                    &candidate.recipient_bare_jid,
                    first_party_service_jid,
                )
                .await?;
                target_cache.insert(recipient_key.clone(), resolved);
            }
            let targets = target_cache
                .get(&recipient_key)
                .expect("target cache populated")
                .clone();
            if targets.is_empty() {
                let reason = SuppressedReason::Xep0357NoRegistration;
                let now_ms = crate::time::now_ms();
                let claimed = self
                    .persist_suppressed_candidate(&candidate, reason, now_ms)
                    .await?;
                if claimed {
                    tracing::info!(
                        notification_class = candidate.class().as_db_value(),
                        push_stage = "suppressed",
                        suppression_reason = reason.as_db_value(),
                        "push pipeline transition"
                    );
                    waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                        reason.telemetry_reason(),
                    );
                    processed += 1;
                }
                continue;
            }
            let context = build_waddle_context(&candidate);
            let now_ms = crate::time::now_ms();
            if self
                .persist_candidate_targets(&candidate, &targets, &context, &rich, now_ms)
                .await?
            {
                processed += 1;
            }
        }
        Ok(processed)
    }

    async fn defer_candidate_policy_error(
        &self,
        candidate: &NotificationCandidate,
    ) -> Result<bool, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let next_policy_error_count = candidate.policy_error_count + 1;
        if next_policy_error_count >= MAX_CANDIDATE_POLICY_ATTEMPTS {
            let reason = SuppressedReason::PolicyRetriesExhausted;
            let claimed = self
                .persist_exhausted_candidate(candidate, next_policy_error_count, now_ms)
                .await?;
            if claimed {
                tracing::info!(
                    notification_class = candidate.class().as_db_value(),
                    push_stage = "suppressed",
                    suppression_reason = reason.as_db_value(),
                    "push pipeline transition"
                );
                waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                    reason.telemetry_reason(),
                );
            }
            return Ok(claimed);
        }
        self.persist_policy_retry(candidate, next_policy_error_count, now_ms)
            .await?;
        Ok(false)
    }
}

pub(super) async fn resolve_first_party_targets(
    push_store: &dyn PushSubscriptionStore,
    recipient: &BareJid,
    first_party_service_jid: &BareJid,
) -> Result<Vec<NotificationOutboxTarget>, NotificationOutboxError> {
    let registrations = push_store
        .get_for_user(&recipient.to_string())
        .await
        .map_err(|error| NotificationOutboxError::Push(error.to_string()))?;
    let first_party_service = first_party_service_jid.to_string();
    let mut targets = Vec::new();
    for registration in registrations {
        if registration.service_jid != first_party_service {
            continue;
        }
        match target_from_subscription(&registration) {
            Ok(Some(target)) if target.push_service_jid() == first_party_service_jid => {
                targets.push(target);
            }
            Ok(Some(_)) => {
                tracing::warn!(



                    "first-party XEP-0357 registration target did not parse back to the configured service"
                );
            }
            Ok(None) => {
                tracing::warn!(


                    "first-party XEP-0357 registration missing node; skipping notification outbox target"
                );
            }
            Err(_) => {
                tracing::warn!(

                    "first-party XEP-0357 registration could not be converted into a notification outbox target"
                );
            }
        }
    }
    Ok(targets)
}

#[cfg(test)]
#[path = "ancestry_tests.rs"]
mod ancestry_tests;
