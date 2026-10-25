//! Durable projection of XEP-0492 per-chat notification settings.
//!
//! The canonical setting remains the XMPP state that clients publish. This
//! module stores only the server-side indexed view used by notification policy
//! reads.

use crate::db::Row;
use jid::BareJid;
use minidom::Element;
use thiserror::Error;
use waddle_xmpp::xep::{
    is_notify_element, parse_notify_fallback_setting, validate_notify_element, NotificationLevel,
};

use crate::db::{Database, DatabaseError};

mod bookmark_derivation;

pub use bookmark_derivation::{
    derive_bookmark_projection_mutation, derive_dm_bookmark_projection_mutation,
    derive_validated_bookmark_projection_mutation, validate_dm_bookmark_publish,
    validate_xep0402_bookmark_publish,
};

mod implementation;
pub use implementation::ConversationKind;
pub use implementation::NotificationSettingsProjection;
pub use implementation::NotificationSettingsProjectionError;
pub use implementation::NotificationSettingsProjectionMutation;
pub use implementation::NotificationSettingsProjectionStore;
pub use implementation::NotificationSettingsSource;
pub use implementation::PushDispatchDecision;
