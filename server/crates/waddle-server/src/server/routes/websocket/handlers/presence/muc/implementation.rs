//! Implementation behind the parent module facade.

use super::get_room_actor_result;
#[cfg(feature = "clustering")]
use super::FullJidDeliveryOutcome;
use super::GetSnapshot;
use super::HashSet;
use super::InFlightLease;
use super::LeaveAskFailure;
use super::LocalDepartureItem;
#[cfg(feature = "clustering")]
use super::MucProxyOrigin;
use super::NotificationPresenceShow;
#[cfg(feature = "clustering")]
use super::OrderedRelayMucProxyKind;
#[cfg(feature = "clustering")]
use super::OrderedRelayMucProxyOutcome;
use super::OrderedRelayRouteOrigin;
#[cfg(feature = "clustering")]
use super::RegisteredRemoteWriteAcceptedDelivery;
use super::ResolvedPrincipal;
use super::ResolverAffiliationSyncSchedule;
use super::ResolverAffiliationSyncScheduler;
use super::ResolverAffiliationSyncTerminalDisposition;
use super::ResolverAffiliationSyncWork;
use super::ResolverAffiliationSyncWorker;
use super::RoomRegistry;
use super::{
    build_muc_conflict_presence_xml, build_muc_join_presence_stanza, build_muc_self_unavailable_xml,
};
use super::{build_muc_join_presence_xml, build_muc_presence_error_xml, MucJoinPresence};
use super::{
    build_subject_message, debug, get_or_create_room_actor, get_room_actor, info, stanza_to_xml,
    warn, Affiliation, BareJid, ErrorType, FullJid, JoinAffiliationGrant, JoinWithAffiliation,
    LeaveByRealJid, Permission, RoomConfig, Session, Stanza, StanzaError, WebSocketState,
    XmppChannelRecord,
};
use super::{get_managed_channel_for_room, parse_room_jid_context};
use super::{resolve_managed_channel_affiliation, server_permission_allowed};

#[cfg(any(test, feature = "clustering"))]
pub async fn handle_muc_join(
    state: &WebSocketState,
    domain: &str,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    presence_show: Option<crate::notification_activity::NotificationPresenceShow>,
    connection: MucJoinConnectionContext<'_>,
) -> Vec<String> {
    handle_muc_join_with_ordered_relay(
        state,
        MucJoinRequest {
            domain,
            room_jid,
            sender_jid,
            nick,
            presence_show,
            occupancy_session: connection.occupancy_session,
            authenticated_session: connection.authenticated_session,
            registry_owner: connection.registry_owner,
            ordered_relay_origin: None,
        },
    )
    .await
}

/// Whether the joining connection still owns its registry slot at the commit
/// boundary. A connection that lost the slot during the join's awaits is
/// superseded: committing now would install it over the replacement (#1703).
pub(in crate::server::routes::websocket::handlers::presence::muc) fn join_connection_still_owns_slot(
    state: &WebSocketState,
    sender_jid: &FullJid,
    registry_owner: Option<&waddle_xmpp::registry::LocalConnectionBindingId>,
) -> bool {
    registry_owner.is_none_or(|owner| {
        state
            .deps
            .protocol
            .connection_registry
            .is_owned_by(sender_jid, owner)
    })
}

#[cfg(any(test, feature = "clustering"))]
pub struct MucJoinConnectionContext<'a> {
    pub occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
    pub authenticated_session: Option<&'a Session>,
    /// The connection's registry ownership token, re-validated right
    /// before the join is committed (locally or relayed): a connection that
    /// lost its slot during the join's awaits must not install itself over
    /// the replacement (#1703). `None` for server-originated joins.
    pub registry_owner: Option<&'a waddle_xmpp::registry::LocalConnectionBindingId>,
}

pub struct MucJoinRequest<'a> {
    pub domain: &'a str,
    pub room_jid: &'a BareJid,
    pub sender_jid: &'a FullJid,
    pub nick: &'a str,
    pub presence_show: Option<NotificationPresenceShow>,
    pub occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
    pub authenticated_session: Option<&'a Session>,
    pub registry_owner: Option<&'a waddle_xmpp::registry::LocalConnectionBindingId>,
    pub ordered_relay_origin: Option<OrderedRelayRouteOrigin>,
}

pub(in crate::server::routes::websocket::handlers::presence::muc) struct MucJoinWork<'a> {
    pub(in crate::server::routes::websocket::handlers::presence::muc) domain: String,
    pub(in crate::server::routes::websocket::handlers::presence::muc) room_jid: &'a BareJid,
    pub(in crate::server::routes::websocket::handlers::presence::muc) sender_jid: &'a FullJid,
    pub(in crate::server::routes::websocket::handlers::presence::muc) nick: String,
    pub(in crate::server::routes::websocket::handlers::presence::muc) presence_show:
        Option<NotificationPresenceShow>,
    pub(in crate::server::routes::websocket::handlers::presence::muc) occupancy_session:
        waddle_xmpp_core::OccupancySessionGeneration,
    pub(in crate::server::routes::websocket::handlers::presence::muc) registry_owner:
        Option<&'a waddle_xmpp::registry::LocalConnectionBindingId>,
    pub(in crate::server::routes::websocket::handlers::presence::muc) authenticated_session:
        Option<&'a Session>,
    pub(in crate::server::routes::websocket::handlers::presence::muc) ordered_relay_origin:
        Option<OrderedRelayRouteOrigin>,
}

pub async fn handle_muc_join_with_ordered_relay(
    state: &WebSocketState,
    request: MucJoinRequest<'_>,
) -> Vec<String> {
    let _request_timer = waddle_xmpp::metrics::MucOccupancyHandlerTimer::start("join_request");
    info!(
        room = %request.room_jid,
        nick = %request.nick,
        user = %request.sender_jid.to_bare(),
        "MUC join request"
    );

    handle_muc_join_unlocked(
        state,
        MucJoinWork {
            domain: request.domain.to_string(),
            room_jid: request.room_jid,
            sender_jid: request.sender_jid,
            nick: request.nick.to_string(),
            presence_show: request.presence_show,
            occupancy_session: request.occupancy_session,
            registry_owner: request.registry_owner,
            authenticated_session: request.authenticated_session,
            ordered_relay_origin: request.ordered_relay_origin,
        },
    )
    .await
}

/// Best-effort resolver-affiliation sync into an EXISTING live room
/// actor when join admission rejects before any actor message (review
/// F3). Never creates an actor — a rejection must not spawn rooms —
/// and never blocks the rejection on failure: the sync is a staleness
/// repair, the authoritative admission decision was already made by
/// the resolver. The actor-side handler is provenance-aware
/// (`update_affiliation_from_resolver`), so explicit grants survive.
/// The sync shares the `admission_revision` the rejection decision was
/// computed against; the actor refuses it if any admission/affiliation
/// change (e.g. the re-granted user's successful join) landed in
/// between, so a delayed sync can never clear a live occupant's fresh
/// affiliation.
pub(in crate::server::routes::websocket::handlers::presence::muc) const RESOLVER_SYNC_MAX_ATTEMPTS: usize = 3;
pub(in crate::server::routes::websocket::handlers::presence::muc) const RESOLVER_SYNC_MAILBOX_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
pub(in crate::server::routes::websocket::handlers::presence::muc) const RESOLVER_SYNC_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);

pub(in crate::server::routes::websocket::handlers::presence::muc) fn sync_resolver_affiliation_on_rejection_with_registry(
    existing_room_actor: Option<&kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>>,
    scheduler: &std::sync::Arc<ResolverAffiliationSyncScheduler>,
    room_registry: Option<RoomRegistry>,
    room_jid: &BareJid,
    jid: BareJid,
    affiliation: Affiliation,
    expected_admission_revision: u64,
) {
    let Some(actor) = existing_room_actor else {
        return;
    };
    let work = ResolverAffiliationSyncWork {
        affiliation,
        expected_admission_revision,
    };
    let worker = match scheduler.schedule(room_jid, &jid, actor.id(), work) {
        crate::server::routes::websocket::ResolverAffiliationSyncSchedule::Started(worker) => {
            worker
        }
        ResolverAffiliationSyncSchedule::Updated => {
            debug!(
                room = %room_jid,
                %jid,
                "Updated the in-flight resolver affiliation repair with a newer verdict"
            );
            return;
        }
        ResolverAffiliationSyncSchedule::Coalesced => {
            debug!(
                room = %room_jid,
                %jid,
                "Coalesced an identical resolver affiliation repair"
            );
            return;
        }
        ResolverAffiliationSyncSchedule::Stale => {
            debug!(
                room = %room_jid,
                %jid,
                "Ignored a stale resolver affiliation repair because a newer revision is queued"
            );
            return;
        }
        ResolverAffiliationSyncSchedule::AtCapacity => {
            debug!(
                room = %room_jid,
                %jid,
                "Skipped resolver affiliation repair because the bounded scheduler is full"
            );
            return;
        }
    };
    let actor = actor.clone();
    let room_jid = room_jid.clone();
    // This repair must not extend the rejected stanza path. The cloned
    // ActorRef also guarantees retries target the same existing incarnation:
    // they never consult the registry and therefore cannot create a room.
    // Reusing the captured revision makes every delayed attempt harmless once
    // a newer admission or affiliation mutation has landed.
    tokio::spawn(run_resolver_affiliation_sync_worker(
        actor,
        room_registry,
        room_jid,
        jid,
        *worker,
    ));
}

