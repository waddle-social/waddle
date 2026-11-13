//! This node's durable-mailbox poller and its hint transport.
//!
//! The poller is the authority for finding work: it scans the lanes that owe
//! this node a decision, on an interval and whenever a peer's hint arrives. A
//! hint only shortens the wait; a lost one costs at most one interval. Each
//! pass examines a bounded number of lanes and hands them to a bounded
//! number of workers, so a backlog cannot occupy more than that many ingress
//! connections, and a lane whose head is slow delays no other lane while a
//! worker is free.
//!
//! Working a lane depends on what its head entry carries. A room acceptance
//! runs the owner's ordinary relayed-groupchat commit, rebuilt from canonical
//! rows; that commit asserts the room claim and records the decision. A room
//! request (join, presence update, leave or relayed IQ) runs the owner's
//! handler for the stored stanza and then one transaction that asserts the
//! room claim, latches the decision, freezes the handler's reply as the
//! response, writes durable occupancy and moves the lane's cursor.
//!
//! Before this node decides any entry of a room under a claim it has not
//! served yet, it loads the room's durable occupancy and seats it in the
//! room actor. Lanes of a room whose claim moved here are adopted only after
//! that load.

use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use waddle_xmpp::ingress::mailbox::{MailboxOperationKey, MailboxOwnerDecision, RoomRequestKind};
use waddle_xmpp::muc::room_actor::{RecoveredOccupant, ReportOccupancy, SeedRecoveredOccupants};
use waddle_xmpp::muc::room_registry_actor::GetOrRestoreDurableRoom;
use waddle_xmpp::ownership::NodeIdentity;
use waddle_xmpp::telemetry::attributes::MailboxDeliveryOutcome;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::{Affiliation, Role};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

use super::delivery::{decide_direct, deliver_reserved_muc_proxy};
use super::{
    MucProxyOrigin, NodeId, OrderedRelayDeliveryBridge, OrderedRelayDeliveryServices,
    OrderedRelayMucProxyKind, RelayHandle,
};
use crate::ingress::mailbox_worker::{
    ClaimedRoomAcceptance, MailboxAttemptRecord, MailboxLaneHead, MAILBOX_CLAIM_LIMIT,
    MAILBOX_UNCONSUMED_GRACE,
};
use crate::ingress::origin_gate::MailboxHintSink;
use crate::ingress::room_request::{ClaimedRoomRequest, RoomOccupancyChange, RoomRequestOutcome};
use crate::ingress_uow::{MailboxLaneId, RoomOccupantLookup, StoredRoomRequest};
use crate::server::routes::websocket::handlers::presence::room_request::{
    presence_request_refused, room_request_error,
};

/// How long the poller waits for a hint before it scans anyway.
const MAILBOX_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Poll passes between two adoption passes. A room whose claim moved is
/// served by its new owner within this many poll intervals.
const MAILBOX_ADOPTION_PASSES: u32 = 5;
/// Poll passes between two sweeps of unconsumed decisions.
const MAILBOX_SWEEP_PASSES: u32 = 30;
/// Entries of one lane a worker decides before it returns the lane to the
/// next pass: the lane's pending cap, so one visit can drain a full lane.
const MAILBOX_LANE_DRAIN_LIMIT: u32 = 64;
/// Budget of one hint. A hint that does not arrive is not retried.
const MAILBOX_HINT_TIMEOUT: Duration = Duration::from_secs(2);
/// Budget of restoring a room and seating its recovered occupants.
const OCCUPANCY_SEAT_TIMEOUT: Duration = Duration::from_secs(5);

impl OrderedRelayDeliveryBridge {
    /// Wake this node's mailbox poller. Called when a peer's hint arrives.
    pub(in crate::clustering) fn wake_mailbox_poller(&self) {
        self.mailbox_wake.notify_one();
    }
}

