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
