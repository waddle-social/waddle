use super::*;

/// Flagship delivered-message accounting for the direct remote-resource
/// path: it bypasses the counted owner-node delivery channels AND the
/// deliberately uncounted socket endpoint, so the counter must be
/// bumped from this seam — once, on the owner node, upon the socket
/// node's acknowledgment.
pub(super) fn record_remote_resource_delivered(stanza: &Stanza) {
    if let Some(message_kind) = waddle_xmpp::telemetry::messages::delivered_message_kind(stanza) {
        waddle_xmpp::telemetry::messages::record_delivered_message(message_kind);
    }
}
