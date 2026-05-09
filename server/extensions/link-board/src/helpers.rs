use super::*;

use core::fmt::Write as _;

#[cfg(not(test))]
use crate::bindings::waddle::extension::runtime;
use sha2::{Digest, Sha256};

pub(super) fn field_value(fields: &[types::FormFieldValue], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|field| field.name.value == name)
        .and_then(|field| field.values.first())
        .map(|value| value.value.clone())
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

pub(super) fn links_node_template() -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{{room}}:links"),
    }
}

pub(super) fn links_node(room: &types::RoomJid) -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{}:links", room.value),
    }
}

pub(super) fn normalized_url(url: &str) -> String {
    let trimmed = url.trim();
    if let Some(without_hash) = trimmed.split('#').next() {
        without_hash.to_ascii_lowercase()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

pub(super) fn link_item_id(normalized: &str) -> String {
    hashed_id("link", normalized)
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

pub(super) fn enrichment_id(value: &str) -> types::EnrichmentId {
    types::EnrichmentId {
        value: value.to_string(),
    }
}

pub(super) fn launch_id(value: &str) -> types::LaunchId {
    types::LaunchId {
        value: value.to_string(),
    }
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
