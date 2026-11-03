//! Implementation behind the parent module facade.

use super::routes;
use super::run_ingress_custody_sweep;

use super::Arc;
use super::CriticalNodeFailure;
use super::DbExecute;
use super::Duration;
use super::HashMap;
use super::HashSet;
use super::LeaveSessionSelector;
use super::LocalDepartureRetryOutcome;
#[cfg(feature = "clustering")]
use super::MembershipGenerationFilter;
use super::MirrorUnregisterOutcome;
use super::NodeLifecycle;
use super::NotificationDrainDeps;
#[cfg(feature = "clustering")]
use super::OrderedRelayRouteOrigin;
#[cfg(feature = "clustering")]
use super::OrderedRelayRouteOriginKind;
#[cfg(feature = "clustering")]
use super::OrphanReaperCursorUpdate;
#[cfg(feature = "clustering")]
use super::OrphanTerminalCleanupFailureReason;
#[cfg(feature = "clustering")]
use super::OrphanTerminalCleanupLane;
#[cfg(feature = "clustering")]
use super::OrphanWorkQueue;
#[cfg(feature = "clustering")]
use super::OrphanWorker;
#[cfg(feature = "clustering")]
use super::OrphanWorkerFailureReason;
#[cfg(feature = "clustering")]
use super::OrphanedSmSessionClaimPage;
use super::PendingRecovery;
use super::PromotionBatchGuard;
use super::PromotionScrubOutcome;
use super::PromotionSessionGuard;
#[cfg(feature = "clustering")]
use super::RoomOrphanReconciliationOutcome;
#[cfg(feature = "clustering")]
use super::RoomOrphanScanCursor;
use super::RoomRegistryActorPolicy;
#[cfg(feature = "clustering")]
use super::SmOrphanScanCursor;
use super::TerminalOverflowPromotionDeps;
#[cfg(feature = "clustering")]
use super::VecDeque;
use super::WebSocketState;
use super::{debug, error, info, warn, Instrument};
use super::{Janitor, MetricAttribute, SweepOutcome};

/// Named root span for one janitor sweep tick (#1483).
///
/// Janitor loops run with no active span, so the actor.handle_message
/// spans their asks mint would root the trace — exactly the shape the
/// #1438 span-noise sampler drops. One root per sweep (never per actor
/// message) keeps the sweep's actor work parented and traceable.
/// `parent: None` guarantees root semantics even if a caller is
/// instrumented. The span name is documented in `telemetry::span_noise`
/// and must never be added to its suppression lists.
pub(in crate::server::session_janitors) fn janitor_sweep_span(janitor: Janitor) -> tracing::Span {
    tracing::info_span!(parent: None, "janitor.sweep", janitor = janitor.value())
}

/// The active span's OTel context, captured at work-enqueue time so a
/// later attempt can *link* back to the sweep that found the work.
#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) fn current_sweep_context(
) -> opentelemetry::trace::SpanContext {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    tracing::Span::current()
        .context()
        .span()
        .span_context()
        .clone()
}

/// Root span for one orphan-reaper work-item attempt (#1483).
///
/// Worker attempts must NOT run under the enqueuing sweep's live span:
/// retry queues and pending inventories would hold that root open
/// indefinitely — unbounded duration, lost on crash, the very
/// `actor.lifecycle` pathology the #1438 sampler kills. Each attempt
/// gets its own short-lived root instead, linked (not parented) to the
/// enqueuing sweep when its context is valid. The span name is
/// documented in `telemetry::span_noise` and must never be added to its
/// suppression lists.
#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) fn orphan_work_span(
    lane: &'static str,
    sweep_context: &opentelemetry::trace::SpanContext,
) -> tracing::Span {
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let span = tracing::info_span!(
        parent: None,
        "janitor.orphan_work",
        janitor = Janitor::OrphanReaper.value(),
        lane,
    );
    if sweep_context.is_valid() {
        span.add_link(sweep_context.clone());
    }
    span
}

/// Default interval for the auth-state TTL janitor.
pub(in crate::server::session_janitors) const AUTH_JANITOR_INTERVAL: Duration =
    Duration::from_secs(60);

/// Default interval for the persistent-room dormancy janitor.
pub(in crate::server::session_janitors) const ROOM_DORMANCY_JANITOR_INTERVAL: Duration =
    Duration::from_secs(300);

/// Owner-IQ destroy completions are normally drained inline. This short
/// backstop also covers reconciliation completed by a reaper after the
/// original request lost its registry reply.
pub(in crate::server::session_janitors) const DESTROY_COMPLETION_JANITOR_INTERVAL: Duration =
    Duration::from_secs(1);

/// Default interval for the empty-`UserActor` reaper (ADR-0017 Phase 1
/// Slice 2). Matches the room dormancy cadence: orphaned empty actors are
/// harmless between sweeps (they route to `NotConnected`/detached), so a
/// 5-minute reap keeps `UserRegistryActor.users` bounded without hot-looping.
pub(in crate::server::session_janitors) const USER_ACTOR_REAPER_INTERVAL: Duration =
    Duration::from_secs(300);

/// A receipt is read only by the bounded recovery of the attempt that minted
/// it. One day is far beyond that window, so older rows are stranded cleanup.
#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) const ADMIN_RECEIPT_RETENTION: Duration =
    Duration::from_secs(24 * 60 * 60);
#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) const ADMIN_RECEIPT_PRUNE_LIMIT: usize = 256;
pub(in crate::server::session_janitors) const LOCAL_MUC_DEPARTURE_INTERVAL: Duration =
    Duration::from_secs(2);

/// Retry retained local MUC departures. These entries share the lifetime of
/// local room occupancy, so a process crash discards both sides together.
pub(crate) fn spawn_local_muc_departure_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(LOCAL_MUC_DEPARTURE_INTERVAL);
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            super::run_local_muc_departure_sweep(&state)
                .instrument(janitor_sweep_span(Janitor::LocalMucDeparture))
                .await;
            waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                Janitor::LocalMucDeparture,
                SweepOutcome::Completed,
            );
        }
    });
}

/// Items one sweep pass drains at most; the remainder stay due for the next
/// tick so a post-outage backlog cannot pin the janitor on one serial pass.
pub(in crate::server::session_janitors) const LOCAL_MUC_DEPARTURE_SWEEP_BUDGET: usize = 256;

pub(crate) async fn run_local_muc_departure_sweep(state: &WebSocketState) {
    use crate::admin::channels::{broadcast_group_dm_leave, GroupDmLeaveEffect};
    use crate::server::routes::websocket::{
        broadcast_muc_leave_to_remaining_resumable,
        broadcast_muc_muji_clear_to_remaining_resumable, get_room_actor_result,
        maybe_evict_empty_room, LeaveFanOutProgress, LocalDepartureItem, PendingLocalDeparture,
    };
    use waddle_xmpp::muc::durable::OccupancyLeaveCause;
    use waddle_xmpp::muc::room_actor::{LeaveByRealJid, LeaveDisposition};

    let mut batch = state
        .deps
        .protocol
        .pending_local_muc_departures
        .take_due_bounded(std::time::Instant::now(), LOCAL_MUC_DEPARTURE_SWEEP_BUDGET);
    // Owed acknowledgements run before any departure retry of this pass: a
    // drained acknowledgement is invisible to the barrier below (it scans the
    // inventory), and an acknowledgement that fails here is requeued, where
    // the barrier does see it.
    batch.sort_by_key(|pending| !matches!(pending.item, LocalDepartureItem::AckReceipt { .. }));
    for pending in batch {
        let mut item = pending.item;
        // #1647 (codex round 27): one inventory entry can stand for SEVERAL
        // owed receipts — the actor retains them per (JID, nick) while this
        // inventory coalesces per (room, JID, cause). After each successful
        // replay+ack, the entry re-drives (bounded) until the fallback
        // replay runs dry, so no owed receipt is left vetoing dormancy.
        const MAX_RECEIPT_DRAIN: u32 = 16;
        let mut drained_receipts: u32 = 0;
        loop {
            match item {
                LocalDepartureItem::FullJidSweep {
                    jid,
                    selector,
                    attempt,
                    remote_ceiling,
                    removal,
                } => {
                    // A live registration of the same full JID does NOT drop
                    // the sweep: the new connection has not necessarily
                    // rejoined every room the terminated session occupied.
                    // The sweep's attempt (minted at the ORIGINAL cleanup) is
                    // the fence — sessions that (re)joined since are
                    // `Superseded` by the actor, older occupancies converge.
                    match routes::websocket::redrive_local_muc_cleanup(
                        state,
                        &jid,
                        selector,
                        attempt,
                        remote_ceiling,
                        removal,
                    )
                    .await
                    {
                        routes::websocket::MucCleanupOutcome::Completed => {
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Completed,
                            );
                        }
                        routes::websocket::MucCleanupOutcome::Failed => {
                            // The cleanup re-recorded whatever it could not
                            // finish; keep THIS sweep's attempt count and
                            // backoff instead of re-arming from zero.
                            state
                                .deps
                                .protocol
                                .pending_local_muc_departures
                                .requeue_with_backoff(PendingLocalDeparture {
                                    item: LocalDepartureItem::FullJidSweep {
                                        jid,
                                        selector,
                                        attempt,
                                        remote_ceiling,
                                        removal,
                                    },
                                    attempts: pending.attempts,
                                    not_before: pending.not_before,
                                });
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Requeued,
                            );
                        }
                    }
                    break;
                }
                LocalDepartureItem::RoomDeparture {
                    room,
                    jid,
                    cause,
                    selector,
                    attempt,
                    notified,
                    removal,
                } => {
                    // A reconnected JID does NOT short-circuit the retry:
                    // the retained attempt predates the rejoin, so the
                    // actor's own fences classify it (order fence =>
                    // Superseded; receipt replay => the owed effects) instead
                    // of the janitor guessing from live-connection state and
                    // dropping a departure the room never observed.
                    let actor = match get_room_actor_result(state, &room).await {
                        Ok(Some(actor)) => actor,
                        Ok(None) => {
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::ActorGone,
                            );

                            break;
                        }
                        Err(_) => {
                            state
                                .deps
                                .protocol
                                .pending_local_muc_departures
                                .requeue_with_backoff(PendingLocalDeparture {
                                    item: LocalDepartureItem::RoomDeparture {
                                        room,
                                        jid,
                                        cause,
                                        selector,
                                        attempt,
                                        notified: notified.clone(),
                                        removal,
                                    },
                                    attempts: pending.attempts,
                                    not_before: pending.not_before,
                                });
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Requeued,
                            );
                            break;
                        }
                    };
                    // Acknowledgement barrier: an owed acknowledgement for this
                    // JID names a receipt whose effects already ran. Deliver
                    // EVERY one (the actor keeps one receipt per JID, so the
                    // live one may sit behind a superseded attempt's ack)
                    // before this retry may fall back to the JID's receipt, or
                    // the retry would replay those effects.
                    let mut ack_blocked = false;
                    while let Some(ack_attempt) = state
                        .deps
                        .protocol
                        .pending_local_muc_departures
                        .pending_ack_for(&room, &jid)
                    {
                        if routes::websocket::try_ack_departure_receipt(&actor, ack_attempt).await {
                            state
                                .deps
                                .protocol
                                .pending_local_muc_departures
                                .complete_ack(&room, &jid, ack_attempt);
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Acknowledged,
                            );
                        } else {
                            ack_blocked = true;
                            break;
                        }
                    }
                    if ack_blocked {
                        state
                            .deps
                            .protocol
                            .pending_local_muc_departures
                            .requeue_with_backoff(PendingLocalDeparture {
                                item: LocalDepartureItem::RoomDeparture {
                                    room,
                                    jid,
                                    cause,
                                    selector,
                                    attempt,
                                    notified: notified.clone(),
                                    removal,
                                },
                                attempts: pending.attempts,
                                not_before: pending.not_before,
                            });
                        crate::metrics::record_local_departure_retry(
                            LocalDepartureRetryOutcome::AckBarrier,
                        );
                        break;
                    }
                    // A LIVE task still owns a departure of this (room, JID):
                    // retrying now could replay its receipt concurrently with
                    // its own fan-out and duplicate the departure effects.
                    // Requeue and let the live task finish (or die — the
                    // in-flight entry then converts to a retained departure
                    // and merges with this one).
                    if state
                        .deps
                        .protocol
                        .pending_local_muc_departures
                        .in_flight_exists_for(&room, &jid)
                    {
                        state
                            .deps
                            .protocol
                            .pending_local_muc_departures
                            .requeue_with_backoff(PendingLocalDeparture {
                                item: LocalDepartureItem::RoomDeparture {
                                    room,
                                    jid,
                                    cause,
                                    selector,
                                    attempt,
                                    notified: notified.clone(),
                                    removal,
                                },
                                attempts: pending.attempts,
                                not_before: pending.not_before,
                            });
                        crate::metrics::record_local_departure_retry(
                            LocalDepartureRetryOutcome::InFlightBarrier,
                        );
                        break;
                    }
                    match routes::websocket::ask_leave_bounded(
                        &actor,
                        LeaveByRealJid {
                            sender_jid: jid.clone(),
                            cause,
                            session: selector,
                            attempt,
                            origin: waddle_xmpp::muc::room_actor::LeaveOrigin::RetainedRetry,
                        },
                    )
                    .await
                    {
                        Ok(LeaveDisposition::Left(outcome)) => {
                            match cause {
                                OccupancyLeaveCause::Disconnect => {
                                    // Idempotent SFU teardown: the live task
                                    // may have died between its leave ask and
                                    // its own unregister call, leaving the
                                    // media participant allocated until the
                                    // SFU's long timeout (#1647, codex
                                    // closing round).
                                    // See `unbound_policy_for_left`: a live removal
                                    // tears an unbound registration down, a replayed
                                    // receipt keeps it (#1703).
                                    match (outcome.provenance, selector) {
                                        // A replayed receipt answers an owed earlier
                                        // departure; the same connection may have
                                        // re-joined (new nick) with the SAME generation
                                        // since, so it is no evidence about the current
                                        // registration (#1703).
                                        (
                                            waddle_xmpp::muc::room_actor::DepartureProvenance::ReplayedReceipt,
                                            LeaveSessionSelector::Generation(_),
                                        ) => {}
                                        (
                                            waddle_xmpp::muc::room_actor::DepartureProvenance::Live,
                                            LeaveSessionSelector::Generation(occupant),
                                        ) => {
                                            let _ = routes::websocket::muc_call_sfu::unregister_participant_from_room_if_occupant_matches(
                                                state, &room, &jid, occupant, waddle_sfu::UnboundOccupantPolicy::TearDown, None,);
                                        }
                                        // Membership-authoritative departures (admin removals) keep the
                                        // ungated teardown: no connection owns them.
                                        (
                                            _,
                                            LeaveSessionSelector::Any
                                            | LeaveSessionSelector::JoinedAtOrBefore(_),
                                        ) => {
                                            routes::websocket::muc_call_sfu::unregister_participant_from_room_ungated(
                                                state, &room, &jid,
                                            );
                                        }
                                    }
                                    broadcast_muc_leave_to_remaining_resumable(
                                        state,
                                        &room,
                                        &jid,
                                        &outcome,
                                        // The retained item carries WHY, so a
                                        // delayed replay emits the same
                                        // XEP-0045 status codes the inline
                                        // sweep would have (#1814): a ghost
                                        // eviction still says 333 here.
                                        removal,
                                        Some(LeaveFanOutProgress {
                                            skip: &notified,
                                            record: None,
                                        }),
                                    )
                                    .await;
                                    broadcast_muc_muji_clear_to_remaining_resumable(
                                        state,
                                        &room,
                                        &jid,
                                        &outcome,
                                        Some(LeaveFanOutProgress {
                                            skip: &notified,
                                            record: None,
                                        }),
                                    )
                                    .await;
                                    let _ = maybe_evict_empty_room(state, &room, &outcome).await;
                                }
                                OccupancyLeaveCause::Administrative => {
                                    broadcast_group_dm_leave(
                                        &state.deps.app_state,
                                        state.deps.protocol.connection_registry.as_ref(),
                                        &jid,
                                        state
                                            .deps
                                            .protocol
                                            .connection_registry
                                            .entry(&jid)
                                            .is_some(),
                                        &GroupDmLeaveEffect::from(outcome.as_ref()),
                                    );
                                }
                                OccupancyLeaveCause::Explicit => {
                                    // An explicit leave whose reply was lost
                                    // after enqueue: the leaver got a wait-class
                                    // bounce, so deliver its §7.14 self-echo now
                                    // and fan out to the remaining occupants.
                                    routes::websocket::echo_muc_self_unavailable(
                                        state,
                                        &room,
                                        &jid,
                                        &outcome.leaving_room_jid,
                                        outcome.affiliation,
                                        removal,
                                    )
                                    .await;
                                    broadcast_muc_leave_to_remaining_resumable(
                                        state,
                                        &room,
                                        &jid,
                                        &outcome,
                                        // The retained item carries WHY, so a
                                        // delayed replay emits the same
                                        // XEP-0045 status codes the inline
                                        // sweep would have (#1814): a ghost
                                        // eviction still says 333 here.
                                        removal,
                                        Some(LeaveFanOutProgress {
                                            skip: &notified,
                                            record: None,
                                        }),
                                    )
                                    .await;
                                    broadcast_muc_muji_clear_to_remaining_resumable(
                                        state,
                                        &room,
                                        &jid,
                                        &outcome,
                                        Some(LeaveFanOutProgress {
                                            skip: &notified,
                                            record: None,
                                        }),
                                    )
                                    .await;
                                    match (outcome.provenance, selector) {
                                        // A replayed receipt answers an owed earlier
                                        // departure; the same connection may have
                                        // re-joined (new nick) with the SAME generation
                                        // since, so it is no evidence about the current
                                        // registration (#1703).
                                        (
                                            waddle_xmpp::muc::room_actor::DepartureProvenance::ReplayedReceipt,
                                            LeaveSessionSelector::Generation(_),
                                        ) => {}
                                        (
                                            waddle_xmpp::muc::room_actor::DepartureProvenance::Live,
                                            LeaveSessionSelector::Generation(occupant),
                                        ) => {
                                            let _ = routes::websocket::muc_call_sfu::unregister_participant_from_room_if_occupant_matches(
                                                state, &room, &jid, occupant, waddle_sfu::UnboundOccupantPolicy::TearDown, None,);
                                        }
                                        // Membership-authoritative departures (admin removals) keep the
                                        // ungated teardown: no connection owns them.
                                        (
                                            _,
                                            LeaveSessionSelector::Any
                                            | LeaveSessionSelector::JoinedAtOrBefore(_),
                                        ) => {
                                            routes::websocket::muc_call_sfu::unregister_participant_from_room_ungated(
                                                state, &room, &jid,
                                            );
                                        }
                                    }
                                    let _ = maybe_evict_empty_room(state, &room, &outcome).await;
                                }
                            }
                            // Effects ran: only now is the receipt no longer owed.
                            routes::websocket::ack_departure_receipt(
                                &state.deps.protocol.pending_local_muc_departures,
                                &actor,
                                &room,
                                &jid,
                                outcome.acknowledge,
                            )
                            .await;
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Completed,
                            );
                            if drained_receipts < MAX_RECEIPT_DRAIN {
                                drained_receipts += 1;
                                // Re-drive: another (older-nick) receipt of
                                // this (JID, cause) may still be owed. The
                                // consumed attempt is inert on the re-ask, so
                                // the RetainedRetry fallback replays the next
                                // receipt or answers NotOccupant. Progress is
                                // NOT carried over — it belonged to the
                                // acknowledged attempt's fan-out.
                                item = LocalDepartureItem::RoomDeparture {
                                    room,
                                    jid,
                                    cause,
                                    selector,
                                    attempt,
                                    notified: HashSet::new(),
                                    removal,
                                };
                                continue;
                            }
                            // The per-pass bound was reached with receipts
                            // possibly still owed: requeue for the next sweep
                            // instead of dropping the responsibility (#1647,
                            // codex closing round).
                            state
                                .deps
                                .protocol
                                .pending_local_muc_departures
                                .requeue_with_backoff(PendingLocalDeparture {
                                    item: LocalDepartureItem::RoomDeparture {
                                        room,
                                        jid,
                                        cause,
                                        selector,
                                        attempt,
                                        notified: HashSet::new(),
                                        removal,
                                    },
                                    attempts: pending.attempts,
                                    not_before: pending.not_before,
                                });
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Requeued,
                            );
                            break;
                        }
                        Ok(LeaveDisposition::Deferred { watermark }) => {
                            state
                                .deps
                                .protocol
                                .pending_local_muc_departures
                                .requeue_with_backoff(PendingLocalDeparture {
                                    item: LocalDepartureItem::RoomDeparture {
                                        room,
                                        jid,
                                        cause,
                                        selector: match selector {
                                            LeaveSessionSelector::Generation(_) => selector,
                                            LeaveSessionSelector::Any
                                            | LeaveSessionSelector::JoinedAtOrBefore(_) => {
                                                LeaveSessionSelector::JoinedAtOrBefore(watermark)
                                            }
                                        },
                                        attempt,
                                        notified: notified.clone(),
                                        removal,
                                    },
                                    attempts: pending.attempts,
                                    not_before: pending.not_before,
                                });
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Requeued,
                            );
                            break;
                        }
                        Ok(LeaveDisposition::NotOccupant) => {
                            if let LeaveSessionSelector::Generation(occupant) = selector {
                                let _ = routes::websocket::muc_call_sfu::unregister_participant_from_room_if_occupant_matches(
                                    state, &room, &jid, occupant, waddle_sfu::UnboundOccupantPolicy::Keep, None,);
                            }
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::NotOccupant,
                            );

                            break;
                        }
                        Ok(LeaveDisposition::Superseded { current_generation }) => {
                            if let LeaveSessionSelector::Generation(occupant) = selector {
                                // Only a live same-generation rejoin protects
                                // this media. Absence is cleanup proof too.
                                if current_generation != Some(occupant) {
                                    let _ = routes::websocket::muc_call_sfu::unregister_participant_from_room_if_occupant_matches(
                                        state, &room, &jid, occupant, waddle_sfu::UnboundOccupantPolicy::Keep, None,);
                                }
                            }
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Superseded,
                            );
                            // #1647 (codex round 28): a Superseded answer
                            // consumed ONE unreplayable receipt (or bounced
                            // off a tombstoned attempt) — receipts of OTHER
                            // nicks may still be owed behind it. Keep
                            // draining (bounded) until the fallback answers
                            // NotOccupant; a tombstone-only Superseded loop
                            // terminates at the drain bound.
                            if drained_receipts < MAX_RECEIPT_DRAIN {
                                drained_receipts += 1;
                                item = LocalDepartureItem::RoomDeparture {
                                    room,
                                    jid,
                                    cause,
                                    selector,
                                    attempt,
                                    notified: HashSet::new(),
                                    removal,
                                };
                                continue;
                            }
                            break;
                        }
                        Ok(LeaveDisposition::Suppressed {
                            nick,
                            affiliation,
                            attempt: acknowledge,
                        }) => {
                            if matches!(cause, OccupancyLeaveCause::Explicit) {
                                // Store-less room mid-destroy/dormancy: only
                                // the leaver's §7.14 self-presence is owed.
                                routes::websocket::echo_muc_self_unavailable(
                                    state,
                                    &room,
                                    &jid,
                                    &nick.occupant_jid(&room),
                                    affiliation,
                                    removal,
                                )
                                .await;
                            }
                            match selector {
                                LeaveSessionSelector::Generation(occupant) => {
                                    let _ = routes::websocket::muc_call_sfu::unregister_participant_from_room_if_occupant_matches(
                                        state, &room, &jid, occupant, waddle_sfu::UnboundOccupantPolicy::Keep, None,);
                                }
                                // Membership-authoritative explicit departures keep the ungated
                                // teardown: no connection owns them.
                                LeaveSessionSelector::Any
                                | LeaveSessionSelector::JoinedAtOrBefore(_)
                                    if matches!(cause, OccupancyLeaveCause::Explicit) =>
                                {
                                    routes::websocket::muc_call_sfu::unregister_participant_from_room_ungated(
                                        state, &room, &jid,
                                    );
                                }
                                LeaveSessionSelector::Any
                                | LeaveSessionSelector::JoinedAtOrBefore(_) => {}
                            }
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Completed,
                            );
                            routes::websocket::ack_departure_receipt(
                                &state.deps.protocol.pending_local_muc_departures,
                                &actor,
                                &room,
                                &jid,
                                acknowledge,
                            )
                            .await;
                            break;
                        }
                        Err(routes::websocket::LeaveAskFailure::Timeout) => {
                            // A timeout is not proof of retirement: keep the
                            // departure and back off.
                            state
                                .deps
                                .protocol
                                .pending_local_muc_departures
                                .requeue_with_backoff(PendingLocalDeparture {
                                    item: LocalDepartureItem::RoomDeparture {
                                        room,
                                        jid,
                                        cause,
                                        selector,
                                        attempt,
                                        notified: notified.clone(),
                                        removal,
                                    },
                                    attempts: pending.attempts,
                                    not_before: pending.not_before,
                                });
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Requeued,
                            );
                            break;
                        }
                        Err(_) => {
                            item = LocalDepartureItem::ConfirmRetired {
                                room,
                                jid,
                                actor: actor.id(),
                                cause,
                                selector,
                                attempt,
                                notified,
                                removal,
                            };
                        }
                    }
                }
                LocalDepartureItem::InFlight {
                    room,
                    jid,
                    cause,
                    selector,
                    attempt,
                    notified,
                    removal,
                } => {
                    // The live task never completed its write-ahead entry:
                    // retry as a retained departure under the same attempt so
                    // the actor's receipt (if the departure did commit) is
                    // replayed, or the session is removed if it never was.
                    item = LocalDepartureItem::RoomDeparture {
                        room,
                        jid,
                        cause,
                        selector,
                        attempt,
                        notified,
                        removal,
                    };
                }
                LocalDepartureItem::EvictEmptyRoom {
                    room,
                    occupancy_revision,
                } => {
                    match waddle_xmpp::muc::RoomRegistry::wrap(
                        state.deps.protocol.room_registry.clone(),
                    )
                    .destroy_room_if_inactive(
                        room.clone(),
                        occupancy_revision,
                        waddle_xmpp::muc::room_actor::SealGuard::EmptyNonPersistent,
                    )
                    .await
                    {
                        // Destroyed, already absent, or refused because a
                        // newer join bumped the revision: definitive.
                        Ok(outcome) if outcome.is_definitive() => {
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Completed,
                            );
                            break;
                        }
                        // Retained by the registry (uncertain durable commit,
                        // release backlog, seal ask failure) or the ask itself
                        // failed: still owed.
                        Ok(_) | Err(_) => {
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Requeued,
                            );
                            state
                                .deps
                                .protocol
                                .pending_local_muc_departures
                                .requeue_with_backoff(PendingLocalDeparture {
                                    item: LocalDepartureItem::EvictEmptyRoom {
                                        room,
                                        occupancy_revision,
                                    },
                                    attempts: pending.attempts,
                                    not_before: pending.not_before,
                                });
                            break;
                        }
                    }
                }
                LocalDepartureItem::AckReceipt {
                    room,
                    jid,
                    attempt,
                    absent_sweeps,
                } => {
                    match get_room_actor_result(state, &room).await {
                        // No registered actor: the room is gone for good (drop
                        // the ack) — or a live-roster handoff is carrying the
                        // receipt to a successor that is not published yet, in
                        // which case dropping the ack would let a later retained
                        // departure replay the receipt. Retry a few consecutive
                        // absent-room sweeps first (its own budget: ask
                        // timeouts and NotAuthoritative answers do not count).
                        Ok(None) if absent_sweeps >= routes::websocket::ACK_ABSENT_ROOM_RETRIES => {
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Abandoned,
                            );
                            break;
                        }
                        Ok(None) => {
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Requeued,
                            );
                            state
                                .deps
                                .protocol
                                .pending_local_muc_departures
                                .requeue_with_backoff(PendingLocalDeparture {
                                    item: LocalDepartureItem::AckReceipt {
                                        room,
                                        jid,
                                        attempt,
                                        absent_sweeps: absent_sweeps.saturating_add(1),
                                    },
                                    attempts: pending.attempts,
                                    not_before: pending.not_before,
                                });
                            break;
                        }
                        Ok(Some(actor))
                            if routes::websocket::try_ack_departure_receipt(&actor, attempt)
                                .await =>
                        {
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Acknowledged,
                            );
                            break;
                        }
                        Ok(Some(_)) | Err(_) => {
                            crate::metrics::record_local_departure_retry(
                                LocalDepartureRetryOutcome::Requeued,
                            );
                            state
                                .deps
                                .protocol
                                .pending_local_muc_departures
                                .requeue_with_backoff(PendingLocalDeparture {
                                    item: LocalDepartureItem::AckReceipt {
                                        room,
                                        jid,
                                        attempt,
                                        // A registered actor was reached: the
                                        // absent-room budget starts over.
                                        absent_sweeps: 0,
                                    },
                                    attempts: pending.attempts,
                                    not_before: pending.not_before,
                                });
                            break;
                        }
                    }
                }
                LocalDepartureItem::ConfirmRetired {
                    room,
                    jid,
                    actor: recorded,
                    cause,
                    selector,
                    attempt,
                    notified,
                    removal,
                } => match get_room_actor_result(state, &room).await {
                    Ok(None) => {
                        crate::metrics::record_local_departure_retry(
                            LocalDepartureRetryOutcome::Retired,
                        );
                        break;
                    }
                    Ok(Some(current)) if current.id() != recorded => {
                        item = LocalDepartureItem::RoomDeparture {
                            room,
                            jid,
                            cause,
                            selector,
                            attempt,
                            notified,
                            removal,
                        };
                    }
                    Ok(Some(_)) | Err(_) => {
                        crate::metrics::record_local_departure_retry(
                            LocalDepartureRetryOutcome::AwaitingReap,
                        );
                        state
                            .deps
                            .protocol
                            .pending_local_muc_departures
                            .requeue_with_backoff(PendingLocalDeparture {
                                item: LocalDepartureItem::ConfirmRetired {
                                    room,
                                    jid,
                                    actor: recorded,
                                    cause,
                                    selector,
                                    attempt,
                                    notified,
                                    removal,
                                },
                                attempts: pending.attempts,
                                not_before: pending.not_before,
                            });
                        break;
                    }
                },
            }
        }
    }
}

