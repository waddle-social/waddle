//! Discovery helpers and response parsers for extension commands.
//!
//! Parsing mirrors the wasm chat client
//! (`chat/src/lib/xmpp/extension-commands/{disco,xml}.ts`) so both
//! native clients resolve the same service, the same command list,
//! and the same typed results from identical wire traffic.

use minidom::Element;

use crate::discovery::{DiscoInfoResult, DiscoItem};
use crate::xep::xep0050::{AdHocAction, AdHocStatus, NS_COMMANDS, NS_DATA_FORMS};

use super::types::{
    ExtensionCommandDescriptor, ExtensionCommandForm, ExtensionCommandItemRef,
    ExtensionCommandMetadata, ExtensionCommandNote, ExtensionCommandResult, ExtensionCommandScope,
    ExtensionFieldType, ExtensionFormFieldDescriptor, ExtensionFormOption, ExtensionNoteType,
};
use super::{EXTENSION_COMMAND_FORM_TYPE, INVOKE_COMMAND_NODE, NS_WADDLE_EXTENSION_1};

// ── Service discovery ────────────────────────────────────────────────

/// Conventional extension service JID: `extensions.<domain>`.
pub fn fallback_extension_service_jid(domain: &str) -> String {
    format!("extensions.{domain}")
}

/// Probe order for the extension service: every component the domain
/// advertised, then the conventional `extensions.<domain>` JID, then
/// the domain itself — deduplicated, first qualified candidate wins.
pub fn extension_service_candidates(domain: &str, discovered_jids: &[String]) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let fallback = fallback_extension_service_jid(domain);
    for jid in discovered_jids
        .iter()
        .map(String::as_str)
        .chain([fallback.as_str(), domain])
    {
        if !candidates.iter().any(|candidate| candidate == jid) {
            candidates.push(jid.to_string());
        }
    }
    candidates
}

/// A component qualifies as the extension service only when it
/// advertises BOTH the Waddle extension feature and XEP-0050 command
/// support.
pub fn is_extension_service(info: &DiscoInfoResult) -> bool {
    info.has_feature(NS_WADDLE_EXTENSION_1) && info.has_feature(NS_COMMANDS)
}

// ── Command-list discovery ───────────────────────────────────────────

/// Filter the service's XEP-0050 command list: keep items with a
/// node, drop the internal [`INVOKE_COMMAND_NODE`], fall back to the
/// service JID when the item omits its own, and to the node when the
/// item has no display name.
pub fn extension_command_refs(
    items: Vec<DiscoItem>,
    service_jid: &str,
) -> Vec<ExtensionCommandItemRef> {
    items
        .into_iter()
        .filter_map(|item| {
            let node = item.node.filter(|node| node != INVOKE_COMMAND_NODE)?;
            let item_service_jid = if item.jid.is_empty() {
                service_jid.to_string()
            } else {
                item.jid
            };
            let name = item
                .name
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| node.clone());
            Some(ExtensionCommandItemRef {
                service_jid: item_service_jid,
                node,
                name,
            })
        })
        .collect()
}

/// Composer metadata from a command's `disco#info` extension forms.
/// Both FORM_TYPEs are accepted: [`NS_WADDLE_EXTENSION_1`] (the
/// service-level form) and [`EXTENSION_COMMAND_FORM_TYPE`] (the
/// per-command form the server emits today).
pub fn parse_extension_command_metadata(info: &DiscoInfoResult) -> ExtensionCommandMetadata {
    let mut metadata = ExtensionCommandMetadata::default();
    for form in info.forms.iter().filter(|form| {
        matches!(
            form.form_type.as_deref(),
            Some(NS_WADDLE_EXTENSION_1) | Some(EXTENSION_COMMAND_FORM_TYPE)
        )
    }) {
        let value = |var: &str| {
            form.fields
                .iter()
                .find(|field| field.var == var)
                .and_then(|field| field.values.first())
                .filter(|value| !value.is_empty())
                .cloned()
        };
        if let Some(scope) =
            value("waddle#command_scope").and_then(|v| ExtensionCommandScope::parse(&v))
        {
            metadata.scope = Some(scope);
        }
        if let Some(prefix) = value("waddle#composer_prefix") {
            metadata.composer_prefix = Some(prefix);
        }
        if let Some(inline_field) = value("waddle#inline_field") {
            metadata.inline_field = Some(inline_field);
        }
        if value("waddle#composer_execute").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            metadata.composer_execute = true;
        }
    }
    metadata
}

/// Combine a command-list item with its (possibly absent) metadata.
/// A command whose `disco#info` failed defaults to global scope with
/// no composer affordances — the wasm client's fallback.
pub fn extension_command_descriptor(
    item: ExtensionCommandItemRef,
    metadata: ExtensionCommandMetadata,
) -> ExtensionCommandDescriptor {
    ExtensionCommandDescriptor {
        service_jid: item.service_jid,
        node: item.node,
        name: item.name,
        scope: metadata.scope.unwrap_or(ExtensionCommandScope::Global),
        composer_prefix: metadata.composer_prefix,
        inline_field: metadata.inline_field,
        composer_execute: metadata.composer_execute,
    }
}

