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

mod barrier;
mod live;
mod pending;
mod stanza;
#[cfg(test)]
mod tests;
mod types;

use std::collections::VecDeque;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use kameo::actor::ActorRef;
use tracing::{debug, instrument};
use waddle_xmpp::pending_delivery::flush::{
    build_replay_stanza, MaterializedPayload, ReplayReason,
};
use waddle_xmpp::pending_delivery::storage::{PendingDeliveryStorage, PendingStorageError};
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRowId};
use waddle_xmpp::protocol::dm_routing::{
    classify_dm_intake, DmRouting, LiveDecision, OnlineResources, PendingDecision,
};
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::registry::{ConnectionRegistry, SendResult, UserRegistryActor};
use waddle_xmpp::stream_management::{
    DetachedSession, SmRegistryError, SmSessionDrainConfirmation, SmSessionPromotionAuthority,
    SmSessionPromotionLease, TOMBSTONE_CLOCK_SKEW_SLACK,
};
use waddle_xmpp::Stanza;

pub(crate) use barrier::session_has_unclassified_barrier;
use live::{build_online_resources, collect_live_targets};
use pending::{insert_pending, promote_as_transient, DeliveryHandles, PendingInsertAuthority};
use stanza::{parse_stanza, promote_iq, promote_presence};
pub use types::{PromotedOutcome, PromotionSummary};

/// Load fail-closed delivery policy and run one exact generation through the
/// promotion/tombstone window while its lease remains held by the caller.
pub(crate) async fn promote_with_tombstone_window(
    session: &DetachedSession,
    lease: &SmSessionPromotionLease,
    context: &'static str,
    deps: DisplacedPromotionDeps<'_>,
) -> Result<PromotionSummary, waddle_xmpp::xep::xep0191::BlockingStorageError> {
    let blocklist = Blocklist::new(
        deps.blocking_storage
            .list_blocked_jid_entries(&session.jid.to_bare())
            .await?,
    );
    let recent_tombstones =
        recent_tombstones_for_promotion(deps.sm_registry, context).unwrap_or_default();
    let summary = promote_session_unacked_with_promotion_authority(
        session,
        PromotionExecutionDeps {
            registry: deps.connection_registry,
            user_registry: deps.user_registry,
            pending_storage: deps.pending_storage,
            blocklist: &blocklist,
            server_domain: deps.server_domain,
            recent_tombstones: &recent_tombstones,
        },
        deps.sm_registry,
        lease,
    )
    .await;
    scrub_pending_for_tombstones_recorded_during_promotion(
        deps.sm_registry,
        deps.pending_storage,
        &recent_tombstones,
        context,
    )
    .await;
    Ok(summary)
}

/// Release linked pending rows under the captured fence before retiring the
/// durable SM generation. Registry confirmation releases ClaimStore last.
#[cfg(test)]
async fn release_pending_then_confirm_drained(
    sm_registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    session: &DetachedSession,
    lease: &mut SmSessionPromotionLease,
) -> Result<SmSessionDrainConfirmation, waddle_xmpp::pending_delivery::storage::PendingStorageError>
{
    release_pending_then_confirm_drained_observing(
        sm_registry,
        pending_storage,
        session,
        lease,
        |_| {},
    )
    .await
}

async fn release_pending_then_confirm_drained_observing<F>(
    sm_registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    session: &DetachedSession,
    lease: &mut SmSessionPromotionLease,
    observe_claim_release: F,
) -> Result<SmSessionDrainConfirmation, waddle_xmpp::pending_delivery::storage::PendingStorageError>
where
    F: FnMut(waddle_xmpp::stream_management::SmClaimReleaseRetryOutcome),
{
    if lease.authority() == SmSessionPromotionAuthority::CurrentDurable {
        let guard = match sm_registry.lock_current_promotion_mutation(lease).await {
            Ok(guard) => guard,
            Err(waddle_xmpp::stream_management::SmRegistryError::PromotionAuthorityLost) => {
                return Ok(if sm_registry.abandon_promotion_authority(lease) {
                    SmSessionDrainConfirmation::AuthorityLost
                } else {
                    SmSessionDrainConfirmation::Unconfirmed
                });
            }
            Err(error) => {
                tracing::warn!(
                    stream_id = %session.stream_id,
                    %error,
                    "Q6 drain: could not validate current-generation authority before release"
                );
                return Ok(SmSessionDrainConfirmation::Unconfirmed);
            }
        };
        let release_result = pending_storage
            .release_claim_under_sm_fence(guard.session_id(), guard.claim_fence())
            .await;
        drop(guard);
        match release_result {
            Ok(_) => {}
            Err(
                error @ waddle_xmpp::pending_delivery::storage::PendingStorageError::NotOwner {
                    ..
                },
            ) => {
                tracing::warn!(
                    stream_id = %session.stream_id,
                    %error,
                    "Q6 drain: linked pending-row release proved SM authority was lost"
                );
                return Ok(if sm_registry.abandon_promotion_authority(lease) {
                    SmSessionDrainConfirmation::AuthorityLost
                } else {
                    SmSessionDrainConfirmation::Unconfirmed
                });
            }
            Err(error) => return Err(error),
        }
    }
    Ok(sm_registry
        .confirm_drained_under_observing(lease, observe_claim_release)
        .await)
}