impl MailboxHintSink for OrderedRelayDeliveryBridge {
    /// Tell `target` that an entry was appended for it. Sent in the
    /// background and never awaited by the appender: the entry is already
    /// committed, and the target's poller finds it without the hint.
    fn hint(&self, target: NodeIdentity) {
        let stop_token = self.stop_token.clone();
        tokio::spawn(async move {
            let mut relay = RelayHandle::new(NodeId::new(target.node_id), stop_token)
                .with_ask_timeouts(MAILBOX_HINT_TIMEOUT, MAILBOX_HINT_TIMEOUT);
            let delivered = tokio::time::timeout(MAILBOX_HINT_TIMEOUT, relay.mailbox_hint())
                .await
                .is_ok_and(|result| result.is_ok());
            waddle_xmpp::telemetry::reliability::increment_mailbox_hint(delivered);
        });
    }
}

/// Start this node's mailbox poller. It runs until the bridge's stop token
/// fires and does nothing while remote room acceptance uses the relay.
pub fn spawn_mailbox_poller(bridge: Arc<OrderedRelayDeliveryBridge>) {
    tokio::spawn(async move {
        // Where the next pass resumes. A full page moves it forward, so
        // lanes whose heads are deferred or dead-lettered cannot keep the
        // poller from the lanes behind them; a short page starts over.
        let mut resume_after = None;
        let mut adoption_after = None;
        let mut sweep_after = None;
        let mut pass: u32 = 0;
        let workers = LaneWorkers::new();
        loop {
            tokio::select! {
                biased;
                _ = bridge.stop_token.cancelled() => return,
                _ = bridge.mailbox_wake.notified() => {}
                _ = tokio::time::sleep(MAILBOX_POLL_INTERVAL) => {}
            }
            let Some(services) = bridge.services.get().cloned() else {
                continue;
            };
            pass = pass.wrapping_add(1);
            if pass.is_multiple_of(MAILBOX_ADOPTION_PASSES) {
                adoption_after = adopt_once(&services, adoption_after).await;
            }
            resume_after = poll_once(&services, &workers, resume_after).await;
            if pass.is_multiple_of(MAILBOX_SWEEP_PASSES) {
                sweep_after = sweep_once(&services, sweep_after).await;
                report_unconsumed_requests(&services).await;
            }
        }
    });
}

/// The ingress authority, while this node carries remote room acceptance
/// through the mailbox.
fn mailbox_authority(
    services: &Arc<OrderedRelayDeliveryServices>,
) -> Option<Arc<crate::ingress::IngressAuthority>> {
    let state = services.web_socket_state.upgrade()?;
    let authority = Arc::clone(&state.deps.protocol.ingress);
    authority.mailbox_acceptance_enabled().then_some(authority)
}

/// Adopt lanes of rooms whose claim moved to this node, so the next backlog
/// scan finds them. Returns where the next adoption pass resumes.
async fn adopt_once(
    services: &Arc<OrderedRelayDeliveryServices>,
    resume_after: Option<MailboxLaneId>,
) -> Option<MailboxLaneId> {
    let authority = mailbox_authority(services)?;
    let target = services.node_identity.current();
    match authority
        .adopt_transferred_lanes(&target, resume_after, |room| {
            let authority = Arc::clone(&authority);
            let target = target.clone();
            async move { seat_room_occupancy(services, &authority, &room, &target).await }
        })
        .await
    {
        Ok(adoption) => {
            if adoption.adopted > 0 {
                tracing::info!(
                    adopted = adoption.adopted,
                    "mailbox poller adopted lanes of rooms this node now owns"
                );
            }
            adoption.resume_after
        }
        Err(error) => {
            tracing::warn!(%error, "mailbox poller could not adopt transferred lanes");
            None
        }
    }
}

/// Consume decided entries no origin read. Returns where the next sweep
/// resumes.
async fn sweep_once(
    services: &Arc<OrderedRelayDeliveryServices>,
    resume_after: Option<waddle_xmpp::ingress::MessageKey>,
) -> Option<waddle_xmpp::ingress::MessageKey> {
    let authority = mailbox_authority(services)?;
    match authority
        .sweep_unconsumed_decisions(MAILBOX_UNCONSUMED_GRACE, resume_after)
        .await
    {
        Ok(sweep) => {
            if sweep.consumed > 0 || sweep.unproven > 0 {
                tracing::info!(
                    consumed = sweep.consumed,
                    unproven = sweep.unproven,
                    "mailbox poller swept decided entries no origin consumed"
                );
            }
            sweep.resume_after
        }
        Err(error) => {
            tracing::warn!(%error, "mailbox poller could not sweep unconsumed decisions");
            None
        }
    }
}