/// Upper bound on each `UserRegistryActor` ask the reaper makes (`ListUsers`,
/// per-user `ReapUserIfEmpty`, `UserCount`). A wedged or backed-up registry
/// must never hang the janitor task indefinitely (Copilot review on PR #1177):
/// on timeout the sweep logs and moves on (or ends), retrying next interval.
///
/// Sized to *strictly exceed* the registry's internal per-child ask bound
/// (`user_registry::CHILD_ACTOR_TIMEOUT`, 2s): `ReapUserIfEmpty` itself asks the
/// child `UserActor` for its `ResourceCount` bounded at that inner timeout, so a
/// janitor timeout equal to the inner one could fire simultaneously and abandon
/// a reap the registry actually completes — undercounting
/// `waddle_user_actor_reaped_total` (Greptile review on PR #1177). The extra
/// headroom lets the outer ask observe the reply instead of racing it; it is
/// harmless for the child-less `ListUsers`/`UserCount` reads, which reply fast.
pub(in crate::server::session_janitors) const REAPER_ASK_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-sweep cap for the stale-node watchdog that runs before the orphaned
/// SM-session claim scan. This bounds the raw-heartbeat discovery pass; each
/// candidate still has to pass `NodeLeaseStore::expire`'s CAS before any
/// claim can be stolen.
#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) const STALE_NODE_WATCHDOG_CANDIDATE_LIMIT: usize = 64;

/// Per-sweep bound for proactive `RoomActor` orphan reconciliation. Room
/// claims are cheap rows but adoption can hydrate durable state, so the
/// janitor deliberately amortizes a large node loss across sweeps.
#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) const ORPHANED_ROOM_CANDIDATE_LIMIT: usize = 64;

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) const ORPHANED_ROOM_RELEASE_TIMEOUT: Duration =
    Duration::from_secs(1);

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) const ORPHANED_SM_SCAN_TIMEOUT: Duration =
    Duration::from_secs(5);

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) const ORPHAN_WORK_QUEUE_CAPACITY: usize = 128;

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) const ORPHAN_WORK_ATTEMPT_TIMEOUT: Duration =
    Duration::from_secs(5);

/// Hard wall-clock bound for one outer orphan-reconciliation pass. Individual
/// hydration and release jobs have their own tighter deadlines, but the sweep
/// also awaits lease-store and RoomRegistry operations. If any of those
/// dependencies wedges after a claim steal, continuing to advertise this
/// node's lease could strand authority with no retained actor responsibility.
/// Self-fence on expiry so another node can recover the exact claim.
#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) const ORPHAN_REAPER_SWEEP_TIMEOUT: Duration =
    Duration::from_secs(60);
pub(in crate::server::session_janitors) fn pending_delivery_max_age_days_from_env() -> u32 {
    const DEFAULT_DAYS: u32 = 30;
    const MIN_DAYS: u32 = 1;
    const MAX_DAYS: u32 = 365;
    std::env::var("WADDLE_PENDING_DELIVERY_MAX_AGE_DAYS")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .map(|v| v.clamp(MIN_DAYS, MAX_DAYS))
        .unwrap_or(DEFAULT_DAYS)
}

pub(in crate::server::session_janitors) fn max_promotion_attempts_from_env() -> u32 {
    const DEFAULT_ATTEMPTS: u32 = 5;
    const MIN_ATTEMPTS: u32 = 2;
    const MAX_ATTEMPTS: u32 = 1024;
    std::env::var("WADDLE_SM_PROMOTION_MAX_ATTEMPTS")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .map(|v| v.clamp(MIN_ATTEMPTS, MAX_ATTEMPTS))
        .unwrap_or(DEFAULT_ATTEMPTS)
}

pub(crate) fn max_drain_duration_from_env() -> std::time::Duration {
    const DEFAULT_SECS: u64 = 30;
    const MIN_SECS: u64 = 1;
    const MAX_SECS: u64 = 600;
    let secs = std::env::var("WADDLE_DRAIN_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(|v| v.clamp(MIN_SECS, MAX_SECS))
        .unwrap_or(DEFAULT_SECS);
    std::time::Duration::from_secs(secs)
}

pub(in crate::server::session_janitors) fn notification_outbox_retention_days_from_env() -> u32 {
    const DEFAULT_DAYS: u32 = 30;
    const MIN_DAYS: u32 = 1;
    const MAX_DAYS: u32 = 365;
    std::env::var("WADDLE_NOTIFICATION_OUTBOX_RETENTION_DAYS")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .map(|v| v.clamp(MIN_DAYS, MAX_DAYS))
        .unwrap_or(DEFAULT_DAYS)
}

pub(in crate::server::session_janitors) fn notification_outbox_prune_batch_from_env() -> usize {
    const DEFAULT_BATCH: usize = 1_000;
    const MIN_BATCH: usize = 1;
    const MAX_BATCH: usize = 10_000;
    std::env::var("WADDLE_NOTIFICATION_OUTBOX_PRUNE_BATCH")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .map(|v| v.clamp(MIN_BATCH, MAX_BATCH))
        .unwrap_or(DEFAULT_BATCH)
}

pub(crate) fn spawn_sm_expiry_janitor(websocket_state: &Arc<WebSocketState>) {
    // XEP-0198 expired-session janitor. Without this, detached SM sessions
    // whose resume window elapses leave MUC occupants in their rooms forever.
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        // Skip the first tick (immediate) so we don't sweep before the
        // server has accepted any connections.
        ticker.tick().await;
        let mut custody_cursor = None;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            run_sm_expiry_sweep_with_custody_cursor(&state, &mut custody_cursor).await;
        }
    });
}

/// Authority absence or replacement makes this generation permanently
/// non-resumable: fresh binds mint generations, and resume never republishes
/// one. Recover locally owned queues through the same custody/promotion path
/// as expiry instead of leaving pre-cutover sessions stranded until timeout.
pub(in crate::server::session_janitors) async fn drain_noncurrent_occupancy_sessions(
    state: &WebSocketState,
    promotion_batch: &mut PromotionBatchGuard<'_>,
) -> bool {
    let registry = &state.deps.protocol.sm_session_registry;
    let candidates = match registry.detached_occupancy_inventory() {
        Ok(candidates) => candidates,
        Err(error) => {
            warn!(%error, "SM janitor: detached occupancy inventory failed");
            return false;
        }
    };
    let mut completed = true;
    for (stream_id, jid, generation) in candidates {
        match crate::occupancy_authority::is_current(
            state.deps.app_state.db_pool.global(),
            &jid,
            generation,
        )
        .await
        {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                completed = false;
                warn!(%stream_id, %jid, %error, "SM janitor: occupancy authority lookup failed; retaining detached session");
                continue;
            }
        }
        // The registry rechecks the exact immutable SM claim fence and
        // detached identity under its stream lock. Never steal a foreign
        // claim or take a snapshot already claimed for an in-flight resume.
        match registry
            .invalidate_detached_session_for_occupancy(&stream_id, &jid, generation)
            .await
        {
            Ok(Some(session)) => {
                // Adopt custody before the next await, including the next
                // candidate's authority lookup, so cancellation keeps Q.
                promotion_batch.push(session);
            }
            Ok(None) => {}
            Err(error) => {
                completed = false;
                warn!(%stream_id, %jid, %error, "SM janitor: noncurrent occupancy drain failed; retaining detached session");
            }
        }
    }
    completed
}

pub(in crate::server::session_janitors) async fn run_sm_expiry_sweep_with_custody_cursor(
    state: &Arc<WebSocketState>,
    custody_cursor: &mut Option<waddle_xmpp::stream_management::SmIngressAppendKey>,
) {
    async {
        let mut sweep_failed = false;
        let drained: Vec<waddle_xmpp::stream_management::DetachedSession> = match state
            .deps
            .protocol
            .sm_session_registry
            .drain_expired()
            .await
        {
            Ok(sessions) => sessions,
            Err(err) => {
                sweep_failed = true;
                warn!(error = %err, "SM janitor: drain_expired failed");
                Vec::new()
            }
        };
        if !drained.is_empty() {
            info!(
                count = drained.len(),
                "SM janitor: cleaning up expired detached sessions"
            );
        }
        let mut promotion_batch =
            PromotionBatchGuard::new(&state.deps.protocol.sm_session_registry, drained);
        if !drain_noncurrent_occupancy_sessions(state, &mut promotion_batch).await {
            sweep_failed = true;
        }
        while let Some(pending_session) = promotion_batch.pop() {
            let mut promotion_guard = PromotionSessionGuard::new(
                &state.deps.protocol.sm_session_registry,
                pending_session,
            );
            // Repeat row-ownership discovery on every pass (PR #1669 round 8):
            // this session's replay queue can contain copies of durable
            // sequence-bound pending rows (a terminal-recovery session
            // reinserted after a storage failure, or a flush stanza that was
            // unacked at expiry). Release those rows back to ordinary
            // redelivery and strip the copies BEFORE promotion; promoting a
            // copy whose row still exists duplicates a Transient payload or
            // collides with the Archived uniqueness key.
            let row_release = crate::sm_promotion::release_row_backed_replay_copies(
                &state.deps.protocol.sm_session_registry,
                &state.deps.protocol.pending_delivery_storage,
                promotion_guard.session_mut(),
            )
            .await;
            // The guard owns the same payload reconciliation mutates, so a
            // cancellation or failed confirm restores the stripped queue —
            // never the pre-reconciliation replay copies.
            let session = promotion_guard.session().clone();
            // Ordering barrier for retried sessions: a previous cleanup can
            // have QUEUED earlier rows whose re-drive aborted (their SM
            // sequences were pruned, so this pass cannot rediscover them
            // from the replay queue). Any unflushed backlog for the user
            // must reach a live replacement BEFORE this session's replay
            // promotes later traffic to it — re-drive unconditionally when
            // rows were released this pass or any unflushed rows remain.
            let has_unflushed_backlog = match state
                .deps
                .protocol
                .pending_delivery_storage
                .list(&session.jid.to_bare())
                .await
            {
                Ok(rows) => rows.iter().any(|row| row.outbound_sequence.is_none()),
                Err(_) => true,
            };
            let redrive_outcome = if row_release.released_rows || has_unflushed_backlog {
                routes::websocket::redrive_terminal_pending_rows_to_live_resource(
                    state,
                    &session.jid.to_bare(),
                )
                .await
            } else {
                routes::websocket::TerminalRedriveOutcome::Settled
            };
            // Waiting for an earlier row's ACK/writer is not a failed
            // promotion. Preserve any prior real failure count, and retain
            // the tail without spending the dead-letter budget on ordering.
            if redrive_outcome == routes::websocket::TerminalRedriveOutcome::OrderingDeferred
                && !row_release.ownership_unknown
                && !row_release.release_failed_known_rows
            {
                if crate::sm_promotion::reinsert_failed_session_for_retry(
                    &state.deps.protocol.sm_session_registry,
                    session.clone(),
                )
                .await
                {
                    promotion_guard.complete();
                } else {
                    sweep_failed = true;
                }
                continue;
            }
            let released_redrive_aborted = redrive_outcome.blocks_promotion();
            // Promotion must wait while an earlier row is still pending at a
            // live replacement: an aborted re-drive (rows released but not
            // enqueued) or a failed sequence release (rows still claimed
            // until release_claim) would both let later replay traffic
            // overtake the earlier row. Defer the whole session to the next
            // pass; the dead-letter cap still bounds repeated failures.
            if row_release.ownership_unknown
                || released_redrive_aborted
                || row_release.release_failed_known_rows
            {
                sweep_failed = true;
                let attempts = match state
                    .deps
                    .protocol
                    .sm_session_registry
                    .record_promotion_session_failure(&session)
                    .await
                {
                    Ok(n) => n,
                    Err(record_error) => {
                        warn!(
                            jid = %session.jid,
                            %record_error,
                            "SM janitor: row-ownership discovery failed and \
                             record_promotion_failure also failed; preserving \
                             session state for retry"
                        );
                        if crate::sm_promotion::reinsert_failed_session_for_retry(
                            &state.deps.protocol.sm_session_registry,
                            session.clone(),
                        )
                        .await
                        {
                            promotion_guard.complete();
                        }
                        continue;
                    }
                };
                if attempts >= max_promotion_attempts_from_env() {
                    waddle_xmpp::telemetry::reliability::increment_sm_promotion_dead_lettered();
                    error!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        attempts,
                        "SM janitor: row reconciliation has repeatedly failed; \
                         dead-lettering the durable row to break the retry loop"
                    );
                    if confirm_expired_ingress_stream(state, &session).await {
                        promotion_guard.complete();
                        // Dead-lettering discards the replay queue, but the
                        // rows the dead stream still claims must go back to
                        // ordinary redelivery — and a live replacement whose
                        // once-only offline flush is spent needs the
                        // re-drive. Gated on the confirmed drain: with the
                        // guard still armed the retained session retries,
                        // and releasing its rows now would let that retry
                        // re-promote a row a replacement meanwhile consumed.
                        let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(
                            session.stream_id.clone(),
                        );
                        match state
                            .deps
                            .protocol
                            .pending_delivery_storage
                            .release_claim(&session_id)
                            .await
                        {
                            Ok(_) => {
                                routes::websocket::redrive_terminal_pending_rows_to_live_resource(
                                    state,
                                    &session.jid.to_bare(),
                                )
                                .await;
                            }
                            Err(error) => {
                                sweep_failed = true;
                                warn!(
                                    jid = %session.jid,
                                    stream_id = %session.stream_id,
                                    %error,
                                    "SM janitor: dead-letter release_claim failed; rows remain \
                                     claimed for the claim-expiry janitor"
                                );
                            }
                        }
                    } else {
                        sweep_failed = true;
                    }
                    continue;
                }
                warn!(
                    jid = %session.jid,
                    attempts,
                    ownership_unknown = row_release.ownership_unknown,
                    release_failed = row_release.release_failed_known_rows,
                    redrive_aborted = released_redrive_aborted,
                    "SM janitor: row reconciliation cannot settle ahead of promotion; \
                     SKIPPING this session so replay traffic cannot overtake or \
                     duplicate its durable rows. Retried on the next janitor pass."
                );
                if crate::sm_promotion::reinsert_failed_session_for_retry(
                    &state.deps.protocol.sm_session_registry,
                    session.clone(),
                )
                .await
                {
                    promotion_guard.complete();
                }
                continue;
            }
            let blocklist = match state
                .deps
                .protocol
                .blocking_storage
                .list_blocked_jid_entries(&session.jid.to_bare())
                .await
            {
                Ok(jids) => waddle_xmpp::protocol::session_state::Blocklist::new(jids),
                Err(error) => {
                    sweep_failed = true;
                    waddle_xmpp::telemetry::reliability::increment_sm_promotion_blocklist_failed();
                    let attempts = match state
                        .deps
                        .protocol
                        .sm_session_registry
                        .record_promotion_session_failure(&session)
                        .await
                    {
                        Ok(n) => n,
                        Err(record_error) => {
                            warn!(
                                jid = %session.jid,
                                error = %error,
                                record_error = %record_error,
                                "SM janitor: blocklist load failed and \
                                 record_promotion_failure also failed; preserving \
                                 session state for retry"
                            );
                            if crate::sm_promotion::reinsert_failed_session_for_retry(
                                &state.deps.protocol.sm_session_registry,
                                session.clone(),
                            )
                            .await
                            {
                                promotion_guard.complete();
                            }
                            continue;
                        }
                    };
                    if attempts >= max_promotion_attempts_from_env() {
                        waddle_xmpp::telemetry::reliability::increment_sm_promotion_dead_lettered();
                        error!(
                            jid = %session.jid,
                            stream_id = %session.stream_id,
                            attempts,
                            error = %error,
                            "SM janitor: blocklist load has repeatedly failed; \
                             dead-lettering the durable row to break the retry loop"
                        );
                        if confirm_expired_ingress_stream(state, &session).await {
                            promotion_guard.complete();
                        }
                        continue;
                    }
                    warn!(
                        jid = %session.jid,
                        attempts,
                        error = %error,
                        "SM janitor: blocklist load failed; SKIPPING promotion to \
                         preserve fail-closed XEP-0191 policy. Durable SM row will \
                         be retried on the next janitor pass."
                    );
                    // Make the retry promise true: drain_expired scans only
                    // memory, so the drained session must go back into the
                    // map for the next tick to see it.
                    if crate::sm_promotion::reinsert_failed_session_for_retry(
                        &state.deps.protocol.sm_session_registry,
                        session.clone(),
                    )
                    .await
                    {
                        promotion_guard.complete();
                    }
                    continue;
                }
            };
            // Round-2 review R2 + round-3 finding 1: retractions
            // racing this drain window are invisible to the registry
            // scrub (the sessions are off both maps); fetch the
            // recent-tombstone record PER SESSION, immediately
            // before this session's promotion, so even a retraction
            // landing mid-batch is still seen.
            let recent_tombstones = match crate::sm_promotion::recent_tombstones_for_promotion(
                &state.deps.protocol.sm_session_registry,
                "SM janitor",
            ) {
                Ok(records) => records,
                Err(_) => {
                    sweep_failed = true;
                    Vec::new()
                }
            };
            let summary = crate::sm_promotion::promote_session_with_custody(
                &session,
                TerminalOverflowPromotionDeps {
                    sm_registry: &state.deps.protocol.sm_session_registry,
                    registry: &state.deps.protocol.connection_registry,
                    user_registry: &state.deps.protocol.user_registry,
                    pending_storage: &state.deps.protocol.pending_delivery_storage,
                    blocklist: &blocklist,
                    server_domain: state.deps.auth_state.xmpp_domain.as_str(),
                    recent_tombstones: &recent_tombstones,
                    allow_unfenced_effects: true,
                },
            )
            .await;
            // Finding B (retraction-vs-promotion TOCTOU): a
            // retraction recorded after the snapshot above raced
            // this session's promotion — re-scrub the pending rows
            // it may have just inserted BEFORE confirm_drained.
            if crate::sm_promotion::scrub_pending_for_tombstones_recorded_during_promotion(
                &state.deps.protocol.sm_session_registry,
                &state.deps.protocol.pending_delivery_storage,
                &recent_tombstones,
                "SM janitor",
            )
            .await
                == PromotionScrubOutcome::Failed
            {
                sweep_failed = true;
            }
            if summary.queued + summary.redelivered + summary.bounced + summary.not_promotable > 0
                || summary.storage_failed > 0
            {
                info!(
                    jid = %session.jid,
                    redelivered = summary.redelivered,
                    queued = summary.queued,
                    bounced = summary.bounced,
                    dropped = summary.dropped,
                    not_promotable = summary.not_promotable,
                    unparseable = summary.unparseable,
                    scrubbed = summary.scrubbed,
                    storage_failed = summary.storage_failed,
                    "SM janitor: Q6 promotion completed"
                );
            }
            if summary.has_storage_failure() {
                sweep_failed = true;
                waddle_xmpp::telemetry::reliability::add_sm_promotion_storage_failed(u64::from(
                    summary.storage_failed,
                ));
                let attempts = match state
                    .deps
                    .protocol
                    .sm_session_registry
                    .record_promotion_session_failure(&session)
                    .await
                {
                    Ok(n) => n,
                    Err(error) => {
                        warn!(
                            jid = %session.jid,
                            %error,
                            "SM janitor: record_promotion_failure failed; \
                             preserving session state for retry"
                        );
                        if crate::sm_promotion::prune_promoted_then_reinsert_for_retry(
                            &state.deps.protocol.sm_session_registry,
                            session.clone(),
                            &summary,
                        )
                        .await
                        {
                            promotion_guard.complete();
                        }
                        continue;
                    }
                };
                if attempts >= max_promotion_attempts_from_env() {
                    waddle_xmpp::telemetry::reliability::increment_sm_promotion_dead_lettered();
                    error!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        attempts,
                        storage_failed = summary.storage_failed,
                        "SM janitor: Q6 promotion repeatedly failed; \
                         dead-lettering the durable row to break the retry loop"
                    );
                    if confirm_expired_ingress_stream(state, &session).await {
                        promotion_guard.complete();
                    }
                    continue;
                }
                warn!(
                    jid = %session.jid,
                    attempts,
                    storage_failed = summary.storage_failed,
                    "SM janitor: promotion had storage failures; \
                     preserving session state for retry"
                );
                if crate::sm_promotion::prune_promoted_then_reinsert_for_retry(
                    &state.deps.protocol.sm_session_registry,
                    session.clone(),
                    &summary,
                )
                .await
                {
                    promotion_guard.complete();
                }
                continue;
            }
            if confirm_expired_ingress_stream(state, &session).await {
                promotion_guard.complete();
            } else {
                sweep_failed = true;
                continue;
            }
            let session_id =
                waddle_xmpp::pending_delivery::SmSessionId::new(session.stream_id.clone());

            // Same replacement re-check as the unclean-disconnect
            // path: a fresh bind that superseded this expired
            // detached session broadcasts its own presence, so a
            // late unavailable would pin subscribers on offline for
            // an online JID.
            if routes::websocket::broadcast_unavailable_if_no_replacement(
                state,
                &session.jid,
                session.presence_available,
            )
            .await
                == routes::websocket::handlers::presence::TerminatedPresenceBroadcastOutcome::Failed
            {
                sweep_failed = true;
            }
            // ADR-0017 Phase 1 (Greptile P1 on PR #1177): gate the DashMap
            // removal on the EXPIRED session's own SM stream id, not a plain
            // `unregister`. A plain unregister removes whatever currently
            // holds the full JID — which is a live REPLACEMENT session S2 if
            // it rebound the same resource after this session (S1) detached.
            // The removed entry's binding ID would then be S2's token,
            // so the actor mirror below would evict S2's actor-tree entry
            // too — and under Slice 1 the actor tree is the bare-JID
            // selection source, so S2 would silently stop receiving
            // messages. `unregister_if_sm_stream_id` removes only when the
            // current entry is genuinely S1's (matching published stream id),
            // so S2 is left untouched.
            //
            // For the common case, S1 already had its DashMap + actor
            // entries pruned at detach time (`cleanup_connection_shutdown`),
            // so this returns `None` and the mirror is skipped — the actor
            // entry is already gone, not leaked.
            let removed_entry = state
                .deps
                .protocol
                .connection_registry
                .unregister_if_sm_stream_id(
                    &session.jid,
                    &waddle_xmpp::pending_delivery::SmSessionId::new(session.stream_id.clone()),
                );
            // Mirror the (S1-gated) unregister into the actor tree. The
            // removed entry is guaranteed to be S1's, so its token cannot
            // evict a replacement.
            if let Some(entry) = removed_entry {
                if crate::server::dual_registration::mirror_unregister(
                    &state.deps.protocol.user_registry,
                    &session.jid,
                    Some(entry.binding_id()),
                )
                .await
                    == MirrorUnregisterOutcome::Failed
                {
                    sweep_failed = true;
                }
            }
            // PR #438 review (Qodo issue #2): the periodic SM expiry
            // janitor takes its own cleanup path; without this drop
            // it leaks `resource_to_ver` and `pending` entries for
            // every detached session that times out. The other
            // disconnect/expiry paths already call this; mirror it
            // here so the caps state is bounded across all five
            // tear-down code paths.
            state
                .deps
                .protocol
                .caps_resolver
                .drop_resource(&session.jid);
            // The MUC sweep is generation-scoped (#1703): a live same-JID
            // replacement keeps every room it re-joined (`Superseded`), and
            // the rooms it did not re-join are only this expired session's to
            // converge — so it runs regardless of the registry entry.
            {
                #[cfg(feature = "clustering")]
                {
                    let cleanup_origin = OrderedRelayRouteOrigin {
                        kind: OrderedRelayRouteOriginKind::SmSession(session_id.clone()),
                        sender_entity: waddle_xmpp::ownership::Entity::new(
                            waddle_xmpp::ownership::EntityType::UserActor,
                            session.jid.to_bare().to_string(),
                        ),
                        inbound_sequence: 0,
                        handoff: None,
                    };
                    if routes::websocket::cleanup_muc_presence_for_jid_with_origin(
                        state,
                        &session.jid,
                        LeaveSessionSelector::Generation(session.occupancy_session),
                        cleanup_origin,
                    )
                    .await
                        == routes::websocket::MucCleanupOutcome::Failed
                    {
                        sweep_failed = true;
                    }
                }
                #[cfg(not(feature = "clustering"))]
                {
                    if routes::websocket::cleanup_muc_presence_for_jid(
                        state,
                        &session.jid,
                        LeaveSessionSelector::Generation(session.occupancy_session),
                    )
                    .await
                        == routes::websocket::MucCleanupOutcome::Failed
                    {
                        sweep_failed = true;
                    }
                }
            }
            let (claim_released, freed_rows) = match state
                .deps
                .protocol
                .pending_delivery_storage
                .release_claim(&session_id)
                .await
            {
                Ok(freed) => (true, freed),
                Err(error) => {
                    sweep_failed = true;
                    warn!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        error = %error,
                        "SM janitor: pending_delivery release_claim failed; \
                         rows remain claimed and will be released by claim-expiry janitor"
                    );
                    (false, 0)
                }
            };
            // `freed_rows > 0` re-drives rows whose release-failure fact was
            // recorded only in a previous pass's connection-local state (for
            // example a receiver-tail release_row failure before the retry
            // was reinserted): whenever settling this stream frees claimed
            // rows, they need the re-drive regardless of which pass learned
            // about them first.
            if claim_released
                && (row_release.release_failed_known_rows || freed_rows > 0 || summary.queued > 0)
            {
                // Rows the release_claim above just freed (their sequence
                // release had failed), and rows this promotion freshly queued
                // while a replacement raced it, have no future trigger if a
                // live resource already spent its once-only offline flush —
                // re-drive them now. With release_claim failed the rows are
                // still claimed and a flush would be a no-op; the claim-expiry
                // janitor recovers them on its own schedule.
                routes::websocket::redrive_terminal_pending_rows_to_live_resource(
                    state,
                    &session.jid.to_bare(),
                )
                .await;
            }
        }
        if !retry_pending_sm_ownership(state).await {
            sweep_failed = true;
        }
        if !run_ingress_custody_sweep(state, custody_cursor).await {
            sweep_failed = true;
        }
        if !super::run_ingress_retirement_sweep(state).await {
            sweep_failed = true;
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::SmExpiry,
            if sweep_failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::SmExpiry))
    .await;
}

