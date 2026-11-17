//! The router's metrics. No attribute names a JID, a domain or a stanza id.

use waddle_xmpp::counter_add;

/// A presence from an S2S stream carried an address outside the identity
/// profile, or was not valid presence, and was dropped without an answer.
pub(super) fn unusable_presence_dropped() {
    counter_add!(
        "waddle.s2s.route.unusable_presence",
        "{stanza}",
        "Presence from S2S streams dropped unanswered because an address it carries is outside the identity profile or it is not valid presence.",
        1,
    );
}

/// A stanza from an S2S stream that may be a remote room's traffic for a
/// local resource found its room's queue, its worker's queue or its remote
/// domain's share of it full, and the stream stopped reading until it fit.
pub(super) fn room_traffic_waited() {
    counter_add!(
        "waddle.s2s.route.room_traffic.waits",
        "{stanza}",
        "Stanzas from S2S streams to local full JIDs that found the room delivery queues full, so that their stream stopped reading until they fit.",
        1,
    );
}

/// A stanza that waited for a place on the room delivery queues found none
/// within the wait's deadline: it was lost and its stream was closed.
pub(super) fn room_traffic_overloaded() {
    counter_add!(
        "waddle.s2s.route.room_traffic.overloaded",
        "{stanza}",
        "Stanzas from S2S streams to local full JIDs that found no place on the room delivery queues within the wait's deadline, so that their stream was closed with resource-constraint.",
        1,
    );
}

/// Delivering one stanza of a remote room's traffic to its local occupant
/// took longer than the delivery timeout, and the worker moved on.
pub(super) fn room_delivery_timed_out() {
    counter_add!(
        "waddle.s2s.route.room_traffic.delivery_timeouts",
        "{stanza}",
        "Deliveries of remote room traffic to local occupants abandoned after the delivery timeout.",
        1,
    );
}

/// A stanza that may be a remote room's traffic for a local resource was
/// dropped unanswered because reading its occupancy from the shared store
/// failed, so whether it is room traffic could not be decided.
pub(super) fn room_traffic_store_failed() {
    counter_add!(
        "waddle.s2s.route.room_traffic.store_failures",
        "{stanza}",
        "Stanzas from S2S streams to local full JIDs dropped unanswered because the remote room occupancy store failed while deciding whether they are room traffic.",
        1,
    );
}

/// A stanza for the MUC service found its room's queue, its worker's queue
/// or its remote domain's share or rate exhausted, and the stream stopped
/// reading until it fit.
pub(super) fn room_request_waited() {
    counter_add!(
        "waddle.s2s.route.room_requests.waits",
        "{stanza}",
        "Stanzas from S2S streams to the MUC service that found the room request queues or their domain's request rate exhausted, so that their stream stopped reading until they fit.",
        1,
    );
}

/// A stanza for the MUC service found no place within the wait's deadline:
/// it was lost and its stream was closed.
pub(super) fn room_request_overloaded() {
    counter_add!(
        "waddle.s2s.route.room_requests.overloaded",
        "{stanza}",
        "Stanzas from S2S streams to the MUC service that found no place on the room request queues within the wait's deadline, so that their stream was closed with resource-constraint.",
        1,
    );
}

/// The room host did not decide a remote principal's request in time, and
/// the worker moved on without an answer.
pub(super) fn room_request_timed_out() {
    counter_add!(
        "waddle.s2s.route.room_requests.timeouts",
        "{stanza}",
        "Requests of remote principals to the MUC service abandoned after the room host's deadline.",
        1,
    );
}

/// A local room's stanza for a remote JID was not sent: the domain is no
/// longer admitted, the JID holds no seat here any more, or the stanza was
/// not addressed from the MUC service to the entity it answers.
pub(super) fn seat_traffic_refused() {
    counter_add!(
        "waddle.s2s.route.seat_traffic.refused",
        "{stanza}",
        "Stanzas of local rooms for remote JIDs refused when they were to be sent, because the remote domain is not admitted, the JID holds no seat or the stanza is misaddressed.",
        1,
    );
}

/// A remote occupant's seat ended because a stanza of its room could not
/// be handed to the stream for its domain.
pub(super) fn seat_ended_undeliverable() {
    counter_add!(
        "waddle.s2s.route.seats.ended_undeliverable",
        "{seat}",
        "Seats of remote occupants ended because a stanza of their room found no place for the remote domain or could not be handed to its stream.",
        1,
    );
}

/// A remote occupant's seat ended because a stream of its domain pair
/// failed.
pub(super) fn seat_ended_stream_lost() {
    counter_add!(
        "waddle.s2s.route.seats.ended_stream_lost",
        "{seat}",
        "Seats of remote occupants ended because a stream between their domain and the MUC service failed.",
        1,
    );
}

/// A remote occupant's seat ended because its room stopped being open to
/// accounts at other servers.
pub(super) fn seat_ended_room_closed() {
    counter_add!(
        "waddle.s2s.route.seats.ended_room_closed",
        "{seat}",
        "Seats of remote occupants ended because their room's federated setting was turned off.",
        1,
    );
}

