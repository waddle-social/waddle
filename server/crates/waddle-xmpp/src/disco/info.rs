//! Service Discovery: disco#info handling.

use crate::admin::NS_ADMIN_CHANNELS_AFFILIATIONS;
use crate::admin::NS_ADMIN_CHANNELS_CREATE;
use crate::admin::NS_ADMIN_CHANNELS_DELETE;
use crate::admin::NS_ADMIN_CHANNELS_KICK;
use crate::admin::NS_ADMIN_CHANNELS_LIST;
use crate::admin::NS_ADMIN_CHANNELS_OCCUPANTS;
use crate::admin::NS_ADMIN_CHANNELS_SET_AFFILIATION;
use crate::admin::NS_ADMIN_CHANNELS_UPDATE;
use crate::admin::NS_ADMIN_SPACES_CREATE;
use crate::admin::NS_ADMIN_SPACES_DELETE;
use crate::admin::NS_ADMIN_SPACES_LIST;
use crate::admin::NS_ADMIN_SPACES_MEMBERS;
use crate::admin::NS_ADMIN_SPACES_SET_ROLE;
use crate::admin::NS_ADMIN_SPACES_UPDATE;
use crate::admin::NS_ADMIN_USERS_LIST;
use crate::admin::NS_GROUP_DM_CREATE;
use crate::admin::NS_GROUP_DM_LEAVE;
use crate::xep::xep0430::NS_INBOX;
pub use waddle_xmpp_core::disco::info::{
    build_disco_info_response, build_disco_info_response_with_extensions,
    community_service_features, is_disco_info_query, muc_room_features, muc_service_features,
    parse_disco_info_response, pubsub_service_features, push_service_features,
    spaces_service_features, upload_service_features, DiscoInfoQuery, DiscoInfoResponse, Feature,
    Identity, RoomMembership, RoomPersistence, RoomVisibility, DISCO_INFO_NS,
};
use xmpp_parsers::iq::Iq;

use crate::XmppError;

/// Parse a disco#info query from an IQ stanza.
pub fn parse_disco_info_query(iq: &Iq) -> Result<DiscoInfoQuery, XmppError> {
    waddle_xmpp_core::disco::info::parse_disco_info_query(iq).map_err(Into::into)
}

/// Server-level disco features, extended with the XEP-0430 inbox
/// advertisement (canonical `urn:xmpp:inbox:1` namespace).
pub fn server_features() -> Vec<Feature> {
    let mut features = waddle_xmpp_core::disco::info::server_features();
    features.push(Feature::new(NS_INBOX));
    // Admin V1 — XEP-0050 ad-hoc command for listing users with a
    // prefix filter (owner-gated). The XEP-0050 node identifier
    // doubles as the disco feature URI so clients can detect support
    // without enumerating commands first; if the server doesn't ship
    // admin, this advert is absent and the chat client's admin panel
    // can fall back to the "Admin only" empty state.
    features.push(Feature::new(NS_ADMIN_USERS_LIST));
    // Admin V2 — XEP-0050 ad-hoc commands for Spaces + Channels CRUD.
    // Each node identifier is an owner-gated `<command/>` URI plus the
    // disco feature URI so clients can detect support without enumerating
    // commands first. The handlers are registered alongside admin V1's
    // `users:list` in the WebSocket command registry.
    features.push(Feature::new(NS_ADMIN_SPACES_LIST));
    features.push(Feature::new(NS_ADMIN_SPACES_CREATE));
    features.push(Feature::new(NS_ADMIN_SPACES_UPDATE));
    features.push(Feature::new(NS_ADMIN_SPACES_DELETE));
    features.push(Feature::new(NS_ADMIN_SPACES_MEMBERS));
    features.push(Feature::new(NS_ADMIN_SPACES_SET_ROLE));
    features.push(Feature::new(NS_ADMIN_CHANNELS_LIST));
    features.push(Feature::new(NS_ADMIN_CHANNELS_CREATE));
    features.push(Feature::new(NS_ADMIN_CHANNELS_UPDATE));
    features.push(Feature::new(NS_ADMIN_CHANNELS_DELETE));
    features.push(Feature::new(NS_ADMIN_CHANNELS_OCCUPANTS));
    features.push(Feature::new(NS_ADMIN_CHANNELS_AFFILIATIONS));
    features.push(Feature::new(NS_ADMIN_CHANNELS_SET_AFFILIATION));
    features.push(Feature::new(NS_ADMIN_CHANNELS_KICK));
    features.push(Feature::new(NS_GROUP_DM_CREATE));
    features.push(Feature::new(NS_GROUP_DM_LEAVE));
    features
}

