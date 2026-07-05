//! XEP-0198 SM-expiry promotion (issue #209 slice (d) phase 4,
//! locked Q6 = B).
//!
//! When a detached XEP-0198 SM session's resume window closes (or
//! the server gracefully drains a live session at shutdown), the
//! server MUST treat its unacked stanzas the way XEP-0198 §5
//! line 364 prescribes:
//!
//! > "treat unacknowledged stanzas in the same way that it would
//! > treat a stanza sent to an unavailable resource, by either
//! > returning an error to the sender, delivery to an alternate
//! > resource, or committing the stanza to offline storage."
//!
//! The locked Q6 = B priority chain implements all three options in
//! priority order: **alt-resource → offline-storage → service-
//! unavailable error**. Each unacked stanza is re-run through the
//! [`classify_dm_intake`] classifier (locked Q6b: "promotion filter
//! delegates to classify_dm_intake" — single source of truth for
//! the type/hint matrix) and the resulting [`DmRouting`] gates which
//! branch fires.

mod live;
mod pending;
mod stanza;
#[cfg(test)]
mod tests;
mod types;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::{debug, instrument};
use waddle_xmpp::pending_delivery::flush::{
    build_replay_stanza, MaterializedPayload, ReplayReason,
};
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::PendingPayload;
use waddle_xmpp::protocol::dm_routing::{
    classify_dm_intake, DmRouting, LiveDecision, OnlineResources, PendingDecision,
};
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::registry::{ConnectionRegistry, SendResult};
use waddle_xmpp::stream_management::DetachedSession;
use waddle_xmpp::Stanza;

use live::{build_online_resources, collect_live_targets};
use pending::{insert_pending, promote_as_transient};
use stanza::{parse_stanza, promote_iq, promote_presence};
pub use types::{PromotedOutcome, PromotionSummary};

