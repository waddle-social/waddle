use super::*;

pub(super) fn manifest() -> types::ExtensionManifest {
    types::ExtensionManifest {
        id: plugin_id(),
        name: display(PLUGIN_NAME),
        version: types::PluginVersion {
            value: VERSION.to_string(),
        },
        payloads: vec![
            payload_rule(types::PayloadSurface::MessageEnrichment, "poll"),
            payload_rule(types::PayloadSurface::MessageEnrichment, "vote-ack"),
            payload_rule(types::PayloadSurface::LaunchPayload, "vote-request"),
            payload_rule(types::PayloadSurface::PubsubItem, "poll"),
            payload_rule(types::PayloadSurface::PubsubItem, "results"),
            payload_rule(types::PayloadSurface::PubsubItem, "vote"),
        ],
        capabilities: vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::Commands,
            types::ExtensionCapability::Launch,
            types::ExtensionCapability::PubsubPublish,
            types::ExtensionCapability::HostMessageSend,
            types::ExtensionCapability::UiDeclarative,
        ],
        commands: vec![
            command_descriptor(
                COMMAND_NODE,
                "Create Decision Poll",
                types::CommandScope::Channel,
            ),
            command_descriptor(
                INVOKE_NODE,
                "Run Decision Poll action",
                types::CommandScope::Channel,
            ),
        ],
        routes: vec![types::ExtensionRouteDescriptor {
            plugin: plugin_id(),
            id: types::RouteId {
                value: "polls".to_string(),
            },
            label: display("Polls"),
            scope: types::ExtensionRouteScope::Channel,
            surface: types::ExtensionRouteSurface::ListView,
            state_node: polls_node_template(),
            payload_namespace: payload_namespace(),
        }],
        pubsub_nodes: vec![
            polls_node_template(),
            results_node_template(),
            votes_node_template(),
        ],
        artifact: None,
    }
}
