//! Service Discovery: disco#info handling.

pub use waddle_xmpp_core::disco::info::{
    build_disco_info_response, build_disco_info_response_with_extensions,
    community_service_features, is_disco_info_query, muc_room_features, muc_service_features,
    parse_disco_info_response, pubsub_service_features, push_service_features,
    spaces_service_features, upload_service_features, DiscoInfoQuery, DiscoInfoResponse, Feature,
    Identity, DISCO_INFO_NS,
};
use xmpp_parsers::iq::Iq;

use crate::XmppError;

/// Parse a disco#info query from an IQ stanza.
pub fn parse_disco_info_query(iq: &Iq) -> Result<DiscoInfoQuery, XmppError> {
    waddle_xmpp_core::disco::info::parse_disco_info_query(iq).map_err(Into::into)
}

/// Server-level disco features, extended with the XEP-0430 inbox
/// advertisement (canonical `urn:xmpp:inbox:0` namespace).
pub fn server_features() -> Vec<Feature> {
    let mut features = waddle_xmpp_core::disco::info::server_features();
    features.push(Feature::new(crate::xep::xep0430::NS_INBOX));
    // Admin V1 — XEP-0050 ad-hoc command for listing users with a
    // prefix filter (owner-gated). The XEP-0050 node identifier
    // doubles as the disco feature URI so clients can detect support
    // without enumerating commands first; if the server doesn't ship
    // admin, this advert is absent and the chat client's admin panel
    // can fall back to the "Admin only" empty state.
    features.push(Feature::new(crate::admin::NS_ADMIN_USERS_LIST));
    // Admin V2 — XEP-0050 ad-hoc commands for Spaces + Channels CRUD.
    // Each node identifier is an owner-gated `<command/>` URI plus the
    // disco feature URI so clients can detect support without enumerating
    // commands first. The handlers are registered alongside admin V1's
    // `users:list` in the WebSocket command registry.
    features.push(Feature::new(crate::admin::NS_ADMIN_SPACES_LIST));
    features.push(Feature::new(crate::admin::NS_ADMIN_SPACES_CREATE));
    features.push(Feature::new(crate::admin::NS_ADMIN_SPACES_UPDATE));
    features.push(Feature::new(crate::admin::NS_ADMIN_SPACES_DELETE));
    features.push(Feature::new(crate::admin::NS_ADMIN_SPACES_MEMBERS));
    features.push(Feature::new(crate::admin::NS_ADMIN_SPACES_SET_ROLE));
    features.push(Feature::new(crate::admin::NS_ADMIN_CHANNELS_LIST));
    features.push(Feature::new(crate::admin::NS_ADMIN_CHANNELS_CREATE));
    features.push(Feature::new(crate::admin::NS_ADMIN_CHANNELS_UPDATE));
    features.push(Feature::new(crate::admin::NS_ADMIN_CHANNELS_DELETE));
    features.push(Feature::new(crate::admin::NS_ADMIN_CHANNELS_OCCUPANTS));
    features.push(Feature::new(crate::admin::NS_ADMIN_CHANNELS_AFFILIATIONS));
    features.push(Feature::new(
        crate::admin::NS_ADMIN_CHANNELS_SET_AFFILIATION,
    ));
    features.push(Feature::new(crate::admin::NS_ADMIN_CHANNELS_KICK));
    features
}

/// XMPP-native A/V calling feature namespaces. Returned as a separate
/// list so disco-info builders only emit them when the
/// LiveKit-backed handlers are actually registered at startup. Each
/// namespace is covered by a custom test suite (per the CLAUDE.md
/// "advertised feature must have testable behavior" rule) and a real
/// handler — see [`crate::protocol::handlers::jingle`],
/// [`crate::protocol::handlers::extdisco`], and the typed XEP
/// modules under `crate::xep`.
pub fn call_features() -> Vec<Feature> {
    // `Feature::waddle_muc_call()` is advertised once
    // `register_call_handlers` has wired the `MucCallHandler` IQ
    // surface — that handler is the backing behavior for the
    // namespace (mints per-room join tokens, registers participants
    // in the SFU registry, and unwinds via request-leave). The
    // `<call xmlns='urn:waddle:muc-call:0'/>` presence extension is
    // a permissive marker that flows through the MUC presence
    // pipeline unchanged; chat-side parsers ignore malformed shapes.
    vec![
        Feature::jingle(),
        Feature::jingle_rtp(),
        Feature::jingle_rtp_audio(),
        Feature::jingle_rtp_video(),
        Feature::jingle_message(),
        Feature::ext_disco(),
        Feature::waddle_livekit_transport(),
        Feature::waddle_muc_call(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_features_omit_call_namespaces_by_default() {
        // The base list is used by tests that don't register call
        // handlers; advertising A/V features there would be a lie.
        let features = server_features();
        for feature in call_features() {
            assert!(
                !features.contains(&feature),
                "{} leaked into server_features() — gating must be done by callers, not here",
                feature.0
            );
        }
    }

    #[test]
    fn call_features_returns_eight_canonical_namespaces() {
        let features = call_features();
        let expected = [
            "urn:xmpp:jingle:1",
            "urn:xmpp:jingle:apps:rtp:1",
            "urn:xmpp:jingle:apps:rtp:audio",
            "urn:xmpp:jingle:apps:rtp:video",
            "urn:xmpp:jingle-message:0",
            "urn:xmpp:extdisco:2",
            "urn:waddle:transports:livekit:0",
            "urn:waddle:muc-call:0",
        ];
        assert_eq!(features.len(), expected.len());
        for ns in expected {
            assert!(
                features.contains(&Feature::new(ns)),
                "call_features() missing {ns}"
            );
        }
    }

    #[test]
    fn call_features_advertises_muc_call_with_backing_handler() {
        // urn:waddle:muc-call:0 IQ surface is implemented by
        // `MucCallHandler` (request-join / request-leave). Advertising
        // it is now correct per the CLAUDE.md rule "advertised feature
        // must have testable backing behavior."
        let features = call_features();
        assert!(features.contains(&Feature::waddle_muc_call()));
    }
}
