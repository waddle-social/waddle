//! Service discovery (XEP-0030), HTTP upload (XEP-0363), inbox (XEP-0430),
//! push notifications, and XEP-0503 Spaces topology discovery.

pub(crate) mod ids;
mod iq;
mod parsing;
mod push_vapid;
mod types;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
mod ext;

pub use push_vapid::{
    parse_push_vapid_from_disco_info, PushVapidAdvertisement, PushVapidDiscoParseError,
    PUSH_VAPID_FIELD_KID, PUSH_VAPID_FIELD_PUBLIC_KEY, PUSH_VAPID_FORM_TYPE,
};

pub use iq::{
    build_disco_info_iq, build_disco_items_iq, build_muc_admin_affiliation_list_iq,
    build_muc_admin_affiliation_set_iq, build_muc_admin_role_set_iq, build_pubsub_items_iq,
    build_roster_get_iq, build_upload_slot_iq, build_user_search_form_iq, build_user_search_iq,
    build_waddle_inbox_mark_read_iq, parse_muc_admin_affiliation_query, parse_roster_result,
    parse_user_search_form, parse_user_search_result,
};
pub use parsing::{
    parse_disco_info_result, parse_disco_items_result, parse_space_channels_result,
    parse_upload_slot, space_from_disco_item,
};
pub use types::{
    DiscoDataField, DiscoDataForm, DiscoFeature, DiscoIdentity, DiscoInfoResult, DiscoItem,
    DiscoveredChannel, DiscoveredChannelType, DiscoveredComponentServices, DiscoveredSpace,
    DiscoveredTopology, MucAdminAffiliationItem, RosterResult, SpaceNode, UploadSlot,
    UserSearchForm, UserSearchItem, UserSearchQuery, UserSearchResult, WaddleInboxMarkRead,
};

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use ext::DiscoveryExt;

pub const DISCO_INFO_NS: &str = "http://jabber.org/protocol/disco#info";
pub const DISCO_ITEMS_NS: &str = "http://jabber.org/protocol/disco#items";
pub const UPLOAD_NS: &str = "urn:xmpp:http:upload:0";
/// Waddle-private namespace retained for the legacy `<mark-read/>` IQ.
///
/// XEP-0430's conformant `urn:xmpp:inbox:1` surface lives in
/// [`waddle_xmpp::xep::xep0430`] — the server-side wire types live in
/// that crate. The native discovery client doesn't consume the stream
/// today; the WASM browser client does, via the inbox-streaming flow.
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
/// `http://jabber.org/protocol/muc` — the XEP-0045 feature
/// advertised by Multi-User Chat services. Used at service
/// discovery time to identify which component hosts MUC rooms.
pub const MUC_NS: &str = "http://jabber.org/protocol/muc";
/// Synthetic space id assigned to MUC rooms that are *not* bookmarked
/// into any XEP-0503 space. Lets the UI place every joinable room
/// under some space without requiring server-side bookmark
/// administration first. Keep stable: clients may store the id as
/// part of their last-selected-space state.
pub const STANDALONE_SPACE_ID: &str = "standalone";
pub const USER_SEARCH_NS: &str = "jabber:iq:search";
pub const MUC_ADMIN_NS: &str = "http://jabber.org/protocol/muc#admin";
pub use waddle_xmpp_core::roster::ROSTER_NS;

#[cfg(test)]
mod tests;
