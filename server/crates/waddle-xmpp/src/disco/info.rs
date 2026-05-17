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

/// Server-level disco features, extended with Waddle inbox advertisement.
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
    // Admin V2 — Spaces + Channels CRUD commands. Same discovery
    // contract as V1: the XEP-0050 node identifier doubles as the
    // disco#info feature URI. Listed here so an admin client can
    // detect server support for the V2 surface without first
    // walking the commands list. Order mirrors the wire-protocol
    // section of `docs/superpowers/specs/2026-05-17-admin-v2-design.md`.
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
