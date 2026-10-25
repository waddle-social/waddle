//! Service Discovery: disco#info handling.

use minidom::Element;
use std::collections::HashSet;
use tracing::debug;
use xmpp_parsers::iq::Iq;

use crate::CoreError;

mod features;
#[cfg(test)]
mod tests;

pub use features::Feature;

mod implementation;
pub use implementation::build_disco_info_response;
pub use implementation::build_disco_info_response_with_extensions;
pub use implementation::community_service_features;
pub use implementation::is_disco_info_query;
pub use implementation::muc_room_features;
pub use implementation::muc_service_features;
pub use implementation::parse_disco_info_query;
pub use implementation::parse_disco_info_response;
pub use implementation::pubsub_service_features;
pub use implementation::push_service_features;
pub use implementation::server_features;
pub use implementation::spaces_service_features;
pub use implementation::upload_service_features;
pub use implementation::DiscoInfoQuery;
pub use implementation::DiscoInfoResponse;
pub use implementation::Identity;
pub use implementation::RoomMembership;
pub use implementation::RoomPersistence;
pub use implementation::RoomVisibility;
pub use implementation::DISCO_INFO_NS;
use implementation::NS_CHATSTATES;
use implementation::NS_COMMANDS;
use implementation::NS_MUC_SELF_PING_OPTIMIZATION;
use implementation::SERVER_INFO_FORM_TYPE;

#[cfg(test)]
use implementation::NS_DATA_FORMS;
