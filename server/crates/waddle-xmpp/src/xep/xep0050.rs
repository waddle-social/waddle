//! XEP-0050: Ad-Hoc Commands
//!
//! Provides a framework for executing multi-step commands between XMPP
//! entities using data forms (XEP-0004).
//!
//! ## Overview
//!
//! Ad-hoc commands allow entities to advertise and execute structured operations.
//! Commands are discovered via service discovery (XEP-0030) on the well-known
//! node `http://jabber.org/protocol/commands`, and executed as IQ set/result
//! exchanges that may span multiple steps.
//!
//! ## Flow
//!
//! 1. **Discovery**: Client queries disco#items on the commands node to list
//!    available commands.
//! 2. **Execution**: Client sends `<command action='execute'/>` to start.
//! 3. **Multi-step**: Server responds with status `executing` and a data form;
//!    client fills and submits. This repeats until the server sets status
//!    `completed` or `canceled`.
//!
//! ## Runtime Status
//!
//! This module provides parser/builder helpers for XEP-0050 payloads.
//! The server runtime does not currently advertise or dispatch ad-hoc commands.
//! Unsupported command requests therefore receive the normal
//! `<service-unavailable/>` IQ error path instead.

use std::fmt;
use std::str::FromStr;

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::iq::{Iq, IqType};

use super::xep0004::{DataForm, DataFormError, FromElement, IntoElement};

/// Namespace for XEP-0050 Ad-Hoc Commands.
pub const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";

/// Well-known disco#items node for listing available commands.
pub const NODE_COMMANDS: &str = "http://jabber.org/protocol/commands";

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
// FromElement / IntoElement
// ---------------------------------------------------------------------------

impl FromElement for Note {
    type Error = CommandError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        let note_type = elem
            .attr("type")
            .map(|t| t.parse::<NoteType>())
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            note_type,
            text: elem.text(),
        })
    }
}

impl IntoElement for Note {
    fn into_element(&self) -> Element {
        let mut builder =
            Element::builder("note", NS_COMMANDS).attr("type", self.note_type.as_str());
        builder = builder.append(minidom::Node::Text(self.text.clone()));
        builder.build()
    }
}

impl FromElement for AllowedActions {
    type Error = CommandError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        let execute_default = elem
            .attr("execute")
            .map(|a| a.parse::<Action>())
            .transpose()?
            .unwrap_or(Action::Execute);

        let prev = elem.children().any(|c| c.name() == "prev");
        let next = elem.children().any(|c| c.name() == "next");
        let complete = elem.children().any(|c| c.name() == "complete");

        Ok(Self {
            execute_default,
            prev,
            next,
            complete,
        })
    }
}

impl IntoElement for AllowedActions {
    fn into_element(&self) -> Element {
        let mut builder =
            Element::builder("actions", NS_COMMANDS).attr("execute", self.execute_default.as_str());

        if self.prev {
            builder = builder.append(Element::builder("prev", NS_COMMANDS).build());
        }
        if self.next {
            builder = builder.append(Element::builder("next", NS_COMMANDS).build());
        }
        if self.complete {
            builder = builder.append(Element::builder("complete", NS_COMMANDS).build());
        }

        builder.build()
    }
}

impl FromElement for Command {
    type Error = CommandError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        if elem.name() != "command" || elem.ns() != NS_COMMANDS {
            return Err(CommandError::NotACommand);
        }

        let node = elem
            .attr("node")
            .ok_or(CommandError::MissingNode)?
            .to_string();

        let session_id = elem.attr("sessionid").map(|s| s.to_string());

        let action = elem
            .attr("action")
            .map(|a| a.parse::<Action>())
            .transpose()?;

        let status = elem
            .attr("status")
            .map(|s| s.parse::<Status>())
            .transpose()?;

        let actions = elem
            .children()
            .find(|c| c.name() == "actions" && c.ns() == NS_COMMANDS)
            .map(AllowedActions::from_element)
            .transpose()?;

        let notes = elem
            .children()
            .filter(|c| c.name() == "note" && c.ns() == NS_COMMANDS)
            .map(Note::from_element)
            .collect::<Result<Vec<_>, _>>()?;

        let form = elem
            .children()
            .find(|c| c.name() == "x" && c.ns() == super::xep0004::NS_DATA_FORMS)
            .map(DataForm::from_element)
            .transpose()?;

