//! Service discovery (XEP-0030), HTTP upload (XEP-0363), inbox (XEP-0430),
//! push notifications, and XEP-0503 Spaces topology discovery.

mod ids;
mod iq;
mod parsing;
mod types;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod ext;

pub use iq::{
    build_disable_push_iq, build_disco_info_iq, build_disco_items_iq, build_enable_push_iq,
    build_inbox_iq, build_muc_admin_affiliation_list_iq, build_muc_admin_affiliation_set_iq,
    build_pubsub_items_iq, build_roster_get_iq, build_upload_slot_iq, build_user_search_form_iq,
    build_user_search_iq, build_waddle_inbox_mark_read_iq, build_waddle_inbox_query_iq,
    parse_muc_admin_affiliation_query, parse_roster_result, parse_user_search_form,
    parse_user_search_result, parse_waddle_inbox_result,
};
pub use parsing::{
    parse_disco_info_result, parse_disco_items_result, parse_inbox_result,
    parse_space_channels_result, parse_spaces_from_disco_items, parse_upload_slot,
    space_from_disco_item,
};
pub use types::{
    DiscoDataField, DiscoDataForm, DiscoFeature, DiscoIdentity, DiscoInfoResult, DiscoItem,
    DiscoveredChannel, DiscoveredChannelType, DiscoveredSpace, DiscoveredTopology, InboxEntry,
    MucAdminAffiliationItem, RosterResult, SpaceNode, UploadSlot, UserSearchForm, UserSearchItem,
    UserSearchQuery, UserSearchResult, WaddleInboxConversation, WaddleInboxMarkRead,
    WaddleInboxQuery, WaddleInboxResult,
};

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use ext::DiscoveryExt;

pub const DISCO_INFO_NS: &str = "http://jabber.org/protocol/disco#info";
pub const DISCO_ITEMS_NS: &str = "http://jabber.org/protocol/disco#items";
pub const UPLOAD_NS: &str = "urn:xmpp:http:upload:0";
pub const INBOX_NS: &str = "erlang-solutions.com:xmpp:inbox:0";
pub const WADDLE_INBOX_NS: &str = "urn:waddle:inbox:0";
pub const PUSH_NS: &str = "urn:xmpp:push:0";
pub const CLIENT_NS: &str = "jabber:client";
pub const DATA_FORMS_NS: &str = "jabber:x:data";
pub const RSM_NS: &str = "http://jabber.org/protocol/rsm";
pub const PUBSUB_NS: &str = "http://jabber.org/protocol/pubsub";
pub const PUBSUB_METADATA_FORM_TYPE: &str = "http://jabber.org/protocol/pubsub#meta-data";
pub const BOOKMARKS_NS: &str = "urn:xmpp:bookmarks:1";
pub const SPACES_NS: &str = "urn:xmpp:spaces:0";
pub const WADDLE_ROOM_METADATA_FORM_TYPE: &str = "urn:waddle:room:0";
pub const USER_SEARCH_NS: &str = "jabber:iq:search";
pub const MUC_ADMIN_NS: &str = "http://jabber.org/protocol/muc#admin";
pub use waddle_xmpp_core::roster::ROSTER_NS;

#[cfg(test)]
mod tests;