/// Walk a session's unacked queue, promoting each stanza per the
/// locked Q6 = B priority chain. Each promoted `pending_delivery`
/// row's `original_receipt_at` is the per-stanza receipt time
/// preserved on the [`DetachedUnackedStanza`] (issue #209 PR #361:
/// previously a wall-clock fallback at expiry — now correct per
/// XEP-0203 §4.1 + XEP-0198 §5 line 364).
///
/// `recent_tombstones` is the SM registry's recently applied
/// XEP-0424/0425 tombstone record (round-2 review R2): a retraction
/// that raced the drain — session off both registry maps, pending row
/// not yet inserted — is invisible to the scrub's four phases, so the
/// promotion re-checks here and drops matching stanzas (counted as
/// `scrubbed` in the summary) instead of resurrecting retracted
/// content into `pending_delivery`.
#[instrument(
    skip(session, registry, pending_storage, blocklist, recent_tombstones),
    fields(stream_id = %session.stream_id, jid = %session.jid)
)]
pub async fn promote_session_unacked(
    session: &DetachedSession,
    registry: &ConnectionRegistry,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    blocklist: &Blocklist,
    server_domain: &str,
    recent_tombstones: &[waddle_xmpp::stream_management::RecentTombstoneRecord],
) -> PromotionSummary {
    let mut summary = PromotionSummary::default();
    let recipient_bare = session.jid.to_bare();

    // Snapshot the recipient's currently-online resources for the
    // classifier. Empty in the common SM-expiry case (otherwise
    // the session wouldn't have been detached in the first place,
    // unless other resources joined after detach).
    let online = build_online_resources(registry, &recipient_bare);

    // Round-2 review R2: select the sequences a recently applied
    // tombstone matches, using the same shared matcher as the
    // registry / pending-delivery scrubs. Round-3 review finding 2: a
    // tombstone applies BACKWARD in time only — a stanza whose
    // original receipt postdates the tombstone's recording is a NEW
    // message that merely reuses the wire id in the same scope, not
    // the retracted one, and must promote normally.
    let tombstoned_sequences: std::collections::HashSet<u32> = if recent_tombstones.is_empty() {
        std::collections::HashSet::new()
    } else {
        let entries: Vec<(u32, String)> = session
            .unacked_stanzas
            .iter()
            .map(|entry| (entry.sequence, entry.stanza_xml.clone()))
            .collect();
        let receipt_by_sequence: std::collections::HashMap<u32, DateTime<Utc>> = session
            .unacked_stanzas
            .iter()
            .map(|entry| (entry.sequence, entry.original_receipt_at))
            .collect();
        recent_tombstones
            .iter()
            .flat_map(|record| {
                waddle_xmpp::tombstone::matching_tombstone_sequences(
                    &entries,
                    &record.key.target_id,
                    &record.key.archive_jid,
                )
                .into_iter()
                .filter(|sequence| {
                    receipt_by_sequence
                        .get(sequence)
                        .is_some_and(|received_at| *received_at <= record.recorded_at_utc)
                })
                .collect::<Vec<u32>>()
            })
            .collect()
    };

    for entry in &session.unacked_stanzas {
        if tombstoned_sequences.contains(&entry.sequence) {
            debug!(
                stream_id = %session.stream_id,
                sequence = entry.sequence,
                "Q6 promotion: stanza matches a recent tombstone; scrubbed"
            );
            summary.record(entry.sequence, &PromotedOutcome::Scrubbed);
            continue;
        }
        let outcome = match parse_stanza(&entry.stanza_xml) {
            Some(Stanza::Message(message)) => {
                let ctx = PromotionContext {
                    online: &online,
                    blocklist,
                    registry,
                    pending_storage,
                    original_receipt_fallback: entry.original_receipt_at,
                    server_domain,
                };
                promote_one(message, entry.sequence, ctx).await
            }
            Some(Stanza::Iq(iq)) => promote_iq(*iq, registry).await,
            Some(Stanza::Presence(presence)) => promote_presence(presence, registry).await,
            None => PromotedOutcome::Unparseable,
        };
        debug!(
            stream_id = %session.stream_id,
            sequence = entry.sequence,
            ?outcome,
            "Q6 promotion: per-stanza outcome"
        );
        summary.record(entry.sequence, &outcome);
    }

    debug!(
        stream_id = %session.stream_id,
        redelivered = summary.redelivered,
        queued = summary.queued,
        bounced = summary.bounced,
        dropped = summary.dropped,
        not_promotable = summary.not_promotable,
        unparseable = summary.unparseable,
        scrubbed = summary.scrubbed,
        storage_failed = summary.storage_failed,
        "Q6 promotion: session summary"
    );
    summary
}

/// Dependencies for [`promote_displaced_sessions`]. Grouped so the
/// two displacement call sites (max_sessions eviction at detach,
/// fresh-bind invalidation at registration) share one signature.
pub struct DisplacedPromotionDeps<'a> {
    pub sm_registry: &'a waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    pub connection_registry: &'a ConnectionRegistry,
    pub pending_storage: &'a Arc<dyn PendingDeliveryStorage>,
    pub blocking_storage: &'a dyn waddle_xmpp::xep::xep0191::BlockingStorage,
    pub server_domain: &'a str,
}

/// Run the XEP-0198 §5 promote → confirm chain on sessions the SM
/// registry displaced (issue #1097): max_sessions overflow eviction
/// and fresh-bind invalidation previously dropped these sessions'
/// unacked queues silently.
///
/// Mirrors the SM-expiry janitor's contract: a blocklist-load or
/// promotion storage failure records a promotion failure and
/// PRESERVES the session's durable rows (a later restart rehydrates
/// them and the janitor retries, including its dead-letter cap);
/// success confirms the drain, erasing the durable rows, and releases
/// any pending_delivery claim held by the dead stream.
/// Put a session whose promotion failed back into the SM registry
/// (forced expired, durable state untouched) so the SM-expiry
/// janitor's next `drain_expired` pass retries the promote → confirm
/// chain without waiting for a restart. Best-effort: on registry
/// failure the durable rows still survive and a restart retries.
pub async fn reinsert_failed_session_for_retry(
    sm_registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    session: DetachedSession,
) {
    let stream_id = session.stream_id.clone();
    let jid = session.jid.clone();
    if let Err(error) = sm_registry.reinsert_for_retry(session).await {
        tracing::warn!(
            jid = %jid,
            stream_id = %stream_id,
            %error,
            "failed to re-insert SM session for promotion retry; durable rows \
             will only be retried after a restart"
        );
    }
}