/// Retry abandoned enrollment and failed terminal retirement from their durable rows.
pub(crate) async fn run_ingress_retirement_sweep(state: &WebSocketState) -> bool {
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(2);
    tokio::time::timeout(BUDGET, async {
        let candidates = match state
            .deps
            .protocol
            .ingress
            .next_retirement_candidates()
            .await
        {
            Ok(candidates) => candidates,
            Err(error) => {
                warn!(%error, "SM janitor: ingress retirement scan failed");
                return false;
            }
        };
        let mut completed = true;
        for stream in candidates {
            state
                .deps
                .protocol
                .ingress
                .mark_retirement_candidate_attempted(&stream)
                .await;

            // Enrollment follows claim acquisition. Snapshot after listing each row,
            // so a fresh <enable/> cannot disappear behind an earlier snapshot.
            match state
                .deps
                .protocol
                .sm_session_registry
                .has_retirement_protection(&stream)
                .await
            {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    warn!(%stream, %error, "SM janitor: ingress retirement ownership probe failed");
                    completed = false;
                    continue;
                }
            }
            // PostgreSQL checks claim absence under a transaction-held table lock,
            // protecting foreign owners and claims acquired after the local check.
            if let Err(error) = state.deps.protocol.ingress.forget_stream(&stream).await {
                warn!(%stream, %error, "SM janitor: orphan ingress retirement failed");
                completed = false;
            }
        }
        completed
    })
    .await
    .unwrap_or(false)
}

pub(in crate::server::session_janitors) async fn retry_pending_sm_ownership(
    state: &WebSocketState,
) -> bool {
    const COMBINED_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
    let registry = &state.deps.protocol.sm_session_registry;
    let hydration = async {
        registry.retry_pending_reclaimed_hydrations(64).await;
    };
    let releases = async {
        registry.retry_pending_claim_releases(64).await;
    };
    let completed = run_sm_retries_with_budget(COMBINED_RETRY_BUDGET, hydration, releases).await;
    if !completed {
        warn!(
            budget = ?COMBINED_RETRY_BUDGET,
            "SM janitor: ownership reconciliation exhausted its post-expiry tick budget"
        );
    }
    completed
}

pub(in crate::server::session_janitors) async fn run_sm_retries_with_budget<H, R>(
    budget: std::time::Duration,
    hydration: H,
    releases: R,
) -> bool
where
    H: std::future::Future<Output = ()>,
    R: std::future::Future<Output = ()>,
{
    // Both inventories retain responsibility until their individual item
    // succeeds or ownership is disproved, so cancelling this joined tail at
    // the tick deadline is safe. Polling concurrently prevents a degraded
    // hydration backend from starving exact terminal releases every tick.
    tokio::time::timeout(budget, async {
        tokio::join!(hydration, releases);
    })
    .await
    .is_ok()
}

pub(in crate::server::session_janitors) async fn confirm_expired_ingress_stream(
    state: &routes::websocket::WebSocketState,
    session: &waddle_xmpp::stream_management::DetachedSession,
) -> bool {
    let stream_id = &waddle_xmpp::pending_delivery::SmSessionId::new(&session.stream_id);
    if !state
        .deps
        .protocol
        .sm_session_registry
        .confirm_promotion_session(session)
        .await
    {
        return false;
    }
    crate::sm_promotion::confirm_pending_promotion(
        &state.deps.protocol.pending_delivery_storage,
        stream_id,
    )
    .await;
    // The SM claim must be released before the ingress retirement's absence fence.
    if let Err(error) = state.deps.protocol.ingress.forget_stream(stream_id).await {
        warn!(%stream_id, %error, "SM janitor: ingress stream retirement failed");
    }
    true
}

/// ADR-0017 Phase 3 Slice 5, element 9 (quoted verbatim): *"any node may
/// steal such [orphaned] claims (fenced CAS) and then expire or promote
/// them, after first committing the expire CAS on the owner's `nodes`
/// row."* A no-op build/runtime configuration (clustering disabled, or this
/// binary built without the `clustering` Cargo feature) does nothing —
/// [`run_orphan_reaper_sweep`] itself is feature-gated, and the outer
/// function here is always callable so its call site in `http.rs` needs no
/// `#[cfg]` of its own, mirroring the other eight janitor spawns.
///
/// `interval` is `ClusteringConfig::orphan_reaper_interval`
/// (`WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS`, default 120s) — ADR-0017
/// Phase 3 Slice 11 corrigenda (deviation 111) made this cadence env-
/// overridable, the same way every sibling cluster timer already is, so the
/// multi-process harness's kill-one hydration capstone does not have to
/// wait out the full 120s production default in real wall-clock time.
pub(crate) fn spawn_orphan_reaper_janitor(
    websocket_state: &Arc<WebSocketState>,
    interval: std::time::Duration,
) {
    #[cfg(feature = "clustering")]
    {
        let shutdown_drain_timeout = websocket_state.deps.shutdown.drain_timeout();
        let weak_state = Arc::downgrade(websocket_state);
        let registry = websocket_state.deps.protocol.sm_session_registry.clone();
        let Some(stop) = websocket_state
            .deps
            .app_state
            .clustering_claims
            .stop_token
            .clone()
        else {
            // The binary may include clustering support while runtime
            // clustering is disabled. In that configuration no clustering
            // supervisor or registry-lifetime watcher may be attached to the
            // process-wide shutdown token.
            return;
        };
        let Some(fatal_fence) = websocket_state
            .deps
            .app_state
            .clustering_claims
            .fatal_fence
            .clone()
        else {
            return;
        };
        let room_registry_actor = websocket_state.deps.protocol.room_registry.clone();
        let node_lifecycle = websocket_state.deps.app_state.node_lifecycle.clone();
        tokio::spawn(async move {
            let terminal_room_registry =
                waddle_xmpp::muc::RoomRegistry::wrap(room_registry_actor.clone());
            let mut supervisor = OrphanReaperSupervisor::new_with_fatal_fence(
                registry.clone(),
                stop.clone(),
                fatal_fence,
                node_lifecycle,
            );
            let mut ticker = tokio::time::interval(interval);
            // Skip the first (immediate) tick, mirroring the other janitors
            // — no need to sweep before the node-lease loop has even
            // registered this node.
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = supervisor.workers.fatal_fence.cancelled() => break,
                    _ = ticker.tick() => {}
                }
                if !supervisor.is_healthy() {
                    error!("orphan reaper worker exited; restarting workers with retained work");
                    supervisor = supervisor.restarted(registry.clone(), stop.clone()).await;
                    if supervisor.workers.fatal_fence.is_cancelled() {
                        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                            Janitor::OrphanReaper,
                            SweepOutcome::Failed,
                        );
                        break;
                    }
                }
                let Some(state) = weak_state.upgrade() else {
                    break;
                };
                let sweep_outcome = supervise_orphan_reaper_sweep(
                    &supervisor.workers.cancel,
                    &supervisor.workers.fatal_fence,
                    &supervisor.workers.node_lifecycle,
                    ORPHAN_REAPER_SWEEP_TIMEOUT,
                    run_orphan_reaper_sweep_with_workers(&state, &supervisor.workers),
                )
                .await;
                let mut workers_healthy = supervisor.is_healthy();
                let mut stop_after_sweep = false;
                match sweep_outcome {
                    OrphanSweepOutcome::Completed | OrphanSweepOutcome::Failed => {}
                    OrphanSweepOutcome::Cancelled => {
                        stop_after_sweep = true;
                    }
                    OrphanSweepOutcome::TimedOut => {
                        error!(
                            timeout = ?ORPHAN_REAPER_SWEEP_TIMEOUT,
                            "orphan reaper sweep timed out; self-fencing node because claim handoff state may be uncertain"
                        );
                        stop_after_sweep = true;
                    }
                }
                if !stop_after_sweep && !workers_healthy {
                    error!(
                        "orphan reaper worker exited during sweep; restarting workers with retained work"
                    );
                    supervisor = supervisor.restarted(registry.clone(), stop.clone()).await;
                    if supervisor.workers.fatal_fence.is_cancelled() {
                        stop_after_sweep = true;
                    }
                    workers_healthy = false;
                }
                // A shutdown-cancelled sweep is neither completed nor failed;
                // skipping the heartbeat keeps routine deploys from
                // registering failed sweeps (an absent heartbeat during
                // shutdown is the honest signal).
                if sweep_outcome != OrphanSweepOutcome::Cancelled {
                    waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                        Janitor::OrphanReaper,
                        orphan_sweep_heartbeat_outcome(sweep_outcome, workers_healthy),
                    );
                }
                if stop_after_sweep {
                    break;
                }
            }
            // Snapshot and stop SM/release workers before any other terminal
            // await. A fatal fence must not leave a live worker able to start
            // another ownership CAS while RoomRegistry cleanup is in flight.
            let pending_room_handoffs = supervisor.shutdown_terminal().await;
            match terminal_room_registry
                .drain_room_ownership_for_shutdown_with_timeout(
                    pending_room_handoffs,
                    shutdown_drain_timeout,
                )
                .await
            {
                Ok(outcome) if outcome.retained > 0 => {
                    error!(
                        released = outcome.released,
                        preserved_live = outcome.preserved_live,
                        retained = outcome.retained,
                        "orphan reaper: terminal RoomRegistry claim drain retained exact fences for node-expiry recovery"
                    );
                }
                Ok(outcome) => {
                    debug!(
                        released = outcome.released,
                        preserved_live = outcome.preserved_live,
                        "orphan reaper: terminal RoomRegistry claim drain completed"
                    );
                }
                Err(error) => {
                    error!(%error, "orphan reaper: terminal RoomRegistry claim drain failed; node-expiry recovery remains authoritative");
                }
            }
        });
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = websocket_state;
        let _ = interval;
    }
}

#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::session_janitors) enum OrphanSweepOutcome {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) fn orphan_sweep_heartbeat_outcome(
    outcome: OrphanSweepOutcome,
    workers_healthy: bool,
) -> SweepOutcome {
    if outcome == OrphanSweepOutcome::Completed && workers_healthy {
        SweepOutcome::Completed
    } else {
        SweepOutcome::Failed
    }
}

/// Bound and cancellation-arm the complete outer sweep, not only its worker
/// jobs. A timeout can occur after a fenced steal but before the typed actor
/// handoff returns; in that uncertain state the only safe generic recovery is
/// to stop this node's lease lifecycle and let another incarnation reclaim it.
#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) async fn supervise_orphan_reaper_sweep<F>(
    cancel: &tokio_util::sync::CancellationToken,
    fatal_fence: &tokio_util::sync::CancellationToken,
    node_lifecycle: &NodeLifecycle,
    timeout: Duration,
    sweep: F,
) -> OrphanSweepOutcome
where
    F: std::future::Future<Output = bool>,
{
    tokio::select! {
        biased;
        _ = cancel.cancelled() => OrphanSweepOutcome::Cancelled,
        _ = fatal_fence.cancelled() => OrphanSweepOutcome::Cancelled,
        result = tokio::time::timeout(timeout, sweep) => match result {
            Ok(true) => OrphanSweepOutcome::Completed,
            // The sweep observed the cancellation token itself before the
            // biased select arms fired — that's a shutdown, not a failure.
            Ok(false) if cancel.is_cancelled() || fatal_fence.is_cancelled() => {
                OrphanSweepOutcome::Cancelled
            }
            Ok(false) => OrphanSweepOutcome::Failed,
            Err(_) => {
                node_lifecycle.begin_fenced_recovery();
                fatal_fence.cancel();
                crate::clustering::metrics::record_orphan_worker_failure(
                    crate::clustering::metrics::OrphanWorker::Sweep,
                    crate::clustering::metrics::OrphanWorkerFailureReason::Timeout,
                );
                OrphanSweepOutcome::TimedOut
            }
        }
    }
}

pub(crate) async fn spawn_critical_registry_supervisor(websocket_state: &Arc<WebSocketState>) {
    let node_lifecycle = websocket_state.deps.app_state.node_lifecycle.clone();
    let process_stop = websocket_state.deps.shutdown.stop_token();
    let room_registry = websocket_state.deps.protocol.room_registry.clone();
    let user_registry = websocket_state.deps.protocol.user_registry.clone();
    let room_registry_liveness = room_registry.clone();
    let user_registry_liveness = user_registry.clone();
    let room_armed = spawn_room_registry_lifetime_watch(
        room_registry,
        node_lifecycle.clone(),
        process_stop.clone(),
    );
    let user_armed =
        spawn_user_registry_lifetime_watch(user_registry, node_lifecycle, process_stop);
    let (room_arm, user_arm) = tokio::join!(room_armed, user_armed);
    for outcome in [
        room_arm.unwrap_or(CriticalRegistryArm::Failed(
            CriticalNodeFailure::RoomRegistryTerminated,
        )),
        user_arm.unwrap_or(CriticalRegistryArm::Failed(
            CriticalNodeFailure::UserRegistryTerminated,
        )),
    ] {
        if let CriticalRegistryArm::Failed(failure) = outcome {
            websocket_state.deps.app_state.node_lifecycle.fail(failure);
            websocket_state.deps.shutdown.stop_token().cancel();
        }
    }
    // Close the arm-to-promotion gap synchronously. A registry that died
    // after its shutdown future was first polled but before this barrier is
    // failed before `finish_startup` can linearize Starting -> Serving.
    if !room_registry_liveness.is_alive() {
        websocket_state
            .deps
            .app_state
            .node_lifecycle
            .fail(CriticalNodeFailure::RoomRegistryTerminated);
        websocket_state.deps.shutdown.stop_token().cancel();
    }
    if !user_registry_liveness.is_alive() {
        websocket_state
            .deps
            .app_state
            .node_lifecycle
            .fail(CriticalNodeFailure::UserRegistryTerminated);
        websocket_state.deps.shutdown.stop_token().cancel();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::session_janitors) enum CriticalRegistryArm {
    Armed,
    Failed(CriticalNodeFailure),
}

pub(in crate::server::session_janitors) fn spawn_room_registry_lifetime_watch(
    room_registry: kameo::actor::ActorRef<waddle_xmpp::muc::room_registry_actor::RoomRegistryActor>,
    node_lifecycle: NodeLifecycle,
    process_stop: tokio_util::sync::CancellationToken,
) -> tokio::sync::oneshot::Receiver<CriticalRegistryArm> {
    let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let shutdown = room_registry.wait_for_shutdown();
        tokio::pin!(shutdown);
        if futures::poll!(shutdown.as_mut()).is_ready() {
            node_lifecycle.fail(CriticalNodeFailure::RoomRegistryTerminated);
            process_stop.cancel();
            let _ = armed_tx.send(CriticalRegistryArm::Failed(
                CriticalNodeFailure::RoomRegistryTerminated,
            ));
            return;
        }
        let _ = armed_tx.send(CriticalRegistryArm::Armed);
        tokio::select! {
            biased;
            // Ordered process shutdown is the sole non-fatal exit. Keeping it
            // first means a registry that terminates as part of that shutdown
            // cannot race into a terminal failure latch.
            _ = process_stop.cancelled() => {}
            _ = shutdown => {
                // Room ownership retry state is actor-local. Losing the
                // registry while this node's lease remains fresh can strand
                // every retained exact fence, even while idle, so registry
                // lifetime is clustering-critical.
                node_lifecycle.fail(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated);
                process_stop.cancel();
            }
        }
    });
    armed_rx
}

pub(in crate::server::session_janitors) fn spawn_user_registry_lifetime_watch(
    user_registry: kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    node_lifecycle: NodeLifecycle,
    process_stop: tokio_util::sync::CancellationToken,
) -> tokio::sync::oneshot::Receiver<CriticalRegistryArm> {
    let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let shutdown = user_registry.wait_for_shutdown();
        tokio::pin!(shutdown);
        if futures::poll!(shutdown.as_mut()).is_ready() {
            node_lifecycle.fail(CriticalNodeFailure::UserRegistryTerminated);
            process_stop.cancel();
            let _ = armed_tx.send(CriticalRegistryArm::Failed(
                CriticalNodeFailure::UserRegistryTerminated,
            ));
            return;
        }
        let _ = armed_tx.send(CriticalRegistryArm::Armed);
        tokio::select! {
            biased;
            _ = process_stop.cancelled() => {}
            _ = shutdown => {
                // UserActor claim/retry state is registry-local. Continuing
                // to advertise this node after it dies would accept sockets
                // whose logical owners cannot be activated safely.
                node_lifecycle.fail(crate::clustering::CriticalNodeFailure::UserRegistryTerminated);
                process_stop.cancel();
            }
        }
    });
    armed_rx
}

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) async fn orphan_reaper_self_lease_is_fresh(
    node_lease: &dyn crate::clustering::claims::NodeLeaseStore,
    me: &waddle_xmpp::ownership::NodeIdentity,
    lease_ttl: Duration,
    phase: &'static str,
) -> bool {
    match node_lease.is_fresh(me, lease_ttl).await {
        Ok(true) => true,
        Ok(false) => {
            debug!(
                node_id = %me.node_id,
                node_epoch = %me.node_epoch,
                phase,
                "orphan reaper: skipping sweep because this node's own lease is not fresh"
            );
            false
        }
        Err(error) => {
            warn!(
                node_id = %me.node_id,
                node_epoch = %me.node_epoch,
                phase,
                %error,
                "orphan reaper: failed to prove this node's own lease freshness; skipping sweep"
            );
            false
        }
    }
}

#[cfg(feature = "clustering")]
#[derive(Clone)]
pub(in crate::server::session_janitors) struct SmHydrationWork {
    pub(in crate::server::session_janitors) entity: waddle_xmpp::ownership::Entity,
    pub(in crate::server::session_janitors) fence:
        waddle_xmpp::stream_management::persistence::SmClaimFence,
    pub(in crate::server::session_janitors) reservation:
        waddle_xmpp::stream_management::ReclaimedClaimReservation,
    /// The enqueuing sweep's span context, carried for a span *link* on
    /// each attempt — never a live `Span`, which retry queues and pending
    /// inventories would hold open indefinitely.
    pub(in crate::server::session_janitors) sweep_context: opentelemetry::trace::SpanContext,
}

#[cfg(feature = "clustering")]
#[derive(Clone)]
pub(in crate::server::session_janitors) enum PendingSmHydration {
    Reserved {
        entity: waddle_xmpp::ownership::Entity,
        attempted_owner: waddle_xmpp::ownership::NodeIdentity,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    },
    Won(SmHydrationWork),
}

#[cfg(feature = "clustering")]
#[derive(Clone)]
pub(in crate::server::session_janitors) struct ExactReleaseWork {
    pub(in crate::server::session_janitors) claim_store:
        Arc<dyn waddle_xmpp::ownership::ClaimStore>,
    pub(in crate::server::session_janitors) entity: waddle_xmpp::ownership::Entity,
    pub(in crate::server::session_janitors) owner: waddle_xmpp::ownership::NodeIdentity,
    pub(in crate::server::session_janitors) epoch: waddle_xmpp::ownership::ClaimEpoch,
    /// See [`SmHydrationWork::sweep_context`].
    pub(in crate::server::session_janitors) sweep_context: opentelemetry::trace::SpanContext,
}

#[cfg(feature = "clustering")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::server::session_janitors) struct ExactReleaseKey {
    pub(in crate::server::session_janitors) entity: waddle_xmpp::ownership::Entity,
    pub(in crate::server::session_janitors) owner: waddle_xmpp::ownership::NodeIdentity,
    pub(in crate::server::session_janitors) epoch: waddle_xmpp::ownership::ClaimEpoch,
}

#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::session_janitors) enum WorkEnqueueOutcome {
    Enqueued,
    AlreadyTracked,
    /// Receiver exited, but the exact work remains in the supervisor's
    /// restart inventory and will be requeued by the replacement worker.
    RetainedForRestart,
    /// The bounded channel was full. The exact work remains in the pending
    /// inventory and a bounded background sender is actively redriving it.
    RetainedForRedrive,
    Rejected,
}

#[cfg(feature = "clustering")]
impl WorkEnqueueOutcome {
    pub(in crate::server::session_janitors) const fn is_accepted(self) -> bool {
        matches!(
            self,
            Self::Enqueued
                | Self::AlreadyTracked
                | Self::RetainedForRestart
                | Self::RetainedForRedrive
        )
    }
}