// ── Response parsing ─────────────────────────────────────────────────

/// Typed shape errors for extension command responses. These indicate
/// a server-side bug or schema drift, never a user mistake.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtensionResponseError {
    #[error("extension command response missing <command/>")]
    MissingCommand,
    #[error("extension command response missing or unknown status")]
    InvalidStatus,
}

/// Parse a command IQ result into a typed [`ExtensionCommandResult`].
pub fn parse_extension_command_result(
    iq: &Element,
) -> Result<ExtensionCommandResult, ExtensionResponseError> {
    let command = iq
        .get_child("command", NS_COMMANDS)
        .ok_or(ExtensionResponseError::MissingCommand)?;
    let status = command
        .attr("status")
        .and_then(AdHocStatus::parse)
        .ok_or(ExtensionResponseError::InvalidStatus)?;
    let session_id = command.attr("sessionid").map(str::to_string);
    let notes = command
        .children()
        .filter(|child| child.name() == "note" && child.ns() == NS_COMMANDS)
        .filter_map(|note| {
            let value = note.text().trim().to_string();
            (!value.is_empty()).then(|| ExtensionCommandNote {
                note_type: ExtensionNoteType::parse(note.attr("type")),
                value,
            })
        })
        .collect();
    let actions = parse_allowed_actions(command, status);
    let form = command.get_child("x", NS_DATA_FORMS).map(parse_form);
    Ok(ExtensionCommandResult {
        status,
        session_id,
        actions,
        form,
        notes,
    })
}

/// Allowed next-stage actions, mirroring the wasm client: the
/// `<actions/>` children plus its `execute` default; when the
/// command is `executing`, XEP-0050 §3.4 implies `complete` if no
/// `<actions/>` was sent, and `cancel` is always available.
fn parse_allowed_actions(command: &Element, status: AdHocStatus) -> Vec<AdHocAction> {
    let actions_element = command.get_child("actions", NS_COMMANDS);
    let mut allowed: Vec<AdHocAction> = Vec::new();
    let push = |action: AdHocAction, allowed: &mut Vec<AdHocAction>| {
        if !allowed.contains(&action) {
            allowed.push(action);
        }
    };
    if let Some(element) = actions_element {
        for (name, action) in [
            ("next", AdHocAction::Next),
            ("prev", AdHocAction::Prev),
            ("previous", AdHocAction::Prev),
            ("complete", AdHocAction::Complete),
            ("cancel", AdHocAction::Cancel),
        ] {
            if element.get_child(name, NS_COMMANDS).is_some() {
                push(action, &mut allowed);
            }
        }
        if let Some(action) = element.attr("execute").and_then(parse_next_stage_action) {
            push(action, &mut allowed);
        }
    }
    if status == AdHocStatus::Executing {
        if actions_element.is_none() {
            push(AdHocAction::Complete, &mut allowed);
        }
        push(AdHocAction::Cancel, &mut allowed);
    }
    allowed
}

fn parse_next_stage_action(value: &str) -> Option<AdHocAction> {
    match value {
        "next" => Some(AdHocAction::Next),
        "prev" | "previous" => Some(AdHocAction::Prev),
        "complete" => Some(AdHocAction::Complete),
        "cancel" => Some(AdHocAction::Cancel),
        _ => None,
    }
}

fn parse_form(x: &Element) -> ExtensionCommandForm {
    let child_text = |name: &str| {
        x.get_child(name, NS_DATA_FORMS)
            .map(Element::text)
            .filter(|text| !text.trim().is_empty())
    };
    ExtensionCommandForm {
        title: child_text("title"),
        instructions: child_text("instructions"),
        fields: x
            .children()
            .filter(|child| child.name() == "field" && child.ns() == NS_DATA_FORMS)
            .map(parse_field)
            .collect(),
    }
}

fn parse_field(field: &Element) -> ExtensionFormFieldDescriptor {
    let values = field
        .children()
        .filter(|child| child.name() == "value" && child.ns() == NS_DATA_FORMS)
        .map(Element::text)
        .collect();
    let options = field
        .children()
        .filter(|child| child.name() == "option" && child.ns() == NS_DATA_FORMS)
        .filter_map(|option| {
            let value = option
                .get_child("value", NS_DATA_FORMS)
                .map(Element::text)?;
            Some(ExtensionFormOption {
                label: option.attr("label").map(str::to_string),
                value,
            })
        })
        .collect();
    ExtensionFormFieldDescriptor {
        var: field.attr("var").unwrap_or_default().to_string(),
        label: field.attr("label").map(str::to_string),
        field_type: ExtensionFieldType::parse(field.attr("type")),
        required: field.get_child("required", NS_DATA_FORMS).is_some(),
        options,
        values,
    }
}