        Ok(Self {
            node,
            session_id,
            action,
            status,
            actions,
            notes,
            form,
        })
    }
}

impl IntoElement for Command {
    fn into_element(&self) -> Element {
        let mut builder = Element::builder("command", NS_COMMANDS).attr("node", &self.node);

        if let Some(ref sid) = self.session_id {
            builder = builder.attr("sessionid", sid);
        }

        if let Some(action) = self.action {
            builder = builder.attr("action", action.as_str());
        }

        if let Some(status) = self.status {
            builder = builder.attr("status", status.as_str());
        }

        if let Some(ref actions) = self.actions {
            builder = builder.append(actions.into_element());
        }

        for note in &self.notes {
            builder = builder.append(note.into_element());
        }

        if let Some(ref form) = self.form {
            builder = builder.append(form.into_element());
        }

        builder.build()
    }
}

// ---------------------------------------------------------------------------
// IQ helpers
// ---------------------------------------------------------------------------

/// Check if an IQ stanza is an ad-hoc command request (IQ set with command element).
pub fn is_command_request(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Set(elem) if elem.name() == "command" && elem.ns() == NS_COMMANDS)
}

/// Parse an ad-hoc command from an IQ set stanza.
pub fn parse_command_from_iq(iq: &Iq) -> Result<Command, CommandError> {
    match &iq.payload {
        IqType::Set(elem) if elem.name() == "command" && elem.ns() == NS_COMMANDS => {
            Command::from_element(elem)
        }
        _ => Err(CommandError::NotACommandIq),
    }
}

/// Build an IQ result containing a command response.
pub fn build_command_result(original_iq: &Iq, command: &Command) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(command.into_element())),
    }
}

/// Build a command IQ error response with a specific condition.
///
/// The error element includes the command-specific condition from the
/// `http://jabber.org/protocol/commands` namespace alongside the standard
/// stanza error condition via the `other` extension field.
pub fn build_command_error(
    original_iq: &Iq,
    error_type: &str,
    stanza_condition: &str,
    command_condition: Option<&str>,
) -> Iq {
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    let et = match error_type {
        "modify" => ErrorType::Modify,
        "cancel" => ErrorType::Cancel,
        "auth" => ErrorType::Auth,
        "wait" => ErrorType::Wait,
        _ => ErrorType::Cancel,
    };

    let dc = match stanza_condition {
        "bad-request" => DefinedCondition::BadRequest,
        "not-allowed" => DefinedCondition::NotAllowed,
        "forbidden" => DefinedCondition::Forbidden,
        "item-not-found" => DefinedCondition::ItemNotFound,
        "feature-not-implemented" => DefinedCondition::FeatureNotImplemented,
        "not-acceptable" => DefinedCondition::NotAcceptable,
        "service-unavailable" => DefinedCondition::ServiceUnavailable,
        _ => DefinedCondition::UndefinedCondition,
    };

    let mut stanza_error = StanzaError::new(et, dc, "en", "");

    if let Some(cc) = command_condition {
        stanza_error.other = Some(Element::builder(cc, NS_COMMANDS).build());
    }

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Error(stanza_error),
    }
}

/// Build a `bad-request` error with an optional command-specific condition.
pub fn build_bad_request(original_iq: &Iq, command_condition: Option<&str>) -> Iq {
    build_command_error(original_iq, "modify", "bad-request", command_condition)
}

/// Build a `not-allowed` error (e.g., requester lacks permission).
pub fn build_not_allowed(original_iq: &Iq) -> Iq {
    build_command_error(original_iq, "cancel", "not-allowed", None)
}

/// Build a `forbidden` error.
pub fn build_forbidden(original_iq: &Iq) -> Iq {
    build_command_error(original_iq, "auth", "forbidden", None)
}

/// Build an `item-not-found` error (unknown command node).
pub fn build_item_not_found(original_iq: &Iq) -> Iq {
    build_command_error(original_iq, "cancel", "item-not-found", None)
}

/// Build a `bad-sessionid` error (invalid or expired session).
pub fn build_bad_session_id(original_iq: &Iq) -> Iq {
    build_command_error(original_iq, "modify", "bad-request", Some("bad-sessionid"))
}

/// Build a `session-expired` error.
pub fn build_session_expired(original_iq: &Iq) -> Iq {
    build_command_error(
        original_iq,
        "modify",
        "not-allowed",
        Some("session-expired"),
    )
}