/// A session reached a terminal Q6 disposition, but retiring its exact SM
/// generation did not. Keeping registry and pending-delivery failures distinct
/// lets callers handle lost promotion authority without treating it as a
/// transient pending-store outage.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PromotionRetirementError {
    #[error("SM registry retirement failed: {0}")]
    Registry(#[from] SmRegistryError),
    #[error("pending-delivery retirement failed: {0}")]
    Pending(#[from] PendingStorageError),
}

/// Retire a terminally handled session without letting cancellation replay its
/// stanza payload. Terminal handling includes both successful Q6 promotion and
/// a deliberate dead-letter decision.
///
/// The guard's in-memory carrier is cleared synchronously, before the first
/// retirement await. Its exact sequence set is then removed from the durable
/// current or terminal generation before linked pending claims are released
/// and the generation is confirmed. If any await is cancelled, `Drop` can
/// therefore restore only an empty retirement token, never the Q6 payload.
pub(crate) async fn retire_promoted_session(
    sm_registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    promotion_guard: &mut PromotionSessionGuard<'_>,
    lease: &mut SmSessionPromotionLease,
) -> Result<SmSessionDrainConfirmation, PromotionRetirementError> {
    retire_promoted_session_observing(sm_registry, pending_storage, promotion_guard, lease, |_| {})
        .await
}

/// Retire a promoted session while synchronously observing the exact shared
/// claim release outcome, if this generation proves itself to be the last
/// durable carrier. The observer runs before the retirement future can reach
/// another cancellation point.
pub(crate) async fn retire_promoted_session_observing<F>(
    sm_registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    promotion_guard: &mut PromotionSessionGuard<'_>,
    lease: &mut SmSessionPromotionLease,
    observe_claim_release: F,
) -> Result<SmSessionDrainConfirmation, PromotionRetirementError>
where
    F: FnMut(waddle_xmpp::stream_management::SmClaimReleaseRetryOutcome),
{
    let sequences = promotion_guard.take_retirement_sequences();
    let checkpoint_result = match lease.authority() {
        SmSessionPromotionAuthority::CurrentDurable => {
            sm_registry
                .delete_unacked_sequences_under(lease, &sequences)
                .await
        }
        SmSessionPromotionAuthority::TerminalDurable => {
            sm_registry
                .delete_terminal_unacked_sequences_under(lease, &sequences)
                .await
        }
        SmSessionPromotionAuthority::ObsoleteGeneration => Ok(0),
    };
    if let Err(error) = checkpoint_result {
        if promotion_registry_error_is_authority_lost(&error) {
            return Ok(if sm_registry.abandon_promotion_authority(lease) {
                SmSessionDrainConfirmation::AuthorityLost
            } else {
                SmSessionDrainConfirmation::Unconfirmed
            });
        }
        return Err(error.into());
    }
    Ok(release_pending_then_confirm_drained_observing(
        sm_registry,
        pending_storage,
        promotion_guard.session(),
        lease,
        observe_claim_release,
    )
    .await?)
}

pub(crate) fn promotion_registry_error_is_authority_lost(
    error: &waddle_xmpp::stream_management::SmRegistryError,
) -> bool {
    matches!(
        error,
        waddle_xmpp::stream_management::SmRegistryError::PromotionAuthorityLost
            | waddle_xmpp::stream_management::SmRegistryError::Persistence(
                waddle_xmpp::stream_management::persistence::SmPersistenceError::NotOwner { .. }
            )
    )
}

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
    skip(session, registry, user_registry, pending_storage, blocklist, recent_tombstones),
    fields(stream_id = %session.stream_id, jid = %session.jid)
)]
#[cfg(test)]
pub async fn promote_session_unacked(
    session: &DetachedSession,
    registry: &ConnectionRegistry,
    user_registry: &ActorRef<UserRegistryActor>,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    blocklist: &Blocklist,
    server_domain: &str,
    recent_tombstones: &[waddle_xmpp::stream_management::RecentTombstoneRecord],
) -> PromotionSummary {
    let source_session_id =
        waddle_xmpp::pending_delivery::SmSessionId::new(session.stream_id.clone());
    promote_session_unacked_with_authority(
        session,
        PromotionExecutionDeps {
            registry,
            user_registry,
            pending_storage,
            blocklist,
            server_domain,
            recent_tombstones,
        },
        PendingInsertAuthority::TestCurrent {
            session_id: &source_session_id,
            fence: None,
        },
    )
    .await
}

