use crate::bindings::waddle::extension::types;
use crate::constants::{AI_COMMAND, COMMAND_NODE, PLUGIN_NAME, VERSION};
use crate::ui::{display, payload_namespace, plugin_id};

pub(crate) fn manifest() -> types::ExtensionManifest {
    types::ExtensionManifest {
        id: plugin_id(),
        name: display(PLUGIN_NAME),
        version: types::PluginVersion {
            value: VERSION.to_string(),
        },
        payloads: vec![payload_rule(
            types::PayloadSurface::MessageEnrichment,
            "assistant-answer",
        )],
        capabilities: vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::HostMamRead,
            types::ExtensionCapability::HostMembersRead,
            types::ExtensionCapability::HostPresenceRead,
            types::ExtensionCapability::HostRosterRead,
            types::ExtensionCapability::HostChannelsRead,
            types::ExtensionCapability::HostSpacesRead,
            types::ExtensionCapability::HostMessageSend,
            types::ExtensionCapability::OutboundHttpRequest,
            types::ExtensionCapability::Commands,
        ],
        commands: vec![command_descriptor(
            COMMAND_NODE,
            AI_COMMAND,
            types::CommandScope::Global,
        )],
        routes: vec![],
        pubsub_nodes: vec![],
        artifact: None,
    }
}

fn payload_rule(surface: types::PayloadSurface, root: &str) -> types::PayloadRule {
    types::PayloadRule {
        surface,
        root: types::PayloadRoot {
            namespace: payload_namespace(),
            local_name: root.to_string(),
        },
    }
}

fn command_descriptor(
    node: &str,
    name: &str,
    scope: types::CommandScope,
) -> types::CommandDescriptor {
    types::CommandDescriptor {
        node: types::CommandNode {
            value: node.to_string(),
        },
        name: display(name),
        scope,
    }
}
