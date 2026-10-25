//! RFC 6121 Roster Management implementation.
//!
//! The roster (contact list) is a key feature of XMPP instant messaging.
//! This module implements:
//!
//! - Roster get: Retrieve the user's contact list
//! - Roster set: Add, update, or remove contacts
//! - Roster push: Notify connected resources when roster changes
//!
//! ## Protocol Overview
//!
//! Roster get:
//! ```xml
//! <iq type='get' id='roster-1'>
//!   <query xmlns='jabber:iq:roster'/>
//! </iq>
//! ```
//!
//! Roster set (add/update):
//! ```xml
//! <iq type='set' id='roster-2'>
//!   <query xmlns='jabber:iq:roster'>
//!     <item jid='contact@example.com' name='Contact Name'>
//!       <group>Friends</group>
//!     </item>
//!   </query>
//! </iq>
//! ```
//!
//! Roster set (remove):
//! ```xml
//! <iq type='set' id='roster-3'>
//!   <query xmlns='jabber:iq:roster'>
//!     <item jid='contact@example.com' subscription='remove'/>
//!   </query>
//! </iq>
//! ```
//!
//! Roster push (server to client):
//! ```xml
//! <iq type='set' id='push-1'>
//!   <query xmlns='jabber:iq:roster'>
//!     <item jid='contact@example.com' subscription='both'/>
//!   </query>
//! </iq>
//! ```

pub use waddle_xmpp_core::roster::{
    AskType, RosterItem, RosterVersion, RosterVersionRequest, Subscription, ROSTER_NS,
};

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use std::collections::HashSet;
use tracing::debug;
use xmpp_parsers::iq::Iq;

use crate::XmppError;

#[cfg(test)]
mod tests;

mod service;
pub use service::build_roster_push;
pub use service::build_roster_result;
pub use service::build_roster_result_empty;
pub use service::is_roster_get;
pub use service::is_roster_set;
pub use service::parse_roster_get;
pub use service::parse_roster_set;
use service::parse_roster_set_item;
pub use service::RosterQuery;
pub use service::RosterSetResult;
