//! Typed parsers for `urn:waddle:group-dm:*` command responses.
//!
//! Responses are XEP-0050 `status='completed'` commands carrying a
//! XEP-0004 result form with top-level fields only (no §3.4
//! multiple-items shape), so parsing goes through the typed
//! [`DataForm`] from [`parse_command_response`]. Error IQs never
//! reach these parsers: the runtime surfaces them as
//! `ClientError::StanzaError` before response correlation completes.

use jid::BareJid;
use xmpp_parsers::data_forms::DataForm;

use crate::xep::xep0050::{parse_command_response, AdHocStatus};

use super::types::{GroupDmCreated, GroupDmLeft, GroupDmRenamed};

/// Typed shape errors for group-DM command responses. These indicate
/// a server-side bug or schema drift, never a user mistake.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GroupDmResponseError {
    #[error("group-dm response is not a XEP-0050 command result")]
    MissingCommand,
    #[error("group-dm command did not complete")]
    NotCompleted,
    #[error("group-dm response missing result form")]
    MissingForm,
    #[error("group-dm response missing required field `{var}`")]
    MissingField { var: String },
    #[error("group-dm response field `{var}` is not a valid bare JID")]
    InvalidJid { var: String },
}

type ParseResult<T> = Result<T, GroupDmResponseError>;

fn completed_form(iq: &minidom::Element) -> ParseResult<DataForm> {
    let response = parse_command_response(iq).ok_or(GroupDmResponseError::MissingCommand)?;
    if response.status != AdHocStatus::Completed {
        return Err(GroupDmResponseError::NotCompleted);
    }
    response.form.ok_or(GroupDmResponseError::MissingForm)
}

fn field_text(form: &DataForm, var: &str) -> Option<String> {
    form.fields
        .iter()
        .find(|f| f.var.as_deref() == Some(var))
        .and_then(|f| f.values.first())
        .cloned()
}

fn field_text_non_empty(form: &DataForm, var: &str) -> Option<String> {
    field_text(form, var).filter(|value| !value.is_empty())
}

fn required_bare_jid(form: &DataForm, var: &str) -> ParseResult<BareJid> {
    let raw =
        field_text_non_empty(form, var).ok_or_else(|| GroupDmResponseError::MissingField {
            var: var.to_string(),
        })?;
    raw.parse::<BareJid>()
        .map_err(|_| GroupDmResponseError::InvalidJid {
            var: var.to_string(),
        })
}

/// Lenient optional boolean: absent or unparseable values read as
/// `None` — the flags are informational echoes, never load-bearing.
fn optional_bool(form: &DataForm, var: &str) -> Option<bool> {
    match field_text_non_empty(form, var)?.as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// Parse a completed `urn:waddle:group-dm:create:0` result.
pub fn parse_group_dm_create_result(iq: &minidom::Element) -> ParseResult<GroupDmCreated> {
    let form = completed_form(iq)?;
    Ok(GroupDmCreated {
        room_jid: required_bare_jid(&form, "room_jid")?,
        is_public: optional_bool(&form, "is_public"),
        members_only: optional_bool(&form, "members_only"),
        persistent: optional_bool(&form, "persistent"),
    })
}

/// Parse a completed `urn:waddle:group-dm:rename:0` result. An empty
/// echoed `name` means the custom name was cleared.
pub fn parse_group_dm_rename_result(iq: &minidom::Element) -> ParseResult<GroupDmRenamed> {
    let form = completed_form(iq)?;
    Ok(GroupDmRenamed {
        room_jid: required_bare_jid(&form, "room_jid")?,
        name: field_text_non_empty(&form, "name"),
    })
}

/// Parse a completed `urn:waddle:group-dm:leave:0` result.
pub fn parse_group_dm_leave_result(iq: &minidom::Element) -> ParseResult<GroupDmLeft> {
    let form = completed_form(iq)?;
    Ok(GroupDmLeft {
        room_jid: required_bare_jid(&form, "room_jid")?,
        left: optional_bool(&form, "left").unwrap_or(false),
    })
}