/// Report decided room requests no origin consumed. The count is published
/// on every successful read; the log line is written only when it is not
/// zero, once per sweep.
async fn report_unconsumed_requests(services: &Arc<OrderedRelayDeliveryServices>) {
    let Some(authority) = mailbox_authority(services) else {
        return;
    };
    match authority
        .count_unconsumed_request_decisions(MAILBOX_UNCONSUMED_GRACE)
        .await
    {
        Ok(count) => {
            waddle_xmpp::telemetry::reliability::record_room_request_unconsumed_decisions(count);
            if count > 0 {
                tracing::warn!(
                    count,
                    "decided room requests were not consumed by their origin"
                );
            }
        }
        Err(error) => {
            tracing::warn!(%error, "mailbox poller could not count unconsumed room requests");
        }
    }
}

impl OrderedRelayDeliveryBridge {
    /// Whether `envelope` carries a room kind this node no longer accepts
    /// on the relay: a join, an occupant presence or a Muji IQ, while room
    /// requests travel through the mailbox. Such an envelope is answered
    /// with the unsupported-envelope refusal before its sequence is
    /// reserved, so the sender rolls the sequence back and nothing ran.
    pub(in crate::clustering) async fn refuses_relayed_room_kind(
        &self,
        envelope: &super::RemoteStanzaEnvelope,
    ) -> bool {
        let super::OrderedRelayPayload::MucProxy { kind, .. } = &envelope.payload else {
            return false;
        };
        if !matches!(
            kind,
            OrderedRelayMucProxyKind::JoinPresence
                | OrderedRelayMucProxyKind::OccupantPresence
                | OrderedRelayMucProxyKind::MujiJingleIq
        ) {
            return false;
        }
        let Some(state) = self
            .services
            .get()
            .and_then(|services| services.web_socket_state.upgrade())
        else {
            return false;
        };
        state
            .deps
            .protocol
            .ingress
            .room_requests_carried_now()
            .await
    }
}

/// The poller's lane workers: at most [`MAILBOX_CLAIM_LIMIT`] lanes are
/// worked at a time, each by its own task, and a lane is worked by one task
/// at a time.
///
/// A worker holds its lane until the lane's head is idle, deferred or dead
/// lettered, which for a head whose owner attempt runs to its receiver
/// budget is several seconds. The poller does not wait for a worker: a
/// pass hands each lane to the next free worker and ends, so a lane that
/// is held occupies one worker while the others keep draining the lanes
/// this pass and later passes find.
struct LaneWorkers {
    permits: Arc<tokio::sync::Semaphore>,
    busy: Arc<std::sync::Mutex<std::collections::HashSet<MailboxLaneId>>>,
}

impl LaneWorkers {
    fn new() -> Self {
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(MAILBOX_CLAIM_LIMIT)),
            busy: Arc::default(),
        }
    }

    /// Whether a worker holds `lane`.
    fn is_busy(&self, lane: MailboxLaneId) -> bool {
        self.busy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&lane)
    }

    /// Hand `lane` to the next free worker, waiting for one. `false` when
    /// no worker can be had, which ends the pass.
    async fn work(
        &self,
        services: &Arc<OrderedRelayDeliveryServices>,
        authority: &Arc<crate::ingress::IngressAuthority>,
        lane: MailboxLaneId,
        target: &NodeIdentity,
    ) -> bool {
        let Ok(permit) = Arc::clone(&self.permits).acquire_owned().await else {
            return false;
        };
        self.busy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(lane);
        let busy = Arc::clone(&self.busy);
        let services = Arc::clone(services);
        let authority = Arc::clone(authority);
        let target = target.clone();
        tokio::spawn(async move {
            // Both are released when the worker ends, however it ends.
            let _permit = permit;
            let _released = LaneRelease { busy, lane };
            work_lane(&services, &authority, lane, &target).await;
        });
        true
    }
}