#[derive(Clone, Copy)]
pub(crate) struct PromotionExecutionDeps<'a> {
    pub registry: &'a ConnectionRegistry,
    pub user_registry: &'a ActorRef<UserRegistryActor>,
    pub pending_storage: &'a Arc<dyn PendingDeliveryStorage>,
    pub blocklist: &'a Blocklist,
    pub server_domain: &'a str,
    pub recent_tombstones: &'a [waddle_xmpp::stream_management::RecentTombstoneRecord],
}

pub(crate) async fn promote_session_unacked_with_promotion_authority(
    session: &DetachedSession,
    deps: PromotionExecutionDeps<'_>,
    sm_registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    lease: &SmSessionPromotionLease,
) -> PromotionSummary {
    promote_session_unacked_with_authority(
        session,
        deps,
        match lease.authority() {
            SmSessionPromotionAuthority::CurrentDurable => PendingInsertAuthority::CurrentSm {
                registry: sm_registry,
                lease,
            },
            SmSessionPromotionAuthority::TerminalDurable => PendingInsertAuthority::TerminalSm {
                registry: sm_registry,
                lease,
            },
            SmSessionPromotionAuthority::ObsoleteGeneration => {
                PendingInsertAuthority::ObsoleteGeneration
            }
        },
    )
    .await
}

