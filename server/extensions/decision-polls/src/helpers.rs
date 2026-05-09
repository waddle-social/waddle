use super::*;

use core::fmt::Write as _;

#[cfg(not(test))]
use crate::bindings::waddle::extension::runtime;
use sha2::{Digest, Sha256};

pub(super) fn option_key(index: usize, field: &str) -> &'static str {
    match (index, field) {
        (0, "id") => "option-0-id",
        (0, "label") => "option-0-label",
        (1, "id") => "option-1-id",
        (1, "label") => "option-1-label",
        (2, "id") => "option-2-id",
        (2, "label") => "option-2-label",
        (3, "id") => "option-3-id",
        (3, "label") => "option-3-label",
        (4, "id") => "option-4-id",
        (4, "label") => "option-4-label",
        _ => "option-extra",
    }
}

pub(super) fn poll_options(
    fields: &[types::FormFieldValue],
) -> Result<Vec<PollOption>, types::ExtensionError> {
    let raw_options = field_values(fields, "options");
    if raw_options.is_empty() {
        return Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "missing required field options",
        ));
    }
    let options = raw_options
        .iter()
        .flat_map(|value| value.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(5)
        .enumerate()
        .map(|(index, label)| PollOption {
            id: format!("option-{}", index + 1),
            label: label.to_string(),
        })
        .collect::<Vec<_>>();
    if options.len() < 2 {
        return Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "polls require at least two options",
        ));
    }
    Ok(options)
}

pub(super) fn duration_seconds(value: &str) -> Result<i64, types::ExtensionError> {
    match value {
        "15m" => Ok(15 * 60),
        "1h" => Ok(60 * 60),
        "1d" => Ok(24 * 60 * 60),
        "1w" => Ok(7 * 24 * 60 * 60),
        _ => Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "unsupported poll duration",
        )),
    }
}

pub(super) fn closes_at(duration_seconds: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(duration_seconds)).to_rfc3339()
}

pub(super) fn required_field(
    fields: &[types::FormFieldValue],
    name: &str,
) -> Result<String, types::ExtensionError> {
    field_value(fields, name)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            extension_error(
                types::ExtensionErrorCode::InvalidRequest,
                &format!("missing required field {name}"),
            )
        })
}

pub(super) fn field_value(fields: &[types::FormFieldValue], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|field| field.name.value == name)
        .and_then(|field| field.values.first())
        .map(|value| value.value.clone())
}

pub(super) fn field_values(fields: &[types::FormFieldValue], name: &str) -> Vec<String> {
    fields
        .iter()
        .find(|field| field.name.value == name)
        .map(|field| {
            field
                .values
                .iter()
                .map(|value| value.value.clone())
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn payload(
    root: &str,
    attrs: Vec<(&str, String)>,
    text: &str,
) -> types::ExtensionPayload {
    let namespace = payload_namespace();
    types::ExtensionPayload {
        namespace: namespace.clone(),
        root: types::PayloadRoot {
            namespace: namespace.clone(),
            local_name: root.to_string(),
        },
        tokens: vec![
            types::XmlToken::StartElement(types::XmlElement {
                namespace,
                local_name: root.to_string(),
                attributes: attrs
                    .into_iter()
                    .map(|(name, value)| types::XmlAttribute {
                        namespace: None,
                        local_name: name.to_string(),
                        value,
                    })
                    .collect(),
            }),
            types::XmlToken::Text(text.to_string()),
            types::XmlToken::EndElement,
        ],
    }
}

pub(super) fn payload_rule(surface: types::PayloadSurface, root: &str) -> types::PayloadRule {
    types::PayloadRule {
        surface,
        root: types::PayloadRoot {
            namespace: payload_namespace(),
            local_name: root.to_string(),
        },
    }
}

pub(super) fn command_descriptor(
    node: &str,
    name: &str,
    scope: types::CommandScope,
    composer_prefix: Option<&str>,
    inline_field: Option<&str>,
) -> types::CommandDescriptor {
    types::CommandDescriptor {
        node: types::CommandNode {
            value: node.to_string(),
        },
        name: display(name),
        scope,
        composer_prefix: composer_prefix.map(str::to_string),
        inline_field: inline_field.map(str::to_string),
    }
}

pub(super) fn polls_node_template() -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{{room}}:polls"),
    }
}

pub(super) fn results_node_template() -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{{room}}:results"),
    }
}

pub(super) fn votes_node_template() -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{{room}}:votes:{{poll-id}}"),
    }
}

pub(super) fn polls_node(room: &types::RoomJid) -> types::PubsubNode {
    typed_node(room, "polls")
}

pub(super) fn results_node(room: &types::RoomJid) -> types::PubsubNode {
    typed_node(room, "results")
}

pub(super) fn votes_node(room: &types::RoomJid, poll_id: &str) -> types::PubsubNode {
    typed_node(room, &format!("votes:{poll_id}"))
}

pub(super) fn typed_node(room: &types::RoomJid, suffix: &str) -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{}:{suffix}", room.value),
    }
}

pub(super) fn form_option(label: &str, value: &str) -> types::FormFieldOption {
    types::FormFieldOption {
        label: Some(display(label)),
        value: form_value(value),
    }
}

pub(super) fn form_value(value: &str) -> types::DataFormValue {
    types::DataFormValue {
        value: value.to_string(),
    }
}

pub(super) fn action_id(value: &str) -> types::UiActionId {
    types::UiActionId {
        value: value.to_string(),
    }
}

pub(super) fn stable_id(value: &str) -> String {
    hashed_id("voter", value)
}

pub(super) fn hashed_id(prefix: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut id = String::with_capacity(prefix.len() + 65);
    id.push_str(prefix);
    id.push('-');
    for byte in digest {
        let _ = write!(&mut id, "{byte:02x}");
    }
    id
}

pub(super) fn bare_jid_value(value: &str) -> &str {
    value.split_once('/').map_or(value, |(bare, _)| bare)
}

pub(super) fn plugin_id() -> types::PluginId {
    types::PluginId {
        value: PLUGIN_ID.to_string(),
    }
}

pub(super) fn payload_namespace() -> types::PayloadNamespace {
    types::PayloadNamespace {
        value: PLUGIN_NS.to_string(),
    }
}

pub(super) fn display(value: &str) -> types::DisplayText {
    types::DisplayText {
        value: value.to_string(),
    }
}

pub(super) fn timestamp() -> types::Timestamp {
    types::Timestamp {
        value: current_timestamp_value(),
    }
}

#[cfg(not(test))]
pub(super) fn current_timestamp_value() -> String {
    runtime::current_timestamp()
}

#[cfg(test)]
pub(super) fn current_timestamp_value() -> String {
    "2026-04-27T00:00:00Z".to_string()
}

pub(super) fn extension_error(
    code: types::ExtensionErrorCode,
    message: &str,
) -> types::ExtensionError {
    types::ExtensionError {
        code,
        message: display(message),
    }
}

pub(super) fn extension_error_from_host_tool(error: types::HostToolError) -> types::ExtensionError {
    let code = match error.code {
        types::HostToolErrorCode::Denied => types::ExtensionErrorCode::Denied,
        types::HostToolErrorCode::InvalidRequest => types::ExtensionErrorCode::InvalidRequest,
        types::HostToolErrorCode::NotFound => types::ExtensionErrorCode::InvalidRequest,
        types::HostToolErrorCode::Unsupported => types::ExtensionErrorCode::UnsupportedEvent,
        types::HostToolErrorCode::TemporaryFailure => types::ExtensionErrorCode::TemporaryFailure,
    };
    types::ExtensionError {
        code,
        message: error.message,
    }
}
