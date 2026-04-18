//! Sans-I/O XMPP protocol state machine.
//!
//! This module implements the XMPP protocol as a pure synchronous state
//! machine. It consumes [`InboundEvent`]s and emits [`OutboundEvent`]s. All
//! I/O, async work, and interaction with external resources (connection
//! registry, MUC rooms, MAM storage, actors) happens in a transport-specific
//! *interpreter* layer that lives outside this module.
//!
//! The architecture is modelled on the sans-I/O pattern used by `hyper`,
//! `h2`, and `quinn`. See the refactor plan at
//! `~/.claude/plans/snuggly-roaming-flame.md` for the full design rationale.
//!
//! # Layering
//!
//! ```text
//! Transport adapter (WebSocket / TCP)
//!     ↓ InboundEvent
//! XmppStateMachine (pure, sync)  ← this module
//!     ↓ OutboundEvent
//! Effect interpreter (async, in transport adapter)
//! ```
//!
//! Only the middle layer — the state machine — lives here. The transport
//! adapter and interpreter are the caller's responsibility.

pub mod dispatch;
pub mod event;
pub mod frame;
pub mod handlers;
pub mod machine;
pub mod phase;
pub mod traits;

pub use dispatch::StanzaDispatcher;
pub use event::{CallbackId, InboundEvent, OutboundEvent, StanzaContext, TimerId};
pub use frame::InboundFrame;
pub use machine::XmppStateMachine;
pub use phase::ConnectionPhase;
pub use traits::{IqHandler, MessageHandler, PresenceHandler};