/// Marks a worker's lane free again when the worker's task ends.
struct LaneRelease {
    busy: Arc<std::sync::Mutex<std::collections::HashSet<MailboxLaneId>>>,
    lane: MailboxLaneId,
}

impl Drop for LaneRelease {
    fn drop(&mut self) {
        self.busy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.lane);
    }
}

/// One pass: scan this node's backlog after `resume_after` and hand its
/// lanes to the workers, skipping a lane a worker still holds. Returns
/// where the next pass resumes.
async fn poll_once(
    services: &Arc<OrderedRelayDeliveryServices>,
    workers: &LaneWorkers,
    resume_after: Option<MailboxLaneId>,
) -> Option<MailboxLaneId> {
    let state = services.web_socket_state.upgrade()?;
    let authority = Arc::clone(&state.deps.protocol.ingress);
    if !authority.mailbox_acceptance_enabled() {
        return None;
    }
    let target = services.node_identity.current();
    let lanes = match authority.scan_mailbox_backlog(&target, resume_after).await {
        Ok(lanes) => lanes,
        Err(error) => {
            tracing::warn!(%error, "mailbox poller could not scan its backlog");
            return None;
        }
    };
    for lane in &lanes {
        if workers.is_busy(*lane) {
            continue;
        }
        if !workers.work(services, &authority, *lane, &target).await {
            return None;
        }
    }
    let full_page = lanes.len() >= crate::ingress::mailbox_worker::MAILBOX_SCAN_LIMIT as usize;
    lanes.last().copied().filter(|_| full_page)
}

/// Work one lane from its head and record how each attempt ended.
///
/// The mailbox is the only transport of a stanza to its room owner, so a
/// sender's burst sits on one lane behind one coalesced hint. The worker
/// therefore keeps going while each head is decided, up to
/// [`MAILBOX_LANE_DRAIN_LIMIT`] entries, and leaves the lane at the first
/// head that is idle, deferred or dead-lettered.
async fn work_lane(
    services: &Arc<OrderedRelayDeliveryServices>,
    authority: &Arc<crate::ingress::IngressAuthority>,
    lane: MailboxLaneId,
    target: &NodeIdentity,
) {
    for _ in 0..MAILBOX_LANE_DRAIN_LIMIT {
        let outcome = match authority.claim_room_acceptance(lane, target).await {
            Ok(MailboxLaneHead::Idle) => return,
            Ok(MailboxLaneHead::DeadLettered) => MailboxDeliveryOutcome::DeadLettered,
            Ok(MailboxLaneHead::Ready(claimed)) => {
                let decided = seat_room_occupancy(services, authority, &claimed.room, target).await
                    && accept(services, &claimed).await;
                if !decided {
                    // A commit refused for a sender without durable
                    // occupancy is retried once the actor has written its
                    // roster to the table again.
                    report_room_occupancy(services, &claimed.room).await;
                }
                record_attempt(authority, claimed.lane, claimed.operation, target, decided).await
            }
            Ok(MailboxLaneHead::ReadyRequest(claimed)) => {
                let decided =
                    seat_room_occupancy(services, authority, &claimed.request.room, target).await
                        && decide_request(services, authority, &claimed, target).await;
                record_attempt(authority, claimed.lane, claimed.operation, target, decided).await
            }
            Ok(MailboxLaneHead::ReadyDirect(claimed)) => {
                let decided = decide_direct(services, authority, &claimed, target).await;
                record_attempt(authority, claimed.lane, claimed.operation, target, decided).await
            }
            Err(error) => {
                tracing::warn!(%error, "mailbox poller could not read a lane head");
                MailboxDeliveryOutcome::Retried
            }
        };
        waddle_xmpp::telemetry::reliability::increment_mailbox_delivery(outcome);
        if outcome != MailboxDeliveryOutcome::Decided {
            return;
        }
    }
}

