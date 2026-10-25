//! Server-side implementation surface for extension host tools.
//!
//! This module implements the `waddle-extensions` host-tool trait and limits
//! every operation to existing XMPP-native state:
//! channel discovery, XEP-0503 Spaces-on-PubSub, XEP-0045 room actors,
//! RFC 6121 roster/presence, XEP-0313 MAM, and the typed message pipeline.

use crate::server::routes::interpret::effects::ImmediateSink;
use crate::server::routes::interpret::DeliveryExecutionContext;
use crate::server::routes::websocket::ResolvedPrincipal;
use std::sync::Arc;

use jid::{BareJid, FullJid, Jid};
use kameo::actor::ActorRef;
use waddle_extensions::host_tools::InvocationKind;
use waddle_extensions::{DisplayText, PluginId, RoomJid, StanzaId};
use waddle_xmpp::muc::room_actor::{GetSnapshot, RoomActor};
use waddle_xmpp::muc::room_registry_actor::GetRoom;
use waddle_xmpp::roster::{RosterItem, Subscription};

use crate::auth::Session;
use crate::db::roster::DatabaseRosterStorage;
use crate::permissions::{CheckPermission, Object, ObjectType, Permission, Subject};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;

use super::routes::interpret::{self, Deps};
use super::routes::websocket::WebSocketState;

mod conversions;
mod direct;
mod groupchat;
mod host_tools;
mod queries;
#[cfg(feature = "clustering")]
mod remote_room;
mod settlement;
mod types;

use conversions::{envelope_has_cross_room_launch, envelope_has_roomless_launch};
pub use groupchat::BotRoomLocks;
#[cfg(feature = "clustering")]
pub(crate) use remote_room::{
    relayed_room_send, temporary_failure, EXTENSION_ROOM_MAILBOX_TIMEOUT,
    EXTENSION_ROOM_REPLY_TIMEOUT,
};
pub use types::*;

use direct::DirectDispatchMessage;

#[cfg(test)]
mod tests;

mod implementation;
pub use implementation::ExtensionHostAdapter;
