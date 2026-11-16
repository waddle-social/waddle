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