/// Record how an attempt at a lane head ended. A decided entry has its
/// owner obligations delivered; a failed attempt is counted and deferred, or
/// parked for an operator once its retry budget is spent.
async fn record_attempt(
    authority: &Arc<crate::ingress::IngressAuthority>,
    lane: MailboxLaneId,
    operation: MailboxOperationKey,
    target: &NodeIdentity,
    decided: bool,
) -> MailboxDeliveryOutcome {
    if decided {
        authority.deliver_owner_obligations(operation).await;
        return MailboxDeliveryOutcome::Decided;
    }
    match authority
        .record_room_acceptance_failure(lane, operation, target)
        .await
    {
        Ok(MailboxAttemptRecord::DeadLettered) => MailboxDeliveryOutcome::DeadLettered,
        // The commit landed although the attempt did not finish; its
        // effects were not executed here.
        Ok(MailboxAttemptRecord::Settled) => {
            authority.deliver_owner_obligations(operation).await;
            MailboxDeliveryOutcome::Decided
        }
        Ok(MailboxAttemptRecord::Deferred { .. }) | Err(_) => MailboxDeliveryOutcome::Retried,
    }
}

/// Seat `room`'s durable occupancy in this node's room actor, once per room
/// claim. `true` when the room is seated under its current claim, which
/// `target` must hold.
///
/// A node that took a room's claim has an actor without the occupants its
/// predecessor held in memory. Each row is seated through a join projection
/// (`SeedRecoveredOccupants`), which restores membership and nick, takes the
/// affiliation from the room's restored affiliation list and keeps the
/// recorded role while that affiliation still holds. A row whose connection
/// generation is no longer current is not seated, and the actor removes it
/// from the table when it reports its roster at the end of the same call.
/// Voice requests, pending mediated invitations, call and Muji session
/// state and rate-limit state are not stored and stay empty, so work that
/// depends on them is answered as for unknown state. A row that cannot be
/// parsed is not seated; its occupant's requests are refused until it joins
/// again.
async fn seat_room_occupancy(
    services: &Arc<OrderedRelayDeliveryServices>,
    authority: &Arc<crate::ingress::IngressAuthority>,
    room: &BareJid,
    target: &NodeIdentity,
) -> bool {
    let Some(state) = services.web_socket_state.upgrade() else {
        return false;
    };
    seat_room_occupancy_in(&state, authority, room, target).await
}

/// Seat `room`'s durable occupancy before this node serves a stanza of one
/// of its own connections from the room's actor under claim `epoch`.
///
/// The mailbox poller seats a room when it works an entry for it. A node
/// that took the room's claim another way (its orphan reaper, a steal, a
/// restart) has an actor that holds none of the recorded occupants until
/// then, and would refuse a groupchat stanza of an occupant connected to
/// it. Every path that reads the roster to decide a stanza therefore calls
/// this first; it costs one map lookup once the room is seated under the
/// claim. `true` when the room is seated, or when this deployment keeps no
/// durable occupancy.
pub(crate) async fn ensure_room_occupancy_seated(
    state: &crate::server::routes::websocket::WebSocketState,
    room: &BareJid,
    epoch: waddle_xmpp::ownership::ClaimEpoch,
) -> bool {
    let authority = &state.deps.protocol.ingress;
    if !authority.mailbox_acceptance_enabled() || authority.room_occupancy_seated(room, epoch) {
        return true;
    }
    if !authority.room_requests_carried_now().await {
        return true;
    }
    let Some(target) = authority.node_identity() else {
        return true;
    };
    seat_room_occupancy_in(state, authority, room, &target).await
}

