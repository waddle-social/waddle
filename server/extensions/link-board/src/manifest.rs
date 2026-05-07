use super::*;

pub(super) fn manifest() -> types::ExtensionManifest {
    types::ExtensionManifest {
        id: plugin_id(),
        name: display(PLUGIN_NAME),
        version: types::PluginVersion {
            value: VERSION.to_string(),
        },
        payloads: vec![
            payload_rule(types::PayloadSurface::MessageEnrichment, "link"),
            payload_rule(types::PayloadSurface::LaunchPayload, "link"),
            payload_rule(types::PayloadSurface::PubsubItem, "link"),
        ],
        capabilities: vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::Launch,
            types::ExtensionCapability::PubsubPublish,
            types::ExtensionCapability::UiDeclarative,
        ],
        commands: vec![command_descriptor(
            INVOKE_NODE,
            "Run Link Board action",
            types::CommandScope::Channel,
        )],
        routes: vec![types::ExtensionRouteDescriptor {
            plugin: plugin_id(),
            id: types::RouteId {
                value: "saved-links".to_string(),
            },
            label: display("Saved Links"),
            scope: types::ExtensionRouteScope::Channel,
            surface: types::ExtensionRouteSurface::Gallery,
            state_node: links_node_template(),
            payload_namespace: payload_namespace(),
        }],
        pubsub_nodes: vec![links_node_template()],
        artifact: None,
    }
}
