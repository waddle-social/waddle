//! Multi-User Chat (MUC) implementation.
//!
//! Implements XEP-0045 for group chat functionality, with each room
//! managed as a Kameo actor for concurrent message handling.
//!
//! ## Affiliation Sync
//!
//! This module integrates with Waddle's Zanzibar-based permission system
//! to derive MUC affiliations. See [`affiliation`] for details on the
//! permission-to-affiliation mapping.

pub mod admin;
pub mod affiliation;
pub mod messages;
pub mod owner;
pub mod pin;
pub mod presence;
mod room;
pub mod room_actor;
mod room_affiliations;
mod room_broadcast;
pub mod room_registry;
pub mod room_registry_actor;
mod subject;

pub use admin::{
    build_admin_result, build_admin_set_result, build_role_result, is_affiliation_change_query,
    is_muc_admin_get, is_muc_admin_iq, is_muc_admin_set, is_muc_owner_get, is_muc_owner_set,
    is_role_change_query, parse_admin_query, AdminItem, AdminQuery, AffiliationChangeResult,
    KickBanInfo, MucStatusCode, RoleChangeResult, NS_MUC_ADMIN, NS_MUC_OWNER,
};
pub use messages::{
    create_broadcast_message, is_muc_groupchat, looks_like_muc_jid, MessageRouteResult, MucMessage,
    OutboundMucMessage,
};
pub use owner::{build_config_form, DATA_FORMS_NS, MUC_ROOMCONFIG_NS};
pub use pin::{
    PinChangeRequest, PinPermission, PinPreview, PinStateChange, PinnedEntry, MAX_PINNED_ENTRIES,
    MAX_PREVIEW_LEN as PIN_PREVIEW_MAX_LEN,
};
pub use presence::{
    build_affiliation_change_presence, build_ban_presence, build_kick_presence,
    build_leave_presence, build_occupant_presence, build_occupant_presence_update,
    build_role_change_presence, parse_muc_presence, HistoryRequest, MucJoinRequest,
    MucLeaveRequest, MucPresenceAction, MucPresenceUpdateRequest, OutboundMucPresence,
};
pub use room::{is_remote_jid, MucRoom, Occupant, RoomConfig};
pub use room_actor::RoomActorError;
pub use room_registry::{MucRoomRegistry, RoomHandle, RoomInfo, RoomMessage};
pub use room_registry_actor::RoomRegistryError;
pub use subject::{RoomSubjectTexts, SubjectState};
