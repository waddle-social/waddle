//! Shared typed primitives for Waddle's native XMPP client crates.

pub mod carbons;
pub mod connection;
pub mod disco;
pub mod domain;
pub mod error;
pub mod link_preview;
pub mod mam;
pub mod occupancy_session;
pub mod parser_utils;
pub mod presence;
pub mod pubsub;
pub mod roster;
pub mod stanza;
pub mod types;
pub mod waddle_status_preference;
pub mod waddle_story_reads;
pub mod xcal;
pub mod xep0201;
pub mod xep0359;
pub mod xep0392;
pub mod xep0472;
pub mod xep0501;

pub use carbons::{
    build_carbons_result, build_received_carbon, build_sent_carbon, is_carbons_disable,
    is_carbons_enable, should_copy_message, CARBONS_NS, DELAY_NS, FORWARDED_NS,
};
pub use connection::ConnectionConfig;
pub use disco::{
    build_disco_info_response, build_disco_info_response_with_extensions,
    build_disco_items_response, community_service_features, is_disco_info_query,
    is_disco_items_query, muc_room_features, muc_service_features, parse_disco_info_query,
    parse_disco_items_query, pubsub_service_features, push_service_features, server_features,
    spaces_service_features, upload_service_features, DiscoInfoQuery, DiscoItem, DiscoItemsQuery,
    Feature, Identity, DISCO_INFO_NS, DISCO_ITEMS_NS,
};
pub use domain::{
    managed_room_jid, managed_room_localpart, parse_managed_room_jid, parse_managed_room_localpart,
    ChannelInfo, ChannelRoomInfo, ChannelType, UploadSlotInfo, WaddleDetails, WaddleInfo,
};
pub use error::{CoreError, CoreResult};
pub use link_preview::{
    first_eligible_https_url_text, DirectVideoMediaType, PreviewImageMediaType,
};
pub use mam::{
    build_fin_iq, build_result_messages, is_mam_query, is_mam_query_response_message,
    parse_mam_query, ArchivedMessage, MamQuery, MamResult, DATA_FORMS_NS, FORWARD_NS, MAM_NS,
    RSM_NS, STANZA_ID_NS,
};
pub use occupancy_session::OccupancySessionGeneration;
pub use parser_utils::{ensure_thread_element, extract_thread_parent, reattach_thread_parent};
pub use presence::{
    build_available_presence, build_subscription_presence, build_unavailable_presence,
    parse_subscription_presence, ChatState, PendingSubscription, PresenceAction,
    PresenceSubscriptionRequest, Show, SubscriptionType, UserPresence,
};
pub use pubsub::{
    build_pep_identity, build_pubsub_error, build_pubsub_event, build_pubsub_items_result,
    build_pubsub_publish_result, build_pubsub_retract_event, build_pubsub_success, is_pep_request,
    is_pep_request_to, is_pubsub_event, is_pubsub_iq, parse_pubsub_event, parse_pubsub_iq,
    pep_features, AccessModel, NodeConfig, PepHandler, PubSubError, PubSubEvent, PubSubItem,
    PubSubRequest, PublishModel, SendLastPublishedItem, NS_PUBSUB, NS_PUBSUB_ERRORS,
    NS_PUBSUB_EVENT, NS_PUBSUB_OWNER,
};
pub use stanza::Stanza;
pub use types::{Affiliation, ConnectionState, Moderation, Role, StanzaType, Transport, Voice};
pub use xep0201::{
    build_thread_element, install_thread_element, parse_thread_info, set_thread_id,
    thread_id_from_message, thread_id_from_message_in_stanza_ns, thread_info_from_message,
    thread_info_from_message_in_stanza_ns, ThreadInfo, CLIENT_STANZA_NS, SERVER_STANZA_NS,
    THREAD_ELEMENT,
};
pub use xep0359::{
    add_origin_id, add_stanza_id as add_stanza_id_xep0359, build_origin_id_element,
    build_stanza_id_element, extract_origin_id, extract_origin_id_str, extract_stanza_id_by,
    extract_stanza_ids, has_origin_id, has_stanza_id, is_origin_id_element, is_stanza_id_element,
    remove_stanza_ids_by, strip_all_ids, OriginId, StanzaId, StanzaIdCarrier, NS_SID,
};
