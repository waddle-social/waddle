//! # waddle-xmpp
//!
//! Native XMPP server library for Waddle Social.
//!
//! This crate implements an XMPP server following RFC 6120/6121 and relevant XEPs,
//! designed to be embedded in `waddle-server` for unified deployment.
//!
//! ## Architecture
//!
//! - **Transport**: WebSocket-only XMPP in `waddle-server`
//! - **Connection Registry**: Active WebSocket sessions routed by typed JID
//! - **MUC Room Actors**: Multi-user chat rooms as separate actors
//! - **Stream Processing**: XML stream parsing via xmpp-parsers
//!
//! ## XEP Support
//!
//! MVP:
//! - RFC 6120/6121 (XMPP Core/IM)
//! - XEP-0030 (Service Discovery)
//! - XEP-0045 (Multi-User Chat)
//! - XEP-0198 (Stream Management)
//! - XEP-0280 (Message Carbons)
//! - XEP-0313 (Message Archive Management)

pub mod admin;
pub mod auth;
pub mod c2s;
pub mod carbons;
pub mod commands;
pub mod disco;
pub mod inbox;
pub mod mam;
pub mod metrics;
pub mod muc;
pub mod ownership;
pub mod parser;
pub mod pending_delivery;
pub mod presence;
pub mod prometheus;
pub mod protocol;
pub mod pubsub;
pub mod push;
pub mod registry;
pub mod roster;
pub mod routing;
pub mod stream_management;
pub mod telemetry;
pub mod tombstone;
pub mod xep;

mod app_state;
mod error;
mod types;

pub use app_state::{
    AppState, ScramCredentials, Session, SpaceAccessModel, SpaceDetails, UserDirectoryEntry,
};

pub use error::{
    generate_stream_error, stream_errors, StanzaErrorCondition, StanzaErrorType, XmppError,
};
pub use parser::ns;
pub use routing::{RouterConfig, RoutingDestination, RoutingResult, StanzaRouter};
pub use types::*;
pub use waddle_xmpp_core::{
    managed_room_jid, managed_room_localpart, parse_managed_room_jid, parse_managed_room_localpart,
    ChannelInfo, ChannelRoomInfo, ChannelType, UploadSlotInfo,
};
pub use waddle_xmpp_core::{xep0201, CoreError, Stanza};
pub use xep::xep0077::{RegistrationError, RegistrationRequest};