pub(in crate::server::routes::websocket::handlers::presence::muc) async fn run_resolver_affiliation_sync_worker(
    actor: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    room_registry: Option<RoomRegistry>,
    room_jid: BareJid,
    jid: BareJid,
    mut worker: ResolverAffiliationSyncWorker,
) {
    let mut work = worker.current();
    let mut effective_admission_revision = worker.effective_admission_revision();
    let mut attempt = 1usize;
    loop {
        let result = actor
            .ask(waddle_xmpp::muc::room_actor::SyncResolverAffiliation {
                jid: jid.clone(),
                affiliation: work.affiliation,
                expected_admission_revision: effective_admission_revision,
            })
            // Bound work that has not reached the actor. Once delivered, keep
            // the scheduler guard until the handler actually completes so a
            // slow ownership fence cannot accumulate duplicate mailbox work.
            .mailbox_timeout(RESOLVER_SYNC_MAILBOX_TIMEOUT)
            .await;
        match result {
            Ok(waddle_xmpp::muc::room_actor::ResolverAffiliationSyncOutcome::Applied {
                admission_revision,
            }) => {
                let Some(next) = worker.finish_applied_or_take_update(admission_revision) else {
                    return;
                };
                work = next;
                effective_admission_revision = worker.effective_admission_revision();
                attempt = 1;
                continue;
            }
            Ok(
                waddle_xmpp::muc::room_actor::ResolverAffiliationSyncOutcome::OwnershipUnavailable,
            ) if attempt < RESOLVER_SYNC_MAX_ATTEMPTS => {}
            Ok(outcome) => {
                // Stale revision, sealing, and invite compensation are
                // terminal for this captured rejection decision. A final
                // OwnershipUnavailable is also bounded here rather than
                // keeping a detached task alive indefinitely.
                debug!(
                    room = %room_jid,
                    attempt,
                    outcome = ?outcome,
                    "Skipped resolver affiliation sync on rejected join"
                );
                if outcome
                    == waddle_xmpp::muc::room_actor::ResolverAffiliationSyncOutcome::RoomSealed
                {
                    // This actor incarnation is permanently unable to accept
                    // any queued repair. Release the bounded scheduler slot
                    // before the registry ask, whose mailbox/reply timeout is
                    // intentionally much longer than the repair fast path.
                    worker.close_actor_terminal();
                    if let Some(registry) = room_registry.as_ref() {
                        match registry.reap_sealed_room(room_jid.clone()).await {
                            Ok(true) => debug!(
                                room = %room_jid,
                                "Reaped deposed room after rejected-join affiliation repair"
                            ),
                            Ok(false) => debug!(
                                room = %room_jid,
                                "Rejected-join repair observed a room already absent or replaced"
                            ),
                            Err(error) => warn!(
                                room = %room_jid,
                                %error,
                                "Failed to reap deposed room after rejected-join affiliation repair"
                            ),
                        }
                    }
                    return;
                }
                let disposition = if outcome
                    == waddle_xmpp::muc::room_actor::ResolverAffiliationSyncOutcome::OwnershipUnavailable
                {
                    ResolverAffiliationSyncTerminalDisposition::NonMutatingExhaustion
                } else {
                    ResolverAffiliationSyncTerminalDisposition::InvalidatingOutcome
                };
                let Some(next) = worker.finish_terminal_or_take_update(disposition) else {
                    return;
                };
                work = next;
                effective_admission_revision = worker.effective_admission_revision();
                attempt = 1;
                continue;
            }
            Err(
                error @ (kameo::error::SendError::MailboxFull(_)
                | kameo::error::SendError::Timeout(Some(_))),
            ) if attempt < RESOLVER_SYNC_MAX_ATTEMPTS => {
                debug!(
                    room = %room_jid,
                    attempt,
                    error = ?error,
                    "Resolver affiliation sync was not delivered; retrying"
                );
            }
            Err(
                error @ (kameo::error::SendError::MailboxFull(_)
                | kameo::error::SendError::Timeout(Some(_))),
            ) => {
                warn!(
                    room = %room_jid,
                    attempt,
                    error = ?error,
                    "Resolver affiliation sync retries exhausted"
                );
                let Some(next) = worker.finish_terminal_or_take_update(
                    ResolverAffiliationSyncTerminalDisposition::NonMutatingExhaustion,
                ) else {
                    return;
                };
                work = next;
                effective_admission_revision = worker.effective_admission_revision();
                attempt = 1;
                continue;
            }
            Err(error) => {
                warn!(
                    room = %room_jid,
                    attempt,
                    error = ?error,
                    "Resolver affiliation sync retries exhausted"
                );
                let Some(next) = worker.finish_terminal_or_take_update(
                    ResolverAffiliationSyncTerminalDisposition::InvalidatingOutcome,
                ) else {
                    return;
                };
                work = next;
                effective_admission_revision = worker.effective_admission_revision();
                attempt = 1;
                continue;
            }
        }
        tokio::time::sleep(RESOLVER_SYNC_RETRY_BACKOFF).await;
        if let Some(next) = worker.take_update() {
            work = next;
            effective_admission_revision = worker.effective_admission_revision();
            attempt = 1;
        } else {
            attempt += 1;
        }
    }
}

pub(crate) async fn route_room_presence_to_occupant(
    state: &WebSocketState,
    room_jid: &BareJid,
    recipient: &FullJid,
    stanza: Stanza,
) {
    if super::try_deliver_registered_remote_resource(state, recipient, &stanza).await {
        return;
    }
    let delivered = deliver_room_presence_locally(state, room_jid, recipient, &stanza);
    #[cfg(not(feature = "clustering"))]
    let _ = delivered;
    #[cfg(feature = "clustering")]
    if !delivered {
        let deps =
            crate::server::routes::websocket::interpret_loop::build_interpret_deps(state, None)
                .with_ordered_relay_origin(Some(OrderedRelayRouteOrigin::room(room_jid)));
        let replies = crate::server::routes::interpret::route_to_connection(
            &deps,
            jid::Jid::from(recipient.clone()),
            Box::new(stanza),
            0,
            None,
        )
        .await;
        if !replies.is_empty() {
            warn!(
                room = %room_jid,
                recipient = %recipient,
                reply_count = replies.len(),
                "MUC presence fan-out produced unexpected route fallback replies"
            );
        }
    }
}

/// Local socket delivery of a room presence; `false` when no local socket
/// takes it.
///
/// #1263: `DroppedFull` was previously treated as delivered, so a
/// client whose channel was momentarily full silently missed a room
/// presence and kept a stale occupant roster forever. The frame is
/// provably never enqueued on `DroppedFull`, so retry ONCE
/// immediately — but never sleep: this helper sits inside the
/// sequential join/leave broadcast loops whose non-blocking contract
/// is load-bearing (a zombied consumer must not stall the join path,
/// or "Timed out waiting for self-presence" cascades return; SM
/// review on PR #1277). A persistently full channel surfaces the
/// loss (metric + warn) instead of reporting success — the
/// recipient's roster is stale until its next rejoin/resync, and a
/// genuinely wedged consumer is torn down by the send-stall
/// backstop, whose disconnect cleanup re-syncs occupancy.
pub(crate) fn deliver_room_presence_locally(
    state: &WebSocketState,
    room_jid: &BareJid,
    recipient: &FullJid,
    stanza: &Stanza,
) -> bool {
    let mut retried = false;
    loop {
        match state
            .deps
            .protocol
            .connection_registry
            .try_send_to(recipient, stanza.clone())
        {
            waddle_xmpp::registry::BroadcastOutcome::Delivered => return true,
            waddle_xmpp::registry::BroadcastOutcome::DroppedFull => {
                if !retried {
                    retried = true;
                    continue;
                }
                waddle_xmpp::telemetry::reliability::increment_delivery_retry_exhausted_drop();
                warn!(
                    room = %room_jid,
                    recipient = %recipient,
                    "MUC presence fan-out: recipient channel full; dropped — \
                     occupant roster stale until resync"
                );
                return true;
            }
            waddle_xmpp::registry::BroadcastOutcome::NotConnected
            | waddle_xmpp::registry::BroadcastOutcome::DroppedClosed => return false,
        }
    }
}

/// Ordered-relay handoff for ordinary generated recipient-addressed MUC
/// frames. Durable room effects use the write-accepted sibling below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegisteredRemoteDelivery {
    #[cfg(feature = "clustering")]
    Delivered,
    Absent,
    #[cfg(feature = "clustering")]
    Retryable,
}

pub(crate) async fn registered_remote_resource_delivery(
    state: &WebSocketState,
    target: &FullJid,
    stanza: &Stanza,
) -> RegisteredRemoteDelivery {
    #[cfg(feature = "clustering")]
    {
        let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        else {
            return RegisteredRemoteDelivery::Absent;
        };
        match bridge
            .try_deliver_registered_remote_resource(
                target,
                stanza,
                waddle_xmpp::registry::DeliveryKind::DirectFrame,
                None,
            )
            .await
        {
            Some(FullJidDeliveryOutcome::Delivered) => RegisteredRemoteDelivery::Delivered,
            Some(FullJidDeliveryOutcome::Dropped | FullJidDeliveryOutcome::MaybeCommitted) => {
                RegisteredRemoteDelivery::Retryable
            }
            Some(FullJidDeliveryOutcome::Unavailable | FullJidDeliveryOutcome::QueuedDetached)
            | None => RegisteredRemoteDelivery::Absent,
        }
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (state, target, stanza);
        RegisteredRemoteDelivery::Absent
    }
}

