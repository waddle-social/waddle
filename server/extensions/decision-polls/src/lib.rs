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

struct DecisionPolls;

bindings::export!(DecisionPolls with_types_in bindings);

const PLUGIN_ID: &str = "decision-polls";
const PLUGIN_NAME: &str = "Decision Polls";
const PLUGIN_NS: &str = "urn:waddle:decision-polls:1";
const INVOKE_NODE: &str = "urn:waddle:extension:1:invoke";
const COMMAND_NODE: &str = "urn:waddle:extension:1:decision-polls";
const VERSION: &str = "0.1.0";
const POLL_ID: &str = "next-step";
const QUESTION: &str = "Choose the next step.";
const OPTIONS: [(&str, &str); 3] = [
    ("approve", "Approve"),
    ("revise", "Revise"),
    ("block", "Block"),
];

impl exports::waddle::extension::lifecycle::Guest for DecisionPolls {
    fn init(_config: String) -> Result<types::ExtensionManifest, String> {
        Ok(manifest())
    }
}

impl exports::waddle::extension::framework::Guest for DecisionPolls {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::MessageHook(hook) if hook.body.value.contains("/poll") => {
                vec![visible_message(poll_message(
                    hook.context.waddle_id,
                    hook.context.stanza_id,
                ))]
            }
            types::ExtensionEvent::Command(command) => {
                vec![
                    types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
                        node: polls_node(&command.waddle_id),
                        item_id: Some(types::PubsubItemId {
                            value: POLL_ID.to_string(),
                        }),
                        payload: poll_payload(),
                    }),
                    visible_message(poll_message(command.waddle_id, None)),
                ]
            }
            types::ExtensionEvent::Launch(launch) => {
                let option_id = launch
                    .launch_id
                    .value
                    .strip_prefix("vote-")
                    .unwrap_or(launch.launch_id.value.as_str());
                vec![
                    types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
                        node: votes_node(&launch.context.waddle_id, POLL_ID),
                        item_id: Some(types::PubsubItemId {
                            value: launch.launch_id.value.clone(),
                        }),
                        payload: payload(
                            "vote",
                            vec![
                                ("poll-id", POLL_ID.to_string()),
                                ("option-id", option_id.to_string()),
                            ],
                            "Vote recorded",
                        ),
                    }),
                    types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
                        node: results_node(&launch.context.waddle_id),
                        item_id: Some(types::PubsubItemId {
                            value: POLL_ID.to_string(),
                        }),
                        payload: payload(
                            "results",
                            vec![
                                ("poll-id", POLL_ID.to_string()),
                                ("latest-option-id", option_id.to_string()),
                            ],
                            "Poll results updated",
                        ),
                    }),
                    visible_message(VisibleMessage {
                        ui: vec![view("poll-result", "Poll", "Vote recorded.", &[])],
                        payloads: vec![payload(
                            "vote-ack",
                            vec![("option-id", option_id.to_string())],
                            "Vote recorded",
                        )],
                        launches: vec![],
                    }),
                ]
            }
            _ => vec![],
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
            types::ExtensionCapability::UiDeclarative,
        ],
        commands: vec![
            command_descriptor(COMMAND_NODE, "Create Decision Poll"),
            command_descriptor(INVOKE_NODE, "Run Decision Poll action"),
        ],
        pubsub_nodes: vec![
            pubsub_template("polls"),
            pubsub_template("results"),
            pubsub_template("votes:{poll-id}"),
        ],
        artifact: None,
    }
}

struct VisibleMessage {
    ui: Vec<types::UiView>,
    payloads: Vec<types::ExtensionPayload>,
    launches: Vec<types::LaunchDescriptor>,
}

fn visible_message(message: VisibleMessage) -> types::ExtensionEffect {
    types::ExtensionEffect::EnrichMessage(types::ExtensionEnvelope {
        version: 1,
        enrichments: vec![types::MessageEnrichment {
            id: types::EnrichmentId {
                value: "decision-polls-message".to_string(),
            },
            plugin: plugin_id(),
            capability: types::ExtensionCapability::MessageEnrich,
            payload_namespace: payload_namespace(),
            created_at: types::Timestamp {
                value: "2026-04-27T00:00:00Z".to_string(),
            },
            source: None,
            ui: message.ui,
            payloads: message.payloads,
            launches: message.launches,
        }],
    })
}

fn poll_message(
    waddle_id: types::WaddleId,
    source_stanza_id: Option<types::StanzaId>,
) -> VisibleMessage {
    VisibleMessage {
        ui: vec![view(
            "poll-command",
            PLUGIN_NAME,
            QUESTION,
            &["Approve", "Revise", "Block"],
        )],
        payloads: vec![poll_payload()],
        launches: vote_launches(waddle_id, source_stanza_id),
    }
}

fn view(id: &str, title: &str, text: &str, actions: &[&str]) -> types::UiView {
    let mut blocks = vec![types::UiBlock::Text(types::TextBlock {
        text: display(text),
        style: types::TextStyle::Body,
    })];
    for action in actions {
        blocks.push(types::UiBlock::Action(types::ActionBlock {
            launch_id: types::LaunchId {
                value: format!("vote-{}", action.to_ascii_lowercase()),
            },
            label: display(action),
        }));
    }
    types::UiView {
        id: types::UiViewId {
            value: id.to_string(),
        },
        title: Some(display(title)),
        blocks,
    }
}

fn vote_launches(
    waddle_id: types::WaddleId,
    source_stanza_id: Option<types::StanzaId>,
) -> Vec<types::LaunchDescriptor> {
    OPTIONS
        .into_iter()
        .map(|(choice, label)| types::LaunchDescriptor {
            id: types::LaunchId {
                value: format!("vote-{choice}"),
            },
            plugin: plugin_id(),
            action: types::ActionId {
                value: "vote".to_string(),
            },
            command_node: types::CommandNode {
                value: INVOKE_NODE.to_string(),
            },
            label: display(label),
            context: types::LaunchContext {
                waddle_id: waddle_id.clone(),
                source_stanza_id: source_stanza_id.clone(),
            },
            payloads: vec![payload(
                "vote-request",
                vec![
                    ("poll-id", POLL_ID.to_string()),
                    ("option-id", choice.to_string()),
                ],
                label,
            )],
            fallback: None,
            expires_at: None,
        })
        .collect()
}

fn poll_payload() -> types::ExtensionPayload {
    payload(
        "poll",
        vec![
            ("poll-id", POLL_ID.to_string()),
            ("mode", "single".to_string()),
            ("closes-at", "2026-04-27T11:00:00Z".to_string()),
        ],
        QUESTION,
    )
}

fn payload(root: &str, attrs: Vec<(&str, String)>, text: &str) -> types::ExtensionPayload {
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

fn polls_node(waddle_id: &types::WaddleId) -> types::PubsubNode {
    typed_node(waddle_id, "polls")
}

fn results_node(waddle_id: &types::WaddleId) -> types::PubsubNode {
    typed_node(waddle_id, "results")
}

fn votes_node(waddle_id: &types::WaddleId, poll_id: &str) -> types::PubsubNode {
    typed_node(waddle_id, &format!("votes:{poll_id}"))
}

fn typed_node(waddle_id: &types::WaddleId, suffix: &str) -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:waddle:{}:{suffix}", waddle_id.value),
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