#[cfg(feature = "clustering")]
#[derive(Clone)]
pub(in crate::server::session_janitors) struct OrphanReaperWorkers {
    pub(in crate::server::session_janitors) hydration_tx:
        tokio::sync::mpsc::Sender<SmHydrationWork>,
    pub(in crate::server::session_janitors) release_tx: tokio::sync::mpsc::Sender<ExactReleaseWork>,
    pub(in crate::server::session_janitors) hydration_pending:
        Arc<std::sync::Mutex<HashMap<String, PendingSmHydration>>>,
    pub(in crate::server::session_janitors) release_pending:
        Arc<std::sync::Mutex<HashMap<ExactReleaseKey, ExactReleaseWork>>>,
    /// Exact RoomActor epochs observed as won but not yet accepted into the
    /// registry actor's pending inventory. This synchronous bridge closes the
    /// cancellation gap between the steal CAS returning and the first
    /// subsequent mailbox await.
    pub(in crate::server::session_janitors) room_handoff_pending: Arc<
        std::sync::Mutex<
            HashMap<
                (jid::BareJid, waddle_xmpp::muc::RoomClaimFenceContext),
                waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom,
            >,
        >,
    >,
    pub(in crate::server::session_janitors) room_cursor:
        Arc<std::sync::Mutex<Option<RoomOrphanScanCursor>>>,
    pub(in crate::server::session_janitors) sm_cursor:
        Arc<std::sync::Mutex<Option<SmOrphanScanCursor>>>,
    pub(in crate::server::session_janitors) cancel: tokio_util::sync::CancellationToken,
    pub(in crate::server::session_janitors) fatal_fence: tokio_util::sync::CancellationToken,
    pub(in crate::server::session_janitors) node_lifecycle: NodeLifecycle,
}

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) struct OrphanReaperSupervisor {
    pub(in crate::server::session_janitors) registry:
        Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
    pub(in crate::server::session_janitors) workers: OrphanReaperWorkers,
    pub(in crate::server::session_janitors) cancel: tokio_util::sync::CancellationToken,
    pub(in crate::server::session_janitors) tasks: Vec<(&'static str, tokio::task::JoinHandle<()>)>,
}

#[cfg(feature = "clustering")]
impl OrphanReaperWorkers {
    pub(in crate::server::session_janitors) fn hydration_key(work: &SmHydrationWork) -> String {
        work.entity.id.clone()
    }

    pub(in crate::server::session_janitors) fn reserve_hydration(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        attempted_owner: &waddle_xmpp::ownership::NodeIdentity,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) -> bool {
        let mut pending = self
            .hydration_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if pending.contains_key(&entity.id) {
            return false;
        }
        if pending.len() >= ORPHAN_WORK_QUEUE_CAPACITY {
            crate::clustering::metrics::record_orphan_work_queue_backpressure(
                OrphanWorkQueue::SmHydration,
            );
            return false;
        }
        pending.insert(
            entity.id.clone(),
            PendingSmHydration::Reserved {
                entity: entity.clone(),
                attempted_owner: attempted_owner.clone(),
                reservation,
            },
        );
        crate::clustering::metrics::record_orphan_work_queue_depth(
            OrphanWorkQueue::SmHydration,
            pending.len(),
        );
        true
    }

    pub(in crate::server::session_janitors) fn cancel_hydration_reservation(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
    ) {
        let mut pending = self
            .hydration_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        pending.remove(&entity.id);
        crate::clustering::metrics::record_orphan_work_queue_depth(
            OrphanWorkQueue::SmHydration,
            pending.len(),
        );
    }

    pub(in crate::server::session_janitors) fn enqueue_reserved_hydration(
        &self,
        entity: waddle_xmpp::ownership::Entity,
        fence: waddle_xmpp::stream_management::persistence::SmClaimFence,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) -> WorkEnqueueOutcome {
        let work = SmHydrationWork {
            entity,
            fence,
            reservation,
            sweep_context: current_sweep_context(),
        };
        let key = Self::hydration_key(&work);
        self.hydration_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .insert(key.clone(), PendingSmHydration::Won(work.clone()));
        match self.hydration_tx.try_send(work) {
            Ok(()) => WorkEnqueueOutcome::Enqueued,
            Err(tokio::sync::mpsc::error::TrySendError::Full(work)) => {
                crate::clustering::metrics::record_orphan_work_queue_backpressure(
                    OrphanWorkQueue::SmHydration,
                );
                let tx = self.hydration_tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(work).await;
                });
                WorkEnqueueOutcome::RetainedForRedrive
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                // The supervisor snapshots this map when it replaces a dead
                // worker. Retain the exact work so restart can requeue it.
                crate::clustering::metrics::record_orphan_work_queue_backpressure(
                    OrphanWorkQueue::SmHydration,
                );
                WorkEnqueueOutcome::RetainedForRestart
            }
        }
    }

    pub(in crate::server::session_janitors) fn release_key(
        work: &ExactReleaseWork,
    ) -> ExactReleaseKey {
        ExactReleaseKey {
            entity: work.entity.clone(),
            owner: work.owner.clone(),
            epoch: work.epoch,
        }
    }

    pub(in crate::server::session_janitors) fn has_release_capacity(&self) -> bool {
        self.release_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .len()
            < ORPHAN_WORK_QUEUE_CAPACITY
    }

    pub(in crate::server::session_janitors) fn enqueue_hydration(
        &self,
        entity: waddle_xmpp::ownership::Entity,
        fence: waddle_xmpp::stream_management::persistence::SmClaimFence,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) -> WorkEnqueueOutcome {
        let work = SmHydrationWork {
            entity,
            fence,
            reservation,
            sweep_context: current_sweep_context(),
        };
        if self
            .hydration_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .contains_key(&Self::hydration_key(&work))
        {
            return WorkEnqueueOutcome::AlreadyTracked;
        }
        if !self.reserve_hydration(&work.entity, work.fence.owner(), work.reservation) {
            return WorkEnqueueOutcome::Rejected;
        }
        self.enqueue_reserved_hydration(work.entity, work.fence, work.reservation)
    }

    pub(in crate::server::session_janitors) fn enqueue_release(
        &self,
        work: ExactReleaseWork,
    ) -> WorkEnqueueOutcome {
        let key = Self::release_key(&work);
        let mut pending = self
            .release_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if pending.contains_key(&key) {
            return WorkEnqueueOutcome::AlreadyTracked;
        }
        if pending.len() >= ORPHAN_WORK_QUEUE_CAPACITY {
            crate::clustering::metrics::record_orphan_work_queue_backpressure(
                OrphanWorkQueue::RoomRelease,
            );
            return WorkEnqueueOutcome::Rejected;
        }
        pending.insert(key.clone(), work.clone());
        match self.release_tx.try_send(work) {
            Ok(()) => {
                crate::clustering::metrics::record_orphan_work_queue_depth(
                    OrphanWorkQueue::RoomRelease,
                    pending.len(),
                );
                WorkEnqueueOutcome::Enqueued
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(work)) => {
                crate::clustering::metrics::record_orphan_work_queue_backpressure(
                    OrphanWorkQueue::RoomRelease,
                );
                let tx = self.release_tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(work).await;
                });
                WorkEnqueueOutcome::RetainedForRedrive
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                // A closed receiver means the supervisor must restart the
                // worker. Keep this exact fence in its restart inventory.
                crate::clustering::metrics::record_orphan_work_queue_backpressure(
                    OrphanWorkQueue::RoomRelease,
                );
                WorkEnqueueOutcome::RetainedForRestart
            }
        }
    }

    pub(in crate::server::session_janitors) fn pending_hydrations(&self) -> Vec<SmHydrationWork> {
        self.hydration_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .values()
            .filter_map(|pending| match pending {
                PendingSmHydration::Won(work) => Some(work.clone()),
                PendingSmHydration::Reserved { .. } => None,
            })
            .collect()
    }

    pub(in crate::server::session_janitors) fn pending_unwon_hydration_reservations(
        &self,
    ) -> Vec<(
        waddle_xmpp::ownership::Entity,
        waddle_xmpp::ownership::NodeIdentity,
        waddle_xmpp::stream_management::ReclaimedClaimReservation,
    )> {
        self.hydration_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .values()
            .filter_map(|pending| match pending {
                PendingSmHydration::Reserved {
                    entity,
                    attempted_owner,
                    reservation,
                } => Some((entity.clone(), attempted_owner.clone(), *reservation)),
                PendingSmHydration::Won(_) => None,
            })
            .collect()
    }

    pub(in crate::server::session_janitors) fn pending_releases(&self) -> Vec<ExactReleaseWork> {
        self.release_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .values()
            .cloned()
            .collect()
    }

    /// Restore exact known-won work only after both worker tasks have
    /// stopped. The stopped supervisor becomes a terminal carrier when an
    /// unrelated uncertain reservation cannot be retired; no receiver may
    /// clear these maps again before `shutdown_terminal` snapshots them.
    pub(in crate::server::session_janitors) fn restore_captured_terminal_work(
        &self,
        hydrations: Vec<SmHydrationWork>,
        releases: Vec<ExactReleaseWork>,
    ) {
        let mut hydration_pending = self
            .hydration_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        for work in hydrations {
            hydration_pending.insert(Self::hydration_key(&work), PendingSmHydration::Won(work));
        }
        debug_assert!(hydration_pending.len() <= ORPHAN_WORK_QUEUE_CAPACITY);
        crate::clustering::metrics::record_orphan_work_queue_depth(
            OrphanWorkQueue::SmHydration,
            hydration_pending.len(),
        );
        drop(hydration_pending);

        let mut release_pending = self
            .release_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        for work in releases {
            release_pending.insert(Self::release_key(&work), work);
        }
        debug_assert!(release_pending.len() <= ORPHAN_WORK_QUEUE_CAPACITY);
        crate::clustering::metrics::record_orphan_work_queue_depth(
            OrphanWorkQueue::RoomRelease,
            release_pending.len(),
        );
    }

    pub(in crate::server::session_janitors) fn remember_room_handoff(
        &self,
        pending: waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom,
    ) {
        let key = (pending.room_jid.clone(), pending.claim_fence.clone());
        let mut handoffs = self
            .room_handoff_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if handoffs.contains_key(&key) {
            return;
        }
        // Every entry consumes a RoomRegistry reservation admitted under the
        // same bound before the steal. Keep the post-CAS transition
        // infallible: once ownership is observed as won, dropping the exact
        // fence would be worse than violating a defensive debug assertion.
        debug_assert!(handoffs.len() < ORPHAN_WORK_QUEUE_CAPACITY);
        handoffs.insert(key, pending);
        crate::clustering::metrics::record_orphan_work_queue_depth(
            OrphanWorkQueue::RoomHandoff,
            handoffs.len(),
        );
    }

    pub(in crate::server::session_janitors) fn forget_room_handoff(
        &self,
        room_jid: &jid::BareJid,
        claim_fence: &waddle_xmpp::muc::RoomClaimFenceContext,
    ) {
        let mut handoffs = self
            .room_handoff_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        handoffs.remove(&(room_jid.clone(), claim_fence.clone()));
        crate::clustering::metrics::record_orphan_work_queue_depth(
            OrphanWorkQueue::RoomHandoff,
            handoffs.len(),
        );
    }

    pub(in crate::server::session_janitors) fn pending_room_handoffs(
        &self,
    ) -> Vec<waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom> {
        self.room_handoff_pending
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .values()
            .cloned()
            .collect()
    }

    pub(in crate::server::session_janitors) fn sm_cursor(&self) -> Option<SmOrphanScanCursor> {
        self.sm_cursor
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .clone()
    }

    pub(in crate::server::session_janitors) fn set_sm_cursor(
        &self,
        cursor: Option<SmOrphanScanCursor>,
    ) {
        *self
            .sm_cursor
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = cursor;
    }

    pub(in crate::server::session_janitors) fn room_cursor(&self) -> Option<RoomOrphanScanCursor> {
        self.room_cursor
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .clone()
    }

    pub(in crate::server::session_janitors) fn set_room_cursor(
        &self,
        cursor: Option<RoomOrphanScanCursor>,
    ) {
        *self
            .room_cursor
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = cursor;
    }
}

#[cfg(feature = "clustering")]
impl OrphanReaperSupervisor {
    #[cfg(test)]
    pub(in crate::server::session_janitors) fn new(
        registry: Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
        parent_cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        let node_lifecycle = NodeLifecycle::new();
        let fatal_fence = node_lifecycle.fatal_fence_token();
        Self::new_with_fatal_fence(registry, parent_cancel, fatal_fence, node_lifecycle)
    }

    pub(in crate::server::session_janitors) fn new_with_fatal_fence(
        registry: Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
        parent_cancel: tokio_util::sync::CancellationToken,
        fatal_fence: tokio_util::sync::CancellationToken,
        node_lifecycle: NodeLifecycle,
    ) -> Self {
        let cancel = parent_cancel.child_token();
        let (hydration_tx, mut hydration_rx) =
            tokio::sync::mpsc::channel(ORPHAN_WORK_QUEUE_CAPACITY);
        let (release_tx, mut release_rx) = tokio::sync::mpsc::channel(ORPHAN_WORK_QUEUE_CAPACITY);
        let hydration_pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let release_pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let room_handoff_pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let workers = OrphanReaperWorkers {
            hydration_tx,
            release_tx,
            hydration_pending: hydration_pending.clone(),
            release_pending: release_pending.clone(),
            room_handoff_pending,
            room_cursor: Arc::new(std::sync::Mutex::new(None)),
            sm_cursor: Arc::new(std::sync::Mutex::new(None)),
            cancel: cancel.clone(),
            fatal_fence,
            node_lifecycle,
        };

        let hydration_cancel = cancel.clone();
        let hydration_registry = registry.clone();
        let hydration_task = tokio::spawn(async move {
            let mut queue = VecDeque::new();
            loop {
                if queue.is_empty() {
                    tokio::select! {
                        _ = hydration_cancel.cancelled() => break,
                        work = hydration_rx.recv() => match work { Some(work) => queue.push_back(work), None => break },
                    }
                }
                while let Ok(work) = hydration_rx.try_recv() {
                    queue.push_back(work);
                }
                let Some(work) = queue.pop_front() else {
                    continue;
                };
                // The async block defers span creation to first poll:
                // select! evaluates every branch expression eagerly, and a
                // cancel-arm win would otherwise export a phantom attempt
                // root for work that never ran.
                let result = tokio::select! {
                    _ = hydration_cancel.cancelled() => break,
                    result = async {
                        tokio::time::timeout(
                            ORPHAN_WORK_ATTEMPT_TIMEOUT,
                            hydration_registry.hydrate_reclaimed_typed(
                                &work.entity,
                                &work.fence,
                                work.reservation,
                            ),
                        )
                        .instrument(orphan_work_span("sm_hydration", &work.sweep_context))
                        .await
                    } => result,
                };
                match result {
                    Ok(Ok(
                        waddle_xmpp::stream_management::ReclaimedHydrationOutcome::Hydrated
                        | waddle_xmpp::stream_management::ReclaimedHydrationOutcome::AlreadyPresent
                        | waddle_xmpp::stream_management::ReclaimedHydrationOutcome::LostClaim,
                    )) => {
                        let key = OrphanReaperWorkers::hydration_key(&work);
                        let mut pending = hydration_pending
                            .lock()
                            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
                        pending.remove(&key);
                        crate::clustering::metrics::record_orphan_work_queue_depth(
                            OrphanWorkQueue::SmHydration,
                            pending.len(),
                        );
                        info!("orphan reaper: targeted hydration work complete");
                    }
                    Ok(Ok(
                        waddle_xmpp::stream_management::ReclaimedHydrationOutcome::MissingDurable
                        | waddle_xmpp::stream_management::ReclaimedHydrationOutcome::PoisonReleased,
                    )) => {
                        let cleanup = tokio::select! {
                            _ = hydration_cancel.cancelled() => break,
                            result = async {
                                tokio::time::timeout(
                                    ORPHAN_WORK_ATTEMPT_TIMEOUT,
                                    hydration_registry.release_reclaimed_claim(
                                        &work.entity,
                                        &work.fence,
                                        work.reservation,
                                    ),
                                )
                                .instrument(orphan_work_span("sm_hydration", &work.sweep_context))
                                .await
                            } => result,
                        };
                        match cleanup {
                            Ok(Ok(_)) => {
                                let key = OrphanReaperWorkers::hydration_key(&work);
                                let mut pending = hydration_pending
                                    .lock()
                                    .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
                                pending.remove(&key);
                                crate::clustering::metrics::record_orphan_work_queue_depth(
                                    OrphanWorkQueue::SmHydration,
                                    pending.len(),
                                );
                            }
                            Ok(Err(error)) => {
                                debug!(%error, "orphan reaper: missing/poison SM exact release failed; retrying");
                                queue.push_back(work);
                            }
                            Err(_) => queue.push_back(work),
                        }
                    }
                    Ok(Ok(
                        waddle_xmpp::stream_management::ReclaimedHydrationOutcome::TransientFailure,
                    )) => {
                        queue.push_back(work);
                    }
                    Ok(Ok(
                        waddle_xmpp::stream_management::ReclaimedHydrationOutcome::StaleIdentity,
                    )) => {
                        let cleanup = tokio::time::timeout(
                            ORPHAN_WORK_ATTEMPT_TIMEOUT,
                            hydration_registry.release_reclaimed_claim(
                                &work.entity,
                                &work.fence,
                                work.reservation,
                            ),
                        )
                        .instrument(orphan_work_span("sm_hydration", &work.sweep_context))
                        .await;
                        match cleanup {
                            Ok(Ok(_)) => {
                                let key = OrphanReaperWorkers::hydration_key(&work);
                                let mut pending = hydration_pending
                                    .lock()
                                    .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
                                pending.remove(&key);
                                crate::clustering::metrics::record_orphan_work_queue_depth(
                                    OrphanWorkQueue::SmHydration,
                                    pending.len(),
                                );
                            }
                            Ok(Err(_)) | Err(_) => queue.push_back(work),
                        }
                    }
                    Ok(Err(error)) => {
                        debug!(%error, "orphan reaper: targeted hydration failed; retaining bounded retry");
                        queue.push_back(work);
                    }
                    Err(_) => {
                        debug!(
                            "orphan reaper: targeted hydration timed out; retaining bounded retry"
                        );
                        queue.push_back(work);
                    }
                }
                if !queue.is_empty() {
                    tokio::select! {
                        _ = hydration_cancel.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                }
            }
            hydration_pending
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                .clear();
            crate::clustering::metrics::record_orphan_work_queue_depth(
                OrphanWorkQueue::SmHydration,
                0,
            );
        });

        let release_cancel = cancel.clone();
        let release_task = tokio::spawn(async move {
            let mut queue = VecDeque::new();
            loop {
                if queue.is_empty() {
                    tokio::select! {
                        _ = release_cancel.cancelled() => break,
                        work = release_rx.recv() => match work { Some(work) => queue.push_back(work), None => break },
                    }
                }
                while let Ok(work) = release_rx.try_recv() {
                    queue.push_back(work);
                }
                let Some(work) = queue.pop_front() else {
                    continue;
                };
                // async block: see the hydration lane — select! would
                // otherwise create (and export) the span even when the
                // cancel arm wins before this branch is ever polled.
                let result = tokio::select! {
                    _ = release_cancel.cancelled() => break,
                    result = async {
                        tokio::time::timeout(
                            ORPHANED_ROOM_RELEASE_TIMEOUT,
                            work.claim_store.release_exact(&work.entity, &work.owner, work.epoch),
                        )
                        .instrument(orphan_work_span("room_release", &work.sweep_context))
                        .await
                    } => result,
                };
                match result {
                    Ok(Ok(_)) => {
                        let key = OrphanReaperWorkers::release_key(&work);
                        let mut pending = release_pending
                            .lock()
                            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
                        pending.remove(&key);
                        crate::clustering::metrics::record_orphan_work_queue_depth(
                            OrphanWorkQueue::RoomRelease,
                            pending.len(),
                        );
                    }
                    Ok(Err(error)) => {
                        debug!(%error, entity_id = %work.entity.id, "orphan reaper: exact cleanup retry failed");
                        queue.push_back(work);
                    }
                    Err(_) => {
                        debug!(entity_id = %work.entity.id, "orphan reaper: exact cleanup retry timed out");
                        queue.push_back(work);
                    }
                }
                if !queue.is_empty() {
                    tokio::select! {
                        _ = release_cancel.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                }
            }
            release_pending
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                .clear();
            crate::clustering::metrics::record_orphan_work_queue_depth(
                OrphanWorkQueue::RoomRelease,
                0,
            );
        });
        Self {
            registry,
            workers,
            cancel,
            tasks: vec![
                ("sm_hydration", hydration_task),
                ("room_release", release_task),
            ],
        }
    }

    pub(in crate::server::session_janitors) fn is_healthy(&self) -> bool {
        self.tasks.iter().all(|(_, task)| !task.is_finished())
    }

    pub(in crate::server::session_janitors) async fn restarted(
        self,
        registry: Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
        parent_cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        let hydration = self.workers.pending_hydrations();
        let unwon_reservations = self.workers.pending_unwon_hydration_reservations();
        let releases = self.workers.pending_releases();
        let room_handoffs = self.workers.pending_room_handoffs();
        let room_cursor = self.workers.room_cursor();
        let sm_cursor = self.workers.sm_cursor();
        let stopped = self.shutdown_for_restart().await;
        let fatal_fence = stopped.workers.fatal_fence.clone();
        let previous_registry = stopped.registry.clone();
        let retirement_results = futures::future::join_all(unwon_reservations.into_iter().map(
            |(entity, attempted_owner, reservation)| {
                let registry = previous_registry.clone();
                async move {
                    let entity_id = entity.id.clone();
                    let result = registry
                        .retire_uncertain_reclaimed_claim(&entity, &attempted_owner, reservation)
                        .await;
                    (entity_id, result)
                }
            },
        ))
        .await;
        let mut retirement_failed = false;
        for (entity_id, result) in retirement_results {
            if let Err(error) = result {
                error!(%error, %entity_id, "orphan reaper: uncertain SM claim could not be retired during worker restart; self-fencing node");
                retirement_failed = true;
            }
        }
        if retirement_failed {
            stopped
                .workers
                .restore_captured_terminal_work(hydration, releases);
            stopped.workers.node_lifecycle.begin_fenced_recovery();
            fatal_fence.cancel();
            return stopped;
        }
        let next = Self::new_with_fatal_fence(
            registry,
            parent_cancel,
            fatal_fence,
            stopped.workers.node_lifecycle.clone(),
        );
        next.workers.set_room_cursor(room_cursor);
        next.workers.set_sm_cursor(sm_cursor);
        for handoff in room_handoffs {
            next.workers.remember_room_handoff(handoff);
        }
        for work in hydration {
            // Re-enqueueing re-captures the (empty) ambient context, so a
            // restarted item loses its sweep link; its attempts still get
            // their own `janitor.orphan_work` roots, so the work stays
            // traced.
            if !next
                .workers
                .enqueue_hydration(work.entity, work.fence, work.reservation)
                .is_accepted()
            {
                error!("orphan reaper: failed to requeue hydration after worker restart");
            }
        }
        for work in releases {
            if !next.workers.enqueue_release(work).is_accepted() {
                error!("orphan reaper: failed to requeue exact release after worker restart");
            }
        }
        next
    }

    pub(in crate::server::session_janitors) async fn shutdown_for_restart(mut self) -> Self {
        self.cancel.cancel();
        for (worker, task) in self.tasks.drain(..) {
            if let Err(error) = task.await {
                let reason = if error.is_panic() {
                    OrphanWorkerFailureReason::Panic
                } else {
                    OrphanWorkerFailureReason::Cancelled
                };
                let worker = match worker {
                    "sm_hydration" => OrphanWorker::SmHydration,
                    "room_release" => OrphanWorker::RoomRelease,
                    _ => continue,
                };
                crate::clustering::metrics::record_orphan_worker_failure(worker, reason);
                error!(
                    worker = worker.as_str(),
                    error_class = "join_failed",
                    "orphan reaper worker failed"
                );
            }
        }
        self
    }

    pub(in crate::server::session_janitors) async fn shutdown_terminal(
        mut self,
    ) -> Vec<waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom> {
        let hydrations = self.workers.pending_hydrations();
        let unwon_reservations = self.workers.pending_unwon_hydration_reservations();
        let releases = self.workers.pending_releases();
        let room_handoffs = self.workers.pending_room_handoffs();
        self.cancel.cancel();
        for (worker, task) in self.tasks.drain(..) {
            if let Err(error) = task.await {
                let reason = if error.is_panic() {
                    OrphanWorkerFailureReason::Panic
                } else {
                    OrphanWorkerFailureReason::Cancelled
                };
                let worker = match worker {
                    "sm_hydration" => OrphanWorker::SmHydration,
                    "room_release" => OrphanWorker::RoomRelease,
                    _ => continue,
                };
                crate::clustering::metrics::record_orphan_worker_failure(worker, reason);
                error!(
                    worker = worker.as_str(),
                    error_class = "join_failed",
                    "orphan reaper worker failed"
                );
            }
        }
        let retirement_results = futures::future::join_all(unwon_reservations.into_iter().map(
            |(entity, attempted_owner, reservation)| {
                let registry = self.registry.clone();
                async move {
                    let entity_id = entity.id.clone();
                    let result = registry
                        .retire_uncertain_reclaimed_claim(&entity, &attempted_owner, reservation)
                        .await;
                    (entity_id, result)
                }
            },
        ))
        .await;
        for (entity_id, result) in retirement_results {
            if let Err(error) = result {
                error!(%error, %entity_id, "orphan reaper: terminal uncertain SM claim cleanup failed; retaining capacity reservation until process teardown");
            }
        }
        futures::future::join_all(hydrations.into_iter().map(|work| {
            let registry = self.registry.clone();
            let span = orphan_work_span("sm_hydration", &work.sweep_context);
            async move {
                match tokio::time::timeout(
                    ORPHAN_WORK_ATTEMPT_TIMEOUT,
                    registry.release_reclaimed_claim(
                        &work.entity,
                        &work.fence,
                        work.reservation,
                    ),
                )
                .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        crate::clustering::metrics::record_orphan_terminal_cleanup_failure(
                            OrphanTerminalCleanupLane::SmHydration,
                            OrphanTerminalCleanupFailureReason::Error,
                        );
                        let _ = error;
                        debug!(
                            error_class = "backend",
                            entity_id = %work.entity.id,
                            "orphan reaper: terminal SM cleanup failed"
                        );
                    }
                    Err(_) => {
                        crate::clustering::metrics::record_orphan_terminal_cleanup_failure(
                            OrphanTerminalCleanupLane::SmHydration,
                            OrphanTerminalCleanupFailureReason::Timeout,
                        );
                        debug!(entity_id = %work.entity.id, "orphan reaper: terminal SM cleanup timed out");
                    }
                }
            }
            .instrument(span)
        }))
        .await;
        crate::clustering::metrics::record_orphan_work_queue_depth(OrphanWorkQueue::SmHydration, 0);
        futures::future::join_all(releases.into_iter().map(|work| {
            let span = orphan_work_span("room_release", &work.sweep_context);
            async move {
                match tokio::time::timeout(
                    ORPHANED_ROOM_RELEASE_TIMEOUT,
                    work.claim_store
                        .release_exact(&work.entity, &work.owner, work.epoch),
                )
                .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        crate::clustering::metrics::record_orphan_terminal_cleanup_failure(
                            OrphanTerminalCleanupLane::RoomRelease,
                            OrphanTerminalCleanupFailureReason::Error,
                        );
                        let _ = error;
                        debug!(
                            error_class = "backend",
                            entity_id = %work.entity.id,
                            "orphan reaper: terminal exact cleanup failed"
                        );
                    }
                    Err(_) => {
                        crate::clustering::metrics::record_orphan_terminal_cleanup_failure(
                            OrphanTerminalCleanupLane::RoomRelease,
                            OrphanTerminalCleanupFailureReason::Timeout,
                        );
                        debug!(entity_id = %work.entity.id, "orphan reaper: terminal exact cleanup timed out");
                    }
                }
            }
            .instrument(span)
        }))
        .await;
        crate::clustering::metrics::record_orphan_work_queue_depth(OrphanWorkQueue::RoomRelease, 0);
        crate::clustering::metrics::record_orphan_work_queue_depth(OrphanWorkQueue::RoomHandoff, 0);
        room_handoffs
    }

    #[cfg(test)]
    pub(in crate::server::session_janitors) async fn shutdown(self) {
        self.shutdown_terminal().await;
    }
}