/// After a PARTIAL promotion failure, durably erase the successfully
/// promoted stanzas' `sm_unacked` rows, drop them from the session,
/// and re-insert the remainder for the janitor's next retry (round-2
/// review R4). Without this, every retry tick re-promoted the whole
/// queue and duplicated the already-Queued stanzas.
///
/// Crash-safety ordering: each promoted stanza's `pending_delivery`
/// row was committed inside `promote_session_unacked` BEFORE this
/// delete runs, so at-least-once is preserved — a crash between the
/// two duplicates at most one promotion window, not one per tick. If
/// the durable delete itself fails, the session keeps its FULL queue
/// (memory and storage stay consistent) and the retry may duplicate —
/// at-least-once beats losing the failed stanzas' retry rows.
pub async fn prune_promoted_then_reinsert_for_retry(
    sm_registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    mut session: DetachedSession,
    summary: &PromotionSummary,
) {
    if !summary.promoted_sequences.is_empty() {
        match sm_registry
            .delete_unacked_sequences(&session.stream_id, &summary.promoted_sequences)
            .await
        {
            Ok(_) => {
                session
                    .unacked_stanzas
                    .retain(|entry| !summary.promoted_sequences.contains(&entry.sequence));
            }
            Err(error) => {
                tracing::warn!(
                    jid = %session.jid,
                    stream_id = %session.stream_id,
                    %error,
                    "partial promotion: durable delete of promoted sm_unacked rows \
                     failed; keeping the full queue for retry (may duplicate — \
                     at-least-once preserved)"
                );
            }
        }
    }
    reinsert_failed_session_for_retry(sm_registry, session).await;
}

/// Read the SM registry's recent-tombstone record immediately before a
/// SINGLE session's promotion. Round-3 review finding 1: a per-batch
/// snapshot leaves a mid-loop TOCTOU — a retraction landing after the
/// snapshot but before a later session's pending insert was missed
/// (drained sessions are off both registry maps, so the scrub phases
/// cannot see them either). Fetching per session shrinks the window to
/// that one session's promotion span, which is the intended contract.
///
/// Lock poisoning is process-fatal territory; promotion proceeds
/// without the re-check (the durable phase-4 scrub already covered
/// rows present at scrub time).
pub fn recent_tombstones_for_promotion(
    sm_registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    context: &'static str,
) -> Vec<waddle_xmpp::stream_management::RecentTombstoneRecord> {
    match sm_registry.recent_tombstones() {
        Ok(records) => records,
        Err(error) => {
            tracing::warn!(
                %error,
                context,
                "recent-tombstone read failed; proceeding without the \
                 promotion-time tombstone re-check"
            );
            Vec::new()
        }
    }
}

