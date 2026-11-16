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