pub(crate) async fn registered_remote_resource_write_accepted_delivery(
    state: &WebSocketState,
    target: &FullJid,
    stanza: &Stanza,
) -> RegisteredRemoteDelivery {
    #[cfg(feature = "clustering")]
    {
        let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        else {
            return RegisteredRemoteDelivery::Absent;
        };
        match bridge
            .try_deliver_registered_remote_resource_write_accepted(target, stanza)
            .await
        {
            RegisteredRemoteWriteAcceptedDelivery::Delivered => RegisteredRemoteDelivery::Delivered,
            RegisteredRemoteWriteAcceptedDelivery::Absent => RegisteredRemoteDelivery::Absent,
            RegisteredRemoteWriteAcceptedDelivery::Retryable
            | RegisteredRemoteWriteAcceptedDelivery::RefreshNeeded => {
                RegisteredRemoteDelivery::Retryable
            }
        }
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (state, target, stanza);
        RegisteredRemoteDelivery::Absent
    }
}

pub(crate) async fn try_deliver_registered_remote_resource(
    state: &WebSocketState,
    target: &FullJid,
    stanza: &Stanza,
) -> bool {
    !matches!(
        super::registered_remote_resource_delivery(state, target, stanza).await,
        RegisteredRemoteDelivery::Absent
    )
}

#[cfg(feature = "clustering")]
pub(in crate::server::routes::websocket::handlers::presence::muc) enum RemoteMucJoinDecision {
    Delivered(Vec<Stanza>),
    MaybeCommitted,
}

#[cfg(feature = "clustering")]
pub(in crate::server::routes::websocket::handlers::presence::muc) enum RemoteMucLeaveDecision {
    Delivered(Vec<Stanza>),
    MaybeCommitted,
    RetryableNoEffect,
    LocalFallback,
}

#[cfg(feature = "clustering")]
pub(in crate::server::routes::websocket::handlers::presence::muc) fn remote_muc_join_decision(
    outcome: Option<OrderedRelayMucProxyOutcome>,
) -> Option<RemoteMucJoinDecision> {
    match outcome {
        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(replies)) => {
            Some(RemoteMucJoinDecision::Delivered(replies))
        }
        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::PendingFrames {
            ..
        })
        | Some(OrderedRelayMucProxyOutcome::MaybeCommitted)
        | Some(OrderedRelayMucProxyOutcome::JoinMaybeCommitted) => {
            Some(RemoteMucJoinDecision::MaybeCommitted)
        }
        Some(OrderedRelayMucProxyOutcome::Unavailable)
        | Some(OrderedRelayMucProxyOutcome::Dropped)
        | None => None,
    }
}

#[cfg(feature = "clustering")]
pub(in crate::server::routes::websocket::handlers::presence::muc) fn remote_muc_leave_decision(
    outcome: Option<OrderedRelayMucProxyOutcome>,
) -> RemoteMucLeaveDecision {
    match outcome {
        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(replies)) => {
            RemoteMucLeaveDecision::Delivered(replies)
        }
        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::PendingFrames {
            ..
        })
        | Some(OrderedRelayMucProxyOutcome::MaybeCommitted)
        | Some(OrderedRelayMucProxyOutcome::JoinMaybeCommitted) => {
            RemoteMucLeaveDecision::MaybeCommitted
        }
        Some(OrderedRelayMucProxyOutcome::Unavailable)
        | Some(OrderedRelayMucProxyOutcome::Dropped) => RemoteMucLeaveDecision::RetryableNoEffect,
        None => RemoteMucLeaveDecision::LocalFallback,
    }
}

/// What the managed-channel affiliation/membership resolver reported
/// for a denied join, recorded on the admission-denial log (#1315).
/// This is diagnostic context on the log only — it is never a metric
/// attribute (the counter keys on the stanza error condition alone).
#[derive(Debug, Clone, Copy)]
pub(in crate::server::routes::websocket::handlers::presence::muc) enum ManagedAdmissionResolverOutcome
{
    /// Managed-channel lookup failed before the affiliation resolver
    /// could run.
    ManagedChannelLookupError,
    /// Admission failed before the affiliation resolver was consulted.
    NotConsulted,
    /// The join carried no authenticated session, so the resolver was
    /// never consulted.
    SessionMissing,
    /// The authenticated session's JID failed to parse, so the
    /// resolver was never consulted.
    SessionJidMalformed,
    /// The resolver reported the joiner is banned (outcast).
    Banned,
    /// The resolver reported no channel affiliation for the joiner in a
    /// members-only channel.
    NoAffiliation,
    /// The resolver failed to produce an affiliation (backend error).
    ResolverError,
    /// The resolver produced an affiliation before a later admission
    /// step failed.
    Resolved(Affiliation),
}

impl ManagedAdmissionResolverOutcome {
    pub(in crate::server::routes::websocket::handlers::presence::muc) fn as_str(
        self,
    ) -> &'static str {
        match self {
            Self::ManagedChannelLookupError => "managed-channel-lookup-error",
            Self::NotConsulted => "not-consulted",
            Self::SessionMissing => "session-missing",
            Self::SessionJidMalformed => "session-jid-malformed",
            Self::Banned => "banned",
            Self::NoAffiliation => "no-affiliation",
            Self::ResolverError => "resolver-error",
            Self::Resolved(Affiliation::Outcast) => "outcast",
            Self::Resolved(Affiliation::None) => "none",
            Self::Resolved(Affiliation::Member) => "member",
            Self::Resolved(Affiliation::Admin) => "admin",
            Self::Resolved(Affiliation::Owner) => "owner",
        }
    }
}

/// The typed descriptor of a single join-admission denial: the
/// XEP-0045 §7.2 stanza error it maps to, the concrete refusal site,
/// and the resolver outcome recorded when the room is a managed
/// channel.
pub(in crate::server::routes::websocket::handlers::presence::muc) struct JoinAdmissionDenial {
    /// The RFC 6120 §8.3.3 stanza error condition the counter keys on.
    pub(in crate::server::routes::websocket::handlers::presence::muc) condition:
        waddle_xmpp::telemetry::attributes::StanzaErrorCondition,
    /// Which refusal site produced the denial (#1440). Several very
    /// different wait-type faults share `resource-constraint`, so the
    /// condition alone cannot tell them apart.
    pub(in crate::server::routes::websocket::handlers::presence::muc) deny_reason:
        waddle_xmpp::telemetry::attributes::MucJoinDenyReason,
    /// The `<error type=.../>` class carried on the presence error.
    pub(in crate::server::routes::websocket::handlers::presence::muc) error_type: ErrorType,
    /// What the affiliation/membership resolver reported.
    pub(in crate::server::routes::websocket::handlers::presence::muc) resolver_outcome:
        ManagedAdmissionResolverOutcome,
    /// Human-facing `<text/>` on the presence error.
    pub(in crate::server::routes::websocket::handlers::presence::muc) message: &'static str,
}

pub(in crate::server::routes::websocket::handlers::presence::muc) fn record_stanza_error_condition(
    condition: waddle_xmpp::telemetry::attributes::StanzaErrorCondition,
) {
    // The dispatch span declares this bounded field; recording it here keeps
    // rejection taxonomy queryable without promoting protocol denials to
    // failed operations.
    tracing::Span::current().record("condition", condition.as_str());
}

/// The span-error description for denials that are server-side
/// failures rather than protocol policy.
///
/// `internal-server-error` is failure by definition; the wait-type
/// infrastructure faults (#1440) are recoverable for the client but
/// still failed operations for us, so they keep the ERROR span status
/// they had before they were routed through the denial choke point.
/// Everything else — bans, missing membership, a full room — is
/// successful protocol handling and leaves the span UNSET.
pub(in crate::server::routes::websocket::handlers::presence::muc) fn internal_join_failure_description(
    denial: &JoinAdmissionDenial,
) -> Option<&'static str> {
    use waddle_xmpp::telemetry::attributes::MucJoinDenyReason;
    match denial.deny_reason {
        MucJoinDenyReason::DurableRestorePending => Some("MUC durable restore remained pending"),
        MucJoinDenyReason::OwnershipUnavailable => Some("MUC ownership reconciliation unavailable"),
        _ => matches!(
            denial.condition,
            waddle_xmpp::telemetry::attributes::StanzaErrorCondition::InternalServerError
        )
        .then_some("MUC join admission failed internally"),
    }
}

/// The leave-side twin of the join denial choke point (#1440): a leave
/// bounced because the owning node is unreachable was equally silent
/// server-side. It is not an admission decision, so it stays off the
/// `waddle.muc.admission.denied` counter and only records the
/// disposition log, keyed the same way as a join denial.
pub(in crate::server::routes::websocket::handlers::presence::muc) fn bounce_muc_leave_ownership_unreachable(
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
) -> Vec<String> {
    let condition = waddle_xmpp::telemetry::attributes::StanzaErrorCondition::ResourceConstraint;
    record_stanza_error_condition(condition);
    info!(
        room = %room_jid,
        user = %sender_jid.to_bare(),
        nick = %nick,
        condition = condition.as_str(),
        "MUC leave bounced: room ownership unreachable"
    );
    vec![build_muc_presence_error_xml(
        room_jid,
        nick,
        sender_jid,
        StanzaError::new(
            ErrorType::Wait,
            condition.to_xmpp(),
            "en",
            "This room's ownership is currently unreachable; please retry.",
        ),
    )]
}