/// A stanza for a room another replica holds was carried to that replica.
pub(super) fn room_request_carried() {
    counter_add!(
        "waddle.s2s.route.room_requests.carried",
        "{stanza}",
        "Stanzas from S2S streams for rooms another replica holds that were carried to that replica with their sender's stream evidence.",
        1,
    );
}

/// A stanza for a room another replica holds could not be carried there,
/// and its sender was answered with an error.
pub(super) fn room_request_not_carried() {
    counter_add!(
        "waddle.s2s.route.room_requests.not_carried",
        "{stanza}",
        "Stanzas from S2S streams for rooms another replica holds that could not be carried to that replica, so that their sender was answered with an error.",
        1,
    );
}

/// A stanza for the MUC service was not taken because what already waited
/// for its room filled the room's share of the queues; it was answered as
/// its kind is, and its stream went on.
pub(super) fn room_request_room_full() {
    counter_add!(
        "waddle.s2s.route.room_requests.room_full",
        "{stanza}",
        "Stanzas from S2S streams to the MUC service not taken because their room's share of the room request queues was full.",
        1,
    );
}

/// A stanza for the MUC service was not taken because its account had
/// spent its share of its domain's request rate.
pub(super) fn room_request_account_limited() {
    counter_add!(
        "waddle.s2s.route.room_requests.account_limited",
        "{stanza}",
        "Stanzas from S2S streams to the MUC service not taken because their account had spent its share of its domain's request rate.",
        1,
    );
}

/// A join for the MUC service was beyond its account's share of presence
/// after the account's allowance of answered refusals was spent, and was
/// dropped unanswered.
pub(super) fn room_join_dropped() {
    counter_add!(
        "waddle.s2s.route.room_requests.joins_dropped",
        "{stanza}",
        "Joins from S2S streams to the MUC service dropped unanswered because their account had spent its share of presence and its allowance of answered refusals.",
        1,
    );
}

/// A leave, an error or presence the host ignores was beyond its
/// account's allowance of such stanzas, and was dropped.
pub(super) fn room_request_exempt_dropped() {
    counter_add!(
        "waddle.s2s.route.room_requests.exempt_dropped",
        "{stanza}",
        "Leaves, errors and ignored presence from S2S streams to the MUC service dropped because their account had spent its allowance of such stanzas.",
        1,
    );
}

/// A leave or an error that ends a seat could not be placed, and was kept
/// to be placed again.
pub(super) fn room_leave_kept() {
    counter_add!(
        "waddle.s2s.route.room_requests.leaves_kept",
        "{stanza}",
        "Leaves and errors from S2S streams for rooms another replica holds that could not be placed, kept to be placed again.",
        1,
    );
}

/// A leave or an error that ends a seat could not be placed and could not
/// be kept, or was kept until its time passed, and was dropped.
pub(super) fn room_leave_dropped() {
    counter_add!(
        "waddle.s2s.route.room_requests.leaves_dropped",
        "{stanza}",
        "Leaves and errors from S2S streams for rooms another replica holds dropped because they could not be placed or kept to be placed again in time.",
        1,
    );
}

/// The replica that holds a room did not answer within the placement
/// timeout, and the request's sender was answered with an error.
pub(super) fn room_placement_timed_out() {
    counter_add!(
        "waddle.s2s.route.room_requests.placement_timeouts",
        "{stanza}",
        "Stanzas from S2S streams for rooms another replica holds whose holder did not answer within the placement timeout.",
        1,
    );
}

/// The replica that holds a room said through the room host that it did
/// not answer, or could not be reached, within the host's bound on asking
/// it, and the request's sender was answered with an error.
pub(super) fn room_placement_silent() {
    counter_add!(
        "waddle.s2s.route.room_requests.placement_silent",
        "{stanza}",
        "Stanzas from S2S streams for rooms another replica holds whose holder did not answer or could not be reached within the room host's bound on asking it.",
        1,
    );
}

/// A request for a room found no place among the rooms being placed within
/// the placement timeout, and its sender was answered with an error. The
/// room's holder was not asked.
pub(super) fn room_placement_crowded() {
    counter_add!(
        "waddle.s2s.route.room_requests.placement_crowded",
        "{stanza}",
        "Stanzas from S2S streams for rooms that found no place among the rooms being placed within the placement timeout, so that their sender was answered with an error.",
        1,
    );
}

/// A join was refused because no queue for the occupant's domain could be
/// opened or it was full, so the room's stanzas for the seat could not
/// leave.
pub(super) fn seat_refused_unreachable() {
    counter_add!(
        "waddle.s2s.route.seats.refused_unreachable",
        "{seat}",
        "Joins of remote occupants refused because no place for the room's stanzas to their domain was free.",
        1,
    );
}

/// A copy of a chat state or a marker for a remote seat was dropped: its
/// room or its sender had spent its allowance of them, or the queue for
/// the seat's domain was filling.
pub(super) fn seat_ephemeral_dropped() {
    counter_add!(
        "waddle.s2s.route.seat_traffic.ephemeral_dropped",
        "{stanza}",
        "Copies of chat states and markers for remote seats dropped because their room or sender had spent its allowance or the queue for the remote domain was filling.",
        1,
    );
}

