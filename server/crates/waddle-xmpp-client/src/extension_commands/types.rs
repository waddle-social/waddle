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
    /// Parse a supported wire value, rejecting unrecognized or malformed input.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "global" => Some(Self::Global),
            "channel" => Some(Self::Channel),
            _ => None,
        }
    }

    /// Return the canonical token used in protocol attributes or forms.
    #[must_use]
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
    /// Bare JID of the service handling this request.
    pub service_jid: String,
    /// PubSub or discovery node selected by the request.
    pub node: String,
    /// Human-readable name advertised to clients.
    pub name: String,
}

/// Composer metadata parsed from a command's `disco#info` extension
/// form (`FORM_TYPE` [`super::NS_WADDLE_EXTENSION_1`] or
/// [`super::EXTENSION_COMMAND_FORM_TYPE`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionCommandMetadata {
    /// Scope in which the route, command, or observation applies.
    pub scope: Option<ExtensionCommandScope>,
    /// Prefix recognized by the message composer for this route.
    pub composer_prefix: Option<String>,
    /// Field whose value may be shown inline by the host.
    pub inline_field: Option<String>,
    /// Whether the route may execute directly from the message composer.
    pub composer_execute: bool,
}

/// One discovered extension command with its composer metadata —
/// the typed equivalent of the wasm client's
/// `DiscoveredExtensionCommand`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCommandDescriptor {
    /// Bare JID of the service handling this request.
    pub service_jid: String,
    /// PubSub or discovery node selected by the request.
    pub node: String,
    /// Human-readable name advertised to clients.
    pub name: String,
    /// Scope in which the route, command, or observation applies.
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
    /// A boolean-valued data-form field.
    Boolean,
    /// Read-only text displayed by the data form.
    Fixed,
    /// A data-form value carried through without user presentation.
    Hidden,
    /// A data-form field containing multiple JIDs.
    JidMulti,
    /// A data-form field containing one JID.
    JidSingle,
    /// A data-form field selecting multiple listed options.
    ListMulti,
    /// A data-form field selecting one listed option.
    ListSingle,
    /// A data-form field accepting multiple lines of text.
    TextMulti,
    /// A data-form field accepting text that should be concealed in the UI.
    TextPrivate,
    /// A data-form field accepting a single line of text.
    TextSingle,
}

impl ExtensionFieldType {
    /// Parse a supported wire value, rejecting unrecognized or malformed input.
    #[must_use]
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

    /// Return the canonical token used in protocol attributes or forms.
    #[must_use]
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
    /// Human-readable label rendered by the host.
    pub label: Option<String>,
    /// Underlying validated value.
    pub value: String,
}

/// One `<field/>` of a command-response form, as presented to the
/// palette UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionFormFieldDescriptor {
    /// The `var` attribute; empty only for `fixed` fields, which may
    /// omit it per XEP-0004.
    pub var: String,
    /// Human-readable label rendered by the host.
    pub label: Option<String>,
    /// XEP-0004 field type controlling interpretation of its values.
    pub field_type: ExtensionFieldType,
    /// Whether a value must be supplied for this field.
    pub required: bool,
    /// Forbidden-field defense (wasm client
    /// `isForbiddenExtensionCommandField`): `text-private` fields and
    /// secret-named vars must never render an input or submit — a
    /// third-party extension asking for credentials is hostile.
    pub blocked: bool,
    /// Allowed values offered to the user.
    pub options: Vec<ExtensionFormOption>,
    /// Values associated with this field.
    pub values: Vec<String>,
}

/// The XEP-0004 form of a command response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCommandForm {
    /// Human-readable title rendered by the host.
    pub title: Option<String>,
    /// Instructions displayed to the user.
    pub instructions: Option<String>,
    /// Fields carried by the data form or provider payload.
    pub fields: Vec<ExtensionFormFieldDescriptor>,
}

/// XEP-0050 §3 `<note/>` severity. Unknown or absent types parse as
/// [`Self::Info`] per the XEP's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionNoteType {
    /// Informational command note.
    Info,
    /// A warning note returned by the command.
    Warn,
    /// The operation failed; inspect the associated error details.
    Error,
}

impl ExtensionNoteType {
    /// Parse a supported wire value, rejecting unrecognized or malformed input.
    #[must_use]
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
    /// Severity of the ad-hoc command note.
    pub note_type: ExtensionNoteType,
    /// Underlying validated value.
    pub value: String,
}

/// A parsed extension command response.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionCommandResult {
    /// Protocol status returned by the peer.
    pub status: AdHocStatus,
    /// Identifier of the session associated with this value.
    pub session_id: Option<String>,
    /// Actions the service allows for the next stage, mirroring the
    /// wasm client's semantics: the `<actions/>` children plus its
    /// `execute` default, with XEP-0050 §3.4's implied
    /// complete/cancel when the command is `executing`.
    pub actions: Vec<AdHocAction>,
    /// Data form carried by the command response.
    pub form: Option<ExtensionCommandForm>,
    /// Human-readable notes returned by the command.
    pub notes: Vec<ExtensionCommandNote>,
}

/// One submitted field value pair (`var` → `<value/>` children).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionSubmitField {
    /// XEP-0004 field name whose value is being accessed or validated.
    pub var: String,
    /// Values associated with this field.
    pub values: Vec<String>,
}
