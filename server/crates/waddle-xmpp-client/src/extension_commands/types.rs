//! Typed payloads of the `urn:waddle:extension:1` command surface.

use crate::xep::xep0050::{AdHocAction, AdHocStatus};

/// `waddle#command_scope` — where a command may be offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionCommandScope {
    /// Offered everywhere (DMs and rooms).
    Global,
    /// Offered only inside MUC rooms.
    Channel,
}

impl ExtensionCommandScope {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "global" => Some(Self::Global),
            "channel" => Some(Self::Channel),
            _ => None,
        }
    }

    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Channel => "channel",
        }
    }
}

/// One `<item/>` from the service's XEP-0050 command list, before its
/// per-command `disco#info` metadata has been fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCommandItemRef {
    pub service_jid: String,
    pub node: String,
    pub name: String,
}

/// Composer metadata parsed from a command's `disco#info` extension
/// form (`FORM_TYPE` [`super::NS_WADDLE_EXTENSION_1`] or
/// [`super::EXTENSION_COMMAND_FORM_TYPE`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionCommandMetadata {
    pub scope: Option<ExtensionCommandScope>,
    pub composer_prefix: Option<String>,
    pub inline_field: Option<String>,
    pub composer_execute: bool,
}

/// One discovered extension command with its composer metadata —
/// the typed equivalent of the wasm client's
/// `DiscoveredExtensionCommand`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCommandDescriptor {
    pub service_jid: String,
    pub node: String,
    pub name: String,
    pub scope: ExtensionCommandScope,
    /// Slash trigger (without the leading `/`), e.g. `poll`.
    pub composer_prefix: Option<String>,
    /// Form field the composer may fill inline from trailing text.
    pub inline_field: Option<String>,
    /// Whether a bare `/prefix` executes without opening the palette.
    pub composer_execute: bool,
}

/// XEP-0004 §3.3 field types. Unknown or absent `type` attributes
/// parse as [`Self::TextSingle`] per the XEP's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionFieldType {
    Boolean,
    Fixed,
    Hidden,
    JidMulti,
    JidSingle,
    ListMulti,
    ListSingle,
    TextMulti,
    TextPrivate,
    TextSingle,
}

impl ExtensionFieldType {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("boolean") => Self::Boolean,
            Some("fixed") => Self::Fixed,
            Some("hidden") => Self::Hidden,
            Some("jid-multi") => Self::JidMulti,
            Some("jid-single") => Self::JidSingle,
            Some("list-multi") => Self::ListMulti,
            Some("list-single") => Self::ListSingle,
            Some("text-multi") => Self::TextMulti,
            Some("text-private") => Self::TextPrivate,
            _ => Self::TextSingle,
        }
    }

    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Fixed => "fixed",
            Self::Hidden => "hidden",
            Self::JidMulti => "jid-multi",
            Self::JidSingle => "jid-single",
            Self::ListMulti => "list-multi",
            Self::ListSingle => "list-single",
            Self::TextMulti => "text-multi",
            Self::TextPrivate => "text-private",
            Self::TextSingle => "text-single",
        }
    }
}

/// One `<option/>` of a list field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionFormOption {
    pub label: Option<String>,
    pub value: String,
}

/// One `<field/>` of a command-response form, as presented to the
/// palette UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionFormFieldDescriptor {
    /// The `var` attribute; empty only for `fixed` fields, which may
    /// omit it per XEP-0004.
    pub var: String,
    pub label: Option<String>,
    pub field_type: ExtensionFieldType,
    pub required: bool,
    /// Forbidden-field defense (wasm client
    /// `isForbiddenExtensionCommandField`): `text-private` fields and
    /// secret-named vars must never render an input or submit — a
    /// third-party extension asking for credentials is hostile.
    pub blocked: bool,
    pub options: Vec<ExtensionFormOption>,
    pub values: Vec<String>,
}

/// The XEP-0004 form of a command response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCommandForm {
    pub title: Option<String>,
    pub instructions: Option<String>,
    pub fields: Vec<ExtensionFormFieldDescriptor>,
}

/// XEP-0050 §3 `<note/>` severity. Unknown or absent types parse as
/// [`Self::Info`] per the XEP's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionNoteType {
    Info,
    Warn,
    Error,
}

impl ExtensionNoteType {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("warn") | Some("warning") => Self::Warn,
            Some("error") => Self::Error,
            _ => Self::Info,
        }
    }
}

/// One `<note/>` diagnostic from a command response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCommandNote {
    pub note_type: ExtensionNoteType,
    pub value: String,
}

/// A parsed extension command response.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionCommandResult {
    pub status: AdHocStatus,
    pub session_id: Option<String>,
    /// Actions the service allows for the next stage, mirroring the
    /// wasm client's semantics: the `<actions/>` children plus its
    /// `execute` default, with XEP-0050 §3.4's implied
    /// complete/cancel when the command is `executing`.
    pub actions: Vec<AdHocAction>,
    pub form: Option<ExtensionCommandForm>,
    pub notes: Vec<ExtensionCommandNote>,
}

/// One submitted field value pair (`var` → `<value/>` children).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionSubmitField {
    pub var: String,
    pub values: Vec<String>,
}
