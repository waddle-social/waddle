mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "waddle-extension",
        with: {
            "wasi:logging/logging@0.1.0-draft": generate,
            "wasi:clocks/monotonic-clock@0.2.0": generate,
            "wasi:io/poll@0.2.0": generate,
        },
    });
}

use bindings::exports;
use bindings::waddle::extension::types;

struct LinksTaskBoard;

bindings::export!(LinksTaskBoard with_types_in bindings);

const PLUGIN_ID: &str = "links-task-board";
const PLUGIN_NAME: &str = "Links Task Board";
const PLUGIN_NS: &str = "urn:waddle:links-task-board:1";
const INVOKE_NODE: &str = "urn:waddle:extension:1:invoke";
const COMMAND_NODE: &str = "urn:waddle:extension:1:links-task-board";
const VERSION: &str = "0.1.0";

impl exports::waddle::extension::lifecycle::Guest for LinksTaskBoard {
    fn init(_config: String) -> Result<types::ExtensionManifest, String> {
        Ok(manifest())
    }
}

impl exports::waddle::extension::framework::Guest for LinksTaskBoard {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::MessageHook(hook) => {
                if hook.links.is_empty() {
                    vec![]
                } else {
                    vec![types::ExtensionEffect::EnrichMessage(
                        types::ExtensionEnvelope {
                            version: 1,
                            enrichments: vec![link_enrichment(&hook)],
                        },
                    )]
                }
            }
            types::ExtensionEvent::Launch(launch) => {
                let mut effects = vec![types::ExtensionEffect::PublishPubsub(
                    types::PubsubPublish {
                        node: links_node(&launch.context.waddle_id),
                        item_id: Some(types::PubsubItemId {
                            value: launch.launch_id.value.clone(),
                        }),
                        payload: payload(
                            "link",
                            vec![
                                ("source", "launch".to_string()),
                                ("launch-id", launch.launch_id.value.clone()),
                            ],
                            "Saved link",
                        ),
                    },
                )];

                if launch.launch_id.value == "create-task" {
                    effects.push(types::ExtensionEffect::PublishPubsub(
                        types::PubsubPublish {
                            node: tasks_node(&launch.context.waddle_id, "inbox"),
                            item_id: Some(types::PubsubItemId {
                                value: "task-from-link".to_string(),
                            }),
                            payload: payload(
                                "task",
                                vec![
                                    ("task-id", "task-from-link".to_string()),
                                    ("status", "todo".to_string()),
                                    ("source", "link-launch".to_string()),
                                ],
                                "Review saved link",
                            ),
                        },
                    ));
                }

                effects
            }
            types::ExtensionEvent::Command(command) => {
                vec![
                    types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
                        node: boards_node(&command.waddle_id),
                        item_id: Some(types::PubsubItemId {
                            value: "inbox".to_string(),
                        }),
                        payload: payload(
                            "board",
                            vec![
                                ("board-id", "inbox".to_string()),
                                ("title", "Inbox".to_string()),
                            ],
                            "Links Inbox",
                        ),
                    }),
                    types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
                        node: tasks_node(&command.waddle_id, "inbox"),
                        item_id: Some(types::PubsubItemId {
                            value: "welcome-task".to_string(),
                        }),
                        payload: payload(
                            "task",
                            vec![
                                ("task-id", "welcome-task".to_string()),
                                ("status", "todo".to_string()),
                            ],
                            "Use Save link from a link preview to populate this board.",
                        ),
                    }),
                ]
            }
        };
        Ok(types::ExtensionResponse { effects })
    }
}

fn manifest() -> types::ExtensionManifest {
    types::ExtensionManifest {
        id: plugin_id(),
        name: display(PLUGIN_NAME),
        version: types::PluginVersion {
            value: VERSION.to_string(),
        },
        payloads: vec![
            payload_rule(types::PayloadSurface::MessageEnrichment, "link"),
            payload_rule(types::PayloadSurface::LaunchPayload, "link"),
            payload_rule(types::PayloadSurface::PubsubItem, "board"),
            payload_rule(types::PayloadSurface::PubsubItem, "link"),
            payload_rule(types::PayloadSurface::PubsubItem, "task"),
            payload_rule(types::PayloadSurface::PubsubItem, "opengraph-cache"),
        ],
        capabilities: vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::Commands,
            types::ExtensionCapability::Launch,
            types::ExtensionCapability::PubsubPublish,
            types::ExtensionCapability::ArtifactReference,
            types::ExtensionCapability::UiDeclarative,
        ],
        commands: vec![
            command_descriptor(COMMAND_NODE, "Open Links Task Board"),
            command_descriptor(INVOKE_NODE, "Run Links Task Board action"),
        ],
        pubsub_nodes: vec![
            pubsub_template("links"),
            pubsub_template("boards"),
            pubsub_template("tasks:{board-id}"),
            pubsub_template("opengraph-cache"),
        ],
        artifact: None,
    }
}