#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::session_janitors) enum ReclaimedRegistration {
    Registered,
    Released,
    LostRace,
    CleanupScheduled,
}

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) struct ReclaimedRegistrationContext<'a> {
    pub(in crate::server::session_janitors) workers: &'a OrphanReaperWorkers,
    pub(in crate::server::session_janitors) room_registry: &'a waddle_xmpp::muc::RoomRegistry,
    pub(in crate::server::session_janitors) claim_store:
        &'a Arc<dyn waddle_xmpp::ownership::ClaimStore>,
    pub(in crate::server::session_janitors) me: &'a waddle_xmpp::ownership::NodeIdentity,
}

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) async fn register_reclaimed_epoch_or_cleanup(
    context: ReclaimedRegistrationContext<'_>,
    pending: waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom,
) -> ReclaimedRegistration {
    let room_jid = &pending.room_jid;
    let claim_fence = &pending.claim_fence;
    loop {
        let registration = context
            .room_registry
            .remember_pending_reclaimed_room(
                room_jid.clone(),
                claim_fence.clone(),
                pending.previous_owner.clone(),
            )
            .await;
        if registration.is_ok() {
            return ReclaimedRegistration::Registered;
        }
        if !context.room_registry.actor_ref().is_alive() {
            break;
        }
        // The registry reservation was established before the steal, so
        // demand remains serialized while mailbox backpressure clears. Do
        // not cancel it or release outside the actor while the actor is live.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // A stopped registry cannot create or publish a replacement actor, so
    // exact cleanup can no longer race demand on this registry. Release the
    // won generation now; retain failures in the bounded supervised worker.
    let release = tokio::time::timeout(
        ORPHANED_ROOM_RELEASE_TIMEOUT,
        context
            .claim_store
            .release_exact(&claim_fence.entity, context.me, claim_fence.epoch),
    )
    .await;
    match release {
        Ok(Ok(waddle_xmpp::ownership::ExactReleaseOutcome::Released)) => {
            context.workers.forget_room_handoff(room_jid, claim_fence);
            ReclaimedRegistration::Released
        }
        Ok(Ok(waddle_xmpp::ownership::ExactReleaseOutcome::NotOwned)) => {
            context.workers.forget_room_handoff(room_jid, claim_fence);
            ReclaimedRegistration::LostRace
        }
        Ok(Err(error)) => {
            debug!(room = %room_jid, %error, "orphan reaper: stopped-registry exact cleanup failed; retaining supervised retry");
            loop {
                if context
                    .workers
                    .enqueue_release(ExactReleaseWork {
                        claim_store: context.claim_store.clone(),
                        entity: claim_fence.entity.clone(),
                        owner: context.me.clone(),
                        epoch: claim_fence.epoch,
                        sweep_context: current_sweep_context(),
                    })
                    .is_accepted()
                {
                    context.workers.forget_room_handoff(room_jid, claim_fence);
                    break ReclaimedRegistration::CleanupScheduled;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        Err(_) => loop {
            if context
                .workers
                .enqueue_release(ExactReleaseWork {
                    claim_store: context.claim_store.clone(),
                    entity: claim_fence.entity.clone(),
                    owner: context.me.clone(),
                    epoch: claim_fence.epoch,
                    sweep_context: current_sweep_context(),
                })
                .is_accepted()
            {
                context.workers.forget_room_handoff(room_jid, claim_fence);
                break ReclaimedRegistration::CleanupScheduled;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        },
    }
}

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) async fn reconcile_registered_room_or_self_fence(
    workers: &OrphanReaperWorkers,
    room_registry: &waddle_xmpp::muc::RoomRegistry,
    room_jid: jid::BareJid,
    claim_fence: waddle_xmpp::muc::RoomClaimFenceContext,
    previous_owner: waddle_xmpp::ownership::NodeIdentity,
) -> Result<
    waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome,
    waddle_xmpp::muc::RoomRegistryError,
> {
    let result = room_registry
        .reconcile_reclaimed_room(room_jid.clone(), claim_fence.clone(), previous_owner)
        .await;
    match &result {
        Ok(_) => workers.forget_room_handoff(&room_jid, &claim_fence),
        Err(_) => {
            // Mailbox acceptance is not handler completion. If this uncertain
            // handoff cannot produce a typed outcome, retain the supervisor's
            // exact fence for terminal transfer and stop this node's lease
            // lifecycle so another incarnation can recover after expiry.
            workers.node_lifecycle.begin_fenced_recovery();
            workers.fatal_fence.cancel();
        }
    }
    result
}

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) async fn run_orphan_reaper_sweep_with_workers(
    state: &Arc<WebSocketState>,
    workers: &OrphanReaperWorkers,
) -> bool {
    async {
        use waddle_xmpp::ownership::ClaimError;

        let mut sweep_failed = false;

        let clustering = &state.deps.app_state.clustering_claims;
        let Some((claim_store, identity_handle)) = clustering.claim_pair() else {
            return true;
        };
        let Some(node_lease) = clustering.node_lease.clone() else {
            return true;
        };
        let Some(lease_ttl) = clustering.lease_ttl else {
            return true;
        };

        let me = identity_handle.current();
        if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "start").await {
            return false;
        }

        // Admin commit receipts are deleted after projection; a timed-out or
        // cancelled detached delete strands one row per batch. Retry that
        // cleanup here by age, bounded per sweep, without failing the sweep.
        if let Some(store) = clustering.muc_durable_store.as_ref() {
            match store
                .prune_admin_mutation_receipts(
                    ADMIN_RECEIPT_RETENTION,
                    ADMIN_RECEIPT_PRUNE_LIMIT,
                )
                .await
            {
                Ok(0) => {}
                Ok(pruned) => {
                    info!(pruned, "orphan reaper: removed stranded MUC admin commit receipts");
                }
                Err(error) => {
                    warn!(%error, "orphan reaper: MUC admin receipt cleanup failed; retrying next sweep");
                }
            }
        }

        match node_lease
            .list_heartbeat_stale_nodes(lease_ttl, STALE_NODE_WATCHDOG_CANDIDATE_LIMIT)
            .await
        {
            Ok(stale_nodes) => {
                let candidate_count = stale_nodes.len();
                let mut expired_nodes = 0usize;
                let mut renewed_nodes = 0usize;
                let mut failed_nodes = 0usize;
                for stale_node in stale_nodes {
                    if stale_node == me {
                        debug!(
                            node_id = %me.node_id,
                            node_epoch = %me.node_epoch,
                            "orphan reaper: stale-node watchdog found this node's own heartbeat stale; aborting sweep"
                        );
                        return false;
                    }
                    match node_lease.expire(&stale_node, lease_ttl).await {
                        Ok(true) => expired_nodes += 1,
                        Ok(false) => renewed_nodes += 1,
                        Err(error) => {
                            failed_nodes += 1;
                            warn!(
                                node_id = %stale_node.node_id,
                                node_epoch = %stale_node.node_epoch,
                                %error,
                                "orphan reaper: stale-node watchdog expire failed; retrying next sweep"
                            );
                        }
                    }
                }
                if expired_nodes > 0 {
                    info!(
                        expired_nodes,
                        candidate_count,
                        limit = STALE_NODE_WATCHDOG_CANDIDATE_LIMIT,
                        "orphan reaper: stale-node watchdog committed expired nodes"
                    );
                }
                if renewed_nodes > 0 {
                    debug!(
                        renewed_nodes,
                        candidate_count,
                        "orphan reaper: stale-node watchdog candidates renewed before expire"
                    );
                }
                if failed_nodes > 0 {
                    sweep_failed = true;
                    warn!(
                        failed_nodes,
                        candidate_count,
                        "orphan reaper: stale-node watchdog failed to expire some candidates"
                    );
                }
            }
            Err(error) => {
                sweep_failed = true;
                warn!(%error, "orphan reaper: stale-node watchdog candidate scan failed");
            }
        }

        if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "post-watchdog")
            .await
        {
            return false;
        }

        // ADR-0017 Phase 3 Slice 10 (Q5's rollout-aware placement rule): an
        // old-generation node backs off before racing a matching/newer
        // -generation node for a just-orphaned claim, so each entity moves
        // approximately once per deploy instead of up to N times. Resolved
        // once per sweep (not per candidate) — the current generation cannot
        // meaningfully change mid-sweep, and this keeps the hot per-candidate
        // loop below to a single extra comparison, no extra Postgres round
        // trip per candidate.
        let backoff_delay = match node_lease.current_generation().await {
            Ok(current_generation) => crate::clustering::drain::rollout_backoff_delay(
                clustering.pod_template_hash.as_deref(),
                current_generation.as_deref(),
            ),
            Err(error) => {
                sweep_failed = true;
                debug!(%error, "orphan reaper: current_generation lookup failed; proceeding without backoff");
                std::time::Duration::ZERO
            }
        };

        let room_registry =
            waddle_xmpp::muc::RoomRegistry::wrap(state.deps.protocol.room_registry.clone());
        let mut room_hydrated = 0u64;
        let mut room_released = 0u64;
        let mut room_already_live = 0u64;
        let mut room_pending_retry = 0u64;
        let mut room_lost_race = 0u64;
        let mut room_failed = 0u64;

        // Ordinary destroy/dead-actor/eviction releases use the same orphan
        // reaper cadence, but retry one exact fence per sweep so a slow ownership
        // backend cannot monopolize the registry mailbox.
        if let Err(error) = room_registry.retry_pending_room_releases(1).await {
            sweep_failed = true;
            debug!(%error, "orphan reaper: pending ordinary RoomActor release retry failed");
        }

        // Retry epochs won on an earlier sweep before discovering new work.
        // Each retry is its own bounded registry ask, so a slow store operation
        // cannot monopolize the registry actor as one large batch handler.
        match room_registry
            .list_pending_reclaimed_rooms(ORPHANED_ROOM_CANDIDATE_LIMIT)
            .await
        {
            Ok(pending) => {
                for pending_room in pending {
                    match reconcile_registered_room_or_self_fence(
                        workers,
                        &room_registry,
                        pending_room.room_jid,
                        pending_room.claim_fence,
                        pending_room.previous_owner,
                    )
                    .await
                    {
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::Hydrated) => {
                            room_hydrated += 1;
                        }
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::Released) => {
                            room_released += 1;
                        }
                        Ok(
                            waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::AlreadyLive,
                        ) => room_already_live += 1,
                        Ok(
                            waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::PendingRetry,
                        ) => {
                            room_pending_retry += 1;
                        }
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::LostRace) => {
                            room_lost_race += 1;
                        }
                        Err(error) => {
                            debug!(%error, "orphan reaper: pending RoomActor retry ask failed");
                            return false;
                        }
                    }
                }
            }
            Err(error) => {
                debug!(%error, "orphan reaper: pending RoomActor listing failed");
                workers.node_lifecycle.begin_fenced_recovery();
                workers.fatal_fence.cancel();
                return false;
            }
        }
        if let Ok(mut backlog) = room_registry.pending_reclaimed_room_backlog().await {
            if let Ok(releases) = room_registry.pending_room_release_backlog().await {
                backlog.depth = backlog.depth.saturating_add(releases.depth);
                backlog.oldest_age_ms = backlog.oldest_age_ms.max(releases.oldest_age_ms);
            }
            crate::clustering::metrics::record_room_orphan_pending_backlog(
                backlog.depth,
                backlog.oldest_age_ms,
            );
        }

        // RoomActor counterpart: proactively move a bounded set of claims off
        // committed-dead owners. Unlike detached SM sessions, a room claim may
        // have no durable state at all (for example an ephemeral instant room).
        // The serialized registry adoption below therefore either hydrates the
        // exact won epoch, observes demand-side creation already did so, or
        // releases the unusable claim.
        let (room_candidates, room_page_next_cursor, room_page_has_more, room_scan_succeeded) =
            match node_lease
                .list_orphaned_room_actor_claims_page(
                    workers.room_cursor(),
                    ORPHANED_ROOM_CANDIDATE_LIMIT,
                )
                .await
            {
                Ok(page) => {
                    if page.quarantined > 0 {
                        debug!(
                            quarantined = page.quarantined,
                            "orphan reaper: quarantined malformed stale RoomActor claims"
                        );
                    }
                    (page.candidates, page.next_cursor, page.has_more, true)
                }
                Err(error) => {
                    warn!(%error, "orphan reaper: list_orphaned_room_actor_claims_page failed");
                    (Vec::new(), workers.room_cursor(), false, false)
                }
            };
        let mut room_page_processed = true;
        for candidate in room_candidates {
            if workers.cancel.is_cancelled() {
                return false;
            }
            if !workers.has_release_capacity() {
                room_page_processed = false;
                crate::clustering::metrics::record_orphan_work_queue_backpressure(
                    OrphanWorkQueue::RoomRelease,
                );
                warn!("orphan reaper: pausing RoomActor steals because exact-release cleanup capacity is exhausted");
                break;
            }
            let Ok(room_jid) = candidate.entity.id.parse::<jid::BareJid>() else {
                room_page_processed = false;
                room_failed += 1;
                debug!(
                    entity_id = %candidate.entity.id,
                    "orphan reaper: RoomActor claim id is not a bare JID; leaving stale claim for repair"
                );
                continue;
            };
            match room_registry
                .reserve_pending_reclaimed_room(room_jid.clone())
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    room_page_processed = false;
                    crate::clustering::metrics::record_orphan_work_queue_backpressure(
                        OrphanWorkQueue::RoomAdoption,
                    );
                    warn!("orphan reaper: pausing RoomActor steals because adoption capacity is exhausted");
                    break;
                }
                Err(error) => {
                    room_page_processed = false;
                    room_failed += 1;
                    debug!(room = %room_jid, %error, "orphan reaper: room adoption reservation failed");
                    break;
                }
            }
            if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "pre-room").await
            {
                let _ = room_registry
                    .cancel_pending_reclaimed_room_reservation(room_jid.clone())
                    .await;
                return false;
            }
            if let Err(error) = node_lease.expire(&candidate.owner, lease_ttl).await {
                room_page_processed = false;
                let _ = room_registry
                    .cancel_pending_reclaimed_room_reservation(room_jid.clone())
                    .await;
                room_failed += 1;
                debug!(
                    entity_id = %candidate.entity.id,
                    %error,
                    "orphan reaper: room owner's expire CAS failed"
                );
                continue;
            }
            if !backoff_delay.is_zero() {
                tokio::time::sleep(backoff_delay).await;
            }
            if workers.cancel.is_cancelled() {
                let _ = room_registry
                    .cancel_pending_reclaimed_room_reservation(room_jid.clone())
                    .await;
                return false;
            }
            match node_lease
                .steal_orphaned_room_actor_claim(&candidate.entity, candidate.epoch, &me, lease_ttl)
                .await
            {
                Ok(new_epoch) => {
                    let pending_handoff = waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom {
                        room_jid: room_jid.clone(),
                        claim_fence: waddle_xmpp::muc::RoomClaimFenceContext::new(
                            candidate.entity.clone(),
                            me.clone(),
                            new_epoch,
                        ),
                        previous_owner: candidate.owner.clone(),
                    };
                    // This must remain the first operation after observing the
                    // steal CAS succeed. It is synchronous, so cancellation of
                    // the outer sweep cannot drop the exact won epoch before
                    // terminal shutdown can transfer it into RoomRegistry.
                    workers.remember_room_handoff(pending_handoff.clone());
                    match register_reclaimed_epoch_or_cleanup(
                        ReclaimedRegistrationContext {
                            workers,
                            room_registry: &room_registry,
                            claim_store: &claim_store,
                            me: &me,
                        },
                        pending_handoff.clone(),
                    )
                    .await
                    {
                        ReclaimedRegistration::Registered => {}
                        ReclaimedRegistration::Released => {
                            room_released += 1;
                            continue;
                        }
                        ReclaimedRegistration::LostRace => {
                            room_lost_race += 1;
                            continue;
                        }
                        ReclaimedRegistration::CleanupScheduled => {
                            room_failed += 1;
                            continue;
                        }
                    }
                    let reconciliation = reconcile_registered_room_or_self_fence(
                        workers,
                        &room_registry,
                        room_jid.clone(),
                        pending_handoff.claim_fence,
                        candidate.owner.clone(),
                    )
                    .await;
                    let notify_previous_owner = !matches!(
                        reconciliation,
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::LostRace)
                            | Err(_)
                    );
                    if notify_previous_owner {
                        if let Some(store) = clustering.muc_durable_store.as_ref() {
                            if let Err(error) = store
                                .notify_previous_owner_demoted(
                                    &room_jid,
                                    &candidate.owner.node_id,
                                    &candidate.owner.node_epoch,
                                    new_epoch,
                                )
                                .await
                            {
                                sweep_failed = true;
                                debug!(
                                    room = %room_jid,
                                    %error,
                                    "orphan reaper: best-effort room demotion notification failed"
                                );
                            }
                        }
                    }
                    match reconciliation {
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::Hydrated) => {
                            room_hydrated += 1;
                        }
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::Released) => {
                            room_released += 1;
                        }
                        Ok(
                            waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::AlreadyLive,
                        ) => room_already_live += 1,
                        Ok(
                            waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::PendingRetry,
                        ) => room_pending_retry += 1,
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::LostRace) => {
                            room_lost_race += 1;
                        }
                        Err(error) => {
                            debug!(
                                room = %room_jid,
                                %error,
                                "orphan reaper: reclaimed-room registry adoption ask failed; node self-fenced"
                            );
                            return false;
                        }
                    }
                }
                Err(ClaimError::Conflict) => {
                    room_lost_race += 1;
                    if let Err(error) = room_registry
                        .cancel_pending_reclaimed_room_reservation(room_jid.clone())
                        .await
                    {
                        sweep_failed = true;
                        debug!(room = %room_jid, %error, "orphan reaper: failed to cancel lost-race room adoption reservation");
                    }
                }
                Err(error) => {
                    room_page_processed = false;
                    room_failed += 1;
                    let _ = room_registry
                        .cancel_pending_reclaimed_room_reservation(room_jid.clone())
                        .await;
                    debug!(
                        entity_id = %candidate.entity.id,
                        %error,
                        "orphan reaper: RoomActor claim steal failed"
                    );
                }
            }
        }
        if room_scan_succeeded && room_page_processed {
            let committed_cursor = if room_page_has_more {
                room_page_next_cursor
            } else {
                None
            };
            workers.set_room_cursor(committed_cursor.clone());
            if let Err(error) = node_lease
                .persist_orphan_reaper_cursor(
                    OrphanReaperCursorUpdate::RoomActor(committed_cursor),
                )
                .await
            {
                sweep_failed = true;
                warn!(%error, "orphan reaper: failed to persist RoomActor scan cursor");
            }
        }
        if !room_scan_succeeded || room_failed > 0 {
            sweep_failed = true;
        }
        for (outcome, count) in [
            (
                RoomOrphanReconciliationOutcome::Hydrated,
                room_hydrated,
            ),
            (
                RoomOrphanReconciliationOutcome::Released,
                room_released,
            ),
            (
                RoomOrphanReconciliationOutcome::AlreadyLive,
                room_already_live,
            ),
            (
                RoomOrphanReconciliationOutcome::PendingRetry,
                room_pending_retry,
            ),
            (
                RoomOrphanReconciliationOutcome::LostRace,
                room_lost_race,
            ),
            (
                RoomOrphanReconciliationOutcome::Failed,
                room_failed,
            ),
        ] {
            if count > 0 {
                crate::clustering::metrics::record_room_orphan_reconciliation(outcome, count);
            }
        }
        let room_reconciled = room_hydrated + room_released + room_already_live;
        if room_pending_retry > 0 || room_failed > 0 {
            warn!(
                hydrated = room_hydrated,
                released = room_released,
                already_live = room_already_live,
                pending_retry = room_pending_retry,
                lost_race = room_lost_race,
                failed = room_failed,
                limit = ORPHANED_ROOM_CANDIDATE_LIMIT,
                "orphan reaper: RoomActor reconciliation completed with pending work"
            );
        } else if room_reconciled > 0 {
            info!(
                hydrated = room_hydrated,
                released = room_released,
                already_live = room_already_live,
                pending_retry = room_pending_retry,
                lost_race = room_lost_race,
                failed = room_failed,
                limit = ORPHANED_ROOM_CANDIDATE_LIMIT,
                "orphan reaper: reconciled orphaned RoomActor claims"
            );
        }
        let (page, scan_succeeded) = match tokio::time::timeout(
            ORPHANED_SM_SCAN_TIMEOUT,
            node_lease.list_orphaned_sm_session_claims_page(
                workers.sm_cursor(),
                STALE_NODE_WATCHDOG_CANDIDATE_LIMIT,
            ),
        )
        .await
        {
            Ok(Ok(page)) => (page, true),
            Ok(Err(error)) => {
                debug!(%error, "orphan reaper: SM-session candidate page failed; room lane already completed");
                (
                    OrphanedSmSessionClaimPage {
                        candidates: Vec::new(),
                        next_cursor: workers.sm_cursor(),
                        has_more: false,
                        quarantined: 0,
                    },
                    false,
                )
            }
            Err(_) => {
                debug!(
                    "orphan reaper: SM-session candidate scan timed out; room lane already completed"
                );
                (
                    OrphanedSmSessionClaimPage {
                        candidates: Vec::new(),
                        next_cursor: workers.sm_cursor(),
                        has_more: false,
                        quarantined: 0,
                    },
                    false,
                )
            }
        };
        crate::clustering::metrics::record_sm_orphan_candidate_page(
            page.candidates.len(),
            page.has_more,
            page.quarantined,
        );
        let page_has_more = page.has_more;
        let page_next_cursor = page.next_cursor.clone();
        let candidates = page.candidates;
        let mut stolen = 0usize;
        let mut page_processed = true;
        for candidate in candidates {
            let Some(reservation) = state
                .deps
                .protocol
                .sm_session_registry
                .reserve_reclaimed_claim_capacity(&candidate.entity)
            else {
                page_processed = false;
                break;
            };
            if !workers.reserve_hydration(&candidate.entity, &me, reservation) {
                state
                    .deps
                    .protocol
                    .sm_session_registry
                    .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                warn!("orphan reaper: pausing SM steals because hydration capacity is exhausted");
                page_processed = false;
                break;
            }
            if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "pre-candidate")
                .await
            {
                workers.cancel_hydration_reservation(&candidate.entity);
                state
                    .deps
                    .protocol
                    .sm_session_registry
                    .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                return false;
            }
            // Element 9's ordering requirement: commit the expire CAS on the
            // dead owner's row FIRST. Idempotent/best-effort — a failure here
            // just means the steal below is also likely to lose (the owner
            // row is not yet committed-expired), retried next sweep.
            if let Err(error) = node_lease.expire(&candidate.owner, lease_ttl).await {
                sweep_failed = true;
                page_processed = false;
                workers.cancel_hydration_reservation(&candidate.entity);
                debug!(
                    entity_id = %candidate.entity.id,
                    %error,
                    "orphan reaper: expire on the dead owner's row failed; retrying next sweep"
                );
                state
                    .deps
                    .protocol
                    .sm_session_registry
                    .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                continue;
            }
            // ADR-0017 Phase 3 Slice 10: a placement heuristic only — never
            // affects correctness. The reaper-specific stale-owner epoch CAS
            // remains the sole authority over who actually wins; this only
            // decides who tries first.
            if !backoff_delay.is_zero() {
                tokio::time::sleep(backoff_delay).await;
            }
            if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "pre-steal")
                .await
            {
                workers.cancel_hydration_reservation(&candidate.entity);
                state
                    .deps
                    .protocol
                    .sm_session_registry
                    .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                return false;
            }
            match node_lease
                .steal_orphaned_sm_session_claim(&candidate.entity, candidate.epoch, &me, lease_ttl)
                .await
            {
                Ok(new_epoch) => {
                    stolen += 1;
                    match workers.enqueue_reserved_hydration(
                        candidate.entity,
                        waddle_xmpp::stream_management::persistence::SmClaimFence::new(
                            me.clone(),
                            new_epoch,
                        ),
                        reservation,
                    ) {
                        WorkEnqueueOutcome::Enqueued | WorkEnqueueOutcome::AlreadyTracked => {}
                        WorkEnqueueOutcome::RetainedForRestart => debug!("orphan reaper: hydration worker stopped after steal; responsibility retained for supervisor restart"),
                        WorkEnqueueOutcome::RetainedForRedrive => debug!("orphan reaper: hydration channel full after steal; responsibility retained for active redrive"),
                        WorkEnqueueOutcome::Rejected => debug!("orphan reaper: reserved hydration channel rejected work after steal; claim remains fenced until node expiry"),
                    }
                }
                Err(ClaimError::Conflict) => {
                    workers.cancel_hydration_reservation(&candidate.entity);
                    state
                        .deps
                        .protocol
                        .sm_session_registry
                        .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                    // Another node (or this same node's own re-registration
                    // reacquisition step, ADR-0017 Phase 3 plan deviation #19)
                    // already reclaimed it, or the "dead" owner actually
                    // renewed concurrently — safe, no-op.
                }
                Err(error) => {
                    sweep_failed = true;
                    page_processed = false;
                    workers.cancel_hydration_reservation(&candidate.entity);
                    state
                        .deps
                        .protocol
                        .sm_session_registry
                        .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                    warn!(
                        entity_id = %candidate.entity.id,
                        %error,
                        "orphan reaper: steal_orphaned_sm_session_claim failed"
                    );
                }
            }
        }
        if scan_succeeded && page_processed {
            let committed_cursor = if page_has_more {
                page_next_cursor
            } else {
                None
            };
            workers.set_sm_cursor(committed_cursor.clone());
            if let Err(error) = node_lease
                .persist_orphan_reaper_cursor(
                    OrphanReaperCursorUpdate::SmSession(committed_cursor),
                )
                .await
            {
                sweep_failed = true;
                warn!(%error, "orphan reaper: failed to persist SM-session scan cursor");
            }
        }
        if !scan_succeeded {
            sweep_failed = true;
        }
        if stolen > 0 {
            info!(
                stolen,
                "orphan reaper: reclaimed orphaned SM-session claims"
            );
        }

        !sweep_failed
    }
    .instrument(janitor_sweep_span(Janitor::OrphanReaper))
    .await
}

