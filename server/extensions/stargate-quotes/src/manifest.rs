use crate::bindings::waddle::extension::types;
use crate::constants::{COMMAND_NAME, COMMAND_NODE, PLUGIN_NAME, VERSION};
use crate::ui::{display, plugin_id};

pub(crate) fn manifest() -> types::ExtensionManifest {
    types::ExtensionManifest {
        id: plugin_id(),
        name: display(PLUGIN_NAME),
        version: types::PluginVersion {
            value: VERSION.to_string(),
        },
        payloads: vec![],
        capabilities: vec![
            types::ExtensionCapability::Commands,
            types::ExtensionCapability::HostMessageSend,
            types::ExtensionCapability::MessageEnrich,
        ],
        commands: vec![types::CommandDescriptor {
            node: types::CommandNode {
                value: COMMAND_NODE.to_string(),
            },
            name: display(COMMAND_NAME),
            scope: types::CommandScope::Channel,
            composer_prefix: Some("stargate".to_string()),
            inline_field: None,
        }],
        routes: vec![],
        pubsub_nodes: vec![],
        profile: Some(types::ExtensionProfile {
            display_name: display(PLUGIN_NAME),
            description: Some(display("Random Stargate quotes")),
            accent: Some("blue".to_string()),
            avatar: None,
            bot_hat_label: Some(display("Bot")),
        }),
        artifact: None,
    }
}