#[instrument(
    skip(session, deps, authority),
    fields(stream_id = %session.stream_id, jid = %session.jid)
)]
async fn promote_session_unacked_with_authority(
    session: &DetachedSession,
    deps: PromotionExecutionDeps<'_>,
    authority: PendingInsertAuthority<'_>,
) -> PromotionSummary {
    let PromotionExecutionDeps {
        registry,
        user_registry,
        pending_storage,
        blocklist,
        server_domain,
        recent_tombstones,
    } = deps;
    let mut summary = PromotionSummary::default();
    let recipient_bare = session.jid.to_bare();

    let source_session_id = match authority {
        PendingInsertAuthority::CurrentSm { lease, .. } => Some(lease.session_id()),
        // Terminal and obsolete work is generation-qualified. A pending row
        // linked only by the bare stream id may belong to a current same-id
        // successor, so these authorities must neither classify nor reuse it.
        PendingInsertAuthority::TerminalSm { .. } | PendingInsertAuthority::ObsoleteGeneration => {
            None
        }
        #[cfg(test)]
        PendingInsertAuthority::TestCurrent { session_id, .. } => Some(session_id),
    };

    // Purpose is the recovery-policy boundary. Classify every typed resume
    // barrier before any application-only pending-row preflight can fail: a
    // generic storage error must never downgrade a permanent barrier
    // invariant into the transient retry/dead-letter budget.
    let barrier_pending_links =
        barrier::load_pending_links(session, source_session_id, pending_storage).await;
    for entry in session
        .unacked_stanzas
        .iter()
        .filter(|entry| entry.is_resume_barrier())
    {
        let outcome = barrier::classify(session, entry, &barrier_pending_links);
        summary.record(entry.sequence, &outcome);
    }
    let has_application_entries = session
        .unacked_stanzas
        .iter()
        .any(|entry| !entry.is_resume_barrier());

    let linked_pending_by_sequence = match (source_session_id, has_application_entries) {
        (Some(source_session_id), true) => {
            let rows = match pending_storage
                .list_claimed_by_session(source_session_id)
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(
                        stream_id = %session.stream_id,
                        %error,
                        "Q6 promotion: could not classify existing pending-row ownership"
                    );
                    for entry in session
                        .unacked_stanzas
                        .iter()
                        .filter(|entry| !entry.is_resume_barrier())
                    {
                        summary.record(entry.sequence, &PromotedOutcome::StorageFailure);
                    }
                    return summary;
                }
            };
            let mut linked = std::collections::HashMap::new();
            for row in rows {
                let structurally_valid = row.recipient == recipient_bare
                    && row.flushed_in_session.as_ref() == Some(source_session_id)
                    && row.outbound_sequence.is_some();
                if !structurally_valid {
                    tracing::error!(
                        stream_id = %session.stream_id,
                        row_id = %row.id,
                        row_recipient = %row.recipient,
                        returned_session = ?row.flushed_in_session,
                        outbound_sequence = ?row.outbound_sequence,
                        "Q6 promotion: exact session lookup returned an ambiguous pending-row relation"
                    );
                    for entry in session
                        .unacked_stanzas
                        .iter()
                        .filter(|entry| !entry.is_resume_barrier())
                    {
                        summary.record(entry.sequence, &PromotedOutcome::StorageFailure);
                    }
                    return summary;
                }
                let sequence = row.outbound_sequence.expect("validated above");
                if linked.insert(sequence, row.id).is_some() {
                    tracing::error!(
                        stream_id = %session.stream_id,
                        sequence,
                        "Q6 promotion: multiple pending rows claim one SM sequence"
                    );
                    for entry in session
                        .unacked_stanzas
                        .iter()
                        .filter(|entry| !entry.is_resume_barrier())
                    {
                        summary.record(entry.sequence, &PromotedOutcome::StorageFailure);
                    }
                    return summary;
                }
            }
            linked
        }
        (None, _) | (_, false) => std::collections::HashMap::new(),
    };

    // Snapshot the recipient's currently-online resources for the
    // classifier. Empty in the common SM-expiry case (otherwise
    // the session wouldn't have been detached in the first place,
    // unless other resources joined after detach).
    let online = build_online_resources(registry, user_registry, &recipient_bare).await;

    // Round-2 review R2: select the sequences a recently applied
    // tombstone matches, using the same shared matcher as the
    // registry / pending-delivery scrubs. Round-3 review finding 2: a
    // tombstone applies BACKWARD in time only — a stanza whose
    // original receipt postdates the tombstone's recording is a NEW
    // message that merely reuses the wire id in the same scope, not
    // the retracted one, and must promote normally. Round-4 review:
    // the two stamps can come from different clocks (persistence
    // restore across a restart, multi-node stamps, NTP step-back), so
    // the boundary carries a skew slack — a receipt reading up to
    // TOMBSTONE_CLOCK_SKEW_SLACK "after" the recording still scrubs.
    // Real wire-id reuse arriving that close to its own retraction is
    // implausible; retracted-content delivery is the worse failure.
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
                waddle_xmpp::tombstone::matching_tombstone_sequences(&entries, &record.key)
                    .into_iter()
                    .filter(|sequence| {
                        receipt_by_sequence
                            .get(sequence)
                            .is_some_and(|received_at| {
                                *received_at <= record.recorded_at_utc + TOMBSTONE_CLOCK_SKEW_SLACK
                            })
                    })
                    .collect::<Vec<u32>>()
            })
            .collect()
    };

    for entry in &session.unacked_stanzas {
        let linked_pending_row = linked_pending_by_sequence.get(&entry.sequence);
        if entry.is_resume_barrier() {
            continue;
        }
        let outcome = if tombstoned_sequences.contains(&entry.sequence) {
            debug!(
                stream_id = %session.stream_id,
                sequence = entry.sequence,
                "Q6 promotion: stanza matches a recent tombstone; scrubbed"
            );
            PromotedOutcome::Scrubbed
        } else {
            match parse_stanza(&entry.stanza_xml) {
                Some(Stanza::Message(message)) => {
                    let ctx = PromotionContext {
                        online: &online,
                        blocklist,
                        registry,
                        user_registry,
                        pending_storage,
                        original_receipt_fallback: entry.original_receipt_at,
                        server_domain,
                        authority,
                        reuse_claimed_pending: linked_pending_row.is_some(),
                    };
                    promote_one(message, entry.sequence, ctx).await
                }
                Some(Stanza::Iq(iq)) => promote_iq(*iq, registry).await,
                Some(Stanza::Presence(presence)) => promote_presence(presence, registry).await,
                None => PromotedOutcome::Unparseable,
            }
        };
        let outcome = finalize_linked_pending_row(
            outcome,
            linked_pending_row,
            pending_storage,
            authority,
            entry.sequence,
        )
        .await;
        debug!(
            stream_id = %session.stream_id,
            sequence = entry.sequence,
            ?outcome,
            "Q6 promotion: per-stanza outcome"
        );
        summary.record(entry.sequence, &outcome);
        if matches!(outcome, PromotedOutcome::AuthorityLost) {
            break;
        }
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
        quarantined = summary.quarantined,
        "Q6 promotion: session summary"
    );
    summary
}