fn link_enrichment(hook: &types::MessageHook) -> types::MessageEnrichment {
    let first = &hook.links[0];
    let url = first.url.value.clone();
    types::MessageEnrichment {
        id: enrichment_id("link-preview"),
        plugin: plugin_id(),
        capability: types::ExtensionCapability::MessageEnrich,
        payload_namespace: payload_namespace(),
        created_at: timestamp(),
        source: hook
            .context
            .stanza_id
            .clone()
            .map(|stanza_id| types::MessageSource {
                stanza_id,
                body_range: Some(first.range),
            }),
        ui: vec![view(
            "link-card",
            PLUGIN_NAME,
            &format!("Preview and save {url}"),
            Some("save-link"),
        )],
        payloads: vec![payload(
            "link",
            vec![("url", url.clone()), ("og-cache-item", cache_item_id(&url))],
            "Link preview",
        )],
        launches: vec![
            launch("save-link", "save-link", "Save link", &hook.context),
            launch("create-task", "create-task", "Create task", &hook.context),
        ],
    }
}

fn launch(
    id_value: &str,
    action: &str,
    label: &str,
    context: &types::MessageContext,
) -> types::LaunchDescriptor {
    types::LaunchDescriptor {
        id: launch_id(id_value),
        plugin: plugin_id(),
        action: types::ActionId {
            value: action.to_string(),
        },
        command_node: types::CommandNode {
            value: INVOKE_NODE.to_string(),
        },
        label: display(label),
        context: types::LaunchContext {
            waddle_id: context.waddle_id.clone(),
            source_stanza_id: context.stanza_id.clone(),
        },
        payloads: vec![payload(
            "link",
            vec![("launch-action", action.to_string())],
            label,
        )],
        fallback: None,
        expires_at: None,
    }
}

fn view(id_value: &str, title: &str, text: &str, launch_id_value: Option<&str>) -> types::UiView {
    let mut blocks = vec![types::UiBlock::Text(types::TextBlock {
        text: display(text),
        style: types::TextStyle::Body,
    })];
    if let Some(launch_id_value) = launch_id_value {
        blocks.push(types::UiBlock::Action(types::ActionBlock {
            launch_id: launch_id(launch_id_value),
            label: display("Save"),
        }));
    }
    types::UiView {
        id: types::UiViewId {
            value: id_value.to_string(),
        },
        title: Some(display(title)),
        blocks,
    }
}

fn payload(root: &str, attrs: Vec<(&str, String)>, text: &str) -> types::ExtensionPayload {
    let namespace = payload_namespace();
    let root_record = types::PayloadRoot {
        namespace: namespace.clone(),
        local_name: root.to_string(),
    };
    let mut xml_attrs = Vec::new();
    for (name, value) in attrs {
        xml_attrs.push(types::XmlAttribute {
            namespace: None,
            local_name: name.to_string(),
            value,
        });
    }
    types::ExtensionPayload {
        namespace: namespace.clone(),
        root: root_record,
        tokens: vec![
            types::XmlToken::StartElement(types::XmlElement {
                namespace,
                local_name: root.to_string(),
                attributes: xml_attrs,
            }),
            types::XmlToken::Text(text.to_string()),
            types::XmlToken::EndElement,
        ],
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

fn command_descriptor(node: &str, name: &str) -> types::CommandDescriptor {
    types::CommandDescriptor {
        node: types::CommandNode {
            value: node.to_string(),
        },
        name: display(name),
        composer_prefix: None,
    }
}

fn pubsub_template(suffix: &str) -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:waddle:{{waddle-id}}:{suffix}"),
    }
}

fn links_node(waddle_id: &types::WaddleId) -> types::PubsubNode {
    typed_node(waddle_id, "links")
}

fn boards_node(waddle_id: &types::WaddleId) -> types::PubsubNode {
    typed_node(waddle_id, "boards")
}

fn tasks_node(waddle_id: &types::WaddleId, board_id: &str) -> types::PubsubNode {
    typed_node(waddle_id, &format!("tasks:{board_id}"))
}

fn typed_node(waddle_id: &types::WaddleId, suffix: &str) -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:waddle:{}:{suffix}", waddle_id.value),
    }
}

fn cache_item_id(url: &str) -> String {
    let sanitized: String = url
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("og-{sanitized}")
}

fn enrichment_id(value: &str) -> types::EnrichmentId {
    types::EnrichmentId {
        value: value.to_string(),
    }
}

fn launch_id(value: &str) -> types::LaunchId {
    types::LaunchId {
        value: value.to_string(),
    }
}

fn plugin_id() -> types::PluginId {
    types::PluginId {
        value: PLUGIN_ID.to_string(),
    }
}

fn payload_namespace() -> types::PayloadNamespace {
    types::PayloadNamespace {
        value: PLUGIN_NS.to_string(),
    }
}

fn display(value: &str) -> types::DisplayText {
    types::DisplayText {
        value: value.to_string(),
    }
}

fn timestamp() -> types::Timestamp {
    types::Timestamp {
        value: "2026-04-27T00:00:00Z".to_string(),
    }
}