async fn seat_room_occupancy_in(
    state: &crate::server::routes::websocket::WebSocketState,
    authority: &Arc<crate::ingress::IngressAuthority>,
    room: &BareJid,
    target: &NodeIdentity,
) -> bool {
    let (load, claim) = match authority.load_room_occupancy(room).await {
        Ok(loaded) => loaded,
        Err(error) => {
            tracing::warn!(%error, room = %room, "durable room occupancy not loaded");
            return false;
        }
    };
    let Some((owner, epoch)) = claim else {
        return false;
    };
    if owner != *target {
        return false;
    }
    if authority.room_occupancy_seated(room, epoch) {
        return true;
    }
    if load.unparseable > 0 {
        tracing::warn!(
            room = %room,
            unparseable = load.unparseable,
            "durable occupancy rows that cannot be parsed were not seated"
        );
    }
    let restored = state
        .deps
        .protocol
        .room_registry
        .ask(GetOrRestoreDurableRoom {
            room_jid: room.clone(),
        })
        .reply_timeout(OCCUPANCY_SEAT_TIMEOUT)
        .await;
    let actor = match restored {
        Ok(Some(actor)) => actor,
        // The room has no durable lifecycle: there is no actor to seat. With
        // rows that is a room nothing can be decided against yet; without
        // rows there is nothing to seat and nothing the actor could hold.
        Ok(None) if load.seated.is_empty() => {
            authority.mark_room_occupancy_seated(room.clone(), epoch);
            return true;
        }
        Ok(None) => return false,
        Err(error) => {
            tracing::warn!(%error, room = %room, "room not restored for occupancy seating");
            return false;
        }
    };
    if !load.seated.is_empty() {
        let occupants = load
            .seated
            .into_iter()
            .map(|record| RecoveredOccupant {
                real_jid: record.occupant,
                nick: record.nick,
                role: record.role,
                affiliation: record.affiliation,
                session: record.generation,
                seat: record.seat,
            })
            .collect();
        match actor
            .ask(SeedRecoveredOccupants {
                occupants,
                local_domain: state.deps.auth_state.xmpp_domain.clone(),
            })
            .reply_timeout(OCCUPANCY_SEAT_TIMEOUT)
            .await
        {
            Ok(seating) => {
                tracing::info!(
                    room = %room,
                    seated = seating.seated,
                    discarded = seating.discarded,
                    "seated recovered room occupants"
                );
                if !seating.recorded {
                    return false;
                }
            }
            Err(error) => {
                tracing::warn!(%error, room = %room, "recovered room occupants not seated");
                return false;
            }
        }
    }
    // The room counts as seated only once the table names every occupant
    // the actor holds, including those that joined before this deployment
    // kept durable occupancy. The owner's groupchat commit relies on that.
    match actor
        .ask(ReportOccupancy)
        .reply_timeout(OCCUPANCY_SEAT_TIMEOUT)
        .await
    {
        Ok(true) => {}
        Ok(false) => return false,
        Err(error) => {
            tracing::warn!(%error, room = %room, "room occupancy not reported");
            return false;
        }
    }
    authority.mark_room_occupancy_seated(room.clone(), epoch);
    true
}

/// Ask `room`'s actor to write its roster to the durable occupancy record.
async fn report_room_occupancy(services: &Arc<OrderedRelayDeliveryServices>, room: &BareJid) {
    let Some(state) = services.web_socket_state.upgrade() else {
        return;
    };
    let restored = state
        .deps
        .protocol
        .room_registry
        .ask(GetOrRestoreDurableRoom {
            room_jid: room.clone(),
        })
        .reply_timeout(OCCUPANCY_SEAT_TIMEOUT)
        .await;
    if let Ok(Some(actor)) = restored {
        if let Err(error) = actor
            .ask(ReportOccupancy)
            .reply_timeout(OCCUPANCY_SEAT_TIMEOUT)
            .await
        {
            tracing::warn!(%error, room = %room, "room occupancy not reported");
        }
    }
}

/// The role and affiliation the room gave the requester, read from the
/// self-presence among `responses`, with that presence.
fn seated_as(
    responses: &[Stanza],
) -> Option<(String, Role, Affiliation, xmpp_parsers::presence::Presence)> {
    use xmpp_parsers::muc::user::{
        Affiliation as WireAffiliation, MucUser, Role as WireRole, Status,
    };
    responses.iter().find_map(|stanza| {
        let Stanza::Presence(presence) = stanza else {
            return None;
        };
        let user = presence
            .payloads
            .iter()
            .find_map(|payload| MucUser::try_from(payload.clone()).ok())?;
        if !user.status.contains(&Status::SelfPresence) {
            return None;
        }
        let item = user.items.first()?;
        let nick = presence.from.as_ref()?.resource()?.as_str().to_owned();
        let role = match item.role {
            WireRole::Moderator => Role::Moderator,
            WireRole::Participant => Role::Participant,
            WireRole::Visitor => Role::Visitor,
            WireRole::None => Role::None,
        };
        let affiliation = match item.affiliation {
            WireAffiliation::Owner => Affiliation::Owner,
            WireAffiliation::Admin => Affiliation::Admin,
            WireAffiliation::Member => Affiliation::Member,
            WireAffiliation::Outcast => Affiliation::Outcast,
            WireAffiliation::None => Affiliation::None,
        };
        Some((nick, role, affiliation, presence.clone()))
    })
}