/// Central choke point for join denials (#1315, #1440).
///
/// Every join rejection routes through here so the denial is never
/// invisible: it emits one info-level structured log (bare room JID,
/// bare user JID, nick, condition, deny reason, managed flag, and
/// resolver outcome) and increments the
/// `waddle.muc.admission.denied` counter keyed by the stanza error
/// condition and the refusal site, then returns the XEP-0045 §7.2
/// presence-error frame unchanged. JIDs live on the log only — never
/// as a metric attribute.
pub(in crate::server::routes::websocket::handlers::presence::muc) fn deny_join_admission(
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    managed_channel_confirmed: bool,
    denial: JoinAdmissionDenial,
) -> Vec<String> {
    record_join_admission_denial(
        room_jid,
        sender_jid,
        nick,
        managed_channel_confirmed,
        &denial,
    );
    vec![build_muc_presence_error_xml(
        room_jid,
        nick,
        sender_jid,
        StanzaError::new(
            denial.error_type,
            denial.condition.to_xmpp(),
            "en",
            denial.message,
        ),
    )]
}

/// The telemetry half of [`deny_join_admission`], for the denial sites
/// whose wire frame is not the plain presence error the choke point
/// builds (e.g. the nick-collision `<conflict/>` frame with its status
/// codes) — those record here and return their own frame, so the
/// counter and disposition log still cover every join denial (#1440).
pub(in crate::server::routes::websocket::handlers::presence::muc) fn record_join_admission_denial(
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    managed_channel_confirmed: bool,
    denial: &JoinAdmissionDenial,
) {
    record_stanza_error_condition(denial.condition);
    if let Some(description) = internal_join_failure_description(denial) {
        crate::telemetry::mark_span_error(description);
    }
    info!(
        room = %room_jid,
        user = %sender_jid.to_bare(),
        nick = %nick,
        condition = denial.condition.as_str(),
        deny_reason = waddle_xmpp::telemetry::attributes::MetricAttribute::value(
            &denial.deny_reason
        ),
        managed_channel = managed_channel_confirmed,
        resolver_outcome = denial.resolver_outcome.as_str(),
        "MUC join admission denied"
    );
    waddle_xmpp::counter_add!(
        "waddle.muc.admission.denied",
        "1",
        "MUC join admission denials by stanza error condition and refusal site.",
        1,
        denial.condition,
        denial.deny_reason,
    );
}