// ---------------------------------------------------------------------------
// Disco helpers
// ---------------------------------------------------------------------------

/// Build a disco#items element for the commands node listing.
///
/// Each tuple is `(node, name)` representing an available command.
pub fn build_command_items(original_iq: &Iq, commands: &[(&str, &str)], responder_jid: &str) -> Iq {
    use crate::disco::items::DISCO_ITEMS_NS;

    let mut query = Element::builder("query", DISCO_ITEMS_NS).attr("node", NODE_COMMANDS);

    for (node, name) in commands {
        let item = Element::builder("item", DISCO_ITEMS_NS)
            .attr("jid", responder_jid)
            .attr("node", *node)
            .attr("name", *name)
            .build();
        query = query.append(item);
    }

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(query.build())),
    }
}

/// Check if a disco#items query is for the ad-hoc commands node.
pub fn is_commands_disco_items(iq: &Iq) -> bool {
    use crate::disco::items::DISCO_ITEMS_NS;

    match &iq.payload {
        IqType::Get(elem) => {
            elem.name() == "query"
                && elem.ns() == DISCO_ITEMS_NS
                && elem.attr("node") == Some(NODE_COMMANDS)
        }
        _ => false,
    }
}

/// Check if a disco#info query is for the ad-hoc commands node.
pub fn is_commands_disco_info(iq: &Iq) -> bool {
    use crate::disco::info::DISCO_INFO_NS;

    match &iq.payload {
        IqType::Get(elem) => {
            elem.name() == "query"
                && elem.ns() == DISCO_INFO_NS
                && elem.attr("node") == Some(NODE_COMMANDS)
        }
        _ => false,
    }
}

