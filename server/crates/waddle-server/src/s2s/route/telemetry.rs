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
