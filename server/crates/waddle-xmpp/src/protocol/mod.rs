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
pub mod event;
pub mod frame;
pub mod handlers;
pub mod id_gen;
pub mod machine;
pub mod message_context;
pub mod phase;
pub mod session_state;
pub mod traits;

#[cfg(test)]
mod dispatch_message_tests;

pub use dispatch::{MessageDispatchOutcome, MessageDispatchTermination, StanzaDispatcher};
pub use event::{
    ArchivedMessage, CallbackId, CarbonKind, InboundEvent, MessageRef, OriginIdValue,
    OutboundEvent, StanzaContext, StanzaIdRef, StanzaIdValue, TimerId,
};
pub use frame::InboundFrame;
pub use id_gen::{CounterIdGenerator, FixedIdGenerator, IdGenerator, UuidV4Generator};
pub use machine::XmppStateMachine;
pub use message_context::{MessageContext, MessageContextEnv};
pub use phase::{ConnectionPhase, ScramPendingState};
pub use session_state::{Blocklist, CarbonsState, Locality, MucOccupancy, OccupancyEntry};
pub use traits::{HandlerId, HandlerOutcome, IqHandler, MessageHandler, PresenceHandler};