/// Retire a linked pending row whenever Q6 handled its SM entry by another
/// outcome. `Queued` keeps the existing row until the terminal claim release.
async fn finalize_linked_pending_row(
    outcome: PromotedOutcome,
    linked_row: Option<&PendingRowId>,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    authority: PendingInsertAuthority<'_>,
    sequence: u32,
) -> PromotedOutcome {
    let Some(row_id) = linked_row else {
        return outcome;
    };
    if matches!(
        outcome,
        PromotedOutcome::Queued | PromotedOutcome::StorageFailure | PromotedOutcome::AuthorityLost
    ) {
        return outcome;
    }
    let delete_result = match authority {
        PendingInsertAuthority::CurrentSm { registry, lease } => {
            let guard = match registry.lock_current_promotion_mutation(lease).await {
                Ok(guard) => guard,
                Err(waddle_xmpp::stream_management::SmRegistryError::PromotionAuthorityLost) => {
                    return PromotedOutcome::AuthorityLost;
                }
                Err(error) => {
                    tracing::warn!(
                        sequence,
                        row_id = %row_id,
                        %error,
                        "Q6 promotion: could not validate current-generation authority before linked-row retirement"
                    );
                    return PromotedOutcome::StorageFailure;
                }
            };
            pending_storage
                .delete_row_under_sm_fence(row_id, guard.session_id(), guard.claim_fence())
                .await
        }
        PendingInsertAuthority::TerminalSm { .. } => {
            // A terminal generation never owns a row discovered through the
            // bare successor link. Leave it for the current generation.
            return outcome;
        }
        #[cfg(test)]
        PendingInsertAuthority::TestCurrent { session_id, fence } => {
            pending_storage
                .delete_row_under_sm_fence(row_id, session_id, fence)
                .await
        }
        PendingInsertAuthority::ObsoleteGeneration => {
            // An obsolete same-id generation never owns linked pending state.
            // The row may already have been released and reclaimed by a newer
            // session; a bare delete here would let stale completion erase that
            // newer claim. Leave it for the current generation's fenced ack or
            // terminal release and retire only the obsolete local token.
            return outcome;
        }
    };
    match delete_result {
        Ok(_) => outcome,
        Err(error) => {
            tracing::warn!(
                sequence,
                row_id = %row_id,
                %error,
                "Q6 promotion: could not retire the linked pending row"
            );
            if matches!(
                error,
                waddle_xmpp::pending_delivery::storage::PendingStorageError::NotOwner { .. }
            ) {
                PromotedOutcome::AuthorityLost
            } else {
                PromotedOutcome::StorageFailure
            }
        }
    }
}

/// Dependencies for [`promote_displaced_sessions`]. Grouped so the
/// two displacement call sites (max_sessions eviction at detach,
/// fresh-bind invalidation at registration) share one signature.
pub struct DisplacedPromotionDeps<'a> {
    pub sm_registry: &'a waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    pub connection_registry: &'a ConnectionRegistry,
    pub user_registry: &'a ActorRef<UserRegistryActor>,
    pub pending_storage: &'a Arc<dyn PendingDeliveryStorage>,
    pub blocking_storage: &'a dyn waddle_xmpp::xep::xep0191::BlockingStorage,
    pub server_domain: &'a str,
}

pub(crate) struct PromotionBatchGuard<'a> {
    registry: &'a waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    sessions: VecDeque<DetachedSession>,
}

impl<'a> PromotionBatchGuard<'a> {
    pub(crate) fn new(
        registry: &'a waddle_xmpp::stream_management::InMemorySmSessionRegistry,
        sessions: Vec<DetachedSession>,
    ) -> Self {
        Self {
            registry,
            sessions: sessions.into(),
        }
    }

