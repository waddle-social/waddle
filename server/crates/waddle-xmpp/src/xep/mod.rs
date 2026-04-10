//! XMPP Extension Protocols (XEPs) Implementation
//!
//! This module contains implementations of various XMPP Extension Protocols
//! that extend the core XMPP functionality.
//!
//! ## Implemented XEPs
//!
//! - **XEP-0004**: Data Forms - Typed data form exchange for configuration,
//!   search, and reporting workflows.
//! - **XEP-0012**: Last Activity - Server uptime and user last activity queries.
//! - **XEP-0085**: Chat State Notifications - Typing indicators and
//!   conversational state (active, composing, paused, inactive, gone).
//! - **XEP-0184**: Message Delivery Receipts - Request and acknowledge
//!   message delivery with `<request/>` and `<received/>` elements.
//! - **XEP-0203**: Delayed Delivery - Timestamps on delayed/offline/history
//!   messages via `<delay/>` element with stamp and from attributes.
//! - **XEP-0308**: Last Message Correction - Replace previously sent messages
//!   via `<replace/>` element referencing the original message id.
//! - **XEP-0333**: Displayed Markers - Read receipts via `<markable/>`,
//!   `<displayed/>`, `<received/>`, and `<acknowledged/>` elements.
//! - **XEP-0392**: Consistent Color Generation - Deterministic HSL colors
//!   from input strings via SHA-1 hue mapping with CVD correction.
//! - **XEP-0393**: Message Styling - Inline text formatting parser for
//!   bold, italic, strikethrough, code, code blocks, and block quotes.
//! - **XEP-0359**: Unique and Stable Stanza IDs - Server-assigned `<stanza-id/>`
//!   and client-assigned `<origin-id/>` for stable message referencing.
//! - **XEP-0334**: Message Processing Hints
//! - **XEP-0424**: Message Retraction - Retract previously sent messages
//!   with `<retract/>` and `<retracted/>` tombstone elements.
//! - **XEP-0444**: Message Reactions - Emoji reactions via `<reactions/>`
//!   element containing `<reaction>` children. - Control server-side processing
//!   with no-store, no-copy, no-permanent-store, and store hints.
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
//! - **XEP-0461**: Message Replies - Reply references and thread metadata.
//! - **XEP-0503**: Server-side Spaces - Community discovery via pubsub (read-only Phase A).

pub mod xep0004;
pub mod xep0012;
pub mod xep0048;
pub mod xep0085;
pub mod xep0049;
pub mod xep0184;
pub mod xep0203;
pub mod xep0054;
pub mod xep0077;
pub mod xep0084;
pub mod xep0115;
pub mod xep0153;
pub mod xep0191;
pub mod xep0199;
pub mod xep0223;
pub mod xep0249;
pub mod xep0308;
pub mod xep0333;
pub mod xep0334;
pub mod xep0352;
pub mod xep0359;
pub mod xep0363;
pub mod xep0392;
pub mod xep0393;
pub mod xep0398;
pub mod xep0424;
pub mod xep0402;
pub mod xep0444;
pub mod xep0461;
pub mod xep0503;

pub use xep0004::{
    find_data_form, is_data_form, DataForm, DataFormError, Field, FieldOption, FieldType, FormType,
    FromElement, IntoElement, NS_DATA_FORMS,
};

pub use xep0012::{build_last_activity_response, is_last_activity_query, NS_LAST_ACTIVITY};

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

pub use xep0085::{
    build_chat_state_element, build_chat_state_message, extract_chat_state_from_message,
    is_chat_state_element, is_standalone_notification, parse_chat_state, set_chat_state,
    strip_chat_states, ChatState as Xep0085ChatState, ChatStateCarrier, ChatStateError,
    NS_CHATSTATES,
};

pub use xep0184::{
    build_receipt_message, build_receipt_received_element, build_receipt_request_element,
    extract_receipt_from_message, extract_received_id, has_receipt_received, has_receipt_request,
    is_receipt_received_element, is_receipt_request_element, is_standalone_receipt, set_receipt_received,
    set_receipt_request, strip_receipts, ReceiptCarrier, ReceiptError, ReceiptKind, NS_RECEIPTS,
};