pub async fn promote_displaced_sessions(
    sessions: Vec<DetachedSession>,
    deps: DisplacedPromotionDeps<'_>,
) {
    for session in sessions {
        let blocklist = match deps
            .blocking_storage
            .list_blocked_jid_entries(&session.jid.to_bare())
            .await
        {
            Ok(jids) => waddle_xmpp::protocol::session_state::Blocklist::new(jids),
            Err(error) => {
                waddle_xmpp::prometheus::increment_sm_promotion_blocklist_failed();
                if let Err(record_error) = deps
                    .sm_registry
                    .record_promotion_failure(&session.stream_id)
                    .await
                {
                    tracing::warn!(
                        jid = %session.jid,
                        error = %error,
                        record_error = %record_error,
                        "displaced SM session: blocklist load and failure recording both \
                         failed; preserving durable rows and re-inserting for janitor retry"
                    );
                    reinsert_failed_session_for_retry(deps.sm_registry, session).await;
                    continue;
                }
                tracing::warn!(
                    jid = %session.jid,
                    stream_id = %session.stream_id,
                    error = %error,
                    "displaced SM session: blocklist load failed; SKIPPING promotion to \
                     preserve fail-closed XEP-0191 policy. Re-inserted for retry on the \
                     SM-expiry janitor's next pass."
                );
                reinsert_failed_session_for_retry(deps.sm_registry, session).await;
                continue;
            }
        };
        // Round-2 review R2 + round-3 finding 1: consult the registry's
        // recent-tombstone record PER SESSION, immediately before this
        // session's promotion, so a retraction racing the displacement
        // drain — even one landing mid-batch — still scrubs the
        // in-flight copies.
        let recent_tombstones =
            recent_tombstones_for_promotion(deps.sm_registry, "displaced SM promotion");
        let summary = promote_session_unacked(
            &session,
            deps.connection_registry,
            deps.pending_storage,
            &blocklist,
            deps.server_domain,
            &recent_tombstones,
        )
        .await;
        if summary.has_storage_failure() {
            waddle_xmpp::prometheus::add_sm_promotion_storage_failed(u64::from(
                summary.storage_failed,
            ));
            if let Err(error) = deps
                .sm_registry
                .record_promotion_failure(&session.stream_id)
                .await
            {
                tracing::warn!(
                    jid = %session.jid,
                    %error,
                    "displaced SM session: record_promotion_failure failed; \
                     preserving durable rows for janitor retry"
                );
            }
            tracing::warn!(
                jid = %session.jid,
                stream_id = %session.stream_id,
                storage_failed = summary.storage_failed,
                "displaced SM session: promotion had storage failures; \
                 preserving durable rows and re-inserting for janitor retry"
            );
            prune_promoted_then_reinsert_for_retry(deps.sm_registry, session, &summary).await;
            continue;
        }
        deps.sm_registry.confirm_drained(&session.stream_id).await;
        let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(session.stream_id.clone());
        if let Err(error) = deps.pending_storage.release_claim(&session_id).await {
            tracing::warn!(
                jid = %session.jid,
                stream_id = %session.stream_id,
                error = %error,
                "displaced SM session: pending_delivery release_claim failed; \
                 rows remain claimed and will be released by the claim-expiry janitor"
            );
        }
        debug!(
            jid = %session.jid,
            stream_id = %session.stream_id,
            redelivered = summary.redelivered,
            queued = summary.queued,
            bounced = summary.bounced,
            dropped = summary.dropped,
            not_promotable = summary.not_promotable,
            unparseable = summary.unparseable,
            scrubbed = summary.scrubbed,
            "displaced SM session: Q6 promotion completed"
        );
    }
}

/// Extract the stamp of a `<delay/>` this server itself added to the
/// stanza on a prior replay hop, if one is present.
fn self_stamp_time(
    message: &xmpp_parsers::message::Message,
    server_domain: &str,
) -> Option<DateTime<Utc>> {
    message
        .payloads
        .iter()
        .filter(|payload| {
            waddle_xmpp::xep::xep0203::is_delay_element(payload)
                && payload.attr("from") == Some(server_domain)
        })
        .find_map(|payload| {
            waddle_xmpp::xep::xep0203::parse_delay_element(payload)
                .ok()
                .map(|info| info.stamp)
        })
}

struct PromotionContext<'a> {
    online: &'a OnlineResources,
    blocklist: &'a Blocklist,
    registry: &'a ConnectionRegistry,
    pending_storage: &'a Arc<dyn PendingDeliveryStorage>,
    original_receipt_fallback: DateTime<Utc>,
    server_domain: &'a str,
}