/// #1124: how many janitor intervals a claim must age before the
/// claim-expiry janitor may release it. Three intervals (180s at the
/// default 60s cadence) comfortably outlasts an in-flight flush batch
/// while keeping post-crash orphan recovery prompt.
pub(in crate::server::session_janitors) const PENDING_CLAIM_RELEASE_FLOOR_INTERVALS: i64 = 3;

pub(crate) fn spawn_pending_delivery_claim_janitor(websocket_state: &Arc<WebSocketState>) {
    // pending_delivery claim-expiry janitor (issue #209 slice (d)
    // phase 6 / PR #360): catches claims whose session no longer exists.
    let weak_state = Arc::downgrade(websocket_state);
    let interval_secs = std::env::var("WADDLE_PENDING_DELIVERY_JANITOR_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| v.max(1))
        .unwrap_or(60);
    tokio::spawn(async move {
        let mut recovery = PendingRecovery::default();
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        // Skip the first tick (immediate) so we don't sweep before
        // any flush has had a chance to run.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            super::run_pending_delivery_claim_sweep(&state, interval_secs, &mut recovery).await;
        }
    });
}

pub(crate) async fn run_pending_delivery_claim_sweep(
    state: &Arc<WebSocketState>,
    interval_secs: u64,
    recovery: &mut PendingRecovery,
) {
    let claim_release_floor_ms = i64::try_from(interval_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000)
        .saturating_mul(PENDING_CLAIM_RELEASE_FLOOR_INTERVALS);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let claimed_before_ms = now_ms.saturating_sub(claim_release_floor_ms);
    // Independent periodic recipient-local rediscovery also runs when there
    // are no orphan-release events and the initial presence CAS was spent.
    recovery.tick(state, claimed_before_ms).await;
    async {
        let mut sweep_failed = false;
        let detached_live: Option<Vec<waddle_xmpp::pending_delivery::SmSessionId>> = state
            .deps
            .protocol
            .sm_session_registry
            .live_session_ids()
            .map(|ids| {
                ids.into_iter()
                    .map(waddle_xmpp::pending_delivery::SmSessionId::new)
                    .collect()
            });
        let detached_live = match detached_live {
            Some(v) => v,
            None => {
                warn!(
                    "claim-expiry janitor: SM session registry locks poisoned; \
                     skipping sweep to avoid mass-release"
                );
                waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                    Janitor::PendingDeliveryClaim,
                    SweepOutcome::Failed,
                );
                return;
            }
        };
        let active_live = state
            .deps
            .protocol
            .connection_registry
            .active_sm_stream_ids();
        let mut live_sessions = detached_live;
        live_sessions.extend(active_live);
        match state.deps.protocol.sm_session_registry.pending_claim_protected_streams().await {
            Ok(streams) => live_sessions.extend(streams),
            Err(error) => {
                warn!(%error, "claim-expiry janitor: ACK journal unavailable; skipping orphan release");
                waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                    Janitor::PendingDeliveryClaim, SweepOutcome::Failed,
                );
                return;
            }
        }
        // #1124 claim recency floor: only release claims older
        // than several janitor intervals. Non-SM flushes claim
        // rows under a synthetic `transient:` session id that is
        // never in the live-set; without the floor, a janitor
        // pass overlapping an in-flight flush releases its claims
        // mid-flight and a second resource re-pushes the same
        // offline messages. Fresh claims are skipped this pass and
        // re-examined on later sweeps, so genuinely orphaned
        // (post-crash) claims are still released — just a few
        // intervals later.
        // Adopt claims written without a recency stamp (a
        // pre-#1124 binary during a rolling deploy) so they age
        // into release-eligibility instead of being skipped
        // forever — `list_orphaned_claims` ignores unstamped rows.
        match state
            .deps
            .protocol
            .pending_delivery_storage
            .stamp_unstamped_claims(now_ms)
            .await
        {
            Ok(adopted) if adopted > 0 => {
                debug!(
                    adopted,
                    "claim-expiry janitor: adopted unstamped pending_delivery claims"
                );
            }
            Ok(_) => {}
            Err(error) => {
                sweep_failed = true;
                warn!(error = %error, "claim-expiry janitor: stamp_unstamped_claims failed");
            }
        }
        match state
            .deps
            .protocol
            .pending_delivery_storage
            .list_orphaned_claims(&live_sessions, claimed_before_ms)
            .await
        {
            Ok(orphans) if !orphans.is_empty() => {
                let candidate_count = orphans.len();
                let mut released = 0u64;
                let mut skipped_reclaimed = 0u64;
                for (row_id, session) in orphans {
                    match state
                        .deps
                        .protocol
                        .pending_delivery_storage
                        .release_row_if_session(&row_id, &session)
                        .await
                    {
                        Ok(0) => skipped_reclaimed += 1,
                        Ok(_) => released += 1,
                        Err(error) => {
                            sweep_failed = true;
                            warn!(
                                row_id = %row_id,
                                session = %session,
                                error = %error,
                                "claim-expiry janitor: release_row_if_session failed; row stays \
                                 claimed and will be retried next sweep"
                            );
                        }
                    }
                }
                info!(
                    candidate_count,
                    released,
                    skipped_reclaimed,
                    "claim-expiry janitor: released orphaned pending_delivery claims"
                );
                waddle_xmpp::telemetry::reliability::add_pending_delivery_orphan_claims_released(
                    released,
                );
            }
            Ok(_) => {}
            Err(error) => {
                sweep_failed = true;
                warn!(error = %error, "claim-expiry janitor: list_orphaned_claims failed");
            }
        }

        let swept = state
            .deps
            .protocol
            .pending_delivery_storage
            .sweep_internal_bookkeeping();
        if swept > 0 {
            debug!(
                swept,
                "claim-expiry janitor: pruned idle per-recipient insert locks"
            );
        }

        let max_age_days = i64::from(pending_delivery_max_age_days_from_env());
        let cutoff = chrono::Utc::now() - chrono::Duration::days(max_age_days);
        if let Err(error) = run_pending_ageout_sweep(state, cutoff).await {
            sweep_failed = true;
            warn!(
                %error,
                "pending_delivery aging janitor: delete_older_than failed; \
                 will retry on next sweep"
            );
        }
        sweep_failed |= run_pending_promotion_receipt_sweep(
            state,
            claimed_before_ms,
            &mut recovery.promotion_receipt_cursor,
        )
        .await;
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::PendingDeliveryClaim,
            if sweep_failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::PendingDeliveryClaim))
    .await;
}

/// Run the same retention policy used by the periodic pending-delivery janitor.
pub(in crate::server::session_janitors) async fn run_pending_ageout_sweep(
    state: &WebSocketState,
    cutoff: chrono::DateTime<chrono::Utc>,
) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
    let removed = state
        .deps
        .protocol
        .pending_delivery_storage
        .delete_older_than(cutoff)
        .await?;
    if removed > 0 {
        waddle_xmpp::telemetry::reliability::add_pending_delivery_aged_out(removed);
        info!(removed, %cutoff, "pending_delivery aging janitor: dropped expired rows");
    }
    Ok(removed)
}

/// Receipt age schedules a bounded scan; only proven source absence permits deletion.
/// Advance past live sources and individual failures so they cannot starve later IDs.
pub(in crate::server::session_janitors) async fn run_pending_promotion_receipt_sweep(
    state: &WebSocketState,
    created_before_ms: i64,
    cursor: &mut Option<waddle_xmpp::pending_delivery::SmSessionId>,
) -> bool {
    const PAGE_SIZE: u32 = 128;
    let storage = &state.deps.protocol.pending_delivery_storage;
    let sources = match storage
        .promotion_receipt_sources_before(created_before_ms, cursor.as_ref(), PAGE_SIZE)
        .await
    {
        Ok(sources) => sources,
        Err(error) => {
            warn!(%error, "pending promotion receipt janitor: candidate scan failed; will retry next sweep");
            return true;
        }
    };
    *cursor = if sources.len() == PAGE_SIZE as usize {
        sources.last().cloned()
    } else {
        None
    };
    let mut failed = false;
    for source in sources {
        if let Err(error) = state
            .deps
            .protocol
            .sm_session_registry
            .retire_orphaned_promotion_receipts(&source, storage.as_ref())
            .await
        {
            failed = true;
            warn!(stream_id = %source, %error, "pending promotion receipt janitor: source verification failed; preserving receipts until a later scan");
        }
    }
    failed
}

pub(crate) fn spawn_push_service_publish_job_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    let interval_secs = std::env::var("WADDLE_PUSH_SERVICE_JOB_JANITOR_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| v.max(1))
        .unwrap_or(10);
    let batch_size = std::env::var("WADDLE_PUSH_SERVICE_JOB_JANITOR_BATCH")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|v| v.clamp(1, 1_000))
        .unwrap_or(128);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            run_push_service_publish_job_sweep(&state, batch_size).await;
        }
    });
}

pub(in crate::server::session_janitors) async fn run_push_service_publish_job_sweep(
    state: &WebSocketState,
    batch_size: usize,
) {
    async {
        // Inventory refresh is independent of sends, including an idle queue.
        // Preserve the prior values and timestamp if the read fails.
        match state.deps.protocol.push_service.queue_observation().await {
            Ok(snapshot) => snapshot.record(),
            Err(error) => warn!(%error, "Push provider queue snapshot failed"),
        }
        let outcome = match state
            .deps
            .protocol
            .push_service
            .drain_queued_notification_publish_jobs(batch_size)
            .await
        {
            Ok(results) if !results.is_empty() => {
                debug!(
                    drained = results.len(),
                    "Push Service publish-job janitor drained queued XEP-0357 jobs"
                );
                SweepOutcome::Completed
            }
            Ok(_) => SweepOutcome::Completed,
            Err(error) => {
                warn!(
                    error = %error,
                    "Push Service publish-job janitor failed; queued jobs remain durable"
                );
                SweepOutcome::Failed
            }
        };
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(Janitor::PushPublishJob, outcome);
    }
    .instrument(janitor_sweep_span(Janitor::PushPublishJob))
    .await;
}

pub(crate) fn spawn_notification_outbox_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    let interval_secs = std::env::var("WADDLE_NOTIFICATION_OUTBOX_JANITOR_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| v.max(1))
        .unwrap_or(5);
    let batch_size = std::env::var("WADDLE_NOTIFICATION_OUTBOX_JANITOR_BATCH")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|v| v.clamp(1, 1_000))
        .unwrap_or(128);
    let retention_days = notification_outbox_retention_days_from_env();
    let prune_batch_size = notification_outbox_prune_batch_from_env();
    // Operability: DND suppression now reads the durable
    // `urn:waddle:dnd:0` PEP projection (#367). The
    // `waddle_push_suppressed_total{reason="waddle_dnd"}` counter
    // reflects real recipient state — flat = no users actively in
    // DND, not a wiring placeholder.
    info!("Notification outbox janitor: DND suppression backed by urn:waddle:dnd:0 PEP projection");
    // Slice 2b operability: log the effective XEP-0513 `<active/>`
    // TTL once at startup so operators can read the clamped value
    // without grepping the codebase or re-deriving the env var
    // resolution chain.
    let active_mention_ttl_ms = crate::notification_outbox::active_mention_ttl_ms_from_env();
    let active_mention_ttl_secs = active_mention_ttl_ms / 1_000;
    info!(
        "Push active-mention TTL: {active_mention_ttl_secs}s \
         (default {}s, env WADDLE_PUSH_ACTIVE_MENTION_TTL_SECONDS, \
         clamp [{}s, {}s])",
        crate::notification_outbox::DEFAULT_ACTIVE_MENTION_TTL_SECONDS,
        crate::notification_outbox::MIN_ACTIVE_MENTION_TTL_SECONDS,
        crate::notification_outbox::MAX_ACTIVE_MENTION_TTL_SECONDS,
    );
    tokio::spawn(async move {
        let mut pending_notification_cursor =
            routes::interpret::PendingNotificationRecoveryCursor::default();
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            super::run_notification_outbox_sweep(
                &state,
                batch_size,
                retention_days,
                prune_batch_size,
                &mut pending_notification_cursor,
            )
            .await;
        }
    });
}

pub(crate) fn spawn_call_teardown_outbox_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    let interval_secs = std::env::var("WADDLE_CALL_TEARDOWN_JANITOR_INTERVAL")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.max(1))
        .unwrap_or(5);
    let batch_size = std::env::var("WADDLE_CALL_TEARDOWN_JANITOR_BATCH")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, 1_000))
        .unwrap_or(64);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            run_call_teardown_outbox_sweep(&state, batch_size).await;
        }
    });
}

/// Backstop for committed room mutation effects.  Config-class rows are inert
/// until their origin arms them; the same sweep also detects a dead origin
/// incarnation and arms (never deletes) its truthful committed rows.
pub(crate) fn spawn_room_effect_outbox_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    let interval_secs = std::env::var("WADDLE_ROOM_EFFECT_JANITOR_INTERVAL")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.max(1))
        .unwrap_or(5);
    let batch_size = std::env::var("WADDLE_ROOM_EFFECT_JANITOR_BATCH")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, 1_000))
        .unwrap_or(64);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            run_room_effect_outbox_sweep(&state, batch_size).await;
        }
    });
}

pub(in crate::server::session_janitors) async fn run_room_effect_outbox_sweep(
    state: &WebSocketState,
    batch_size: usize,
) {
    async {
        let store = &state.deps.protocol.room_effect_outbox;
        let mut failed = false;
        match crate::room_effect_outbox::drain::drain_due_effects(
            state,
            crate::time::now_ms(),
            batch_size,
        )
        .await
        {
            Ok(summary) => {
                waddle_xmpp::counter_add!(
                    "waddle.room_effect_outbox.drained",
                    "1",
                    "Completed room-effect outbox deliveries.",
                    summary.drained,
                    Janitor::RoomEffectOutbox,
                );
                waddle_xmpp::counter_add!(
                    "waddle.room_effect_outbox.requeued",
                    "1",
                    "Room-effect outbox rows released for retry during a sweep.",
                    summary.requeued,
                    Janitor::RoomEffectOutbox,
                );
                waddle_xmpp::counter_add!(
                    "waddle.room_effect_outbox.stale",
                    "1",
                    "Room-effect outbox rows discarded as stale during a sweep.",
                    summary.stale,
                    Janitor::RoomEffectOutbox,
                );
                waddle_xmpp::counter_add!(
                    "waddle.room_effect_outbox.dead_lettered",
                    "1",
                    "Room-effect outbox rows dead-lettered after remaining globally unowned for 24h.",
                    summary.dead_lettered,
                    Janitor::RoomEffectOutbox,
                );
            }
            Err(error) => {
                failed = true;
                warn!(%error, "room effect outbox drain failed; rows remain leased or retryable");
            }
        }
        if let Err(error) = store.reap_superseded(crate::time::now_ms()).await {
            failed = true;
            warn!(%error, "room effect outbox superseded-row reaper failed");
        }
        #[cfg(feature = "clustering")]
        if state
            .deps
            .app_state
            .clustering_claims
            .claim_pair()
            .is_some()
        {
            match store.current_producing_nodes().await {
                Ok(nodes) => {
                    if let Err(error) =
                        store.arm_foreign_inert(&nodes, crate::time::now_ms()).await
                    {
                        failed = true;
                        warn!(%error, "room effect outbox foreign inert arming failed");
                    }
                }
                Err(error) => {
                    failed = true;
                    warn!(%error, "room effect outbox live-node query failed");
                }
            }
        } else if let Err(error) = store
            .arm_predecessor_inert(
                &crate::room_effect_outbox::room_effect_origin_instance_id(),
                crate::time::now_ms(),
            )
            .await
        {
            failed = true;
            warn!(%error, "room effect outbox standalone predecessor arming failed");
        }
        #[cfg(not(feature = "clustering"))]
        if let Err(error) = store
            .arm_predecessor_inert(
                &crate::room_effect_outbox::room_effect_origin_instance_id(),
                crate::time::now_ms(),
            )
            .await
        {
            failed = true;
            warn!(%error, "room effect outbox standalone predecessor arming failed");
        }
        match store.queue_depth().await {
            Ok(depth) => waddle_xmpp::histogram_record!(
                "waddle.room_effect_outbox.depth",
                "1",
                "Rows currently retained by the room-effect outbox at sweep time.",
                depth as f64,
                Janitor::RoomEffectOutbox,
            ),
            Err(error) => {
                failed = true;
                warn!(%error, "room effect outbox depth query failed");
            }
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::RoomEffectOutbox,
            if failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::RoomEffectOutbox))
    .await;
}

pub(in crate::server::session_janitors) async fn run_call_teardown_outbox_sweep(
    state: &WebSocketState,
    batch_size: usize,
) {
    async {
        let store = &state.deps.protocol.call_teardown_outbox;
        let mut sweep_failed = false;
        match crate::call_teardown_outbox::drain_due(state, batch_size).await {
            Ok(summary) => {
                waddle_xmpp::telemetry::call::add_call_teardown_outbox_drained(summary.drained);
                waddle_xmpp::telemetry::call::add_call_teardown_outbox_requeued(summary.requeued);
                waddle_xmpp::telemetry::call::add_call_teardown_outbox_failed(summary.failed);
            }
            Err(error) => {
                sweep_failed = true;
                warn!(
                    %error,
                    "call teardown outbox drain failed; durable intents remain retryable"
                );
            }
        }
        match store.queue_stats().await {
            Ok(stats) => {
                waddle_xmpp::telemetry::call::record_call_teardown_outbox_depth(stats.queued_count);
                waddle_xmpp::telemetry::call::record_call_teardown_outbox_oldest_age(
                    stats.oldest_queued_age_ms as f64 / 1_000.0,
                );
            }
            Err(error) => {
                sweep_failed = true;
                warn!(%error, "call teardown outbox queue metrics query failed");
            }
        }
        if let Err(error) = store.prune_failed().await {
            sweep_failed = true;
            warn!(%error, "call teardown outbox failed-row pruning failed");
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::CallTeardownOutbox,
            if sweep_failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::CallTeardownOutbox))
    .await;
}

pub(crate) async fn run_notification_outbox_sweep(
    state: &WebSocketState,
    batch_size: usize,
    retention_days: u32,
    prune_batch_size: usize,
    pending_notification_cursor: &mut routes::interpret::PendingNotificationRecoveryCursor,
) {
    async {
        let mut sweep_failed = false;
        match state.deps.protocol.notification_outbox.queue_observation().await {
            Ok(snapshot) => snapshot.record(),
            Err(error) => {
                sweep_failed = true;
                warn!(%error, "Push outbox queue snapshot failed");
            }
        }
        let first_party_service_jid = match state.deps.service_domains.push.parse() {
            Ok(jid) => jid,
            Err(error) => {
                warn!(
                    error = %error,
                    push_service = %state.deps.service_domains.push,
                    "Notification outbox janitor cannot parse first-party Push Service JID"
                );
                waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                    Janitor::NotificationOutbox,
                    SweepOutcome::Failed,
                );
                return;
            }
        };
        let recovered =
            routes::interpret::reconcile_xep0357_notification_candidates_for_sweep(
                state, batch_size, pending_notification_cursor,
            )
            .await;
        sweep_failed |= recovered.had_failure;
        if recovered.completed > 0 {
            debug!(
                recovered = recovered.completed,
                "Notification outbox janitor recovered XEP-0357 candidates from pending_delivery"
            );
        }
        let recovered_groupchat =
            routes::interpret::reconcile_groupchat_notification_candidates_for_sweep(
                state, batch_size,
            )
            .await;
        sweep_failed |= recovered_groupchat.had_failure;
        if recovered_groupchat.completed > 0 {
            debug!(
                recovered = recovered_groupchat.completed,
                "Notification outbox janitor recovered XEP-0357 groupchat candidates from inbox projections"
            );
        }
        let room_policy = RoomRegistryActorPolicy::new(state.deps.protocol.room_registry.clone());
        let dnd_reader = state.deps.protocol.dnd_reader.as_ref();
        let activity_reader = state.deps.protocol.notification_activity.as_ref();
        let deps = NotificationDrainDeps::new(
            &room_policy,
            dnd_reader,
            activity_reader,
        );
        match state
            .deps
            .protocol
            .notification_outbox
            .drain_pending_candidates_into_outbox(
                state.deps.protocol.push_store.as_ref(),
                state.deps.protocol.blocking_storage.as_ref(),
                state
                    .deps
                    .protocol
                    .notification_settings_projection
                    .as_ref(),
                deps,
                &first_party_service_jid,
                batch_size,
            )
            .await
        {
            Ok(processed) if processed > 0 => {
                debug!(
                    processed,
                    "Notification outbox janitor expanded XEP-0357 candidates into outbox jobs"
                );
            }
            Ok(_) => {}
            Err(error) => {
                sweep_failed = true;
                warn!(
                    error = %error,
                    "Notification outbox janitor failed to process candidates; candidates remain durable"
                );
            }
        }
        match state
            .deps
            .protocol
            .notification_outbox
            .drain_due_outbox_jobs(
                state.deps.protocol.push_service.as_ref(),
                state.deps.protocol.push_store.as_ref(),
                state.deps.protocol.inbox_storage.as_ref(),
                state.deps.protocol.blocking_storage.as_ref(),
                &first_party_service_jid,
                batch_size,
            )
            .await
        {
            Ok(results) if !results.is_empty() => {
                debug!(
                    drained = results.len(),
                    "Notification outbox janitor emitted durable XEP-0357 PubSub publish jobs"
                );
            }
            Ok(_) => {}
            Err(error) => {
                sweep_failed = true;
                warn!(
                    error = %error,
                    "Notification outbox janitor failed; outbox jobs remain durable"
                );
            }
        }
        let cutoff_ms = crate::time::now_ms()
            .saturating_sub(i64::from(retention_days) * 24 * 60 * 60 * 1_000);
        match state
            .deps
            .protocol
            .notification_outbox
            .prune_completed_before(cutoff_ms, prune_batch_size)
            .await
        {
            Ok(outcome) if outcome.total_deleted() > 0 => {
                debug!(
                    candidates_deleted = outcome.candidates_deleted,
                    jobs_deleted = outcome.jobs_deleted,
                    "Notification outbox janitor pruned completed rows"
                );
            }
            Ok(_) => {}
            Err(error) => {
                sweep_failed = true;
                warn!(
                    error = %error,
                    "Notification outbox janitor prune failed; completed rows remain durable"
                );
            }
        }
        match state
            .deps
            .protocol
            .inbox_storage
            .prune_completed_groupchat_notification_recoveries(cutoff_ms, prune_batch_size)
            .await
        {
            Ok(deleted) if deleted > 0 => {
                debug!(
                    deleted,
                    "Notification outbox janitor pruned completed groupchat notification recovery rows"
                );
            }
            Ok(_) => {}
            Err(error) => {
                sweep_failed = true;
                warn!(
                    error = %error,
                    "Notification outbox janitor failed to prune completed groupchat notification recovery rows"
                );
            }
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::NotificationOutbox,
            if sweep_failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::NotificationOutbox))
    .await;
}

