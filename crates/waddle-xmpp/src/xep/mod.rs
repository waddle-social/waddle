//! XMPP Extension Protocols (XEPs) Implementation
//!
//! This module contains implementations of various XMPP Extension Protocols
//! that extend the core XMPP functionality.
//!
//! ## Implemented XEPs
//!
//! - **XEP-0054**: vcard-temp - User profile information via vCard format.
//! - **XEP-0077**: In-Band Registration - Allows users to register accounts
//!   directly through the XMPP connection before authentication.
//! - **XEP-0115**: Entity Capabilities - Efficient service discovery caching
//!   via capability hashes included in presence stanzas.
//! - **XEP-0191**: Blocking Command - User blocking capability for managing
//!   blocklists and silently dropping messages from blocked JIDs.
//! - **XEP-0199**: XMPP Ping - Simple ping/pong for connection liveness.
//! - **XEP-0249**: Direct MUC Invitations - Simple message-based invitations
//!   for inviting users directly to MUC rooms.
//! - **XEP-0352**: Client State Indication - Allows clients to indicate
//!   active/inactive state for traffic optimization.
//! - **XEP-0363**: HTTP File Upload - Server-side support for HTTP-based
//!   file uploads, returning PUT and GET URLs for file transfer.
//! - **XEP-0402**: PEP Native Bookmarks - MUC room bookmarks stored via PEP.

pub mod xep0054;
pub mod xep0077;
pub mod xep0115;
pub mod xep0191;
pub mod xep0199;
pub mod xep0249;
pub mod xep0352;
pub mod xep0363;
pub mod xep0402;

pub use xep0054::{
    is_vcard_query, is_vcard_get, is_vcard_set, parse_vcard_from_iq, parse_vcard_element,
    build_vcard_element, build_vcard_response, build_empty_vcard_response, build_vcard_success,
    build_vcard_error, VCard, VCardPhoto, VCardError, NS_VCARD,
};

pub use xep0077::{
    parse_registration_iq, build_registration_fields_response, build_registration_success,
    build_registration_error, RegistrationRequest, RegistrationError, is_registration_query,
};

pub use xep0115::{
    Caps, CapsCache, CachedDiscoInfo, compute_caps_hash, build_caps_element,
    extract_caps_from_presence, is_caps_node_query, parse_caps_node,
    NS_CAPS, WADDLE_CAPS_NODE,
};

pub use xep0249::{
    DirectInvite, is_direct_invite, message_has_direct_invite,
    parse_direct_invite, parse_direct_invite_from_message,
    build_direct_invite, build_invite_message, NS_CONFERENCE,
};

pub use xep0363::{
    is_upload_request, parse_upload_request, build_upload_slot_response,
    build_upload_error, sanitize_filename, effective_content_type,
    UploadRequest, UploadSlot, UploadError, NS_HTTP_UPLOAD, DEFAULT_MAX_FILE_SIZE,
};

pub use xep0191::{
    is_blocking_query, is_blocklist_get, is_block_set, is_unblock_set,
    parse_blocking_request, build_blocklist_response, build_blocking_success,
    build_block_push, build_unblock_push, build_blocking_error,
    BlockingRequest, BlockingError, NS_BLOCKING,
};

pub use xep0199::{is_ping, build_ping_result, NS_PING};

pub use xep0352::{
    ClientState, is_csi_active, is_csi_inactive, data_contains_csi_active,
    data_contains_csi_inactive, build_csi_feature, NS_CSI, MAX_CSI_BUFFER_SIZE,
    StanzaUrgency, classify_message_urgency, classify_presence_urgency, is_muc_mention,
};

pub use xep0402::{
    Bookmark, BookmarkError, parse_bookmark, build_bookmark_element,
    build_bookmark_item, is_bookmarks_node, NS_BOOKMARKS2, PEP_NODE as BOOKMARKS_PEP_NODE,
};

// Re-export commonly used items at the xep module level
pub mod prelude {
    pub use super::xep0249::message_has_direct_invite;
}
