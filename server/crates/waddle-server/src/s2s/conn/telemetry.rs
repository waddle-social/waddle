//! S2S connection metrics. Attributes come from the telemetry allowlist;
//! no metric carries a domain, an address, a JID or a stream id.

use std::time::Duration;

use waddle_xmpp::telemetry::attributes::{
    S2sDialbackResult, S2sDialbackRole, S2sDirection, S2sOutcome, S2sResource, S2sStrength,
    S2sTransport,
};
use waddle_xmpp::{counter_add, gauge_record, histogram_record};

use super::peer_stream::{AuthenticationStrength, StreamDirection};
use super::registry::Registry;

fn attribute(direction: StreamDirection) -> S2sDirection {
    match direction {
        StreamDirection::Inbound => S2sDirection::Inbound,
        StreamDirection::Outbound => S2sDirection::Outbound,
    }
}

/// A connection ended with `outcome`.
pub(super) fn ended(direction: StreamDirection, outcome: S2sOutcome) {
    counter_add!(
        "waddle.s2s.connections",
        "{connection}",
        "S2S connections that ended, by direction and outcome.",
        1,
        attribute(direction),
        outcome,
    );
}

/// A limit refused a socket or a stream.
pub(super) fn capacity_refused(direction: StreamDirection, resource: S2sResource) {
    counter_add!(
        "waddle.s2s.capacity.refusals",
        "{connection}",
        "S2S sockets and streams refused by a connection limit, by resource.",
        1,
        attribute(direction),
        resource,
    );
}

/// A stream was established `elapsed` after its TCP connection opened.
pub(super) fn established(
    direction: StreamDirection,
    transport: S2sTransport,
    strength: AuthenticationStrength,
    elapsed: Duration,
    registry: &Registry,
) {
    let strength = match strength {
        AuthenticationStrength::Pkix => S2sStrength::Pkix,
        AuthenticationStrength::Dialback => S2sStrength::Dialback,
    };
    counter_add!(
        "waddle.s2s.established",
        "{stream}",
        "S2S streams that completed negotiation over TLS, by how the pair authenticated.",
        1,
        attribute(direction),
        transport,
        strength,
    );
    histogram_record!(
        "waddle.s2s.negotiation.duration",
        "ms",
        "Time from TCP accept or connect to an established S2S stream.",
        elapsed.as_secs_f64() * 1000.0,
        attribute(direction),
    );
    stream_counts(registry);
}

/// Publish the number of established streams in each direction.
pub(super) fn stream_counts(registry: &Registry) {
    for direction in [StreamDirection::Inbound, StreamDirection::Outbound] {
        let count = u64::try_from(registry.count(direction)).unwrap_or(u64::MAX);
        gauge_record!(
            "waddle.s2s.streams",
            "{stream}",
            "Established S2S streams, by direction.",
            count,
            attribute(direction),
        );
    }
}

/// A dialback exchange in `role` concluded with `result`.
pub(super) fn dialback(role: S2sDialbackRole, result: S2sDialbackResult) {
    counter_add!(
        "waddle.s2s.dialback",
        "{exchange}",
        "S2S Server Dialback exchanges, by this server's role and the result.",
        1,
        role,
        result,
    );
}
