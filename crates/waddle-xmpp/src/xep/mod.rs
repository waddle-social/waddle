//! XMPP Extension Protocols (XEPs) Implementation
//!
//! This module contains implementations of various XMPP Extension Protocols
//! that extend the core XMPP functionality.
//!
//! ## Implemented XEPs
//!
//! - **XEP-0048**: Bookmark Storage (Legacy) - Compatibility layer over XEP-0402.
//! - **XEP-0049**: Private XML Storage - Arbitrary per-user XML key-value store.
//! - **XEP-0054**: vcard-temp - User profile information via vCard format.
//! - **XEP-0077**: In-Band Registration - Allows users to register accounts
//!   directly through the XMPP connection before authentication.
//! - **XEP-0084**: User Avatar - PEP-based avatar storage and notifications.
//! - **XEP-0115**: Entity Capabilities - Efficient service discovery caching
//!   via capability hashes included in presence stanzas.
//! - **XEP-0153**: vCard-Based Avatars - Avatar hash in presence stanzas.
//! - **XEP-0191**: Blocking Command - User blocking capability for managing
//!   blocklists and silently dropping messages from blocked JIDs.
//! - **XEP-0199**: XMPP Ping - Simple ping/pong for connection liveness.
//! - **XEP-0223**: Persistent Storage Best Practices - Profile of PubSub.
//! - **XEP-0249**: Direct MUC Invitations - Simple message-based invitations
//!   for inviting users directly to MUC rooms.
//! - **XEP-0352**: Client State Indication - Allows clients to indicate
//!   active/inactive state for traffic optimization.
//! - **XEP-0363**: HTTP File Upload - Server-side support for HTTP-based
//!   file uploads, returning PUT and GET URLs for file transfer.
//! - **XEP-0398**: User Avatar Conversion - Bridge between PEP and vCard avatars.
//! - **XEP-0402**: PEP Native Bookmarks - MUC room bookmarks stored via PEP.

pub mod xep0048;
pub mod xep0049;
pub mod xep0054;
pub mod xep0077;
pub mod xep0084;
pub mod xep0115;
pub mod xep0153;
pub mod xep0191;
pub mod xep0199;
pub mod xep0223;
pub mod xep0249;
pub mod xep0352;
pub mod xep0363;
pub mod xep0398;
pub mod xep0402;

pub use xep0054::{
    build_empty_vcard_response, build_vcard_element, build_vcard_error, build_vcard_response,
    build_vcard_success, is_vcard_get, is_vcard_query, is_vcard_set, parse_vcard_element,
    parse_vcard_from_iq, VCard, VCardError, VCardPhoto, NS_VCARD,
};

pub use xep0077::{
    build_registration_error, build_registration_fields_response, build_registration_success,
    is_registration_query, parse_registration_iq, RegistrationError, RegistrationRequest,
};

pub use xep0115::{
    build_caps_element, compute_caps_hash, extract_caps_from_presence, is_caps_node_query,
    parse_caps_node, CachedDiscoInfo, Caps, CapsCache, NS_CAPS, WADDLE_CAPS_NODE,
};

pub use xep0249::{
    build_direct_invite, build_invite_message, is_direct_invite, message_has_direct_invite,
    parse_direct_invite, parse_direct_invite_from_message, DirectInvite, NS_CONFERENCE,
};

pub use xep0363::{
    build_upload_error, build_upload_slot_response, effective_content_type, is_upload_request,
    parse_upload_request, sanitize_filename, UploadError, UploadRequest, UploadSlot,
    DEFAULT_MAX_FILE_SIZE, NS_HTTP_UPLOAD,
};

pub use xep0191::{
    build_block_push, build_blocking_error, build_blocking_success, build_blocklist_response,
    build_unblock_push, is_block_set, is_blocking_query, is_blocklist_get, is_unblock_set,
    parse_blocking_request, BlockingError, BlockingRequest, NS_BLOCKING,
};

pub use xep0199::{build_ping_result, is_ping, NS_PING};

pub use xep0352::{
    build_csi_feature, classify_message_urgency, classify_presence_urgency,
    data_contains_csi_active, data_contains_csi_inactive, is_csi_active, is_csi_inactive,
    is_muc_mention, ClientState, StanzaUrgency, MAX_CSI_BUFFER_SIZE, NS_CSI,
};

pub use xep0402::{
    build_bookmark_element, build_bookmark_item, is_bookmarks_node, parse_bookmark, Bookmark,
    BookmarkError, NS_BOOKMARKS2, PEP_NODE as BOOKMARKS_PEP_NODE,
};

pub use xep0048::{
    build_legacy_bookmarks_element, from_native_bookmark, is_legacy_bookmarks_namespace,
    parse_legacy_bookmarks, to_native_bookmark, LegacyBookmark, NS_BOOKMARKS_LEGACY,
};

pub use xep0049::{
    build_private_storage_result, build_private_storage_success, is_private_storage_query,
    parse_private_storage_get, parse_private_storage_set, PrivateStorageKey, NS_PRIVATE,
};

pub use xep0084::{
    build_avatar_data, build_avatar_metadata, compute_avatar_hash, is_avatar_data_node,
    is_avatar_metadata_node, parse_avatar_data, parse_avatar_metadata, AvatarInfo,
    NODE_AVATAR_DATA, NODE_AVATAR_METADATA, NS_AVATAR_DATA, NS_AVATAR_METADATA,
};

pub use xep0153::{
    build_vcard_update_element, compute_photo_hash, compute_photo_hash_from_base64,
    has_vcard_update, parse_vcard_update, NS_VCARD_UPDATE,
};

pub use xep0398::{
    pep_avatar_to_vcard_photo, vcard_photo_to_pep_avatar, AvatarConversion,
    DefaultAvatarConversion, NS_PEP_VCARD_CONVERSION,
};

pub use xep0223::{is_private_storage_node, FEATURE_ACCESS_WHITELIST, FEATURE_PERSISTENT_ITEMS};

// Re-export commonly used items at the xep module level
pub mod prelude {
    pub use super::xep0249::message_has_direct_invite;
}