/// The owner's refusal of a request whose requester has an occupancy row
/// that cannot be parsed: the occupant is not seated and must join again.
fn refuse_unseated(request: &StoredRoomRequest) -> RoomRequestOutcome {
    RoomRequestOutcome {
        decision: MailboxOwnerDecision::Rejected,
        responses: room_request_error(
            &request.stanza,
            &request.room,
            &request.occupant,
            ErrorType::Modify,
            DefinedCondition::NotAcceptable,
        ),
        occupancy: RoomOccupancyChange::Unchanged,
    }
}

/// Run one room request against this node's room and decide it. `true`
/// when the decision committed.
///
/// The stored stanza goes through the handler the ordered relay uses for the
/// same kind, so the room actor stays the single writer of room state and
/// produces the reply in the order the requester must receive it: for a
/// join, the other occupants' presence, the self-presence with its status
/// codes, history and the subject. That reply is frozen as the response.
/// Presence to the other occupants is fanned out by the handler itself and
/// is not part of the response.
///
/// A presence update or leave whose generation is not the one seated for
/// the full JID comes from an earlier connection: it is accepted without
/// running the handler and answered with nothing.
async fn decide_request(
    services: &Arc<OrderedRelayDeliveryServices>,
    authority: &Arc<crate::ingress::IngressAuthority>,
    claimed: &ClaimedRoomRequest,
    target: &NodeIdentity,
) -> bool {
    let request = &claimed.request;
    let occupancy = match authority.room_request_occupancy(request).await {
        Ok(occupancy) => occupancy,
        Err(error) => {
            tracing::warn!(%error, "room request occupancy not read");
            return false;
        }
    };
    let stale = matches!(
        (&occupancy, request.kind),
        (RoomOccupantLookup::Seated(row), RoomRequestKind::Presence | RoomRequestKind::Leave)
            if row.generation != request.generation
    );
    let unseated = matches!(occupancy, RoomOccupantLookup::Unparseable)
        && request.kind != RoomRequestKind::Join;
    let outcome = if stale {
        RoomRequestOutcome {
            decision: MailboxOwnerDecision::Accepted,
            responses: Vec::new(),
            occupancy: RoomOccupancyChange::Unchanged,
        }
    } else if unseated {
        refuse_unseated(request)
    } else {
        let kind = match request.kind {
            RoomRequestKind::Join => OrderedRelayMucProxyKind::JoinPresence,
            RoomRequestKind::Presence | RoomRequestKind::Leave => {
                OrderedRelayMucProxyKind::OccupantPresence
            }
            RoomRequestKind::Iq => OrderedRelayMucProxyKind::MujiJingleIq,
        };
        let mut completion = None;
        let frames = match deliver_reserved_muc_proxy(
            services,
            &request.room,
            kind,
            MucProxyOrigin::Connection(request.generation),
            &request.stanza,
            None,
            &mut completion,
        )
        .await
        {
            Ok(frames) => frames,
            Err(reason) => {
                tracing::warn!(?reason, room = %request.room, "room request handler did not finish");
                return false;
            }
        };
        let responses: Vec<Stanza> = frames.into_iter().map(|frame| frame.stanza.0).collect();
        outcome_of(request, &occupancy, responses)
    };
    #[cfg(feature = "test-support")]
    crate::clustering::acceptance_seams::hold_owner_request(
        crate::clustering::acceptance_seams::OwnerAcceptancePoint::BeforeCommit,
        &request.stanza,
    )
    .await;
    let decided = authority
        .decide_room_request(claimed, target, outcome)
        .await;
    #[cfg(feature = "test-support")]
    if decided.is_ok() {
        crate::clustering::acceptance_seams::hold_owner_request(
            crate::clustering::acceptance_seams::OwnerAcceptancePoint::AfterCommit,
            &request.stanza,
        )
        .await;
    }
    match decided {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(%error, room = %request.room, "room request decision not committed");
            false
        }
    }
}