/// Check if a disco#info query is for a specific command node.
pub fn is_command_node_disco_info(iq: &Iq, node: &str) -> bool {
    use crate::disco::info::DISCO_INFO_NS;

    match &iq.payload {
        IqType::Get(elem) => {
            elem.name() == "query" && elem.ns() == DISCO_INFO_NS && elem.attr("node") == Some(node)
        }
        _ => false,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0004::{DataForm, Field, FormType};

    // --- Action ---

    #[test]
    fn test_action_from_str() {
        assert_eq!("execute".parse::<Action>().ok(), Some(Action::Execute));
        assert_eq!("next".parse::<Action>().ok(), Some(Action::Next));
        assert_eq!("prev".parse::<Action>().ok(), Some(Action::Prev));
        assert_eq!("complete".parse::<Action>().ok(), Some(Action::Complete));
        assert_eq!("cancel".parse::<Action>().ok(), Some(Action::Cancel));
        assert!("invalid".parse::<Action>().is_err());
    }

    #[test]
    fn test_action_as_str() {
        assert_eq!(Action::Execute.as_str(), "execute");
        assert_eq!(Action::Next.as_str(), "next");
        assert_eq!(Action::Prev.as_str(), "prev");
        assert_eq!(Action::Complete.as_str(), "complete");
        assert_eq!(Action::Cancel.as_str(), "cancel");
    }

    #[test]
    fn test_action_display() {
        assert_eq!(format!("{}", Action::Execute), "execute");
    }

    // --- Status ---

    #[test]
    fn test_status_from_str() {
        assert_eq!("executing".parse::<Status>().ok(), Some(Status::Executing));
        assert_eq!("completed".parse::<Status>().ok(), Some(Status::Completed));
        assert_eq!("canceled".parse::<Status>().ok(), Some(Status::Canceled));
        assert!("bogus".parse::<Status>().is_err());
    }

    #[test]
    fn test_status_as_str() {
        assert_eq!(Status::Executing.as_str(), "executing");
        assert_eq!(Status::Completed.as_str(), "completed");
        assert_eq!(Status::Canceled.as_str(), "canceled");
    }

    // --- NoteType ---

    #[test]
    fn test_note_type_from_str() {
        assert_eq!("info".parse::<NoteType>().ok(), Some(NoteType::Info));
        assert_eq!("warn".parse::<NoteType>().ok(), Some(NoteType::Warn));
        assert_eq!("error".parse::<NoteType>().ok(), Some(NoteType::Error));
        assert!("unknown".parse::<NoteType>().is_err());
    }

    #[test]
    fn test_note_type_default() {
        assert_eq!(NoteType::default(), NoteType::Info);
    }

    // --- Note ---

    #[test]
    fn test_note_constructors() {
        let n = Note::info("hello");
        assert_eq!(n.note_type, NoteType::Info);
        assert_eq!(n.text, "hello");

        let n = Note::warn("careful");
        assert_eq!(n.note_type, NoteType::Warn);

        let n = Note::error("oops");
        assert_eq!(n.note_type, NoteType::Error);
    }

    #[test]
    fn test_note_roundtrip() {
        let note = Note::warn("be careful");
        let elem = note.into_element();
        let parsed = Note::from_element(&elem).expect("parse note");
        assert_eq!(parsed.note_type, NoteType::Warn);
        assert_eq!(parsed.text, "be careful");
    }

    #[test]
    fn test_note_default_type_when_absent() {
        let elem = Element::builder("note", NS_COMMANDS).build();
        let parsed = Note::from_element(&elem).expect("parse note");
        assert_eq!(parsed.note_type, NoteType::Info);
    }

    // --- AllowedActions ---

    #[test]
    fn test_allowed_actions_roundtrip() {
        let actions = AllowedActions::new(Action::Next)
            .with_prev()
            .with_next()
            .with_complete();

        let elem = actions.into_element();
        let parsed = AllowedActions::from_element(&elem).expect("parse actions");

        assert_eq!(parsed.execute_default, Action::Next);
        assert!(parsed.prev);
        assert!(parsed.next);
        assert!(parsed.complete);
    }

    #[test]
    fn test_allowed_actions_minimal() {
        let actions = AllowedActions::new(Action::Complete);
        let elem = actions.into_element();
        let parsed = AllowedActions::from_element(&elem).expect("parse actions");

        assert_eq!(parsed.execute_default, Action::Complete);
        assert!(!parsed.prev);
        assert!(!parsed.next);
        assert!(!parsed.complete);
    }

    #[test]
    fn test_allowed_actions_default_execute() {
        // No execute attribute => defaults to Execute
        let elem = Element::builder("actions", NS_COMMANDS)
            .append(Element::builder("next", NS_COMMANDS).build())
            .build();
        let parsed = AllowedActions::from_element(&elem).expect("parse actions");
        assert_eq!(parsed.execute_default, Action::Execute);
        assert!(parsed.next);
    }

    // --- Command ---

    #[test]
    fn test_command_builder() {
        let cmd = Command::new("http://example.com/cmd")
            .with_session_id("sess-1")
            .with_action(Action::Execute)
            .with_status(Status::Executing)
            .with_note(Note::info("Step 1"))
            .with_actions(AllowedActions::new(Action::Complete).with_complete())
            .with_form(DataForm::new(FormType::Form).add_field(Field::text_single("name", "")));

        assert_eq!(cmd.node, "http://example.com/cmd");
        assert_eq!(cmd.session_id.as_deref(), Some("sess-1"));
        assert_eq!(cmd.action, Some(Action::Execute));
        assert_eq!(cmd.status, Some(Status::Executing));
        assert_eq!(cmd.notes.len(), 1);
        assert!(cmd.actions.is_some());
        assert!(cmd.form.is_some());
    }

    #[test]
    fn test_command_roundtrip_minimal() {
        let cmd = Command::new("test-node").with_action(Action::Execute);
        let elem = cmd.into_element();
        let parsed = Command::from_element(&elem).expect("parse command");

        assert_eq!(parsed.node, "test-node");
        assert_eq!(parsed.action, Some(Action::Execute));
        assert!(parsed.session_id.is_none());
        assert!(parsed.status.is_none());
        assert!(parsed.actions.is_none());
        assert!(parsed.notes.is_empty());
        assert!(parsed.form.is_none());
    }

    #[test]
    fn test_command_roundtrip_full() {
        let cmd = Command::new("my-command")
            .with_session_id("abc-123")
            .with_status(Status::Executing)
            .with_actions(AllowedActions::new(Action::Next).with_next().with_prev())
            .with_note(Note::info("Please fill this form"))
            .with_form(DataForm::new(FormType::Form).add_field(Field::text_single("username", "")));

        let elem = cmd.into_element();
        let parsed = Command::from_element(&elem).expect("parse command");

        assert_eq!(parsed.node, "my-command");
        assert_eq!(parsed.session_id.as_deref(), Some("abc-123"));
        assert_eq!(parsed.status, Some(Status::Executing));
        assert!(parsed.actions.is_some());
        let actions = parsed.actions.as_ref().expect("actions");
        assert_eq!(actions.execute_default, Action::Next);
        assert!(actions.next);
        assert!(actions.prev);
        assert!(!actions.complete);
        assert_eq!(parsed.notes.len(), 1);
        assert_eq!(parsed.notes[0].text, "Please fill this form");
        assert!(parsed.form.is_some());
    }

    #[test]
    fn test_command_missing_node_error() {
        let elem = Element::builder("command", NS_COMMANDS).build();
        let result = Command::from_element(&elem);
        assert!(result.is_err());
    }

    #[test]
    fn test_command_wrong_element_error() {
        let elem = Element::builder("query", "jabber:iq:last").build();
        let result = Command::from_element(&elem);
        assert!(matches!(result, Err(CommandError::NotACommand)));
    }

    // --- IQ helpers ---

    #[test]
    fn test_is_command_request() {
        let cmd_elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "test-cmd")
            .attr("action", "execute")
            .build();
        let iq = Iq {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "cmd-1".to_string(),
            payload: IqType::Set(cmd_elem),
        };

        assert!(is_command_request(&iq));
    }

    #[test]
    fn test_is_command_request_false_for_get() {
        let cmd_elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "test-cmd")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "cmd-2".to_string(),
            payload: IqType::Get(cmd_elem),
        };

        assert!(!is_command_request(&iq));
    }

    #[test]
    fn test_is_command_request_false_for_wrong_ns() {
        let elem = Element::builder("command", "wrong:ns")
            .attr("node", "test")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "cmd-3".to_string(),
            payload: IqType::Set(elem),
        };

        assert!(!is_command_request(&iq));
    }

    #[test]
    fn test_parse_command_from_iq() {
        let cmd_elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "my-node")
            .attr("action", "execute")
            .build();
        let iq = Iq {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "parse-1".to_string(),
            payload: IqType::Set(cmd_elem),
        };

        let cmd = parse_command_from_iq(&iq).expect("parse command from IQ");
        assert_eq!(cmd.node, "my-node");
        assert_eq!(cmd.action, Some(Action::Execute));
    }

    #[test]
    fn test_parse_command_from_iq_error_on_get() {
        let elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "x")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "parse-2".to_string(),
            payload: IqType::Get(elem),
        };

        assert!(parse_command_from_iq(&iq).is_err());
    }

    #[test]
    fn test_build_command_result() {
        let cmd_elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "test")
            .attr("action", "execute")
            .build();
        let iq = Iq {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "res-1".to_string(),
            payload: IqType::Set(cmd_elem),
        };

        let response_cmd = Command::new("test")
            .with_session_id("sess-1")
            .with_status(Status::Completed)
            .with_note(Note::info("Done"));

        let result = build_command_result(&iq, &response_cmd);

        assert_eq!(result.id, "res-1");
        assert_eq!(result.from, iq.to);
        assert_eq!(result.to, iq.from);
        match &result.payload {
            IqType::Result(Some(elem)) => {
                assert_eq!(elem.name(), "command");
                assert_eq!(elem.ns(), NS_COMMANDS);
                assert_eq!(elem.attr("status"), Some("completed"));
                assert_eq!(elem.attr("sessionid"), Some("sess-1"));
            }
            _ => panic!("Expected Result with command payload"),
        }
    }

    // --- Error helpers ---

    #[test]
    fn test_build_bad_request() {
        use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

        let cmd_elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "test")
            .build();
        let iq = Iq {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "err-1".to_string(),
            payload: IqType::Set(cmd_elem),
        };

        let err = build_bad_request(&iq, Some("bad-payload"));
        match &err.payload {
            IqType::Error(se) => {
                assert_eq!(se.type_, ErrorType::Modify);
                assert_eq!(se.defined_condition, DefinedCondition::BadRequest);
                let ext = se.other.as_ref().expect("command extension");
                assert_eq!(ext.name(), "bad-payload");
                assert_eq!(ext.ns(), NS_COMMANDS);
            }
            _ => panic!("Expected Error payload"),
        }
    }

    #[test]
    fn test_build_bad_session_id() {
        use xmpp_parsers::stanza_error::DefinedCondition;

        let cmd_elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "test")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "err-2".to_string(),
            payload: IqType::Set(cmd_elem),
        };

        let err = build_bad_session_id(&iq);
        match &err.payload {
            IqType::Error(se) => {
                assert_eq!(se.defined_condition, DefinedCondition::BadRequest);
                let ext = se.other.as_ref().expect("command extension");
                assert_eq!(ext.name(), "bad-sessionid");
                assert_eq!(ext.ns(), NS_COMMANDS);
            }
            _ => panic!("Expected Error payload"),
        }
    }

    #[test]
    fn test_build_item_not_found() {
        use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

        let cmd_elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "nonexistent")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "err-3".to_string(),
            payload: IqType::Set(cmd_elem),
        };

        let err = build_item_not_found(&iq);
        match &err.payload {
            IqType::Error(se) => {
                assert_eq!(se.type_, ErrorType::Cancel);
                assert_eq!(se.defined_condition, DefinedCondition::ItemNotFound);
                assert!(se.other.is_none());
            }
            _ => panic!("Expected Error payload"),
        }
    }

    #[test]
    fn test_build_not_allowed() {
        use xmpp_parsers::stanza_error::ErrorType;

        let cmd_elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "x")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "err-4".to_string(),
            payload: IqType::Set(cmd_elem),
        };

        let err = build_not_allowed(&iq);
        match &err.payload {
            IqType::Error(se) => {
                assert_eq!(se.type_, ErrorType::Cancel);
            }
            _ => panic!("Expected Error payload"),
        }
    }

    #[test]
    fn test_build_forbidden() {
        use xmpp_parsers::stanza_error::ErrorType;

        let cmd_elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "x")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "err-5".to_string(),
            payload: IqType::Set(cmd_elem),
        };

        let err = build_forbidden(&iq);
        match &err.payload {
            IqType::Error(se) => {
                assert_eq!(se.type_, ErrorType::Auth);
            }
            _ => panic!("Expected Error payload"),
        }
    }

    #[test]
    fn test_build_session_expired() {
        let cmd_elem = Element::builder("command", NS_COMMANDS)
            .attr("node", "x")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "err-6".to_string(),
            payload: IqType::Set(cmd_elem),
        };

        let err = build_session_expired(&iq);
        match &err.payload {
            IqType::Error(se) => {
                let ext = se.other.as_ref().expect("command extension");
                assert_eq!(ext.name(), "session-expired");
                assert_eq!(ext.ns(), NS_COMMANDS);
            }
            _ => panic!("Expected Error payload"),
        }
    }

    // --- Disco helpers ---

    #[test]
    fn test_is_commands_disco_items() {
        use crate::disco::items::DISCO_ITEMS_NS;

        let query = Element::builder("query", DISCO_ITEMS_NS)
            .attr("node", NODE_COMMANDS)
            .build();
        let iq = Iq {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "disco-items-1".to_string(),
            payload: IqType::Get(query),
        };

        assert!(is_commands_disco_items(&iq));
    }

    #[test]
    fn test_is_commands_disco_items_false_for_other_node() {
        use crate::disco::items::DISCO_ITEMS_NS;

        let query = Element::builder("query", DISCO_ITEMS_NS)
            .attr("node", "some-other-node")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "disco-items-2".to_string(),
            payload: IqType::Get(query),
        };

        assert!(!is_commands_disco_items(&iq));
    }

    #[test]
    fn test_is_commands_disco_info() {
        use crate::disco::info::DISCO_INFO_NS;

        let query = Element::builder("query", DISCO_INFO_NS)
            .attr("node", NODE_COMMANDS)
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "disco-info-1".to_string(),
            payload: IqType::Get(query),
        };

        assert!(is_commands_disco_info(&iq));
    }

    #[test]
    fn test_is_command_node_disco_info() {
        use crate::disco::info::DISCO_INFO_NS;

        let query = Element::builder("query", DISCO_INFO_NS)
            .attr("node", "my-command-node")
            .build();
        let iq = Iq {
            from: None,
            to: None,
            id: "disco-info-2".to_string(),
            payload: IqType::Get(query),
        };

        assert!(is_command_node_disco_info(&iq, "my-command-node"));
        assert!(!is_command_node_disco_info(&iq, "other-node"));
    }

    #[test]
    fn test_build_command_items() {
        let query = Element::builder("query", "http://jabber.org/protocol/disco#items")
            .attr("node", NODE_COMMANDS)
            .build();
        let iq = Iq {
            from: Some("alice@example.com".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "items-1".to_string(),
            payload: IqType::Get(query),
        };

        let commands = vec![("cmd-1", "First Command"), ("cmd-2", "Second Command")];

        let result = build_command_items(&iq, &commands, "example.com");
        match &result.payload {
            IqType::Result(Some(elem)) => {
                assert_eq!(elem.name(), "query");
                assert_eq!(elem.attr("node"), Some(NODE_COMMANDS));
                let items: Vec<_> = elem.children().collect();
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].attr("node"), Some("cmd-1"));
                assert_eq!(items[0].attr("name"), Some("First Command"));
                assert_eq!(items[0].attr("jid"), Some("example.com"));
                assert_eq!(items[1].attr("node"), Some("cmd-2"));
                assert_eq!(items[1].attr("name"), Some("Second Command"));
            }
            _ => panic!("Expected Result with query payload"),
        }
    }

    // --- Multi-step command scenario ---

    #[test]
    fn test_multi_step_command_flow() {
        // Step 1: Client sends execute
        let request = Command::new("change-password").with_action(Action::Execute);
        let request_elem = request.into_element();
        let parsed_request = Command::from_element(&request_elem).expect("parse request");
        assert_eq!(parsed_request.action, Some(Action::Execute));

        // Step 2: Server responds with form
        let response = Command::new("change-password")
            .with_session_id("session-abc")
            .with_status(Status::Executing)
            .with_actions(AllowedActions::new(Action::Complete).with_complete())
            .with_form(
                DataForm::new(FormType::Form).add_field(Field::text_single("new-password", "")),
            );
        let response_elem = response.into_element();
        let parsed_response = Command::from_element(&response_elem).expect("parse response");
        assert_eq!(parsed_response.status, Some(Status::Executing));
        assert!(parsed_response.form.is_some());

        // Step 3: Client submits filled form
        let submit = Command::new("change-password")
            .with_session_id("session-abc")
            .with_action(Action::Complete)
            .with_form(
                DataForm::new(FormType::Submit)
                    .add_field(Field::text_single("new-password", "s3cret")),
            );
        let submit_elem = submit.into_element();
        let parsed_submit = Command::from_element(&submit_elem).expect("parse submit");
        assert_eq!(parsed_submit.action, Some(Action::Complete));
        assert_eq!(parsed_submit.session_id.as_deref(), Some("session-abc"));

        // Step 4: Server responds completed
        let completed = Command::new("change-password")
            .with_session_id("session-abc")
            .with_status(Status::Completed)
            .with_note(Note::info("Password changed successfully"));
        let completed_elem = completed.into_element();
        let parsed_completed = Command::from_element(&completed_elem).expect("parse completed");
        assert_eq!(parsed_completed.status, Some(Status::Completed));
        assert_eq!(parsed_completed.notes.len(), 1);
        assert_eq!(
            parsed_completed.notes[0].text,
            "Password changed successfully"
        );
    }

    // --- Cancel flow ---

    #[test]
    fn test_cancel_command_flow() {
        let cancel = Command::new("some-command")
            .with_session_id("sess-xyz")
            .with_action(Action::Cancel);
        let elem = cancel.into_element();
        let parsed = Command::from_element(&elem).expect("parse cancel");
        assert_eq!(parsed.action, Some(Action::Cancel));

        let canceled_response = Command::new("some-command")
            .with_session_id("sess-xyz")
            .with_status(Status::Canceled)
            .with_note(Note::info("Command canceled"));
        let resp_elem = canceled_response.into_element();
        let parsed_resp = Command::from_element(&resp_elem).expect("parse canceled response");
        assert_eq!(parsed_resp.status, Some(Status::Canceled));
    }

    // --- Multiple notes ---

    #[test]
    fn test_command_with_multiple_notes() {
        let cmd = Command::new("multi-note")
            .with_status(Status::Completed)
            .with_note(Note::info("Step completed"))
            .with_note(Note::warn("But check logs"));

        let elem = cmd.into_element();
        let parsed = Command::from_element(&elem).expect("parse");
        assert_eq!(parsed.notes.len(), 2);
        assert_eq!(parsed.notes[0].note_type, NoteType::Info);
        assert_eq!(parsed.notes[1].note_type, NoteType::Warn);
    }
}
