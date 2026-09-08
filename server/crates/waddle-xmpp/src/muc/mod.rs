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
pub mod durable;
pub mod messages;
pub mod owner;
pub mod pin;
pub mod presence;
mod room;
pub mod room_actor;
mod room_affiliations;
mod room_broadcast;
pub mod room_registry_actor;
pub mod room_registry_handle;
pub mod roominfo;
mod subject;

pub use admin::{
    build_admin_result, build_admin_set_result, build_role_result, is_affiliation_change_query,
    is_muc_admin_get, is_muc_admin_iq, is_muc_admin_set, is_muc_owner_get, is_muc_owner_set,
    is_role_change_query, parse_admin_query, AdminItem, AdminQuery, AffiliationChangeResult,
    KickBanInfo, MucStatusCode, RoleChangeResult, NS_MUC_ADMIN, NS_MUC_OWNER,
};
pub use durable::{
    AdminPresenceKind, AffiliationEntry as DurableAffiliationEntry, DestroyAttemptId,
    DestroyPassword, DestroyReason, DestroyRecipient, DurableRoomState,
    EphemeralProjectionAuthorization, MucDurableFuture, MucDurableStore, MucOccupantNick,
    OccupantPresenceUpdate, OccupantVoiceChange, RoomClaimFenceContext, RoomCommitDatabaseError,
    RoomCommitError, RoomCommitFuture, RoomCommitOutcome, RoomCommittedCoordinates,
    RoomDurableMutation, RoomEffect, RoomEffectIntent, RoomEffectKind, RoomEffectOrdinal,
    RoomEffectReservation, RoomEffectStagingClass, RoomLifecycleId, RoomLifecycleState,
    RoomMutationCommit, RoomMutationEffects, RoomRevision,
};
pub use messages::{
    build_config_change_message, config_change_status_codes, create_broadcast_message,
    is_muc_groupchat, looks_like_muc_jid, MessageRouteResult, MucConfigStatusCode, MucMessage,
    OutboundMucMessage,
};
pub use owner::{build_config_form, DATA_FORMS_NS, MUC_ROOMCONFIG_NS};
pub use pin::{
    PinChangeRequest, PinPermission, PinPreview, PinStateChange, PinnedEntry, MAX_PINNED_ENTRIES,
    MAX_PREVIEW_LEN as PIN_PREVIEW_MAX_LEN,
};
pub use presence::{
    build_affiliation_change_presence, build_ban_presence, build_destroy_notification,
    build_kick_presence, build_leave_presence, build_membership_removal_presence,
    build_occupant_presence, build_occupant_presence_update, build_role_change_presence,
    parse_muc_presence, DestroyRequest, HistoryRequest, MucJoinRequest, MucLeaveRequest,
    MucPresenceAction, MucPresenceStatus, MucPresenceUpdateRequest, OutboundMucPresence,
};
pub use room::{is_remote_jid, AllowPm, MucRoom, Occupant, RoomConfig};
pub use room_actor::{RoomActorError, RoomInfo};
pub use room_registry_actor::RoomRegistryError;
pub use room_registry_handle::{
    RoomRegistry, ROOM_REGISTRY_MAILBOX_CAPACITY, ROOM_REGISTRY_MAILBOX_TIMEOUT,
    ROOM_REGISTRY_REPLY_TIMEOUT, ROOM_REGISTRY_SLOW_ASK_WARN,
};
pub use subject::{
    is_groupchat_subject_change, is_groupchat_subject_change_message, RoomSubjectTexts,
    SubjectState,
};

/// True when `ns` is a MUC *service* namespace whose payloads are
/// authored by the room service, never by an occupant client:
/// `http://jabber.org/protocol/muc` and its `#user` / `#admin` /
/// `#owner` fragments.
///
/// XEP-0313 §Security "MUC message spoofing" and XEP-0045
/// anti-spoofing require the service to strip any occupant-supplied
/// payload in these namespaces from both groupchat messages (before
/// reflect/archive) and private messages (before routing), so an
/// occupant cannot forge affiliation/role/status/invite signalling
/// that appears to come from `room/nick` (#1251, #1268). Shared by the
/// groupchat canonicalizer and the MUC-PM canonicalizer so both agree
/// on the exact namespace set.
pub fn is_muc_service_namespace(ns: &str) -> bool {
    matches!(
        ns,
        presence::NS_MUC | presence::NS_MUC_USER | NS_MUC_ADMIN | NS_MUC_OWNER
    )
}