/// A room's presence that tells an occupant its occupancy ended was
/// dropped: the room still seats the occupant, so the presence was for an
/// earlier occupancy.
pub(super) fn seat_departure_stale() {
    counter_add!(
        "waddle.s2s.route.seat_traffic.stale_departures",
        "{stanza}",
        "Presence of local rooms ending a remote occupant's earlier occupancy, dropped because the room seats the occupant under a later one.",
        1,
    );
}

/// A remote occupant's seat ended because its room no longer seated the
/// occupant when the seats were checked against their rooms.
pub(super) fn seat_ended_unseated() {
    counter_add!(
        "waddle.s2s.route.seats.ended_unseated",
        "{seat}",
        "Seats of remote occupants ended because their room no longer seated the occupant.",
        1,
    );
}

/// A room seated an occupant of another server for which this replica
/// held no seat, and was asked to remove it.
pub(super) fn seat_ghost_removed() {
    counter_add!(
        "waddle.s2s.route.seats.ghosts_removed",
        "{occupant}",
        "Occupants of other servers removed from local rooms because the replica that holds the room held no seat for them.",
        1,
    );
}

/// A remote occupant's seat ended because this replica took it over from
/// one that stopped.
pub(super) fn seat_ended_holder_stopped() {
    counter_add!(
        "waddle.s2s.route.seats.ended_holder_stopped",
        "{seat}",
        "Seats of remote occupants ended after this replica took them over from a replica that stopped.",
        1,
    );
}

/// A row of the seat table that named no seat this replica holds was
/// removed: its removal had failed when its seat ended.
pub(super) fn seat_row_reclaimed() {
    counter_add!(
        "waddle.s2s.route.seats.rows_reclaimed",
        "{seat}",
        "Rows of the seat table removed because the replica that owns them holds no seat for them.",
        1,
    );
}

/// The account side took longer than its deadline to decide one stanza
/// for a local account, and the worker moved on without answering it.
pub(super) fn account_traffic_timed_out() {
    counter_add!(
        "waddle.s2s.route.account_traffic.timeouts",
        "{stanza}",
        "Stanzas from S2S streams to local accounts abandoned because the account side did not decide them before the deadline.",
        1,
    );
}

/// The account side answered one stanza with more replies than the router
/// sends for one stanza; `count` were dropped.
pub(super) fn account_replies_dropped(count: usize) {
    counter_add!(
        "waddle.s2s.route.account_traffic.replies_dropped",
        "{stanza}",
        "Replies to remote senders the account side produced beyond the bound for one received stanza, dropped unsent.",
        u64::try_from(count).unwrap_or(u64::MAX),
    );
}

/// A remote IQ request for a local resource was refused with
/// `resource-constraint` because its domain, or the replica, had too many
/// such requests pending.
pub(super) fn account_request_refused() {
    counter_add!(
        "waddle.s2s.route.account_requests.refused",
        "{stanza}",
        "IQ requests from S2S streams to local resources refused because too many were pending from their domain or on the replica.",
        1,
    );
}

/// A local account's message or presence for a remote JID was refused
/// because the account's share of the pair queues was full.
pub(super) fn account_share_full() {
    counter_add!(
        "waddle.s2s.route.account_traffic.share_full",
        "{stanza}",
        "Messages and presence of local accounts for remote JIDs refused because the account's share of that pair's queue was full.",
        1,
    );
}

/// A request for the Spaces PubSub service was not taken: its account's
/// share or its domain's allowance of requests was spent.
pub(super) fn space_request_limited() {
    counter_add!(
        "waddle.s2s.spaces.requests.limited",
        "{stanza}",
        "Requests from S2S streams for the Spaces PubSub service that were not taken because their account's share or their domain's allowance of requests was spent, answered with resource-constraint or dropped.",
        1,
    );
}

/// The Spaces host did not decide a request within the router's bound: the
/// worker moved on and the answer, if one comes, is dropped.
pub(super) fn space_request_timed_out() {
    counter_add!(
        "waddle.s2s.spaces.requests.timed_out",
        "{stanza}",
        "Requests from S2S streams for the Spaces PubSub service that the Spaces host did not decide within the router's bound, so that no answer was sent.",
        1,
    );
}

/// An event notification of a local Space was queued for a subscriber at
/// another server.
pub(super) fn space_event_queued() {
    counter_add!(
        "waddle.s2s.spaces.events.queued",
        "{stanza}",
        "Event notifications of local Spaces queued for subscribers at other servers.",
        1,
    );
}

/// An event notification of a local Space for a subscriber at another
/// server was dropped: its domain is not admitted now, its pair could open
/// no queue, or the pair's share of event notifications was full.
pub(super) fn space_event_dropped() {
    counter_add!(
        "waddle.s2s.spaces.events.dropped",
        "{stanza}",
        "Event notifications of local Spaces for subscribers at other servers dropped because the subscriber's domain is not admitted now, its domain pair could open no queue or held its share of queued notifications.",
        1,
    );
}