/// XMPP-native A/V calling features the bare server domain
/// advertises. Returned as a separate list so disco-info builders only
/// emit them when the LiveKit-backed handlers are actually registered
/// at startup.
///
/// The domain is not itself a Jingle or RTP endpoint: 1:1 sessions run
/// between clients (which advertise `urn:xmpp:jingle:1` and the RTP
/// features in their own capabilities) and the Muji responder is the
/// `calls.<domain>` mixer ([`calls_mixer_features`]). Advertising
/// `urn:xmpp:jingle:1` or `urn:xmpp:jingle:apps:rtp:*` here would claim
/// behaviour the domain does not have (XEP-0166 / XEP-0167: the
/// advertisement asserts that the entity itself supports them).
pub fn call_features() -> Vec<Feature> {
    vec![
        Feature::jingle_message(),
        Feature::ext_disco(),
        Feature::waddle_livekit_transport(),
        Feature::waddle_call_lifecycle(),
    ]
}

/// Features of the `calls.<domain>` Muji mixer, which is the Jingle
/// responder for XEP-0272 group calls (session-initiate carrying
/// `<muji room='…'/>`, answered with a LiveKit-backed session-accept).
pub fn calls_mixer_features() -> Vec<Feature> {
    vec![
        Feature::disco_info(),
        Feature::muji(),
        Feature::coin(),
        Feature::jingle(),
        Feature::jingle_rtp(),
        Feature::jingle_rtp_audio(),
        Feature::jingle_rtp_video(),
        Feature::waddle_livekit_transport(),
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
    fn server_features_do_not_advertise_message_styling_parser() {
        // XEP-0393 discovery is for entities that support rendering
        // message styling. The server library exposes a conformant body
        // parser and `<unstyled/>` helper, but routing/archive entities
        // must not advertise the client-rendering feature from that alone.
        let features = server_features();
        assert!(!features.contains(&Feature::new(crate::xep::NS_STYLING)));
    }

    #[test]
    fn call_features_omit_endpoint_features_the_domain_cannot_act_on() {
        let features = call_features();
        let expected = [
            "urn:xmpp:jingle-message:0",
            "urn:xmpp:extdisco:2",
            "urn:waddle:transports:livekit:0",
            "urn:waddle:call-lifecycle:0",
        ];
        assert_eq!(features.len(), expected.len());
        for ns in expected {
            assert!(
                features.contains(&Feature::new(ns)),
                "call_features() missing {ns}"
            );
        }
        for ns in [
            "urn:xmpp:jingle:1",
            "urn:xmpp:jingle:apps:rtp:1",
            "urn:xmpp:jingle:apps:rtp:audio",
            "urn:xmpp:jingle:apps:rtp:video",
            "urn:xmpp:jingle:muji:0",
        ] {
            assert!(
                !features.contains(&Feature::new(ns)),
                "the server domain must not advertise {ns}"
            );
        }
    }

    #[test]
    fn calls_mixer_features_advertise_the_muji_responder_surface() {
        // XEP-0272's `urn:xmpp:jingle:muji:0` surface is implemented
        // by the Muji branch of the Jingle dispatcher, whose responder
        // is the `calls.<domain>` mixer.
        let features = calls_mixer_features();
        for ns in [
            "urn:xmpp:jingle:muji:0",
            "urn:xmpp:coin:1",
            "urn:xmpp:jingle:1",
            "urn:xmpp:jingle:apps:rtp:1",
            "urn:xmpp:jingle:apps:rtp:audio",
            "urn:xmpp:jingle:apps:rtp:video",
        ] {
            assert!(features.contains(&Feature::new(ns)), "mixer missing {ns}");
        }
    }
}
