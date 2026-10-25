//! MUC Presence Types
//!
//! Types and utilities for handling MUC room join/leave presence stanzas
//! per XEP-0045.

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

use crate::types::{Affiliation, Role};
use crate::xep::xep0421::OccupantIdentity;

mod outbound;
mod parser;
#[cfg(test)]
mod tests;

pub use outbound::OutboundMucPresence;
pub use parser::{
    parse_muc_presence, HistoryRequest, MucJoinRequest, MucLeaveRequest, MucPresenceAction,
    MucPresenceUpdateRequest, NS_MUC,
};

mod implementation;
pub use implementation::build_affiliation_change_presence;
pub use implementation::build_ban_presence;
pub use implementation::build_destroy_notification;
pub use implementation::build_kick_presence;
pub use implementation::build_leave_presence;
pub use implementation::build_membership_removal_presence;
pub use implementation::build_occupant_presence;
pub use implementation::build_occupant_presence_update;
pub use implementation::build_role_change_presence;
pub use implementation::DestroyRequest;
pub use implementation::MucPresenceStatus;
pub use implementation::MucRemovalCause;
pub use implementation::NS_MUC_USER;
