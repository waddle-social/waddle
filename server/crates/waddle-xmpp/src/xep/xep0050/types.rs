use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use crate::xep::xep0004::{DataForm, DataFormError};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur when parsing or processing ad-hoc commands.
#[derive(Debug, Error)]
pub enum CommandError {
    /// The element is not a valid command element.
    #[error("element is not a command")]
    NotACommand,

    /// Missing required `node` attribute.
    #[error("missing required node attribute")]
    MissingNode,

    /// Invalid or unrecognised `action` attribute value.
    #[error("invalid action: {0}")]
    InvalidAction(String),

    /// Invalid or unrecognised `status` attribute value.
    #[error("invalid status: {0}")]
    InvalidStatus(String),

    /// Invalid note type attribute.
    #[error("invalid note type: {0}")]
    InvalidNoteType(String),

    /// Error parsing an embedded data form.
    #[error("data form error: {0}")]
    DataForm(#[from] DataFormError),

    /// The IQ stanza is not a command request.
    #[error("not a command IQ")]
    NotACommandIq,
}

// ---------------------------------------------------------------------------
// Action
// ---------------------------------------------------------------------------

/// Actions that a requester can specify on a command element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Execute (start or continue) the command.
    Execute,
    /// Move to the next stage.
    Next,
    /// Move to the previous stage.
    Prev,
    /// Request completion of the command.
    Complete,
    /// Cancel the command session.
    Cancel,
}

impl Action {
    /// Return the wire string for this action.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Next => "next",
            Self::Prev => "prev",
            Self::Complete => "complete",
            Self::Cancel => "cancel",
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Action {
    type Err = CommandError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "execute" => Ok(Self::Execute),
            "next" => Ok(Self::Next),
            "prev" => Ok(Self::Prev),
            "complete" => Ok(Self::Complete),
            "cancel" => Ok(Self::Cancel),
            other => Err(CommandError::InvalidAction(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Status values set by the responder on a command element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// The command is in progress; another action is required.
    Executing,
    /// The command completed successfully.
    Completed,
    /// The command was canceled.
    Canceled,
}

impl Status {
    /// Return the wire string for this status.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Executing => "executing",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Status {
    type Err = CommandError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "executing" => Ok(Self::Executing),
            "completed" => Ok(Self::Completed),
            "canceled" => Ok(Self::Canceled),
            other => Err(CommandError::InvalidStatus(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// NoteType
// ---------------------------------------------------------------------------

/// Type of a note element within a command response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NoteType {
    /// Informational message.
    #[default]
    Info,
    /// Warning message.
    Warn,
    /// Error message.
    Error,
}

impl NoteType {
    /// Return the wire string for this note type.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for NoteType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for NoteType {
    type Err = CommandError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            other => Err(CommandError::InvalidNoteType(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Note
// ---------------------------------------------------------------------------

/// A human-readable note attached to a command response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The type of the note.
    pub note_type: NoteType,
    /// The text content.
    pub text: String,
}

impl Note {
    /// Create a new note.
    pub fn new(note_type: NoteType, text: impl Into<String>) -> Self {
        Self {
            note_type,
            text: text.into(),
        }
    }

    /// Create an informational note.
    pub fn info(text: impl Into<String>) -> Self {
        Self::new(NoteType::Info, text)
    }

    /// Create a warning note.
    pub fn warn(text: impl Into<String>) -> Self {
        Self::new(NoteType::Warn, text)
    }

    /// Create an error note.
    pub fn error(text: impl Into<String>) -> Self {
        Self::new(NoteType::Error, text)
    }
}

// ---------------------------------------------------------------------------
// AllowedActions
// ---------------------------------------------------------------------------

/// The set of actions the responder permits at the current stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedActions {
    /// The default action when the requester sends `execute`.
    pub execute_default: Action,
    /// Whether `prev` is allowed.
    pub prev: bool,
    /// Whether `next` is allowed.
    pub next: bool,
    /// Whether `complete` is allowed.
    pub complete: bool,
}

impl AllowedActions {
    /// Create allowed actions with a default execute action.
    pub fn new(execute_default: Action) -> Self {
        Self {
            execute_default,
            prev: false,
            next: false,
            complete: false,
        }
    }

    /// Allow the `prev` action.
    pub fn with_prev(mut self) -> Self {
        self.prev = true;
        self
    }

    /// Allow the `next` action.
    pub fn with_next(mut self) -> Self {
        self.next = true;
        self
    }

    /// Allow the `complete` action.
    pub fn with_complete(mut self) -> Self {
        self.complete = true;
        self
    }
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

/// A parsed ad-hoc command element.
#[derive(Debug, Clone)]
pub struct Command {
    /// The command node identifier.
    pub node: String,
    /// Session identifier (set by the responder for multi-step flows).
    pub session_id: Option<String>,
    /// Action requested by the requester (if present).
    pub action: Option<Action>,
    /// Status set by the responder (if present).
    pub status: Option<Status>,
    /// Allowed actions for the current stage (responder-set).
    pub actions: Option<AllowedActions>,
    /// Notes from the responder.
    pub notes: Vec<Note>,
    /// Embedded data form payload.
    pub form: Option<DataForm>,
}

impl Command {
    /// Create a new command with a node identifier.
    pub fn new(node: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            session_id: None,
            action: None,
            status: None,
            actions: None,
            notes: Vec::new(),
            form: None,
        }
    }

    /// Set the session ID.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set the action.
    pub fn with_action(mut self, action: Action) -> Self {
        self.action = Some(action);
        self
    }

    /// Set the status.
    pub fn with_status(mut self, status: Status) -> Self {
        self.status = Some(status);
        self
    }

    /// Set the allowed actions.
    pub fn with_actions(mut self, actions: AllowedActions) -> Self {
        self.actions = Some(actions);
        self
    }

    /// Add a note.
    pub fn with_note(mut self, note: Note) -> Self {
        self.notes.push(note);
        self
    }

    /// Set the data form payload.
    pub fn with_form(mut self, form: DataForm) -> Self {
        self.form = Some(form);
        self
    }
}

// ---------------------------------------------------------------------------
// CommandDefinition trait
// ---------------------------------------------------------------------------

/// Trait for defining an ad-hoc command that can be registered with the server.
///
/// Implementations provide the command metadata (node, name) and handle
/// execution. The server dispatches incoming command IQs to the appropriate
/// `CommandDefinition` based on the `node` value.
pub trait CommandDefinition: Send + Sync {
    /// The unique node identifier for this command.
    fn node(&self) -> &str;

    /// Human-readable name shown in disco#items listings.
    fn name(&self) -> &str;
}