/// The decision and occupancy change a handler's reply stands for.
fn outcome_of(
    request: &StoredRoomRequest,
    occupancy: &RoomOccupantLookup,
    responses: Vec<Stanza>,
) -> RoomRequestOutcome {
    let refused = match request.kind {
        RoomRequestKind::Join | RoomRequestKind::Presence | RoomRequestKind::Leave => {
            presence_request_refused(&responses)
        }
        RoomRequestKind::Iq => responses.iter().any(|stanza| {
            matches!(stanza, Stanza::Iq(iq)
                if matches!(**iq, xmpp_parsers::iq::Iq::Error { .. }))
        }),
    };
    if refused {
        return RoomRequestOutcome {
            decision: MailboxOwnerDecision::Rejected,
            responses,
            occupancy: RoomOccupancyChange::Unchanged,
        };
    }
    let requested_nick = match &request.stanza {
        Stanza::Presence(presence) => presence
            .to
            .as_ref()
            .and_then(|to| to.resource())
            .map(|resource| resource.as_str().to_owned()),
        Stanza::Message(_) | Stanza::Iq(_) => None,
    };
    let change = match request.kind {
        RoomRequestKind::Iq => RoomOccupancyChange::Unchanged,
        RoomRequestKind::Leave => RoomOccupancyChange::Remove,
        RoomRequestKind::Join => match (seated_as(&responses), requested_nick) {
            (Some((nick, role, affiliation, presence)), _) => RoomOccupancyChange::Seat {
                nick,
                role,
                affiliation,
                presence: Some(presence),
            },
            // The room accepted the join without a self-presence to read
            // the grant from; seat the requester as an unaffiliated
            // participant, the least a seated occupant can be.
            (None, Some(nick)) => RoomOccupancyChange::Seat {
                nick,
                role: Role::Participant,
                affiliation: Affiliation::None,
                presence: None,
            },
            (None, None) => RoomOccupancyChange::Unchanged,
        },
        RoomRequestKind::Presence => match (seated_as(&responses), occupancy) {
            (Some((nick, role, affiliation, presence)), _) => RoomOccupancyChange::Update {
                nick,
                role,
                affiliation,
                presence: Some(presence),
            },
            (None, RoomOccupantLookup::Seated(row)) => RoomOccupancyChange::Update {
                nick: row.nick.clone(),
                role: row.role,
                affiliation: row.affiliation,
                presence: row.presence.clone(),
            },
            (None, RoomOccupantLookup::Absent | RoomOccupantLookup::Unparseable) => {
                RoomOccupancyChange::Unchanged
            }
        },
    };
    RoomRequestOutcome {
        decision: MailboxOwnerDecision::Accepted,
        responses,
        occupancy: change,
    }
}

/// Run the owner's relayed-groupchat commit for one entry. `true` when the
/// owner committed a decision.
///
/// Frames the owner produces for the sender are not written here: their
/// receipts stay pending, and [`IngressAuthority::deliver_owner_obligations`]
/// delivers them through the sender's keyed route right after this returns.
///
/// [`IngressAuthority::deliver_owner_obligations`]: crate::ingress::IngressAuthority::deliver_owner_obligations
async fn accept(
    services: &Arc<OrderedRelayDeliveryServices>,
    claimed: &ClaimedRoomAcceptance,
) -> bool {
    let mut completion = None;
    deliver_reserved_muc_proxy(
        services,
        &claimed.room,
        OrderedRelayMucProxyKind::GroupchatMessage,
        MucProxyOrigin::Server,
        &Stanza::Message(claimed.message.clone()),
        Some(&claimed.admission),
        &mut completion,
    )
    .await
    .is_ok()
}
