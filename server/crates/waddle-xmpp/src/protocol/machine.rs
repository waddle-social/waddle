//! Pure, synchronous XMPP state machine.
//!
//! The core method is [`XmppStateMachine::handle`], which is a pure
//! synchronous function from an [`InboundEvent`] + current state to a
//! `Vec<OutboundEvent>`. No async, no I/O, no mocks required in tests.
//!
//! This module owns only *per-connection* state. Cross-connection state
//! (connection registry, MUC room occupancy, MAM archive) is resolved by
//! the interpreter using the events emitted from here.

mod completion;
mod message_pipeline;
mod pending_op;

use super::dispatch::StanzaDispatcher;
use super::event::{CallbackId, InboundEvent, OutboundEvent, StanzaContext};
use super::frame::InboundFrame;
use super::id_gen::{IdGenerator, UuidV4Generator};
use super::keepalive::{dispatch_tick, KeepaliveConfig, KeepalivePolicy};
use super::message_context::{MessageContext, MessageContextEnv};
use super::phase::ConnectionPhase;
use super::session_state::{Blocklist, CarbonsState, MucOccupancy};
use crate::Stanza;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::Level;

pub use pending_op::{PendingOp, ResumeKind};

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;

mod implementation;
pub use implementation::XmppStateMachine;

#[cfg(test)]
mod xep0191_tests;
