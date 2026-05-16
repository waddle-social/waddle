//! Shared Service Discovery (XEP-0030) helpers.

pub mod info;
pub mod items;

pub use info::{
    build_disco_info_response, build_disco_info_response_with_extensions, is_disco_info_query,
    muc_room_features, muc_service_features, parse_disco_info_query, pubsub_service_features,
    push_service_features, server_features, spaces_service_features, upload_service_features,
    DiscoInfoQuery, Feature, Identity, DISCO_INFO_NS,
};
pub use items::{
    build_disco_items_response, is_disco_items_query, parse_disco_items_query, DiscoItem,
    DiscoItemsQuery, DISCO_ITEMS_NS,
};