/// ADR-0017 Phase 3 Slice 10: feed the SM-session Q6 drain's outcomes into
/// the SAME `claims_released_on_drain`/`claims_abandoned_on_drain` counters
/// the generic per-entity room drain uses (`clustering::drain::
/// run_shutdown_drain`) — one shared observability surface for "how much of
/// this node's owned state made it out cleanly," regardless of which of
/// the two independent drain mechanisms (SM sessions vs. rooms) actually
/// drove a given entity. A no-op on a non-`clustering`-feature build: the
/// Q6 drain itself is unconditionally compiled (it works with or without
/// clustering, via `InProcessClaimStore`), but the Slice-10 metrics module
/// only exists behind the `clustering` Cargo feature.
pub(in crate::server::session_janitors) fn record_sm_drain_outcome(released: bool) {
    #[cfg(feature = "clustering")]
    {
        if released {
            crate::clustering::metrics::record_claims_released_on_drain(1);
        } else {
            crate::clustering::metrics::record_claims_abandoned_on_drain(1);
        }
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = released;
    }
}

/// Keep completed sessions accounted for even if a later session loses the
/// node lease and Q6 returns early. Release retries may settle until ingress
/// drain finishes, so the normal path records just before signaling Q6 done.
pub(in crate::server::session_janitors) struct ConfirmedSmDrainOutcomes<'a> {
    pub(in crate::server::session_janitors) registry:
        &'a waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    pub(in crate::server::session_janitors) stream_ids: Vec<String>,
}

impl ConfirmedSmDrainOutcomes<'_> {
    pub(in crate::server::session_janitors) fn record(&mut self) {
        if self.stream_ids.is_empty() {
            return;
        }
        let pending = self.registry.pending_claim_releases_for(&self.stream_ids);
        let released = self.stream_ids.len().saturating_sub(pending);
        #[cfg(feature = "clustering")]
        {
            if released > 0 {
                crate::clustering::metrics::record_claims_released_on_drain(released as u64);
            }
            if pending > 0 {
                crate::clustering::metrics::record_claims_abandoned_on_drain(pending as u64);
            }
        }
        #[cfg(not(feature = "clustering"))]
        let _ = released;
        self.stream_ids.clear();
    }
}

impl Drop for ConfirmedSmDrainOutcomes<'_> {
    fn drop(&mut self) {
        self.record();
    }
}

pub(in crate::server::session_janitors) fn remaining_sm_drain_count(
    registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
) -> usize {
    registry.live_session_ids().map_or(0, |ids| ids.len())
}

/// Keep an independent inventory of sessions Q6 actually drained. Fatal
/// fencing can demote and clear the registry before cancellation is polled;
/// the owned batch still needs abandonment accounting. Sessions not taken by
/// Q6 use the current live inventory, so an independent expiry janitor's
/// already-confirmed work is not counted as abandoned.
pub(in crate::server::session_janitors) struct RemainingSmDrainOutcomes<'a> {
    pub(in crate::server::session_janitors) registry:
        &'a waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    pub(in crate::server::session_janitors) fatal_fence: tokio_util::sync::CancellationToken,
    pub(in crate::server::session_janitors) observed: HashSet<String>,
    pub(in crate::server::session_janitors) accounted: HashSet<String>,
    pub(in crate::server::session_janitors) recorded: bool,
}

impl RemainingSmDrainOutcomes<'_> {
    pub(in crate::server::session_janitors) fn observe_live(&mut self) {
        if let Some(ids) = self.registry.live_session_ids() {
            self.observed.extend(ids);
        }
    }

    pub(in crate::server::session_janitors) fn observe_drained(
        &mut self,
        sessions: &[waddle_xmpp::stream_management::DetachedSession],
    ) {
        self.observed
            .extend(sessions.iter().map(|session| session.stream_id.clone()));
    }

    pub(in crate::server::session_janitors) fn account(&mut self, stream_id: &str) {
        self.accounted.insert(stream_id.to_string());
    }

    pub(in crate::server::session_janitors) fn record(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        let count = if self.fatal_fence.is_cancelled() {
            self.observe_live();
            self.observed
                .iter()
                .filter(|id| !self.accounted.contains(*id))
                .count()
        } else {
            remaining_sm_drain_count(self.registry)
        };
        if count > 0 {
            #[cfg(feature = "clustering")]
            crate::clustering::metrics::record_claims_abandoned_on_drain(count as u64);
        }
    }
}

impl Drop for RemainingSmDrainOutcomes<'_> {
    fn drop(&mut self) {
        self.record();
    }
}

pub(in crate::server::session_janitors) async fn abandon_shutdown_session_with_loss(
    registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    promotion_guard: &mut PromotionSessionGuard<'_>,
    loss: &waddle_xmpp::stream_management::ShutdownSmClaimLoss,
) {
    let session = promotion_guard.session();
    let stream_id = session.stream_id.clone();
    let jid = session.jid.clone();
    match registry
        .forget_promotion_claim_if_matches(session, loss)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            debug!(%stream_id, "Graceful shutdown: stale abandonment leaves replacement bookkeeping untouched")
        }
        Err(error) => {
            debug!(%stream_id, %error, "Graceful shutdown: proven loss could not retire local bookkeeping")
        }
    }
    promotion_guard.complete();
    record_sm_drain_outcome(false);
    warn!(%jid, %stream_id, "Graceful shutdown: abandoning SM session after loss of claim authority");
}

pub(in crate::server::session_janitors) async fn abandon_shutdown_session_if_claim_lost(
    registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    promotion_guard: &mut PromotionSessionGuard<'_>,
) -> Result<bool, waddle_xmpp::ownership::ClaimError> {
    let stream_id = promotion_guard.session().stream_id.clone();
    let loss = match registry.shutdown_drain_claim_lost(&stream_id).await {
        Ok(loss) => loss,
        Err(error) => {
            debug!(%stream_id, %error, "Graceful shutdown: SM claim lookup is uncertain; retaining session without promotion");
            return Err(error);
        }
    };
    let Some(loss) = loss else {
        return Ok(false);
    };
    abandon_shutdown_session_with_loss(registry, promotion_guard, &loss).await;
    Ok(true)
}

/// Stop on proven loss or uncertainty; only proven loss is accounted as
/// abandoned. Keep this policy identical at every shutdown await boundary.
pub(in crate::server::session_janitors) async fn shutdown_promotion_should_stop(
    registry: &waddle_xmpp::stream_management::InMemorySmSessionRegistry,
    promotion_guard: &mut PromotionSessionGuard<'_>,
    remaining: &mut RemainingSmDrainOutcomes<'_>,
) -> bool {
    match abandon_shutdown_session_if_claim_lost(registry, promotion_guard).await {
        Ok(false) => false,
        Ok(true) => {
            remaining.account(&promotion_guard.session().stream_id);
            true
        }
        Err(_) => true,
    }
}

pub(crate) fn spawn_graceful_shutdown_drain(
    websocket_state: Arc<WebSocketState>,
    drain_token: tokio_util::sync::CancellationToken,
    drain_notify: Arc<tokio::sync::Notify>,
    sm_drain_complete: tokio_util::sync::CancellationToken,
    shutdown_deadline: crate::shutdown_deadline::ShutdownDeadline,
    connections_drained: Arc<std::sync::atomic::AtomicBool>,
) {
    tokio::spawn(super::run_graceful_shutdown_drain(
        websocket_state,
        drain_token,
        drain_notify,
        sm_drain_complete,
        shutdown_deadline,
        connections_drained,
    ));
}

pub(crate) async fn run_graceful_shutdown_drain(
    websocket_state: Arc<WebSocketState>,
    drain_token: tokio_util::sync::CancellationToken,
    drain_notify: Arc<tokio::sync::Notify>,
    sm_drain_complete: tokio_util::sync::CancellationToken,
    shutdown_deadline: crate::shutdown_deadline::ShutdownDeadline,
    connections_drained: Arc<std::sync::atomic::AtomicBool>,
) {
    // Always notify_one on exit (success or early-return) so
    // the runtime's awaiting code never blocks indefinitely
    // on drain completion.
    struct NotifyOnDrop(pub(in crate::server::session_janitors) Arc<tokio::sync::Notify>);
    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }
    let _notify_guard = NotifyOnDrop(drain_notify);
    struct CompleteSmOnDrop(
        pub(in crate::server::session_janitors) tokio_util::sync::CancellationToken,
    );
    impl Drop for CompleteSmOnDrop {
        fn drop(&mut self) {
            self.0.cancel();
        }
    }
    let _sm_completion_guard = CompleteSmOnDrop(sm_drain_complete.clone());

    drain_token.cancelled().await;
    let fatal_fence = websocket_state
        .deps
        .app_state
        .node_lifecycle
        .fatal_fence_token();
    let mut remaining_outcomes = RemainingSmDrainOutcomes {
        registry: &websocket_state.deps.protocol.sm_session_registry,
        fatal_fence: fatal_fence.clone(),
        observed: HashSet::new(),
        accounted: HashSet::new(),
        recorded: false,
    };
    if fatal_fence.is_cancelled() {
        return;
    }
    // #2046 W1.10: the connection and Q6 drains are the first stage of the
    // one enclosing shutdown deadline, shared with the clustering drains.
    let shutdown = shutdown_deadline.start();
    let total_budget = shutdown.budget().drain();
    // ADR-0017 Phase 3 Slice 10: this task's own start-of-drain
    // timestamp, fed into the SAME `drain_duration_ms` histogram the
    // generic per-entity room drain records into (below, once the Q6
    // portion completes) — one shared observability surface across
    // both independent drain mechanisms.
    #[cfg(feature = "clustering")]
    let sm_drain_started = std::time::Instant::now();
    // Issue #1091: live sessions observe the same stop token, send
    // <system-shutdown/> and detach into the SmSessionRegistry.
    // Wait for every connection guard to drop before the Q6 passes
    // below, so live sessions' unacked queues are in the registry
    // when drain_all_for_shutdown runs — otherwise the quiet window
    // could conclude before a slow session finishes detaching.
    //
    // One deadline covers BOTH phases (connection drain + Q6
    // promotion): the pod's terminationGracePeriodSeconds is sized
    // for a single WADDLE_DRAIN_TIMEOUT_SECS budget, so serializing
    // two full budgets would let SIGKILL truncate Q6 promotion.
    // The connection wait gets at most HALF the budget: a single
    // stuck peer must not consume the whole deadline and starve Q6
    // promotion for sessions that already detached cleanly — the
    // stuck session itself falls back to its durable SM row on
    // next startup either way.
    let drain_deadline = shutdown.drain_deadline().into_std();
    info!(
        active_connections = websocket_state.deps.shutdown.active_connections(),
        "Graceful shutdown: waiting for live sessions to close and detach"
    );
    let connection_budget =
        (total_budget / 2).min(drain_deadline.saturating_duration_since(std::time::Instant::now()));
    let all_connections_drained = tokio::select! {
        biased;
        _ = fatal_fence.cancelled() => return,
        drained = websocket_state.deps.shutdown.wait_for_connections_drained_for(connection_budget) => drained,
    };
    if all_connections_drained {
        connections_drained.store(true, std::sync::atomic::Ordering::Release);
    } else {
        warn!(
            remaining_connections = websocket_state.deps.shutdown.active_connections(),
            "Graceful shutdown: connection drain timed out; promoting \
             whatever detached in time. Remaining sessions keep their \
             durable SM rows for next-startup retry."
        );
    }
    info!("Graceful shutdown: starting SM session Q6 drain");
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
    const QUIET_WINDOW_PASSES: u32 = 8;
    // Equal failure counts say nothing about whether storage can recover.
    // Bound failed passes per stream while still giving transient errors
    // and quota capacity a chance to recover during this shutdown.
    const MAX_FAILED_PROMOTION_PASSES: u32 = 3;
    let mut failed_promotion_passes = HashMap::<String, u32>::new();
    // Armed guards retain payloads and pending-promotion markers while
    // keeping restart-only work out of this drain's retry queues.
    let mut parked_sessions = Vec::new();
    let mut empty_passes = 0u32;
    let mut total_drained = 0usize;
    let mut confirmed_streams = ConfirmedSmDrainOutcomes {
        registry: &websocket_state.deps.protocol.sm_session_registry,
        stream_ids: Vec::new(),
    };
    'drain: loop {
        if fatal_fence.is_cancelled() {
            return;
        }
        if std::time::Instant::now() >= drain_deadline {
            waddle_xmpp::telemetry::reliability::increment_sm_drain_timeout();
            // ADR-0017 Phase 3 Slice 10: whatever this node still
            // believes it owns at the timeout never even reached
            // `drain_all_for_shutdown` this pass — abandoned, same as
            // the generic per-entity drain's own budget-overrun path.
            let remaining =
                remaining_sm_drain_count(&websocket_state.deps.protocol.sm_session_registry);
            warn!(
                total_drained,
                remaining,
                "Graceful shutdown: drain timeout reached. Remaining sessions \
                 keep their durable SM rows and will be retried on next startup \
                 via restore_from_persistence + Q6 expiry."
            );
            break;
        }
        // If the remaining shutdown budget expires mid-batch, dropping the
        // future is safe: `DrainedSessionBatch::Drop` restores unprocessed
        // sessions and `PendingPromotionRetryLease::Drop` restores any leased
        // retry payload back into the registry for next-startup retry.
        let drain_budget = drain_deadline.saturating_duration_since(std::time::Instant::now());
        let drained = match tokio::select! {
            biased;
            _ = fatal_fence.cancelled() => return,
            result = tokio::time::timeout(
                drain_budget,
                websocket_state.deps.protocol.sm_session_registry.drain_all_for_shutdown(),
            ) => result,
        } {
            Ok(Ok(s)) => s,
            Ok(Err(error)) => {
                let remaining =
                    remaining_sm_drain_count(&websocket_state.deps.protocol.sm_session_registry);
                warn!(%error, remaining, "Graceful shutdown: drain_all_for_shutdown failed");
                break;
            }
            Err(_) => {
                warn!(
                    remaining_ms = drain_budget.as_millis(),
                    "Graceful shutdown: drain_all_for_shutdown exceeded the remaining shutdown budget"
                );
                // Re-enter at the deadline check so timeout telemetry and
                // abandonment accounting stay centralized above.
                continue;
            }
        };
        let lost_claims = drained.lost_claims;
        let drained = drained.sessions;
        remaining_outcomes.observe_drained(&drained);
        let drained_count = drained.len();
        let mut promotion_batch =
            PromotionBatchGuard::new(&websocket_state.deps.protocol.sm_session_registry, drained);
        let release_retry_budget =
            drain_deadline.saturating_duration_since(std::time::Instant::now());
        if tokio::select! {
            biased;
            _ = fatal_fence.cancelled() => return,
            result = tokio::time::timeout(
                release_retry_budget,
                websocket_state.deps.protocol.sm_session_registry.retry_pending_claim_releases(64),
            ) => result,
        }
        .is_err()
        {
            // Re-enter at the deadline check so timeout telemetry and
            // abandonment accounting stay centralized above.
            continue;
        }
        if drained_count == 0 {
            empty_passes += 1;
            if empty_passes >= QUIET_WINDOW_PASSES {
                break;
            }
            tokio::select! {
                biased;
                _ = fatal_fence.cancelled() => return,
                _ = tokio::time::sleep(
                    POLL_INTERVAL.min(drain_deadline.saturating_duration_since(std::time::Instant::now())),
                ) => {}
            }
            continue;
        }
        empty_passes = 0;
        total_drained += drained_count;
        info!(
            count = drained_count,
            "Graceful shutdown: promoting unacked queues for detached SM sessions"
        );
        while let Some(session) = promotion_batch.pop() {
            let promotion_budget =
                drain_deadline.saturating_duration_since(std::time::Instant::now());
            if promotion_budget.is_zero() {
                // The session guard restores the already-popped session;
                // `PromotionBatchGuard::Drop` restores the rest of the batch.
                // Both keep promotion ownership and its claim intact.
                let _promotion_guard = PromotionSessionGuard::new(
                    &websocket_state.deps.protocol.sm_session_registry,
                    session,
                );
                drop(promotion_batch);
                continue 'drain;
            }
            let promotion_work = async {
                let mut promotion_guard = PromotionSessionGuard::new(
                    &websocket_state.deps.protocol.sm_session_registry,
                    session,
                );
                let metric_stream_id = promotion_guard.session().stream_id.clone();
                if let Some(loss) = lost_claims.get(
                    &waddle_xmpp::pending_delivery::SmSessionId::new(&metric_stream_id),
                ) {
                    abandon_shutdown_session_with_loss(
                        &websocket_state.deps.protocol.sm_session_registry,
                        &mut promotion_guard,
                        loss,
                    )
                    .await;
                    remaining_outcomes.account(&metric_stream_id);
                    return;
                }
                // One read-only fence check avoids reconciling a session
                // that a successor already owns. Keep it adjacent to this
                // session's work: row-backed pending releases and their
                // independently custodied re-drive precede the final SM
                // delete, and a batch-wide ownership snapshot can stale.
                if shutdown_promotion_should_stop(
                    &websocket_state.deps.protocol.sm_session_registry,
                    &mut promotion_guard,
                    &mut remaining_outcomes,
                )
                .await
                {
                    return;
                }
                let row_release = crate::sm_promotion::release_row_backed_replay_copies(
                    &websocket_state.deps.protocol.sm_session_registry,
                    &websocket_state.deps.protocol.pending_delivery_storage,
                    promotion_guard.session_mut(),
                )
                .await;
                let session = promotion_guard.session().clone();
                let has_unflushed_backlog = match websocket_state
                    .deps
                    .protocol
                    .pending_delivery_storage
                    .list(&session.jid.to_bare())
                    .await
                {
                    Ok(rows) => rows.iter().any(|row| row.outbound_sequence.is_none()),
                    Err(_) => true,
                };
                // This can push to a live socket in clustered shutdown, but
                // only from independently durable pending rows. Any
                // row-backed SM replay copy was pruned before release above;
                // the pending row's own claim CAS governs the flush.
                let released_redrive_aborted = (row_release.released_rows || has_unflushed_backlog)
                    && routes::websocket::redrive_terminal_pending_rows_to_live_resource_with_cancel(
                        &websocket_state,
                        &session.jid.to_bare(),
                        fatal_fence.clone(),
                    )
                    .await
                    .blocks_promotion();
                if row_release.ownership_unknown
                    || row_release.release_failed_known_rows
                    || released_redrive_aborted
                {
                    if shutdown_promotion_should_stop(
                        &websocket_state.deps.protocol.sm_session_registry,
                        &mut promotion_guard,
                        &mut remaining_outcomes,
                    )
                    .await
                    {
                        return;
                    }
                    warn!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        ownership_unknown = row_release.ownership_unknown,
                        release_failed = row_release.release_failed_known_rows,
                        redrive_aborted = released_redrive_aborted,
                        "Graceful shutdown: row ownership could not be reconciled; \
                         preserving the session and its claim for retry"
                    );
                    return;
                }
                let blocklist = match websocket_state
                    .deps
                    .protocol
                    .blocking_storage
                    .list_blocked_jid_entries(&session.jid.to_bare())
                    .await
                {
                    Ok(jids) => waddle_xmpp::protocol::session_state::Blocklist::new(jids),
                    Err(error) => {
                        if shutdown_promotion_should_stop(
                            &websocket_state.deps.protocol.sm_session_registry,
                            &mut promotion_guard,
                            &mut remaining_outcomes,
                        )
                        .await
                        {
                            return;
                        }
                        warn!(
                            jid = %session.jid,
                            error = %error,
                            "Graceful shutdown: blocklist load failed; SKIPPING \
                             promotion to preserve fail-closed XEP-0191 policy. \
                             Durable SM row will be retried on next startup."
                        );
                        return;
                    }
                };
                // Round-2 review R2 + round-3 finding 1: per-session
                // recent-tombstone fetch so a retraction landing
                // mid-batch during the shutdown drain is still seen.
                let mut recent_tombstones = Vec::new();
                if let Ok(records) = crate::sm_promotion::recent_tombstones_for_promotion(
                    &websocket_state.deps.protocol.sm_session_registry,
                    "Graceful shutdown",
                ) {
                    recent_tombstones = records;
                }
                let clustered_handoff = websocket_state
                    .deps
                    .app_state
                    .clustering_claims
                    .node_identity
                    .is_some();
                let summary = crate::sm_promotion::promote_session_with_custody(
                    &session,
                    TerminalOverflowPromotionDeps {
                        sm_registry: &websocket_state.deps.protocol.sm_session_registry,
                        registry: &websocket_state.deps.protocol.connection_registry,
                        user_registry: &websocket_state.deps.protocol.user_registry,
                        pending_storage: &websocket_state.deps.protocol.pending_delivery_storage,
                        blocklist: &blocklist,
                        server_domain: websocket_state.deps.auth_state.xmpp_domain.as_str(),
                        recent_tombstones: &recent_tombstones,
                        allow_unfenced_effects: !clustered_handoff,
                    },
                )
                .await;
                // Finding B: same TOCTOU close-out as the SM janitor —
                // re-scrub pending rows for tombstones recorded during
                // this session's promotion window before confirming.
                let _ =
                    crate::sm_promotion::scrub_pending_for_tombstones_recorded_during_promotion(
                        &websocket_state.deps.protocol.sm_session_registry,
                        &websocket_state.deps.protocol.pending_delivery_storage,
                        &recent_tombstones,
                        "Graceful shutdown",
                    )
                    .await;
                // The durable handoff protects SM replay from a successor,
                // but an available recipient still needs its newly queued
                // pending prefix before newer live traffic can overtake it.
                // Pending rows have independent custody and claim CAS.
                let redrive_blocked = clustered_handoff
                    && summary.queued > 0
                    && routes::websocket::redrive_terminal_pending_rows_to_live_resource_with_cancel(
                        &websocket_state,
                        &session.jid.to_bare(),
                        fatal_fence.clone(),
                    )
                    .await
                    .blocks_promotion();
                info!(
                    jid = %session.jid,
                    redelivered = summary.redelivered,
                    queued = summary.queued,
                    bounced = summary.bounced,
                    dropped = summary.dropped,
                    unparseable = summary.unparseable,
                    scrubbed = summary.scrubbed,
                    storage_failed = summary.storage_failed,
                    first_failure = ?summary.first_failure,
                    "Graceful shutdown: Q6 promotion completed for session"
                );
                if summary.has_storage_failure() || redrive_blocked {
                    if shutdown_promotion_should_stop(
                        &websocket_state.deps.protocol.sm_session_registry,
                        &mut promotion_guard,
                        &mut remaining_outcomes,
                    )
                    .await
                    {
                        return;
                    }
                    warn!(
                        jid = %session.jid,
                        storage_failed = summary.storage_failed,
                        redrive_blocked,
                        "Graceful shutdown: promotion or pending redrive incomplete; \
                         preserving durable SM row for restart-time retry"
                    );
                    crate::sm_promotion::prune_promoted_sequences(
                        &websocket_state.deps.protocol.sm_session_registry,
                        promotion_guard.session_mut(),
                        &summary,
                    )
                    .await;
                    let policy_deferred = summary
                        .first_failure
                        .is_some_and(|(_, reason)| reason.requires_restart());
                    let failed_passes = if summary.has_storage_failure() && !policy_deferred {
                        let failures = failed_promotion_passes
                            .entry(session.stream_id.clone())
                            .or_default();
                        *failures += 1;
                        *failures
                    } else {
                        0
                    };
                    if policy_deferred || failed_passes >= MAX_FAILED_PROMOTION_PASSES {
                        warn!(
                            stream_id = %session.stream_id,
                            first_failure = ?summary.first_failure,
                            policy_deferred,
                            failed_passes,
                            retry_budget = MAX_FAILED_PROMOTION_PASSES,
                            "Graceful shutdown: parking promotion after policy deferral or retry budget exhaustion"
                        );
                        parked_sessions.push(promotion_guard);
                    } else if crate::sm_promotion::reinsert_failed_session_for_retry(
                        &websocket_state.deps.protocol.sm_session_registry,
                        promotion_guard.session().clone(),
                    )
                    .await
                    {
                        promotion_guard.complete();
                    }
                    return;
                }
                let confirmed = websocket_state
                    .deps
                    .protocol
                    .sm_session_registry
                    .confirm_promotion_session(&session)
                    .await;
                // ADR-0017 Phase 3 Slice 10: this session's own "final
                // fenced write, then release" sequence — `confirm_drained`
                // deletes the durable row and releases the `ClaimStore`
                // claim only on success (see that method's own doc
                // comment).
                if !confirmed {
                    if shutdown_promotion_should_stop(
                        &websocket_state.deps.protocol.sm_session_registry,
                        &mut promotion_guard,
                        &mut remaining_outcomes,
                    )
                    .await
                    {
                        return;
                    }
                    warn!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        "Graceful shutdown: durable SM confirmation failed; retaining \
                         promotion ownership and pending-delivery claim for retry"
                    );
                    crate::sm_promotion::prune_promoted_sequences(
                        &websocket_state.deps.protocol.sm_session_registry,
                        promotion_guard.session_mut(),
                        &summary,
                    )
                    .await;
                    if crate::sm_promotion::reinsert_failed_session_for_retry(
                        &websocket_state.deps.protocol.sm_session_registry,
                        promotion_guard.session().clone(),
                    )
                    .await
                    {
                        promotion_guard.complete();
                    }
                    return;
                }
                // Durable confirmation and exact claim release are separate
                // outcomes. Retry terminal releases through the quiet window
                // before classifying this claim for drain metrics.
                confirmed_streams.stream_ids.push(session.stream_id.clone());
                remaining_outcomes.account(&metric_stream_id);
                let session_id =
                    waddle_xmpp::pending_delivery::SmSessionId::new(session.stream_id.clone());
                crate::sm_promotion::confirm_pending_promotion(
                    &websocket_state.deps.protocol.pending_delivery_storage,
                    &session_id,
                )
                .await;
                if let Err(error) = websocket_state
                    .deps
                    .protocol
                    .ingress
                    .forget_stream(&session_id)
                    .await
                {
                    warn!(%error, "Graceful shutdown: ingress stream retirement failed");
                }
                if let Err(error) = websocket_state
                    .deps
                    .protocol
                    .pending_delivery_storage
                    .release_claim(&session_id)
                    .await
                {
                    warn!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        error = %error,
                        "Graceful shutdown: pending_delivery release_claim failed; \
                         rows remain claimed and will be released by next-startup \
                        claim-expiry janitor"
                    );
                }
                promotion_guard.complete();
            };
            if tokio::select! {
                biased;
                _ = fatal_fence.cancelled() => return,
                result = tokio::time::timeout(promotion_budget, promotion_work) => result,
            }
            .is_err()
            {
                // Cancelling the per-session future drops its armed
                // `PromotionSessionGuard`; remaining sessions stay in the
                // batch guard. Both restore their payloads with the retained
                // promotion marker/claim, so next startup can retry safely.
                continue 'drain;
            }
        }
        tokio::select! {
            biased;
            _ = fatal_fence.cancelled() => return,
            _ = tokio::time::sleep(
                POLL_INTERVAL.min(drain_deadline.saturating_duration_since(std::time::Instant::now())),
            ) => {}
        }
    }
    if fatal_fence.is_cancelled() {
        return;
    }
    info!(
        total_drained,
        parked_sessions = parked_sessions.len(),
        "Graceful shutdown: SM Q6 drain complete (iterative)"
    );
    let ingress_budget = drain_deadline.saturating_duration_since(std::time::Instant::now());
    let ingress_drained = tokio::select! {
        biased;
        _ = fatal_fence.cancelled() => return,
        drained = websocket_state.deps.protocol.ingress.drain_and_join(ingress_budget) => drained,
    };
    if !ingress_drained {
        warn!(
            timeout_ms = ingress_budget.as_millis(),
            "Graceful shutdown: ingress authority drain exceeded the shutdown budget; stopping unfinished ingress work"
        );
    } else {
        info!("Graceful shutdown: ingress authority drain complete");
    }
    #[cfg(feature = "clustering")]
    crate::clustering::metrics::record_drain_duration_ms(
        sm_drain_started.elapsed().as_secs_f64() * 1000.0,
    );
    confirmed_streams.record();
    remaining_outcomes.record();
    // Admission stopped before this drain began. A late connection close can
    // therefore prove terminal socket closure even if the first wait timed out.
    if websocket_state.deps.shutdown.active_connections() == 0 {
        connections_drained.store(true, std::sync::atomic::Ordering::Release);
    }
    // Node-lease shutdown may now disable this identity. All Q6 fenced
    // writes and ingress authority work are finished; the remaining profile
    // and webhook tracker waits do not belong to the SM ownership drain.
    sm_drain_complete.cancel();

    // Drain the OIDC profile-publish tracker before notifying
    // shutdown complete. Each in-flight `ensure_pep_profile_published`
    // call is bounded by the fetcher's 25s timeout budget +
    // storage latency, so the wait is naturally bounded; calling
    // `close()` first prevents new publishes from racing in
    // during the drain. Without this wait the runtime can tear
    // down mid-step and leave the avatar in a split state
    // (empty `<metadata/>` published but vcard-temp PHOTO not
    // yet stripped — exactly the inconsistency XEP-0398 §3
    // forbids).
    let publish_tracker = websocket_state
        .deps
        .protocol
        .profile_publish_tracker
        .clone();
    publish_tracker.close();
    if !publish_tracker.is_empty() {
        info!(
            in_flight = publish_tracker.len(),
            "Graceful shutdown: awaiting in-flight OIDC profile publishes"
        );
    }
    publish_tracker.wait().await;
    info!("Graceful shutdown: OIDC profile-publish drain complete");

    // Drain in-flight provider webhook dispatch tasks so the ledger
    // status update (`mark_provider_delivery`) lands before teardown.
    // Rows that never reach dispatch (process kill between insert and
    // task start) still stay 'queued' — V1 has no sweep job.
    let dispatch_tracker = websocket_state.deps.provider_dispatch_tasks.clone();
    dispatch_tracker.close();
    if !dispatch_tracker.is_empty() {
        info!(
            in_flight = dispatch_tracker.len(),
            "Graceful shutdown: awaiting in-flight provider webhook dispatches"
        );
    }
    dispatch_tracker.wait().await;
    info!("Graceful shutdown: provider webhook dispatch drain complete");
}

