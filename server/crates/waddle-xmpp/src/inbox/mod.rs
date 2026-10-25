//! Unified inbox — "conversation list with unread counts".
//!
//! An *inbox entry* is a per-user materialised view of the last-message of
//! every conversation the user is part of (1:1 and MUC room), plus an
//! unread counter and timestamp. It gives clients a single query to paint
//! the chat list on app launch without having to fan out to MAM per room.
//!
//! Entries are keyed by [`InboxKey`] — a `(partner, thread_id)` tuple — so
//! that both channel-level and thread-level conversations share the same
//! inbox infrastructure.
//!
//! This module defines the typed in-process model. The protocol wrapper
//! lives in [`crate::xep::xep0430`]; the persistent projection lives in
//! [`storage`].

pub mod runtime;
pub mod storage;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use jid::BareJid;
use minidom::Element;
use serde::{Deserialize, Serialize};

use crate::xep::{CallThreadDuration, CallThreadKind, CallThreadMedia};

#[cfg(test)]
mod tests;

mod service;
pub use service::ConversationKind;
pub use service::InboxEntry;
pub use service::InboxKey;
pub use service::InboxMessage;
pub use service::InboxView;
