//! Sans-I/O XMPP protocol state machine.
//!
//! This module implements the XMPP protocol as a pure synchronous state
//! machine. It consumes [`InboundEvent`]s and emits [`OutboundEvent`]s. All
//! I/O, async work, and interaction with external resources (connection
//! registry, MUC rooms, MAM storage, actors) happens in a caller-owned
//! *interpreter* layer that lives outside this module.
//!
//! The architecture is modelled on the sans-I/O pattern used by `hyper`,
//! `h2`, and `quinn`. See the refactor plan at
//! `~/.claude/plans/snuggly-roaming-flame.md` for the full design rationale.
//!
//! # Layering
//!
//! ```text
//! Transport adapter (WebSocket C2S in `waddle-server`)
//!     ↓ InboundEvent
//! XmppStateMachine (pure, sync)  ← this module
//!     ↓ OutboundEvent
//! Effect interpreter (async, in the caller)
//! ```
//!
//! Only the middle layer — the state machine — lives here. The WebSocket C2S
//! adapter and its async interpreter remain the caller's responsibility.

pub mod dispatch;
pub mod dm_routing;
pub mod event;
pub mod frame;
pub mod handlers;
pub mod id_gen;
pub mod keepalive;
pub mod machine;
pub mod message_context;
pub mod phase;
pub mod room;
pub mod session_state;
pub mod traits;

/// Synthetic resource used by the offline headless recipient pass in
/// `waddle-server`.
pub const HEADLESS_RECIPIENT_RESOURCE: &str = "offline-recipient-pass";

#[cfg(test)]
mod dispatch_message_l2_tests;
#[cfg(test)]
mod dispatch_message_tests;
#[cfg(test)]
mod wire_trace_l4_room_tests;
#[cfg(test)]
mod wire_trace_l4_tests;

pub use dispatch::{MessageDispatchOutcome, MessageDispatchTermination, StanzaDispatcher};
pub use event::{
    ArchiveSide, ArchivedMessage, CallbackId, CarbonKind, InboundEvent, MessageRef, OutboundEvent,
    StanzaContext, TimerId,
};
pub use frame::InboundFrame;
pub use id_gen::{CounterIdGenerator, FixedIdGenerator, IdGenerator, UuidV4Generator};
pub use keepalive::{KeepaliveConfig, KEEPALIVE_TIMER};
pub use machine::XmppStateMachine;
pub use message_context::{MessageContext, MessageContextEnv};
pub use phase::{ConnectionPhase, ScramPendingState};
pub use session_state::{Blocklist, CarbonsState, Locality, MucOccupancy, OccupancyEntry};
pub use traits::{HandlerId, HandlerOutcome, IqHandler, MessageHandler, PresenceHandler};