pub(in crate::server::routes::websocket::handlers::presence::muc) async fn handle_muc_join_unlocked(
    state: &WebSocketState,
    request: MucJoinWork<'_>,
) -> Vec<String> {
    let MucJoinWork {
        domain,
        room_jid,
        sender_jid,
        nick,
        presence_show,
        occupancy_session,
        authenticated_session,
        registry_owner,
        ordered_relay_origin,
    } = request;
    #[cfg(not(feature = "clustering"))]
    let _ = &ordered_relay_origin;

    // Resolver-derived first joins bump the admission revision, so a
    // burst of concurrent first-time joiners can hit several stale
    // revisions in a row — allow a few re-snapshots before giving up
    // (each retry re-reads the current revision; convergence is
    // guaranteed once admissions quiesce).
    // 10 bounds a pathological revision-churn loop while making spurious
    // failure implausible for realistic bursts: each retry re-snapshots the
    // CURRENT revision, so a retry only fails when yet another admission
    // landed inside that single snapshot-to-ask window.
    const MAX_STALE_ADMISSION_RETRIES: u32 = 10;
    let mut stale_admission_retries = 0u32;
    // #1108: a room actor can be sealed+destroyed by the guarded
    // dormancy eviction between our registry lookup and the join ask.
    // The seal refuses the join with a typed retryable error (or the
    // ask fails outright on the stopped actor); one retry re-runs the
    // registry lookup, which respawns the room — the join must never
    // be silently dropped.
    let mut retried_dead_room = false;
    loop {
        let managed_channel = match get_managed_channel_for_room(state, room_jid).await {
            Ok(channel) => channel,
            Err(error) => {
                warn!(room = %room_jid, error = %error, "Failed to resolve managed MUC channel");
                return deny_join_admission(
                    room_jid,
                    sender_jid,
                    &nick,
                    false,
                    JoinAdmissionDenial {
                        condition:
                            waddle_xmpp::telemetry::attributes::StanzaErrorCondition::InternalServerError,
                        deny_reason:
                            waddle_xmpp::telemetry::attributes::MucJoinDenyReason::ManagedChannelLookup,
                        error_type: ErrorType::Wait,
                        resolver_outcome:
                            ManagedAdmissionResolverOutcome::ManagedChannelLookupError,
                        message: "Failed to resolve managed channel for room.",
                    },
                );
            }
        };
        let (existing_room_actor, room_preparation_pending) =
            match get_room_actor_result(state, room_jid).await {
                Ok(actor) => (actor, false),
                Err(
                    waddle_xmpp::muc::room_registry_actor::RoomRegistryError::OwnershipReconciliationPending(_),
                ) => (None, true),
                Err(error) => {
                    warn!(room = %room_jid, %error, "Failed to look up MUC room before join");
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        managed_channel.is_some(),
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::InternalServerError,
                            error_type: ErrorType::Wait,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::RoomLookup,
                            resolver_outcome: ManagedAdmissionResolverOutcome::NotConsulted,
                            message: "Failed to look up room before join.",
                        },
                    );
                }
            };
        let existing_room_snapshot = if let Some(actor) = existing_room_actor.as_ref() {
            match actor.ask(GetSnapshot).await {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
                    if !retried_dead_room
                        && !matches!(&error, kameo::error::SendError::HandlerError(_))
                    {
                        // Room actor destroyed between lookup and
                        // snapshot (#1108) — retry via the registry.
                        retried_dead_room = true;
                        continue;
                    }
                    warn!(room = %room_jid, error = ?error, "Failed to snapshot MUC room before join");
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        managed_channel.is_some(),
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::InternalServerError,
                            error_type: ErrorType::Wait,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::RoomSnapshot,
                            resolver_outcome: ManagedAdmissionResolverOutcome::NotConsulted,
                            message: "Failed to snapshot room before join.",
                        },
                    );
                }
            }
        } else {
            None
        };
        let admission_revision = existing_room_snapshot
            .as_ref()
            .map(|snapshot| snapshot.admission_revision)
            .unwrap_or(0);
        let managed_affiliation = if let Some(channel) = managed_channel.as_ref() {
            let Some(session) = authenticated_session else {
                return deny_join_admission(
                    room_jid,
                    sender_jid,
                    &nick,
                    true,
                    JoinAdmissionDenial {
                        condition:
                            waddle_xmpp::telemetry::attributes::StanzaErrorCondition::NotAuthorized,
                        error_type: ErrorType::Auth,
                        deny_reason:
                            waddle_xmpp::telemetry::attributes::MucJoinDenyReason::SessionMissing,
                        resolver_outcome: ManagedAdmissionResolverOutcome::SessionMissing,
                        message: "Authentication required to join managed channel.",
                    },
                );
            };
            let admission_members_only = existing_room_snapshot
                .as_ref()
                .map(|snapshot| snapshot.room.config.members_only)
                .unwrap_or(channel.members_only);
            let Ok(session_bare) = session.user_jid.parse::<BareJid>() else {
                return deny_join_admission(
                    room_jid,
                    sender_jid,
                    &nick,
                    true,
                    JoinAdmissionDenial {
                        condition:
                            waddle_xmpp::telemetry::attributes::StanzaErrorCondition::InternalServerError,
                        error_type: ErrorType::Wait,
                        deny_reason:
                            waddle_xmpp::telemetry::attributes::MucJoinDenyReason::SessionIdentityMalformed,
                        resolver_outcome: ManagedAdmissionResolverOutcome::SessionJidMalformed,
                        message: "Failed to resolve managed-channel affiliation.",
                    },
                );
            };
            match resolve_managed_channel_affiliation(
                state,
                &session_bare,
                room_jid,
                &channel.id,
                admission_members_only,
                // Join admission repairs a stale Space→channel projection.
                true,
            )
            .await
            {
                Ok(Some(Affiliation::Outcast)) => {
                    // The resolver's Outcast comes from the permission
                    // graph (resolver-derived), so mirror it into a live
                    // actor the same way: a formerly-Member-now-Outcast
                    // user's stale resolver-derived Member entry would
                    // otherwise linger on the room's affiliation list
                    // until eviction. Explicit bans are untouched by the
                    // provenance-aware sync.
                    sync_resolver_affiliation_on_rejection_with_registry(
                        existing_room_actor.as_ref(),
                        &state.deps.protocol.resolver_affiliation_syncs,
                        Some(RoomRegistry::wrap(
                            state.deps.protocol.room_registry.clone(),
                        )),
                        room_jid,
                        // Room affiliations are keyed by the joiner's
                        // bare JID (`JoinWithAffiliation` uses
                        // `sender_jid.to_bare()`), so the sync must use
                        // the same key.
                        sender_jid.to_bare(),
                        Affiliation::Outcast,
                        admission_revision,
                    );
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        true,
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::Forbidden,
                            error_type: ErrorType::Auth,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::ChannelBan,
                            resolver_outcome: ManagedAdmissionResolverOutcome::Banned,
                            message: "Banned from managed channel.",
                        },
                    );
                }
                Ok(Some(affiliation)) => Some(affiliation),
                Ok(None) => {
                    if admission_members_only {
                        // The registration-required rejection returns
                        // BEFORE `JoinWithAffiliation`, so its
                        // `Resolver(None)` write never reaches a live
                        // actor — clear any stale resolver-derived
                        // affiliation from before the revocation here.
                        sync_resolver_affiliation_on_rejection_with_registry(
                            existing_room_actor.as_ref(),
                            &state.deps.protocol.resolver_affiliation_syncs,
                            Some(RoomRegistry::wrap(
                                state.deps.protocol.room_registry.clone(),
                            )),
                            room_jid,
                            // Same key as `JoinWithAffiliation`:
                            // `sender_jid.to_bare()`.
                            sender_jid.to_bare(),
                            Affiliation::None,
                            admission_revision,
                        );
                        return deny_join_admission(
                            room_jid,
                            sender_jid,
                            &nick,
                            true,
                            JoinAdmissionDenial {
                                condition:
                                    waddle_xmpp::telemetry::attributes::StanzaErrorCondition::RegistrationRequired,
                                error_type: ErrorType::Auth,
                                deny_reason:
                                    waddle_xmpp::telemetry::attributes::MucJoinDenyReason::MembershipRequired,
                                resolver_outcome: ManagedAdmissionResolverOutcome::NoAffiliation,
                                message: "Membership required to join managed channel.",
                            },
                        );
                    }
                    Some(Affiliation::None)
                }
                Err(()) => {
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        true,
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::InternalServerError,
                            error_type: ErrorType::Wait,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::AffiliationResolver,
                            resolver_outcome: ManagedAdmissionResolverOutcome::ResolverError,
                            message: "Failed to resolve managed-channel affiliation.",
                        },
                    );
                }
            }
        } else {
            None
        };
        let resolver_outcome = managed_affiliation
            .map(ManagedAdmissionResolverOutcome::Resolved)
            .unwrap_or(ManagedAdmissionResolverOutcome::NotConsulted);

        let (room_actor, created_instant_room) = match existing_room_actor {
            Some(actor) => (actor, false),
            None => {
                if managed_channel.is_none()
                    && !room_preparation_pending
                    && !server_permission_allowed(
                        state,
                        authenticated_session.map(ResolvedPrincipal::from_authenticated_session),
                        Permission::CreateMuc,
                    )
                    .await
                    .unwrap_or(false)
                {
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        false,
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::NotAllowed,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::RoomCreationNotPermitted,
                            error_type: ErrorType::Cancel,
                            resolver_outcome,
                            message: "Creating new MUC rooms is not permitted for this account.",
                        },
                    );
                }

                let config = managed_channel
                    .as_ref()
                    .map(XmppChannelRecord::room_config)
                    .unwrap_or_else(|| RoomConfig {
                        name: room_jid
                            .node()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "Room".to_string()),
                        members_only: false,
                        ..Default::default()
                    });

                let (waddle_id, channel_id) = managed_channel
                    .as_ref()
                    .map(|channel| {
                        let (waddle_id, _) = parse_room_jid_context(room_jid);
                        (waddle_id, channel.id.clone())
                    })
                    .unwrap_or_else(|| parse_room_jid_context(room_jid));

                let initial_affiliations = if managed_channel.is_none() {
                    vec![waddle_xmpp::muc::DurableAffiliationEntry::new(
                        sender_jid.to_bare(),
                        Some(Affiliation::Owner),
                    )]
                } else {
                    Vec::new()
                };
                let acquisition = if managed_channel.is_some() {
                    get_or_create_room_actor(state, room_jid, config, waddle_id, channel_id).await
                } else {
                    RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                        .get_or_create_room_with_initial_affiliations(
                            room_jid.clone(),
                            waddle_xmpp::muc::durable::WaddleId::new(waddle_id),
                            waddle_xmpp::muc::durable::ChannelId::new(channel_id),
                            config,
                            initial_affiliations,
                        )
                        .await
                };
                let acquisition = match acquisition {
                    Ok(acquisition) => acquisition,
                    // ADR-0017 Phase 3 Slice 7 FIX 6 (council-adjudicated):
                    // another node genuinely, currently owns this room's
                    // claim. Phase 4 first tries the ordered relay MUC proxy
                    // so the owning RoomActor remains the single writer.
                    Err(waddle_xmpp::muc::room_registry_actor::RoomRegistryError::ClaimHeldByAnotherNode(_)) => {
                        #[cfg(feature = "clustering")]
                        if let Some(origin) = ordered_relay_origin.as_ref() {
                            if let Some(bridge) = state
                                .deps
                                .app_state
                                .clustering_claims
                                .ordered_relay_delivery_bridge
                                .as_ref()
                            {
                                let mut presence = xmpp_parsers::presence::Presence::new(
                                    xmpp_parsers::presence::Type::None,
                                );
                                presence.from = Some(jid::Jid::from(sender_jid.clone()));
                                presence.to = room_jid
                                    .clone()
                                    .with_resource_str(&nick)
                                    .ok()
                                    .map(jid::Jid::from);
                                if let Some(show) = presence_show {
                                    presence.show = Some(show.to_xep0045());
                                }
                                let stanza = Stanza::Presence(presence);
                                let _remote_muc_membership_guard = state
                                    .deps
                                    .protocol
                                    .remote_muc_memberships
                                    .lock_membership(sender_jid, room_jid)
                                    .await;
                                if !join_connection_still_owns_slot(state, sender_jid, registry_owner) {
                                    debug!(room = %room_jid, sender = %sender_jid, "remote MUC join abandoned: connection superseded before relay");
                                    return Vec::new();
                                }
                                match remote_muc_join_decision(
                                    bridge
                                        .try_proxy_muc_remote(
                                            room_jid,
                                            &stanza,
                                            OrderedRelayMucProxyKind::JoinPresence,
                                            MucProxyOrigin::Connection(
                                                occupancy_session,
                                            ),
                                            origin, None)
                                        .await,
                                ) {
                                    Some(RemoteMucJoinDecision::Delivered(replies)) => {
                                        // A delivered XEP-0045 presence error means the
                                        // owner REFUSED the join: recording a membership
                                        // would overwrite the snapshot of whoever still
                                        // occupies the room under this full JID (#1703).
                                        let refused = replies.iter().any(|reply| {
                                            matches!(
                                                reply,
                                                Stanza::Presence(presence)
                                                    if presence.type_ == xmpp_parsers::presence::Type::Error
                                            )
                                        });
                                        if !refused {
                                            state
                                                .deps
                                                .protocol
                                                .remote_muc_memberships
                                                .record_join(
                                                    sender_jid,
                                                    room_jid,
                                                    &nick,
                                                    occupancy_session,
                                                );
                                        }
                                        return replies
                                            .into_iter()
                                            .map(|reply| stanza_to_xml(&reply))
                                            .collect();
                                    }
                                    Some(RemoteMucJoinDecision::MaybeCommitted) => {
                                        // The remote owner may already have mutated room state; a
                                        // local presence error would lie. Keep cleanup state and let
                                        // the client retry/resynchronize instead.
                                        state
                                            .deps
                                            .protocol
                                            .remote_muc_memberships
                                            .record_join(
                                                sender_jid,
                                                room_jid,
                                                &nick,
                                                occupancy_session,
                                            );
                                        return Vec::new();
                                    }
                                    None => {}
                                }
                            }
                        }
                        return deny_join_admission(
                            room_jid,
                            sender_jid,
                            &nick,
                            managed_channel.is_some(),
                            JoinAdmissionDenial {
                                condition:
                                    waddle_xmpp::telemetry::attributes::StanzaErrorCondition::ResourceConstraint,
                                deny_reason:
                                    waddle_xmpp::telemetry::attributes::MucJoinDenyReason::OwnershipHeldByAnotherNode,
                                error_type: ErrorType::Wait,
                                resolver_outcome,
                                message: "This room's ownership is currently held by another \
                                          node; please retry.",
                            },
                        );
                    }
                    Err(
                        waddle_xmpp::muc::room_registry_actor::RoomRegistryError::OwnershipReconciliationPending(_),
                    ) => {
                        return deny_join_admission(
                            room_jid,
                            sender_jid,
                            &nick,
                            managed_channel.is_some(),
                            JoinAdmissionDenial {
                                condition:
                                    waddle_xmpp::telemetry::attributes::StanzaErrorCondition::ResourceConstraint,
                                deny_reason:
                                    waddle_xmpp::telemetry::attributes::MucJoinDenyReason::OwnershipReconciling,
                                error_type: ErrorType::Wait,
                                resolver_outcome,
                                message: "This room's ownership is being reconciled; please retry.",
                            },
                        );
                    }
                    Err(error) => {
                        warn!(
                            room = %room_jid,
                            %error,
                            "Failed to get or create room actor for MUC join"
                        );
                        return deny_join_admission(
                            room_jid,
                            sender_jid,
                            &nick,
                            managed_channel.is_some(),
                            JoinAdmissionDenial {
                                condition:
                                    waddle_xmpp::telemetry::attributes::StanzaErrorCondition::InternalServerError,
                                error_type: ErrorType::Wait,
                                deny_reason:
                                    waddle_xmpp::telemetry::attributes::MucJoinDenyReason::RoomCreate,
                                resolver_outcome,
                                message: "Failed to get or create the room.",
                            },
                        );
                    }
                };
                let room_created = managed_channel.is_none()
                    && acquisition.creation
                        == waddle_xmpp::muc::room_registry_actor::RoomCreation::Created;
                (acquisition.actor_ref, room_created)
            }
        };

        let affiliation_grant = if created_instant_room {
            // The registry committed the owner before publication whenever a
            // durable store exists. Store-less rooms retain the established
            // in-memory XEP-0045 creator grant.
            JoinAffiliationGrant::CreatorOwner
        } else if let Some(affiliation) = managed_affiliation {
            JoinAffiliationGrant::Resolver(affiliation)
        } else {
            JoinAffiliationGrant::Unaffiliated
        };

        if !join_connection_still_owns_slot(state, sender_jid, registry_owner) {
            debug!(room = %room_jid, sender = %sender_jid, "MUC join abandoned: connection superseded before commit");
            return Vec::new();
        }
        let join_outcome = match room_actor
            .ask(JoinWithAffiliation {
                sender_jid: sender_jid.clone(),
                nick: nick.clone(),
                affiliation_grant,
                session: occupancy_session,
                local_domain: domain.clone(),
                admission_revision,
            })
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                // #1108: sealed-for-destruction room actor, or an ask
                // against an already-stopped actor. Retry once through
                // the registry, which respawns the room; never drop the
                // join silently.
                let room_sealed = matches!(
                    &error,
                    kameo::error::SendError::HandlerError(
                        waddle_xmpp::muc::room_actor::RoomActorError::RoomSealed
                    )
                );
                let room_gone =
                    room_sealed || !matches!(&error, kameo::error::SendError::HandlerError(_));
                if room_gone {
                    if !retried_dead_room {
                        retried_dead_room = true;
                        if room_sealed {
                            // #1108 follow-up: a sealed actor can still
                            // be registered when the guarded destroy's
                            // seal ask timed out — the registry lookup
                            // would hand back the same sealed actor and
                            // the retry would fail identically. Purge it
                            // so get-or-create respawns a fresh room.
                            let _ = RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                                .reap_sealed_room(room_jid.clone())
                                .await;
                        }
                        continue;
                    }
                    warn!(room = %room_jid, nick = %nick, error = ?error, "MUC join failed twice against a destroyed room actor");
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        managed_channel.is_some(),
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::InternalServerError,
                            error_type: ErrorType::Wait,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::RoomEvicted,
                            resolver_outcome,
                            message: "Room was evicted while joining; please retry.",
                        },
                    );
                }
                let nick_collision = matches!(
                    &error,
                    kameo::error::SendError::HandlerError(
                        waddle_xmpp::muc::room_actor::RoomActorError::NickAlreadyInUse(_)
                    )
                );
                if nick_collision {
                    // The conflict frame carries XEP-0045 §7.2.9 specifics the
                    // plain choke-point frame does not, so only the telemetry
                    // half is shared.
                    record_join_admission_denial(
                        room_jid,
                        sender_jid,
                        &nick,
                        managed_channel.is_some(),
                        &JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::Conflict,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::NickConflict,
                            error_type: ErrorType::Cancel,
                            resolver_outcome,
                            message: "Nickname is already in use in this room.",
                        },
                    );
                    return vec![build_muc_conflict_presence_xml(room_jid, &nick, sender_jid)];
                }
                if let kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::RoomActorError::OccupantAlreadyJoinedUnderDifferentNick {
                        current_nick,
                        ..
                    },
                ) = &error
                {
                    // #1107 / XEP-0045 §7.6: nicknames are locked to
                    // identity; a session already in the room under
                    // another nick is refused with <not-acceptable/>
                    // instead of being admitted as a ghost occupancy.
                    // The held nick is context the choke-point log
                    // lacks; DEBUG so the denial has one disposition
                    // line, not two.
                    debug!(
                        room = %room_jid,
                        current_nick = %current_nick,
                        "second-nick join refused; session already present under another nick"
                    );
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        managed_channel.is_some(),
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::NotAcceptable,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::NickLocked,
                            error_type: ErrorType::Cancel,
                            resolver_outcome,
                            message: "You are already in this room under a different nickname.",
                        },
                    );
                }
                if let kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::RoomActorError::StaleAdmissionRevision,
                ) = &error
                {
                    if stale_admission_retries < MAX_STALE_ADMISSION_RETRIES {
                        stale_admission_retries += 1;
                        continue;
                    }
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        managed_channel.is_some(),
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::InternalServerError,
                            error_type: ErrorType::Wait,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::StaleAdmissionRevision,
                            resolver_outcome,
                            message: "Room admission changed while joining; please retry.",
                        },
                    );
                }
                if let kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::RoomActorError::JoinForbidden { reason },
                ) = &error
                {
                    // XEP-0045 §7.2.8: bans map to <forbidden/> even in
                    // members-only rooms (#1265 item 1).
                    let (condition, deny_reason, message) = match reason {
                        waddle_xmpp::muc::room_actor::JoinDenialReason::MembersOnly => (
                            waddle_xmpp::telemetry::attributes::StanzaErrorCondition::RegistrationRequired,
                            waddle_xmpp::telemetry::attributes::MucJoinDenyReason::RoomMembersOnly,
                            "Membership required to join this room.",
                        ),
                        waddle_xmpp::muc::room_actor::JoinDenialReason::Banned => (
                            waddle_xmpp::telemetry::attributes::StanzaErrorCondition::Forbidden,
                            waddle_xmpp::telemetry::attributes::MucJoinDenyReason::RoomBan,
                            "You are banned from this room.",
                        ),
                    };
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        managed_channel.is_some(),
                        JoinAdmissionDenial {
                            condition,
                            deny_reason,
                            error_type: ErrorType::Auth,
                            resolver_outcome,
                            message,
                        },
                    );
                }
                // ADR-0017 Phase 3 Slice 7 FIX 4/FIX 6 (council-adjudicated):
                // this incarnation's durable restore has not (yet) resolved
                // — a genuine backend failure, not a legitimate empty new
                // room. Bounce with the same conformant, recoverable
                // condition the ownership-claim bounce above uses, so the
                // client retries rather than silently never joining.
                if matches!(
                    &error,
                    kameo::error::SendError::HandlerError(
                        waddle_xmpp::muc::room_actor::RoomActorError::RestorePending
                    )
                ) {
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        managed_channel.is_some(),
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::ResourceConstraint,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::DurableRestorePending,
                            error_type: ErrorType::Wait,
                            resolver_outcome,
                            message:
                                "This room's durable state has not finished loading; please retry.",
                        },
                    );
                }
                if matches!(
                    &error,
                    kameo::error::SendError::HandlerError(
                        waddle_xmpp::muc::room_actor::RoomActorError::OwnershipUnavailable
                    )
                ) {
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        managed_channel.is_some(),
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::ResourceConstraint,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::OwnershipUnavailable,
                            error_type: ErrorType::Wait,
                            resolver_outcome,
                            message: "This room's ownership is being reconciled; please retry.",
                        },
                    );
                }
                if matches!(
                    &error,
                    kameo::error::SendError::HandlerError(
                        waddle_xmpp::muc::room_actor::RoomActorError::RoomFull
                    )
                ) {
                    // XEP-0045 §7.2.9: the room has reached its maximum
                    // number of occupants — deny access with a presence
                    // error of type "wait" carrying <service-unavailable/>.
                    // Returning an empty reply here left the client
                    // stalled forever waiting for self-presence (#1111).
                    return deny_join_admission(
                        room_jid,
                        sender_jid,
                        &nick,
                        managed_channel.is_some(),
                        JoinAdmissionDenial {
                            condition:
                                waddle_xmpp::telemetry::attributes::StanzaErrorCondition::ServiceUnavailable,
                            deny_reason:
                                waddle_xmpp::telemetry::attributes::MucJoinDenyReason::RoomFull,
                            error_type: ErrorType::Wait,
                            resolver_outcome,
                            message: "The room has reached its maximum number of occupants.",
                        },
                    );
                }
                // FIX 6 / #1111: no remaining error variant may silently
                // drop the join with no presence reply at all — bounce
                // typed instead of the previous bare `return vec![]`. This
                // is unreachable for the current JoinWithAffiliation error
                // surface (every RoomActorError variant it returns has a
                // typed arm above, and transport failures take the #1108
                // retry path) — kept as a typed fail-safe so a future
                // variant can never stall the client with an empty reply.
                warn!(room = %room_jid, nick = %nick, error = ?error, "Failed to join MUC room");
                return deny_join_admission(
                    room_jid,
                    sender_jid,
                    &nick,
                    managed_channel.is_some(),
                    JoinAdmissionDenial {
                        condition:
                            waddle_xmpp::telemetry::attributes::StanzaErrorCondition::InternalServerError,
                        error_type: ErrorType::Wait,
                        deny_reason:
                            waddle_xmpp::telemetry::attributes::MucJoinDenyReason::RoomActorError,
                        resolver_outcome,
                        message: "Failed to join the room; please retry.",
                    },
                );
            }
        };

        let occupant_count = join_outcome.occupant_count;
        let self_muji = join_outcome
            .existing_occupants
            .iter()
            .find(|existing| existing.nick == nick && existing.jid == *sender_jid)
            .and_then(|existing| existing.muji.as_ref());
        let self_in_call = join_outcome
            .existing_occupants
            .iter()
            .find(|existing| existing.nick == nick && existing.jid == *sender_jid)
            .map(|existing| existing.in_call)
            .unwrap_or_default();

        info!(room = %room_jid, nick = %nick, occupants = occupant_count, "User joined MUC room");

        // Notification activity ingest (slice 2b): a successful MUC join
        // bumps `(sender_bare, room)` activity. The XEP-0513 `<active/>`
        // filter consults this projection to admit ActiveChannelMention
        // pushes for users who are present in the room. `presence_show` is
        // passed in by the caller (`handle_presence`) when the incoming
        // presence carried a typed `<show/>` token; on first join (or
        // when no `<show/>` is present) we record `None` so the column
        // stays NULL until the user actually broadcasts a state.
        crate::server::routes::interpret::record_presence_available_activity_on_state(
            state,
            &sender_jid.to_bare(),
            room_jid,
            presence_show,
        )
        .await;

        let mut responses = Vec::new();

        // Replay one base occupant presence per nick to the joiner, then
        // append extra same-nick Muji payloads for additional sessions
        // that own call state. Active call membership is nick-level, but
        // XEP-0272 preparing is resource-owned coordination state, so the
        // joiner needs the exact full JID that advertised it.
        let mut replayed_nicks = HashSet::new();
        let replay_occupants: Vec<_> = join_outcome
            .existing_occupants
            .iter()
            .filter(|existing| existing.nick != nick)
            .collect();
        for existing in replay_occupants
            .iter()
            .copied()
            .filter(|existing| replayed_nicks.insert(existing.nick.clone()))
        {
            // XEP-0045 §7.2 conformant occupant-list replay, plus the
            // typed `<call xmlns='urn:waddle:muc-call:0'/>` extension when
            // the room actor's snapshot still has an active advertisement
            // for that occupant. Without this enrichment the joiner only
            // sees the chip light up via the NEXT presence update from a
            // call participant, which never happens if the call is steady
            // state — the late joiner is the one we're trying to help.
            responses.push(build_muc_join_presence_xml(MucJoinPresence {
                occupant_id_secret: &state.deps.occupant_id_secret,
                server_hats: &state.deps.app_state.server_hats,
                room_jid,
                nick: &existing.nick,
                to_jid: sender_jid,
                affiliation: existing.affiliation,
                role: existing.role,
                real_jid: &existing.jid,
                disclose_real_jid: true,
                include_self_status: false,
                room_created: false,
                warn_nonanonymous_join: false,
                muji: existing.muji.as_ref(),
                in_call: existing.in_call,
            }));

            for extra in replay_occupants.iter().copied().filter(|candidate| {
                candidate.nick == existing.nick
                    && candidate.jid != existing.jid
                    && (candidate.muji.is_some() || !candidate.in_call.is_empty())
            }) {
                responses.push(build_muc_join_presence_xml(MucJoinPresence {
                    occupant_id_secret: &state.deps.occupant_id_secret,
                    server_hats: &state.deps.app_state.server_hats,
                    room_jid,
                    nick: &extra.nick,
                    to_jid: sender_jid,
                    affiliation: extra.affiliation,
                    role: extra.role,
                    real_jid: &extra.jid,
                    disclose_real_jid: true,
                    include_self_status: false,
                    room_created: false,
                    warn_nonanonymous_join: false,
                    muji: extra.muji.as_ref(),
                    in_call: extra.in_call,
                }));
            }
        }

        // Broadcast the new occupant's presence to all existing occupants.
        // Non-blocking: a zombied/slow consumer must never stall the join path,
        // which is how "Timed out waiting for self-presence" cascades start.
        // Drop accounting is handled inside `try_send_to` (logs + metrics);
        // per-occupant outcome is discarded here because a missed join
        // presence self-heals via the next MUC presence/probe round-trip.
        if !join_outcome.is_same_bare_multi_session_join && !join_outcome.is_existing_session_rejoin
        {
            for existing in &join_outcome.existing_occupants {
                let presence_stanza = build_muc_join_presence_stanza(MucJoinPresence {
                    occupant_id_secret: &state.deps.occupant_id_secret,
                    server_hats: &state.deps.app_state.server_hats,
                    room_jid,
                    nick: &nick,
                    to_jid: &existing.jid,
                    affiliation: join_outcome.new_occupant_affiliation,
                    role: join_outcome.new_occupant_role,
                    real_jid: sender_jid,
                    disclose_real_jid: true,
                    include_self_status: false,
                    room_created: false,
                    warn_nonanonymous_join: false,
                    muji: None,
                    in_call: waddle_xmpp::xep::InCallPresenceState::default(),
                });
                let stanza = Stanza::Presence(presence_stanza);
                route_room_presence_to_occupant(state, room_jid, &existing.jid, stanza).await;
            }
        }

        // Send self-presence to the joining user (with status code 110)
        responses.push(build_muc_join_presence_xml(MucJoinPresence {
            occupant_id_secret: &state.deps.occupant_id_secret,
            server_hats: &state.deps.app_state.server_hats,
            room_jid,
            nick: &nick,
            to_jid: sender_jid,
            affiliation: join_outcome.new_occupant_affiliation,
            role: join_outcome.new_occupant_role,
            real_jid: sender_jid,
            disclose_real_jid: true,
            include_self_status: true,
            room_created: created_instant_room,
            // XEP-0045 §7.2.3: status 100 rides ONLY on the joiner's
            // initial self-presence (#1265 item 4).
            warn_nonanonymous_join: true,
            muji: self_muji,
            in_call: self_in_call,
        }));

        // Same-account sibling resources share one MUC nick. If a
        // sibling already advertised Muji for this nick, reflect exact
        // per-session snapshots after the new session's own plain
        // self-presence so a refresh/new tab can show "call active on
        // another device" without misattributing `<preparing/>` to the
        // joining resource.
        for existing in join_outcome.existing_occupants.iter().filter(|existing| {
            existing.nick == nick
                && existing.jid.to_bare() == sender_jid.to_bare()
                && existing.jid != *sender_jid
                && (existing.muji.is_some() || !existing.in_call.is_empty())
        }) {
            responses.push(build_muc_join_presence_xml(MucJoinPresence {
                occupant_id_secret: &state.deps.occupant_id_secret,
                server_hats: &state.deps.app_state.server_hats,
                room_jid,
                nick: &nick,
                to_jid: sender_jid,
                affiliation: existing.affiliation,
                role: existing.role,
                real_jid: &existing.jid,
                disclose_real_jid: true,
                include_self_status: true,
                room_created: false,
                warn_nonanonymous_join: false,
                muji: existing.muji.as_ref(),
                in_call: existing.in_call,
            }));
        }

        // XEP-0045 §7.2.15 historical room subject. The typed builder
        // produces the conformant envelope: nick-form `from` + `<delay/>`
        // + XEP-0421 `<occupant-id/>` when a setter is known, or bare-from
        // empty `<subject/>` for a never-set room (matching the established
        // resolution of XEP-0421 §3 vs §7.2.15 on never-set rooms).
        let subject_msg = build_subject_message(
            room_jid,
            sender_jid,
            join_outcome.subject_state.as_ref(),
            &state.deps.occupant_id_secret,
        );
        responses.push(stanza_to_xml(&Stanza::Message(subject_msg)));

        // W1.6c seam: queue a live room stanza behind the unwritten burst.
        #[cfg(feature = "test-support")]
        crate::server::routes::websocket::characterization_seams::hold_until_live_stanza_queued(
            state,
            Some(sender_jid),
            registry_owner,
            crate::server::routes::websocket::characterization_seams::HeldBatch::MucJoin,
        )
        .await;

        return responses;
    }
}