/// Sweep expired entries from the durable auth handshake tables
/// (`pending_auth`, `device_auth`, `xmpp_auth_codes`).
///
/// These tables grow on every started OAuth / device / XMPP-OAuth flow
/// and are removed only on the success path. Abandoned flows (network
/// flake, tab close, user typo) leave entries behind. The OAuth specs
/// already bound the validity window (10 minutes for
/// `PendingAuthorization` / `XmppAuthCode`, `device_auth.expires_at`
/// for `DeviceAuthorization`); each row carries that deadline as
/// `expires_at_ms`, so the sweep is a plain SQL delete and any replica
/// may run it (see `AuthHandshakeStore::sweep_expired`).
///
/// Runs on a 60 s ticker. Skips the first immediate tick so a fresh
/// process doesn't sweep before any auth flow has started.
pub(crate) fn spawn_auth_state_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(AUTH_JANITOR_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            run_auth_state_sweep(&state).await;
            sweep_expired_session_rows(&state).await;
        }
    });
}

/// Delete `sessions` rows whose `expires_at` has passed. Expiry alone only
/// makes a row fail validation/resolution; nothing else ever deletes it, and
/// native-SCRAM logins add a resume-fence row per authentication, so without
/// this sweep the table grows monotonically (#1643 review).
pub(in crate::server::session_janitors) async fn sweep_expired_session_rows(
    state: &WebSocketState,
) {
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(error) = state
        .deps
        .auth_state
        .session_manager
        .actor_ref()
        .ask(DbExecute {
            sql: "DELETE FROM sessions WHERE expires_at IS NOT NULL AND expires_at < ?".to_string(),
            params: vec![crate::db::Value::from(now)],
        })
        .await
    {
        warn!(error = %error, "auth janitor: expired session-row sweep failed; will retry next tick");
    }
}

pub(in crate::server::session_janitors) async fn run_auth_state_sweep(state: &WebSocketState) {
    async {
        let counts = match state
            .deps
            .auth_state
            .auth_handshake
            .sweep_expired(chrono::Utc::now())
            .await
        {
            Ok(counts) => counts,
            Err(error) => {
                warn!(error = %error, "auth janitor: sweep failed; will retry next tick");
                waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                    Janitor::AuthState,
                    SweepOutcome::Failed,
                );
                return;
            }
        };
        let total = counts.pending_pruned + counts.device_pruned + counts.xmpp_pruned;
        if total > 0 {
            info!(
                pending_auth_pruned = counts.pending_pruned,
                device_auth_pruned = counts.device_pruned,
                xmpp_auth_codes_pruned = counts.xmpp_pruned,
                pending_auth_remaining = counts.pending_remaining,
                device_auth_remaining = counts.device_remaining,
                xmpp_auth_codes_remaining = counts.xmpp_remaining,
                "auth janitor: pruned expired entries"
            );
        } else {
            debug!(
                pending_auth_remaining = counts.pending_remaining,
                device_auth_remaining = counts.device_remaining,
                xmpp_auth_codes_remaining = counts.xmpp_remaining,
                "auth janitor: no expired entries"
            );
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::AuthState,
            SweepOutcome::Completed,
        );
    }
    .instrument(janitor_sweep_span(Janitor::AuthState))
    .await;
}

/// Per-sweep counts returned by [`sweep_dormant_rooms_once`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DormancySweepCounts {
    pub evicted: usize,
    pub examined: usize,
    pub remaining: usize,
    pub(in crate::server::session_janitors) failed: bool,
}

/// Walk every registered MUC room and destroy the ones that report
/// dormant. Pure helper so the live janitor and unit tests share the
/// same logic. Each room ask is bounded — a hung room actor only
/// stalls its own check, never the sweep.
pub(crate) async fn sweep_dormant_rooms_once(
    websocket_state: &WebSocketState,
) -> super::DormancySweepCounts {
    use waddle_xmpp::muc::room_actor::{IsDormant, SealGuard};
    use waddle_xmpp::muc::room_registry_actor::{DestroyRoomIfInactive, ListRooms, RoomCount};
    let mut counts = DormancySweepCounts::default();
    let rooms = match websocket_state
        .deps
        .protocol
        .room_registry
        .ask(ListRooms)
        .await
    {
        Ok(list) => list,
        Err(error) => {
            counts.failed = true;
            warn!(error = ?error, "room dormancy janitor: ListRooms ask failed");
            return counts;
        }
    };
    counts.examined = rooms.len();
    for room_jid in rooms {
        let actor = match websocket_state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => continue,
            Err(error) => {
                counts.failed = true;
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "room dormancy janitor: GetRoom ask failed; skipping"
                );
                continue;
            }
        };
        // Bounded ask: a hung room actor (e.g. wedged on hydration) must
        // only skip its own check — timeout lands in the existing
        // warn-and-skip arm, treating the room as not-dormant (never reap
        // on uncertainty).
        let status = match actor.ask(IsDormant).reply_timeout(REAPER_ASK_TIMEOUT).await {
            Ok(value) => value,
            Err(error) => {
                counts.failed = true;
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "room dormancy janitor: IsDormant ask failed; skipping"
                );
                continue;
            }
        };
        if !status.dormant {
            continue;
        }
        // #1108: destruction is revision-guarded — the registry asks the
        // room actor to seal itself only if it is still dormant at the
        // probed occupancy revision (checked inside the room actor's
        // serialized mailbox), so a join that landed after the probe
        // above refuses the destroy instead of being orphaned.
        match websocket_state
            .deps
            .protocol
            .room_registry
            .ask(DestroyRoomIfInactive {
                room_jid: room_jid.clone(),
                expected_occupancy_revision: status.occupancy_revision,
                guard: SealGuard::Dormant,
            })
            .await
        {
            Ok(outcome) if outcome.destroyed() => {
                counts.evicted += 1;
                debug!(room = %room_jid, "room dormancy janitor: evicted dormant room");
            }
            Ok(_) => {}
            Err(error) => {
                counts.failed = true;
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "room dormancy janitor: DestroyRoomIfInactive ask failed; will retry next pass"
                );
            }
        }
    }
    counts.remaining = match websocket_state
        .deps
        .protocol
        .room_registry
        .ask(RoomCount)
        .await
    {
        Ok(remaining) => remaining,
        Err(error) => {
            counts.failed = true;
            warn!(error = ?error, "room dormancy janitor: RoomCount ask failed");
            0
        }
    };
    counts
}

/// Periodically evict fully-dormant MUC rooms (no occupants AND no
/// subject AND no pins AND no in-memory affiliations) so the
/// `RoomRegistryActor.rooms` map shrinks back to current
/// working-set rather than growing to the lifetime room set.
///
/// Eviction is safe for dormant rooms because re-entry through
/// `GetOrCreateRoom` spawns a fresh `RoomActor` with identical
/// initial state — see [`waddle_xmpp::muc::MucRoom::is_dormant`].
/// Rooms with subject text, pinned entries, or explicit affiliation
/// grants are intentionally NOT evicted here: those caches are
/// in-memory only and dropping them would lose user-visible state.
///
/// Runs on a 5-minute ticker. Skips the first immediate tick so the
/// process doesn't sweep before any room has been touched.
pub(crate) fn spawn_room_dormancy_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(ROOM_DORMANCY_JANITOR_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            run_room_dormancy_sweep(&state).await;
        }
    });
}

/// Drain typed owner-IQ destroy work that a registry reconciliation completed
/// outside the original WebSocket request. This is intentionally separate
/// from the five-minute dormancy sweep so app-level resurrection cleanup and
/// XEP-0045 §10.9 notifications do not wait for unrelated room churn.
pub(crate) fn spawn_destroy_completion_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(DESTROY_COMPLETION_JANITOR_INTERVAL);
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            let _ = crate::server::routes::websocket::drain_destroy_completions(&state, None).await;
        }
    });
}

pub(in crate::server::session_janitors) async fn run_room_dormancy_sweep(state: &WebSocketState) {
    async {
        let counts = super::sweep_dormant_rooms_once(state).await;
        if counts.evicted > 0 {
            info!(
                examined = counts.examined,
                evicted = counts.evicted,
                remaining = counts.remaining,
                "room dormancy janitor: evicted dormant rooms"
            );
        } else {
            debug!(
                examined = counts.examined,
                remaining = counts.remaining,
                "room dormancy janitor: no dormant rooms"
            );
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::RoomDormancy,
            if counts.failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::RoomDormancy))
    .await;
}

/// Per-sweep counts returned by [`sweep_empty_user_actors_once`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UserReaperSweepCounts {
    pub reaped: usize,
    pub examined: usize,
    pub remaining: usize,
    pub(in crate::server::session_janitors) failed: bool,
    pub(in crate::server::session_janitors) deferred: bool,
}

impl super::UserReaperSweepCounts {
    pub(in crate::server::session_janitors) fn outcome(&self) -> SweepOutcome {
        if self.failed {
            SweepOutcome::Failed
        } else if self.deferred {
            SweepOutcome::Deferred
        } else {
            SweepOutcome::Completed
        }
    }
}

pub(in crate::server::session_janitors) async fn record_user_registry_convergence_status(
    user_registry: &kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    counts: &mut UserReaperSweepCounts,
) {
    use waddle_xmpp::registry::RetryUserRegistryConvergence;

    match user_registry
        .ask(RetryUserRegistryConvergence)
        .mailbox_timeout(REAPER_ASK_TIMEOUT)
        .reply_timeout(REAPER_ASK_TIMEOUT)
        .await
    {
        Ok(report) => {
            let pending_unregisters = report.pending_unregisters;
            let terminal_releases = report.terminal_releases;
            counts.deferred = pending_unregisters > 0 || terminal_releases > 0;
            if report.failed_operations > 0 {
                counts.failed = true;
                warn!(
                    failed_operations = report.failed_operations,
                    pending_unregisters,
                    terminal_releases,
                    "user actor reaper: convergence retry operations failed"
                );
            } else if counts.deferred {
                debug!(
                    pending_unregisters,
                    terminal_releases, "user actor reaper: registry convergence deferred"
                );
            }
        }
        Err(error) => {
            counts.failed = true;
            warn!(error = ?error, "user actor reaper: convergence retry ask failed");
        }
    }
}

/// Walk every registered `UserActor` and reap the ones that report zero
/// connected resources (ADR-0017 Phase 1 Slice 2, Copilot review on PR #1177).
///
/// Each user is reaped via the single atomic [`ReapUserIfEmpty`] registry
/// message — the emptiness check and the removal happen in one registry handler
/// so a concurrent re-registration cannot slip a resource in between (see that
/// message's docs). The janitor only drives the per-user asks; it never reads
/// then removes across two asks, which would reintroduce that race.
///
/// Every registry ask — `ListUsers`, each per-user `ReapUserIfEmpty`, and the
/// closing `UserCount` — is bounded by [`REAPER_ASK_TIMEOUT`], so a wedged or
/// backed-up registry stalls only that ask (logged, skipped) rather than
/// hanging the whole janitor task; the sweep resumes on the next interval.
///
/// Pure helper so the live reaper and unit tests share the same logic.
pub(crate) async fn sweep_empty_user_actors_once(
    websocket_state: &WebSocketState,
) -> UserReaperSweepCounts {
    use waddle_xmpp::registry::{ListUsers, ReapUserIfEmpty, UserCount};
    let mut counts = UserReaperSweepCounts::default();
    let user_registry = &websocket_state.deps.protocol.user_registry;
    record_user_registry_convergence_status(user_registry, &mut counts).await;
    let users = match user_registry
        .ask(ListUsers)
        .mailbox_timeout(REAPER_ASK_TIMEOUT)
        .reply_timeout(REAPER_ASK_TIMEOUT)
        .await
    {
        Ok(list) => list,
        Err(error) => {
            counts.failed = true;
            warn!(error = ?error, "user actor reaper: ListUsers ask failed");
            return counts;
        }
    };
    counts.examined = users.len();
    for bare_jid in users {
        match user_registry
            .ask(ReapUserIfEmpty {
                bare_jid: bare_jid.clone(),
            })
            .mailbox_timeout(REAPER_ASK_TIMEOUT)
            .reply_timeout(REAPER_ASK_TIMEOUT)
            .await
        {
            Ok(true) => {
                counts.reaped += 1;
                waddle_xmpp::telemetry::reliability::increment_user_actor_reaped();
                debug!(jid = %bare_jid, "user actor reaper: reaped empty UserActor");
            }
            Ok(false) => {}
            Err(error) => {
                counts.failed = true;
                warn!(
                    jid = %bare_jid,
                    error = ?error,
                    "user actor reaper: ReapUserIfEmpty ask failed; will retry next pass"
                );
            }
        }
    }
    // Reaping can retire the local actor before its exact durable claim
    // release succeeds, which records new terminal convergence work during
    // this same sweep. Re-check immediately so the retry is prompt and the
    // sweep stays non-complete while ownership cleanup remains pending.
    record_user_registry_convergence_status(user_registry, &mut counts).await;
    counts.remaining = match user_registry
        .ask(UserCount)
        .mailbox_timeout(REAPER_ASK_TIMEOUT)
        .reply_timeout(REAPER_ASK_TIMEOUT)
        .await
    {
        Ok(count) => count,
        Err(error) => {
            counts.failed = true;
            // The closing count is a log-only diagnostic, so a failure is not
            // fatal — but surface it rather than reporting a misleading `0
            // remaining` that reads as "everything was reaped".
            warn!(error = ?error, "user actor reaper: UserCount ask failed; remaining is unknown");
            counts.examined.saturating_sub(counts.reaped)
        }
    };
    counts
}

/// Periodically reap empty `UserActor`s so `UserRegistryActor.users` shrinks
/// back to the current working set rather than growing to the lifetime user
/// set.
///
/// Required by the Slice 2 delivery cutover: production delivery now runs
/// through the actor's `TrySend*`, whose `try_deliver` evicts a closed-channel
/// resource. When that eviction removes a `UserActor`'s last resource without
/// the explicit `UnregisterConnectionAndReportEmpty` prune path running (a
/// dropped best-effort `mirror_unregister`), the empty actor would otherwise
/// linger forever. The `UserActor` deliberately does NOT self-prune on empty
/// (that races an in-flight re-registration and trips the crashed-actor poison
/// path); this out-of-band reaper is the safe alternative.
///
/// Runs on a 5-minute ticker. Skips the first immediate tick so the process
/// doesn't sweep before any resource has been registered.
pub(crate) fn spawn_user_actor_reaper(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(USER_ACTOR_REAPER_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            run_user_actor_reaper_sweep(&state).await;
        }
    });
}

pub(in crate::server::session_janitors) async fn run_user_actor_reaper_sweep(
    state: &WebSocketState,
) {
    async {
        let counts = super::sweep_empty_user_actors_once(state).await;
        if counts.reaped > 0 {
            info!(
                examined = counts.examined,
                reaped = counts.reaped,
                remaining = counts.remaining,
                "user actor reaper: reaped empty UserActors"
            );
        } else {
            debug!(
                examined = counts.examined,
                remaining = counts.remaining,
                "user actor reaper: no empty UserActors"
            );
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::UserActorReaper,
            counts.outcome(),
        );
    }
    .instrument(janitor_sweep_span(Janitor::UserActorReaper))
    .await;
}

/// Interval for the remote-MUC-membership reconciliation janitor
/// (#1249). The 30s scan observes retained membership work; each snapshot
/// gates actual cleanup with exponential backoff. After five failures it
/// moves to 15-minute reconciliation, retaining provenance until recovery.
#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) const REMOTE_MUC_MEMBERSHIP_RECONCILE_INTERVAL: Duration =
    Duration::from_secs(30);

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) fn remote_muc_sweep_outcome(
    had_failure: bool,
) -> SweepOutcome {
    if had_failure {
        SweepOutcome::Failed
    } else {
        SweepOutcome::Completed
    }
}

/// Collect the occupants whose remote MUC memberships need a cleanup
/// re-drive (#1249): an ACTIVE membership entry whose occupancy generation
/// has no live connection-registry entry here and no resumable
/// XEP-0198 session anywhere (this node's memory OR the shared durable
/// store). A replacement sharing the full JID only protects its own
/// generation. Such an entry can only be the residue
/// of a failed (or missed) disconnect cleanup — the join path records
/// memberships strictly while the connection is registered, and both
/// graceful-leave and successful cleanup forget them.
///
/// A detached-but-resumable session keeps its occupancy on purpose
/// (XEP-0198 resume re-attaches to the same room state), so it is NOT a
/// candidate; SM expiry runs its own cleanup pass which restores the
/// membership on failure and thereby feeds this janitor.
#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) struct RemoteMucReconcileCandidates {
    pub(in crate::server::session_janitors) occupants:
        Vec<(jid::FullJid, waddle_xmpp_core::OccupancySessionGeneration)>,
    pub(in crate::server::session_janitors) had_failure: bool,
}

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) async fn collect_remote_muc_reconcile_candidates(
    state: &WebSocketState,
) -> RemoteMucReconcileCandidates {
    let mut candidates = Vec::new();
    let mut had_failure = false;
    for occupant in state
        .deps
        .protocol
        .remote_muc_memberships
        .occupants_with_active_memberships()
    {
        // A live same-full-JID replacement owns ITS memberships; snapshots
        // recorded by other generations are still owed a redrive (#1703).
        let live = state
            .deps
            .protocol
            .connection_registry
            .occupancy_session_of(&occupant);
        let foreign_snapshots = state
            .deps
            .protocol
            .remote_muc_memberships
            .occupancy_sessions_for_occupant_below(
                &occupant,
                u64::MAX,
                live.map_or(
                    MembershipGenerationFilter::Any,
                    MembershipGenerationFilter::Except,
                ),
            );
        if foreign_snapshots.is_empty() {
            continue;
        }
        // Exempt only the matching occupancy generation. A detached
        // replacement does not own a predecessor's retained memberships.
        let generations: HashSet<_> = foreign_snapshots.into_values().collect();
        for generation in generations {
            match state
                .deps
                .protocol
                .sm_session_registry
                .probe_resumable_session_for_occupancy(&occupant, generation)
                .await
            {
                waddle_xmpp::stream_management::ResumableSessionProbe::Present => {}
                waddle_xmpp::stream_management::ResumableSessionProbe::Absent => {
                    candidates.push((occupant.clone(), generation));
                }
                waddle_xmpp::stream_management::ResumableSessionProbe::Failed => {
                    had_failure = true;
                }
            }
        }
    }
    RemoteMucReconcileCandidates {
        occupants: candidates,
        had_failure,
    }
}

/// #1249: periodically re-drive remote MUC unavailable relays whose
/// disconnect-time attempt failed (remote node unreachable, claim
/// lookup failure, origin `UserActor` claim held by another node).
/// `cleanup_remote_muc_presence` restores the membership snapshot on
/// every failed relay together with its retry budget. This janitor checks
/// liveness on every sweep and re-drives only when backoff permits, making
/// cross-node occupancy cleanup convergent instead of
/// one-shot (the root cause of the recurring production error
/// `failed to relay remote MUC unavailable during disconnect cleanup`).
#[cfg(feature = "clustering")]
pub(crate) fn spawn_remote_muc_membership_reconciler(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REMOTE_MUC_MEMBERSHIP_RECONCILE_INTERVAL);
        // A sweep slower than the interval (serial relays against an
        // unreachable node) must not burst-fire missed ticks into a
        // continuous retry loop (race review P2 on PR #1277).
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            run_remote_muc_membership_sweep(&state).await;
        }
    });
}

#[cfg(feature = "clustering")]
pub(in crate::server::session_janitors) async fn run_remote_muc_membership_sweep(
    state: &WebSocketState,
) {
    async {
        let candidates = collect_remote_muc_reconcile_candidates(state).await;
        if candidates.occupants.is_empty() {
            waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                Janitor::RemoteMucMembership,
                remote_muc_sweep_outcome(candidates.had_failure),
            );
            return;
        }
        debug!(
            candidates = candidates.occupants.len(),
            "remote MUC reconciler: checking retained cleanup for departed occupants"
        );
        let mut sweep_failed = candidates.had_failure;
        for (occupant, generation) in candidates.occupants {
            // Re-check liveness IMMEDIATELY before the re-drive
            // (codex review P1 on PR #1277): the candidate list was
            // collected before earlier awaited re-drives, and the
            // same full JID may have resumed in that gap. A
            // registered connection with the selected generation means
            // the occupancy is legitimate again; the membership-generation guards
            // inside the cleanup protect the map but cannot undo a
            // relayed remote leave.
            let live = state
                .deps
                .protocol
                .connection_registry
                .occupancy_session_of(&occupant);
            if live == Some(generation) {
                continue;
            }
            // Earlier redrives can await network I/O. Recheck SM liveness as
            // well before acting on the selected generation.
            match state
                .deps
                .protocol
                .sm_session_registry
                .probe_resumable_session_for_occupancy(&occupant, generation)
                .await
            {
                waddle_xmpp::stream_management::ResumableSessionProbe::Present => continue,
                waddle_xmpp::stream_management::ResumableSessionProbe::Failed => {
                    sweep_failed = true;
                    continue;
                }
                waddle_xmpp::stream_management::ResumableSessionProbe::Absent => {}
            }
            if routes::websocket::redrive_remote_muc_cleanup(
                state,
                &occupant,
                routes::websocket::MembershipGenerationFilter::Only(generation),
            )
            .await
                == routes::websocket::MucCleanupOutcome::Failed
            {
                sweep_failed = true;
            }
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::RemoteMucMembership,
            remote_muc_sweep_outcome(sweep_failed),
        );
    }
    .instrument(janitor_sweep_span(Janitor::RemoteMucMembership))
    .await;
}