pub use xep0199::{build_ping_result, is_ping, NS_PING};

pub use xep0203::{
    add_delay, add_delay_stamp, build_delay_element, build_delay_element_simple,
    extract_delay_from_message, extract_delay_stamp, has_delay, is_delay_element,
    parse_delay_element, strip_delay, DelayCarrier, DelayError, DelayInfo, NS_DELAY,
};

pub use xep0308::{
    build_correction_message, build_replace_element, extract_correction_from_message,
    extract_replaces_id, is_correction_message, is_replace_element, set_correction,
    strip_correction, Correction, CorrectionCarrier, CorrectionError, NS_MESSAGE_CORRECT,
};

pub use xep0333::{
    add_markable, build_acknowledged_element, build_displayed_element, build_displayed_message,
    build_markable_element, build_received_element, extract_marker_from_message, extract_marker_id,
    has_markable, has_marker, is_marker_element, is_standalone_marker, strip_markers, Marker,
    MarkerCarrier, MarkerError, NS_CHAT_MARKERS,
};

pub use xep0334::{
    add_hint, build_hint_element, extract_hints_from_message, has_hint, is_hint_element,
    remove_hint, should_skip_carbons, should_skip_storage, strip_hints, Hint, HintCarrier,
    NS_HINTS,
};

pub use xep0392::{
    apply_cvd_correction, compute_hue, generate_color, generate_color_with_params,
    ConsistentColor, CvdCorrection, HslColor, DEFAULT_LIGHTNESS, DEFAULT_SATURATION,
};

pub use xep0393::{
    blocks_to_html, blocks_to_plain, parse_blocks, parse_spans, spans_to_html, spans_to_plain,
    Block, Span, StyledBody,
};

pub use xep0359::{
    add_origin_id, add_stanza_id as add_stanza_id_xep0359, build_origin_id_element,
    build_stanza_id_element, extract_origin_id as extract_origin_id_xep0359,
    extract_origin_id_str, extract_stanza_id_by, extract_stanza_ids, has_origin_id, has_stanza_id,
    is_origin_id_element, is_stanza_id_element, remove_stanza_ids_by, strip_all_ids,
    OriginId as Xep0359OriginId, StanzaId as Xep0359StanzaId, StanzaIdCarrier, NS_SID,
};

pub use xep0352::{
    build_csi_feature, classify_message_urgency, classify_presence_urgency,
    data_contains_csi_active, data_contains_csi_inactive, is_csi_active, is_csi_inactive,
    is_muc_mention, ClientState, StanzaUrgency, MAX_CSI_BUFFER_SIZE, NS_CSI,
};

pub use xep0424::{
    build_retract_element, build_retracted_element, build_retraction_message,
    build_tombstone_message, extract_retraction_from_message, extract_retracts_id,
    is_retract_element, is_retracted_element, is_retraction_message, is_tombstone_message,
    set_retraction, strip_retraction, Retracted, Retraction, RetractionCarrier, RetractionError,
    RetractionKind, NS_MESSAGE_RETRACT,
};

pub use xep0444::{
    build_reaction_element, build_reaction_message, build_reactions_element,
    extract_reacted_id, extract_reactions_from_message, is_reaction_message,
    is_reactions_element, set_reactions, strip_reactions, ReactionCarrier, ReactionError,
    ReactionSet, NS_REACTIONS,
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

pub use xep0461::{
    build_reply_element, is_reply_element, parse_reply_from_message, set_reply_payload,
    set_thread_id, thread_id_from_message, ReplyReference, NS_REPLY,
};

pub use xep0503::{
    build_channel_item, build_spaces_metadata_form, build_spaces_type_form, NS_SPACES,
};

// Re-export commonly used items at the xep module level
pub mod prelude {
    pub use super::xep0249::message_has_direct_invite;
}
