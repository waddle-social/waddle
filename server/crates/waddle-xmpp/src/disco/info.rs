//! Service Discovery: disco#info handling.

pub use waddle_xmpp_core::disco::info::{
    build_disco_info_response, build_disco_info_response_with_extensions, is_disco_info_query,
    muc_room_features, muc_service_features, parse_disco_info_response, pubsub_service_features,
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
    features
}