    pub(crate) fn pop(&mut self) -> Option<DetachedSession> {
        self.sessions.pop_front()
    }
}

impl Drop for PromotionBatchGuard<'_> {
    fn drop(&mut self) {
        for session in self.sessions.drain(..) {
            if let Err(error) = self.registry.retain_pending_promotion_for_retry(session) {
                tracing::warn!(
                    %error,
                    "cancelled displaced-promotion batch could not restore an unstarted session"
                );
            }
        }
    }
}

pub(crate) struct PromotionSessionGuard<'a> {
    registry: &'a waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    session: DetachedSession,
    armed: bool,
}

impl<'a> PromotionSessionGuard<'a> {
    pub(crate) fn new(
        registry: &'a waddle_xmpp::stream_management::InMemorySmSessionRegistry,
        session: DetachedSession,
    ) -> Self {
        Self {
            registry,
            session,
            armed: true,
        }
    }

    pub(crate) fn session(&self) -> &DetachedSession {
        &self.session
    }

    /// Capture the exact durable sequence set while synchronously turning this
    /// guard into an empty retirement token. This method must remain free of
    /// awaits: `Drop` is the cancellation recovery path.
    fn take_retirement_sequences(&mut self) -> Vec<u32> {
        let sequences = self
            .session
            .unacked_stanzas
            .iter()
            .map(|entry| entry.sequence)
            .collect();
        self.session.unacked_stanzas.clear();
        sequences
    }

    pub(crate) fn complete(&mut self) {
        self.armed = false;
    }
}

