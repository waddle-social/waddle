//! Ad-Hoc Command Handlers (XEP-0050)
//!
//! This module implements the runtime infrastructure for ad-hoc commands,
//! allowing the server to register and dispatch XEP-0050 commands dynamically.

pub mod registry;

pub use registry::{
    CommandContext, CommandHandler, CommandMetadata, CommandRegistry, CommandResult, CommandSession,
};

// Re-export common XEP-0050 types for convenience
pub use crate::xep::xep0050::{
    Action, AllowedActions, Command, Note, NoteType, Status, NODE_COMMANDS, NS_COMMANDS,
};