/// Handle MUC room leave
pub async fn handle_muc_leave(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
    ordered_relay_origin: Option<&OrderedRelayRouteOrigin>,
) -> Vec<String> {
    let _request_timer = waddle_xmpp::metrics::MucOccupancyHandlerTimer::start("leave_request");
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave request");
    #[cfg(not(feature = "clustering"))]
    let _ = ordered_relay_origin;

    // Notification activity ingest (slice 2b): a XEP-0045 leave
    // (explicit `<presence type='unavailable'/>`) is still an
    // engagement signal — the user just acted on the room — so we
    // bump `(sender_bare, room)` activity and clear the persisted
    // `<show/>`. We record before the room-actor teardown so a
    // missing room actor doesn't suppress the activity write; the
    // typed signal happened on the wire regardless.
    crate::server::routes::interpret::record_presence_unavailable_activity_on_state(
        state,
        &sender_jid.to_bare(),
        room_jid,
    )
    .await;

    let Some(room_actor) = get_room_actor(state, room_jid).await else {
        let known_remote_membership = state
            .deps
            .protocol
            .remote_muc_memberships
            .contains(sender_jid, room_jid);
        #[cfg(feature = "clustering")]
        if let Some(origin) = ordered_relay_origin {
            if let Some(bridge) = state
                .deps
                .app_state
                .clustering_claims
                .ordered_relay_delivery_bridge
                .as_ref()
            {
                let mut presence = xmpp_parsers::presence::Presence::new(
                    xmpp_parsers::presence::Type::Unavailable,
                );
                presence.from = Some(jid::Jid::from(sender_jid.clone()));
                presence.to = room_jid
                    .clone()
                    .with_resource_str(nick)
                    .ok()
                    .map(jid::Jid::from);
                let stanza = Stanza::Presence(presence);
                let _remote_muc_membership_guard = state
                    .deps
                    .protocol
                    .remote_muc_memberships
                    .lock_membership(sender_jid, room_jid)
                    .await;
                match remote_muc_leave_decision(
                    bridge
                        .try_proxy_muc_remote(
                            room_jid,
                            &stanza,
                            OrderedRelayMucProxyKind::OccupantPresence,
                            MucProxyOrigin::Connection(occupancy_session),
                            origin,
                            None,
                        )
                        .await,
                ) {
                    RemoteMucLeaveDecision::Delivered(replies) => {
                        // Only this connection's membership is tombstoned: a
                        // replacement that already recorded its join keeps
                        // its snapshot (the owner answered it `Superseded`).
                        state
                            .deps
                            .protocol
                            .remote_muc_memberships
                            .record_leave_if_session(sender_jid, room_jid, occupancy_session);
                        return replies
                            .into_iter()
                            .map(|reply| stanza_to_xml(&reply))
                            .collect();
                    }
                    RemoteMucLeaveDecision::MaybeCommitted => {
                        return Vec::new();
                    }
                    RemoteMucLeaveDecision::RetryableNoEffect => {
                        return bounce_muc_leave_ownership_unreachable(room_jid, sender_jid, nick);
                    }
                    RemoteMucLeaveDecision::LocalFallback => {}
                }
            }
        }
        if known_remote_membership {
            return bounce_muc_leave_ownership_unreachable(room_jid, sender_jid, nick);
        }

        debug!(room = %room_jid, "Room not found for leave");
        // Idempotent on the SFU side — the user could have an SFU
        // participant even when the room actor is gone (process
        // restart, eviction). Tear that down too: this is the live
        // connection's own leave, so a restored (unbound) registration
        // is torn down as well.
        let _ =
            super::super::super::super::muc_call_sfu::unregister_participant_from_room_if_occupant_matches(
                state,
                room_jid,
                sender_jid,
                occupancy_session,
                waddle_sfu::UnboundOccupantPolicy::TearDown,
                None,
            );
        return vec![build_muc_self_unavailable_xml(
            state,
            room_jid,
            nick,
            sender_jid,
            Affiliation::None,
        )];
    };

    let leave_attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
    // Write-ahead: if this task is cancelled after the actor dropped the
    // occupancy but before the fan-out below, the janitor replays the retained
    // outcome under this attempt. Completed once the effects ran.
    let in_flight = LocalDepartureItem::InFlight {
        room: room_jid.clone(),
        jid: sender_jid.clone(),
        cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
        selector: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Generation(occupancy_session),
        attempt: leave_attempt,
        notified: HashSet::new(),
        removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
    };
    state
        .deps
        .protocol
        .pending_local_muc_departures
        .record_in_flight(in_flight.clone());
    let _in_flight_lease = InFlightLease::hold(
        std::sync::Arc::clone(&state.deps.protocol.pending_local_muc_departures),
        in_flight.clone(),
    );
    let outcome = match crate::server::routes::websocket::ask_leave_bounded(
        &room_actor,
        LeaveByRealJid {
            sender_jid: sender_jid.clone(),
            cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
            session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Generation(
                occupancy_session,
            ),
            attempt: leave_attempt,
            origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
        },
    )
    .await
    {
        Ok(waddle_xmpp::muc::room_actor::LeaveDisposition::Left(outcome)) => outcome,
        Ok(waddle_xmpp::muc::room_actor::LeaveDisposition::NotOccupant) => {
            waddle_xmpp::telemetry::reliability::increment_muc_cleanup_not_occupant();
            state
                .deps
                .protocol
                .pending_local_muc_departures
                .complete_in_flight(&in_flight);
            debug!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave for absent occupant");
            // No occupant slot to remove, but a stale SFU
            // participant could still exist — clear it.
            let _ = super::super::super::super::muc_call_sfu::unregister_participant_from_room_if_occupant_matches(
                state,
                room_jid,
                sender_jid,
                occupancy_session,
                waddle_sfu::UnboundOccupantPolicy::TearDown,
                None,);
            return vec![build_muc_self_unavailable_xml(
                state,
                room_jid,
                nick,
                sender_jid,
                Affiliation::None,
            )];
        }
        Ok(waddle_xmpp::muc::room_actor::LeaveDisposition::Superseded { .. }) => {
            waddle_xmpp::telemetry::reliability::increment_muc_cleanup_superseded();
            state
                .deps
                .protocol
                .pending_local_muc_departures
                .complete_in_flight(&in_flight);
            debug!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave superseded by a replacement session");
            let _ = super::super::super::super::muc_call_sfu::unregister_participant_from_room_if_occupant_matches(
                state,
                room_jid,
                sender_jid,
                occupancy_session,
                waddle_sfu::UnboundOccupantPolicy::Keep,
                None,);
            return vec![build_muc_self_unavailable_xml(
                state,
                room_jid,
                nick,
                sender_jid,
                Affiliation::None,
            )];
        }
        Ok(waddle_xmpp::muc::room_actor::LeaveDisposition::Deferred { .. }) => {
            state
                .deps
                .protocol
                .pending_local_muc_departures
                .complete_in_flight(&in_flight);
            return bounce_muc_leave_ownership_unreachable(room_jid, sender_jid, nick);
        }
        Ok(waddle_xmpp::muc::room_actor::LeaveDisposition::Suppressed {
            nick: actor_nick,
            affiliation,
            attempt: acknowledge,
        }) => {
            crate::server::routes::websocket::acknowledge_in_flight(
                &state.deps.protocol.pending_local_muc_departures,
                &room_actor,
                &in_flight,
                acknowledge,
            );
            // Store-less room with a destroy/dormancy in flight: the departure
            // was recorded but nothing fans out. The departing session still
            // gets its XEP-0045 §7.14 self-presence echo (as before #1647).
            let _ = super::super::super::super::muc_call_sfu::unregister_participant_from_room_if_occupant_matches(
                state,
                room_jid,
                sender_jid,
                occupancy_session,
                waddle_sfu::UnboundOccupantPolicy::TearDown,
                None,);
            // The actor's typed nickname is authoritative: the request's
            // `to` JID may carry a nick the sender never held.
            return vec![build_muc_self_unavailable_xml(
                state,
                room_jid,
                actor_nick.as_str(),
                sender_jid,
                affiliation,
            )];
        }
        Err(LeaveAskFailure::Timeout) => {
            // The actor may still complete the departure after this bound:
            // retain it so the janitor replays the outcome (self-echo, fan-out,
            // SFU teardown) under the same attempt id, and bounce wait-class now.
            warn!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave ask timed out; retained for replay");
            state
                .deps
                .protocol
                .pending_local_muc_departures
                .complete_in_flight(&in_flight);
            state.deps.protocol.pending_local_muc_departures.record(
                LocalDepartureItem::RoomDeparture {
                    room: room_jid.clone(),
                    jid: sender_jid.clone(),
                    cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
                    selector: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Generation(
                        occupancy_session,
                    ),
                    attempt: leave_attempt,
                    notified: HashSet::new(),
                    removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
                },
            );
            return bounce_muc_leave_ownership_unreachable(room_jid, sender_jid, nick);
        }
        Err(
            error @ (crate::server::routes::websocket::LeaveAskFailure::Handler(_)
            | LeaveAskFailure::Transport),
        ) => {
            // A sealed answer can follow an ENQUEUED leave whose durable
            // projection was ambiguous (the actor self-seals with its
            // pre-commit roster), and a transport failure leaves the same
            // uncertainty. Mirror the disconnect path: keep a
            // `ConfirmRetired` responsibility bound to THIS actor so the
            // janitor replays the departure against the successor after
            // retirement, instead of dropping the only replay owner.
            state
                .deps
                .protocol
                .pending_local_muc_departures
                .complete_in_flight(&in_flight);
            state.deps.protocol.pending_local_muc_departures.record(
                LocalDepartureItem::ConfirmRetired {
                    room: room_jid.clone(),
                    jid: sender_jid.clone(),
                    actor: room_actor.id(),
                    cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
                    selector: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Generation(
                        occupancy_session,
                    ),
                    attempt: leave_attempt,
                    notified: HashSet::new(),
                    removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
                },
            );
            warn!(room = %room_jid, nick = %nick, sender = %sender_jid, error = ?error, "Failed to leave MUC room; waiting for actor retirement");
            return bounce_muc_leave_ownership_unreachable(room_jid, sender_jid, nick);
        }
    };

    // SFU teardown runs after `LeaveByRealJid` so the MUC's
    // authoritative view drops the occupant first; the membership
    // gate immediately reports the user as a non-occupant and any
    // subsequent `request-join` is rejected before the SFU is
    // touched again. Closes the gap where a client leaves the MUC
    // without sending the call-specific `request-leave` — their SFU
    // participant would otherwise linger until LiveKit's timeout.
    let _ = super::super::super::super::muc_call_sfu::unregister_participant_from_room_if_occupant_matches(
        state,
        room_jid,
        sender_jid,
        occupancy_session,
        waddle_sfu::UnboundOccupantPolicy::TearDown,
        None,
    );

    // Broadcast unavailable presence to remaining occupants (non-blocking).
    // Drop accounting is handled inside `try_send_to`. The same helper
    // is used by `cleanup_muc_presence` for unclean disconnects, so
    // both the explicit-leave path and the tab-close path produce the
    // same wire shape.
    super::super::super::super::cleanup::broadcast_muc_leave_to_remaining_resumable(
        state,
        room_jid,
        sender_jid,
        &outcome,
        // The occupant asked to leave: the §7.14 shape, no removal code.
        waddle_xmpp::muc::MucRemovalCause::Voluntary,
        Some(super::super::super::super::cleanup::LeaveFanOutProgress {
            skip: &super::super::super::super::cleanup::NO_SKIP,
            record: Some((
                &state.deps.protocol.pending_local_muc_departures,
                &in_flight,
            )),
        }),
    )
    .await;
    super::super::super::super::cleanup::broadcast_muc_muji_clear_to_remaining_resumable(
        state,
        room_jid,
        sender_jid,
        &outcome,
        Some(super::super::super::super::cleanup::LeaveFanOutProgress {
            skip: &super::super::super::super::cleanup::NO_SKIP,
            record: Some((
                &state.deps.protocol.pending_local_muc_departures,
                &in_flight,
            )),
        }),
    )
    .await;

    let response = vec![build_muc_self_unavailable_xml(
        state,
        room_jid,
        outcome.nick.as_str(),
        sender_jid,
        outcome.affiliation,
    )];
    // Eviction is not response-critical: hand it off without awaiting so the
    // self-presence below is returned before any registry wait.
    super::super::super::super::cleanup::evict_empty_room_detached(state, room_jid, &outcome);
    // Every effect ran: the write-ahead entry becomes the owed acknowledgement
    // (atomically) and is cleared only on the actor's authoritative answer.
    crate::server::routes::websocket::acknowledge_in_flight(
        &state.deps.protocol.pending_local_muc_departures,
        &room_actor,
        &in_flight,
        outcome.acknowledge,
    );
    response
}