impl Drop for PromotionSessionGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = self
                .registry
                .retain_pending_promotion_for_retry(self.session.clone())
            {
                tracing::warn!(
                    stream_id = %self.session.stream_id,
                    %error,
                    "cancelled displaced promotion could not restore its session"
                );
            }
        }
    }
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
    lease: &mut SmSessionPromotionLease,
    session: DetachedSession,
) -> bool {
    let stream_id = session.stream_id.clone();
    let jid = session.jid.clone();
    if let Err(error) = sm_registry.reinsert_for_retry_under(lease, session).await {
        tracing::warn!(
            jid = %jid,
            stream_id = %stream_id,
            %error,
            "failed to re-insert SM session for promotion retry; durable rows \
             will only be retried after a restart"
        );
        return false;
    }
    true
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
    lease: &mut SmSessionPromotionLease,
    mut session: DetachedSession,
    summary: &PromotionSummary,
) -> bool {
    if !summary.promoted_sequences.is_empty() {
        let delete_result = match lease.authority() {
            SmSessionPromotionAuthority::CurrentDurable => {
                sm_registry
                    .delete_unacked_sequences_under(lease, &summary.promoted_sequences)
                    .await
            }
            SmSessionPromotionAuthority::TerminalDurable => {
                sm_registry
                    .delete_terminal_unacked_sequences_under(lease, &summary.promoted_sequences)
                    .await
            }
            SmSessionPromotionAuthority::ObsoleteGeneration => Ok(0),
        };
        match delete_result {
            Ok(_) => {
                session
                    .unacked_stanzas
                    .retain(|entry| !summary.promoted_sequences.contains(&entry.sequence));
            }
            Err(error) if promotion_registry_error_is_authority_lost(&error) => {
                return sm_registry.abandon_promotion_authority(lease);
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
    reinsert_failed_session_for_retry(sm_registry, lease, session).await
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
) -> Result<
    Vec<waddle_xmpp::stream_management::RecentTombstoneRecord>,
    waddle_xmpp::stream_management::SmRegistryError,
> {
    match sm_registry.recent_tombstones() {
        Ok(records) => Ok(records),
        Err(error) => {
            tracing::warn!(
                %error,
                context,
                "recent-tombstone read failed; proceeding without the \
                 promotion-time tombstone re-check"
            );
            Err(error)
        }
    }
}

/// Close the retraction-vs-promotion TOCTOU (adversarial-review
/// finding B): the recent-tombstone snapshot is fetched BEFORE
/// `promote_session_unacked`. A retraction landing after that fetch
/// finds the session off both registry maps (scrub phases 1-4 see
/// nothing in memory; the pending row is not inserted yet), and the
/// promotion then writes the retracted stanza into `pending_delivery`
/// — delivered verbatim at the next login once `confirm_drained`
/// erased the SM rows.
///
/// Called after `promote_session_unacked` returns and BEFORE
/// `confirm_drained`: re-read the recent tombstones and, for every
/// record NOT present in the pre-promotion snapshot (i.e. recorded
/// during the promotion window), run the pending-delivery scrub so the
/// just-inserted rows are removed again. Idempotent and best-effort —
/// a scrub failure is logged; the row also stays covered by the
/// interpret-layer scrub retry semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionScrubOutcome {
    Completed,
    Failed,
}

pub async fn scrub_pending_for_tombstones_recorded_during_promotion(
    sm_registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    pre_promotion: &[waddle_xmpp::stream_management::RecentTombstoneRecord],
    context: &'static str,
) -> PromotionScrubOutcome {
    let post_promotion = match sm_registry.recent_tombstones() {
        Ok(records) => records,
        Err(error) => {
            tracing::warn!(
                %error,
                context,
                "post-promotion recent-tombstone read failed; a retraction that \
                 raced this promotion window may deliver at next login"
            );
            return PromotionScrubOutcome::Failed;
        }
    };
    let mut completed = true;
    for record in post_promotion
        .iter()
        .filter(|record| !pre_promotion.contains(record))
    {
        match pending_storage.scrub_for_tombstone(&record.key).await {
            Ok(removed) if removed > 0 => {
                debug!(
                    target = record.key.id(),
                    archive = %record.key.archive_jid(),
                    removed,
                    context,
                    "post-promotion re-check: scrubbed pending rows for a \
                     tombstone recorded mid-promotion"
                );
            }
            Ok(_) => {}
            Err(error) => {
                completed = false;
                tracing::warn!(
                    target = record.key.id(),
                    archive = %record.key.archive_jid(),
                    %error,
                    context,
                    "post-promotion re-check: pending scrub failed; retracted \
                     content may deliver at the recipient's next login"
                );
            }
        }
    }
    if completed {
        PromotionScrubOutcome::Completed
    } else {
        PromotionScrubOutcome::Failed
    }
}

pub async fn promote_displaced_sessions(
    sessions: Vec<DetachedSession>,
    deps: DisplacedPromotionDeps<'_>,
) {
    let mut batch_guard = PromotionBatchGuard::new(deps.sm_registry, sessions);
    while let Some(pending_session) = batch_guard.pop() {
        let mut promotion_guard = PromotionSessionGuard::new(deps.sm_registry, pending_session);
        let session = promotion_guard.session().clone();
        let mut lease = match deps.sm_registry.acquire_promotion_lease(&session).await {
            Ok(Some(lease)) => lease,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    stream_id = %session.stream_id,
                    generation_id = ?session.generation_id,
                    %error,
                    "displaced SM session: could not acquire exact promotion lease"
                );
                continue;
            }
        };
        let summary = match promote_with_tombstone_window(
            &session,
            &lease,
            "displaced SM promotion",
            DisplacedPromotionDeps {
                sm_registry: deps.sm_registry,
                connection_registry: deps.connection_registry,
                user_registry: deps.user_registry,
                pending_storage: deps.pending_storage,
                blocking_storage: deps.blocking_storage,
                server_domain: deps.server_domain,
            },
        )
        .await
        {
            Ok(summary) => summary,
            Err(error) => {
                waddle_xmpp::telemetry::reliability::increment_sm_promotion_blocklist_failed();
                if session_has_unclassified_barrier(&session) {
                    tracing::warn!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        error = %error,
                        "displaced SM session: blocklist load failed before resume-barrier \
                         classification; preserving durable rows without consuming the \
                         transient retry budget"
                    );
                    if reinsert_failed_session_for_retry(
                        deps.sm_registry,
                        &mut lease,
                        session.clone(),
                    )
                    .await
                    {
                        promotion_guard.complete();
                    }
                    continue;
                }
                let record_result =
                    if lease.authority() != SmSessionPromotionAuthority::ObsoleteGeneration {
                        deps.sm_registry
                            .record_promotion_failure_under(&lease)
                            .await
                    } else {
                        Ok(0)
                    };
                if let Err(record_error) = record_result {
                    if promotion_registry_error_is_authority_lost(&record_error) {
                        if deps.sm_registry.abandon_promotion_authority(&mut lease) {
                            promotion_guard.complete();
                        }
                        continue;
                    }
                    tracing::warn!(
                        jid = %session.jid,
                        error = %error,
                        record_error = %record_error,
                        "displaced SM session: blocklist load and failure recording both failed"
                    );
                }
                if reinsert_failed_session_for_retry(deps.sm_registry, &mut lease, session.clone())
                    .await
                {
                    promotion_guard.complete();
                }
                continue;
            }
        };
        if summary.has_authority_lost() {
            if deps.sm_registry.abandon_promotion_authority(&mut lease) {
                promotion_guard.complete();
            }
            continue;
        }
        if summary.has_quarantined() {
            tracing::error!(
                jid = %session.jid,
                stream_id = %session.stream_id,
                quarantined = summary.quarantined,
                "displaced SM session: promotion found unreconciled durable invariants; \
                 preserving durable rows without consuming the transient retry budget"
            );
            if prune_promoted_then_reinsert_for_retry(
                deps.sm_registry,
                &mut lease,
                session.clone(),
                &summary,
            )
            .await
            {
                promotion_guard.complete();
            }
            continue;
        }
        if summary.has_storage_failure() {
            waddle_xmpp::telemetry::reliability::add_sm_promotion_storage_failed(u64::from(
                summary.storage_failed,
            ));
            let record_result =
                if lease.authority() != SmSessionPromotionAuthority::ObsoleteGeneration {
                    deps.sm_registry
                        .record_promotion_failure_under(&lease)
                        .await
                } else {
                    Ok(0)
                };
            if let Err(error) = record_result {
                if promotion_registry_error_is_authority_lost(&error) {
                    if deps.sm_registry.abandon_promotion_authority(&mut lease) {
                        promotion_guard.complete();
                    }
                    continue;
                }
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
            if prune_promoted_then_reinsert_for_retry(
                deps.sm_registry,
                &mut lease,
                session.clone(),
                &summary,
            )
            .await
            {
                promotion_guard.complete();
            }
            continue;
        }
        match retire_promoted_session(
            deps.sm_registry,
            deps.pending_storage,
            &mut promotion_guard,
            &mut lease,
        )
        .await
        {
            Ok(
                SmSessionDrainConfirmation::CurrentDurableConfirmed
                | SmSessionDrainConfirmation::TerminalDurableConfirmed
                | SmSessionDrainConfirmation::ObsoleteGenerationRetired
                | SmSessionDrainConfirmation::PayloadRetiredClaimReconciliationPending,
            ) => promotion_guard.complete(),
            Ok(SmSessionDrainConfirmation::AuthorityLost) => {
                promotion_guard.complete();
                continue;
            }
            Ok(SmSessionDrainConfirmation::Unconfirmed) => {
                let retirement_token = promotion_guard.session().clone();
                if reinsert_failed_session_for_retry(deps.sm_registry, &mut lease, retirement_token)
                    .await
                {
                    promotion_guard.complete();
                }
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    jid = %session.jid,
                    stream_id = %session.stream_id,
                    %error,
                    "displaced SM session: retirement checkpoint failed; preserving only the empty retirement token"
                );
                let retirement_token = promotion_guard.session().clone();
                if reinsert_failed_session_for_retry(deps.sm_registry, &mut lease, retirement_token)
                    .await
                {
                    promotion_guard.complete();
                }
                continue;
            }
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
            quarantined = summary.quarantined,
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
    user_registry: &'a ActorRef<UserRegistryActor>,
    pending_storage: &'a Arc<dyn PendingDeliveryStorage>,
    original_receipt_fallback: DateTime<Utc>,
    server_domain: &'a str,
    authority: PendingInsertAuthority<'a>,
    reuse_claimed_pending: bool,
}

/// Promote a single typed [`xmpp_parsers::message::Message`] per the
/// locked Q6 chain.
async fn promote_one(
    message: xmpp_parsers::message::Message,
    sequence: u32,
    mut ctx: PromotionContext<'_>,
) -> PromotedOutcome {
    if waddle_xmpp_core::mam::is_mam_query_response_message(&message) {
        waddle_xmpp::telemetry::reliability::increment_sm_promotion_not_promotable();
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
        let targets =
            collect_live_targets(&routing, &message, ctx.registry, ctx.user_registry).await;
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

    if ctx.reuse_claimed_pending {
        return PromotedOutcome::Queued;
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
                            DeliveryHandles {
                                registry: ctx.registry,
                                user_registry: ctx.user_registry,
                            },
                            ctx.authority,
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
        DeliveryHandles {
            registry: ctx.registry,
            user_registry: ctx.user_registry,
        },
        ctx.authority,
    )
    .await
}