/// Promote a single typed [`xmpp_parsers::message::Message`] per the
/// locked Q6 chain.
async fn promote_one(
    message: xmpp_parsers::message::Message,
    sequence: u32,
    mut ctx: PromotionContext<'_>,
) -> PromotedOutcome {
    if waddle_xmpp_core::mam::is_mam_query_response_message(&message) {
        waddle_xmpp::prometheus::increment_sm_promotion_not_promotable();
        return PromotedOutcome::NotPromotable;
    }

    // Multi-hop Q6 chain (issue #1178): a stanza this promoter already
    // redelivered once carries our own `<delay/>` with the TRUE
    // original receipt time, while the queue entry's receipt time is
    // the (later) redelivery time. Prefer the self-stamp so the
    // promoted `pending_delivery` row — and therefore every later
    // flush of an Archived row, which rehydrates from MAM without any
    // self-stamp — keeps the original time instead of drifting one
    // hop later on each expiry.
    if let Some(stamp) = self_stamp_time(&message, ctx.server_domain) {
        ctx.original_receipt_fallback = stamp;
    }

    let routing: DmRouting = classify_dm_intake(&message, ctx.online, ctx.blocklist);

    // Step 1: alt-resource — if the classifier says live-deliver,
    // route to the recipient's connected resource(s) via the
    // ConnectionRegistry. Locked Q6 = B step 1 (alt-resource) +
    // RFC 6121 §8.5.2 (bare-JID fanout to ALL non-negative-priority
    // resources, not just the highest-priority one — Copilot
    // review on PR #346: earlier code took only the first via
    // `next()` which silently lost deliveries on multi-resource
    // users).
    if !matches!(routing.live, LiveDecision::None) {
        let targets = collect_live_targets(&routing, &message, ctx.registry);
        if !targets.is_empty() {
            let delayed = build_replay_stanza(
                MaterializedPayload::Transient(Box::new(message.clone())),
                ctx.server_domain,
                ctx.original_receipt_fallback,
                ReplayReason::SmRedelivery,
            );
            // Send to all eligible resources; mark redelivered if at
            // least one send succeeds (matches the live-route fanout
            // semantics in interpret.rs's `RouteToConnection` arm).
            let mut delivered_to = None;
            for target in targets {
                if matches!(
                    ctx.registry
                        .send_to(&target, Stanza::Message(delayed.clone()))
                        .await,
                    SendResult::Sent
                ) && delivered_to.is_none()
                {
                    delivered_to = Some(target);
                }
            }
            if let Some(target) = delivered_to {
                return PromotedOutcome::Redelivered { to: target };
            }
        }
        // Classifier said deliver but no live target took the stanza
        // (full-JID target had gone offline by send time, or the
        // socket buffer rejected). Fall through to offline storage.
    }

    // Step 2: offline storage — if the classifier marked the stanza
    // for `pending_delivery`, insert.
    match routing.pending {
        PendingDecision::None => {
            // Neither live nor offline survived — nothing to do.
            // Common reasons: <no-store/>, chat-states-only, or
            // type='error' to a fully-offline recipient (silently
            // dropped per RFC 6121 §8.5.2.1.4).
            return PromotedOutcome::Dropped;
        }
        PendingDecision::Archived | PendingDecision::Transient => {}
    }

    let payload = match routing.pending {
        PendingDecision::Archived => {
            // The classifier said the stanza is MAM-archived. The
            // archive write happened on the original intake (before
            // it was even queued in unacked). For Q6 promotion we
            // need the recipient-by stanza-id to point at; extract
            // from the message itself (it was stamped on intake by
            // the Canonicalize handler).
            let recipient_bare = match message.to.as_ref() {
                Some(jid) => jid.to_bare(),
                None => return PromotedOutcome::Dropped,
            };
            let recipient_jid = jid::Jid::from(recipient_bare.clone());
            let stanza_id =
                match waddle_xmpp_core::xep0359::extract_stanza_id_by(&message, &recipient_jid) {
                    Some(id) => id,
                    None => {
                        debug!(
                            sequence,
                            "Q6 promotion: classifier said Archived but no recipient \
                         <stanza-id> stamp present; falling back to Transient"
                        );
                        // Fallback: store inline as Transient so the
                        // message isn't lost, with a warn marker for the
                        // chain-misconfiguration suspicion.
                        return promote_as_transient(
                            message,
                            recipient_bare,
                            ctx.pending_storage,
                            ctx.original_receipt_fallback,
                            ctx.registry,
                        )
                        .await;
                    }
                };
            PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
                stanza_id,
                recipient_jid,
            ))
        }
        PendingDecision::Transient => PendingPayload::Transient(Box::new(message.clone())),
        PendingDecision::None => unreachable!("guarded above"),
    };

    let recipient_bare = match message.to.as_ref() {
        Some(jid) => jid.to_bare(),
        None => return PromotedOutcome::Dropped,
    };

    insert_pending(
        recipient_bare,
        payload,
        ctx.pending_storage,
        ctx.original_receipt_fallback,
        &message,
        ctx.registry,
    )
    .await
}
