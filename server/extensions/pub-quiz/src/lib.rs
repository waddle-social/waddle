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

struct PubQuiz;

bindings::export!(PubQuiz with_types_in bindings);

const PLUGIN_ID: &str = "pub-quiz";
const PLUGIN_NAME: &str = "Pub Quiz";
const PLUGIN_NS: &str = "urn:waddle:pub-quiz:1";
const INVOKE_NODE: &str = "urn:waddle:extension:1:invoke";
const COMMAND_NODE: &str = "urn:waddle:extension:1:pub-quiz";
const VERSION: &str = "0.1.0";
const GAME_ID: &str = "xmpp-baseline";
const QUESTION_ID: &str = "xep-commands";
const QUESTION: &str = "Which XEP defines Ad-Hoc Commands?";
const CHOICES: [(&str, &str); 3] = [("a", "XEP-0030"), ("b", "XEP-0050"), ("c", "XEP-0060")];

impl exports::waddle::extension::lifecycle::Guest for PubQuiz {
    fn init(_config: String) -> Result<types::ExtensionManifest, String> {
        Ok(manifest())
    }
}

impl exports::waddle::extension::framework::Guest for PubQuiz {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::MessageHook(hook) if hook.body.value.contains("/quiz") => {
                vec![visible_message(quiz_message(
                    hook.context.waddle_id,
                    hook.context.stanza_id,
                ))]
            }
            types::ExtensionEvent::Command(command) => {
                vec![visible_message(bot_message(
                    "Quiz question posted.",
                    PLUGIN_NAME,
                    command.waddle_id,
                    None,
                ))]
            }
            types::ExtensionEvent::Launch(launch_invocation) => {
                let choice_id = launch_invocation
                    .launch_id
                    .value
                    .strip_prefix("answer-")
                    .unwrap_or(launch_invocation.launch_id.value.as_str())
                    .to_string();
                vec![
                    types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
                        node: submissions_node(&launch_invocation.context.waddle_id, GAME_ID),
                        item_id: Some(types::PubsubItemId {
                            value: launch_invocation.launch_id.value.clone(),
                        }),
                        payload: payload(
                            "submission",
                            vec![
                                ("game-id", GAME_ID.to_string()),
                                ("question-id", QUESTION_ID.to_string()),
                                ("choice-id", choice_id.clone()),
                            ],
                            "Recorded quiz answer",
                        ),
                    }),
                    types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
                        node: leaderboard_node(&launch_invocation.context.waddle_id),
                        item_id: Some(types::PubsubItemId {
                            value: "current".to_string(),
                        }),
                        payload: payload(
                            "leaderboard",
                            vec![
                                ("game-id", GAME_ID.to_string()),
                                ("latest-choice", choice_id),
                            ],
                            "Leaderboard updated",
                        ),
                    }),
                    visible_message(bot_message(
                        "Answer recorded.",
                        "Pub Quiz",
                        launch_invocation.context.waddle_id,
                        None,
                    )),
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
            payload_rule(types::PayloadSurface::MessageEnrichment, "quiz-question"),
            payload_rule(types::PayloadSurface::LaunchPayload, "answer-request"),
            payload_rule(types::PayloadSurface::PubsubItem, "game"),
            payload_rule(types::PayloadSurface::PubsubItem, "submission"),
            payload_rule(types::PayloadSurface::PubsubItem, "leaderboard"),
        ],
        capabilities: vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::Commands,
            types::ExtensionCapability::Launch,
            types::ExtensionCapability::PubsubPublish,
            types::ExtensionCapability::UiDeclarative,
        ],
        commands: vec![
            command_descriptor(COMMAND_NODE, "Start Pub Quiz"),
            command_descriptor(INVOKE_NODE, "Run Pub Quiz action"),
        ],
        pubsub_nodes: vec![
            pubsub_template("games"),
            pubsub_template("submissions:{game-id}"),
            pubsub_template("leaderboard"),
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
                value: "pub-quiz-message".to_string(),
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

fn quiz_message(
    waddle_id: types::WaddleId,
    source_stanza_id: Option<types::StanzaId>,
) -> VisibleMessage {
    bot_message(
        "Quiz question posted.",
        PLUGIN_NAME,
        waddle_id,
        source_stanza_id,
    )
}

fn bot_message(
    body: &str,
    title: &str,
    waddle_id: types::WaddleId,
    source_stanza_id: Option<types::StanzaId>,
) -> VisibleMessage {
    let _ = body;
    VisibleMessage {
        ui: vec![view("quiz-command", title, QUESTION)],
        payloads: vec![payload(
            "quiz-question",
            vec![
                ("game-id", GAME_ID.to_string()),
                ("question-id", QUESTION_ID.to_string()),
                ("closes-at", "2026-04-27T10:05:00Z".to_string()),
            ],
            QUESTION,
        )],
        launches: CHOICES
            .into_iter()
            .map(|(choice_id, label)| {
                launch(
                    &format!("answer-{choice_id}"),
                    "answer",
                    label,
                    waddle_id.clone(),
                    source_stanza_id.clone(),
                )
            })
            .collect(),
    }
}

fn view(id: &str, title: &str, text: &str) -> types::UiView {
    let mut blocks = vec![types::UiBlock::Text(types::TextBlock {
        text: display(text),
        style: types::TextStyle::Body,
    })];
    for (choice_id, label) in CHOICES {
        blocks.push(types::UiBlock::Action(types::ActionBlock {
            launch_id: launch_id(&format!("answer-{choice_id}")),
            label: display(label),
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

fn launch(
    id: &str,
    action: &str,
    label: &str,
    waddle_id: types::WaddleId,
    source_stanza_id: Option<types::StanzaId>,
) -> types::LaunchDescriptor {
    types::LaunchDescriptor {
        id: launch_id(id),
        plugin: plugin_id(),
        action: types::ActionId {
            value: action.to_string(),
        },
        command_node: types::CommandNode {
            value: INVOKE_NODE.to_string(),
        },
        label: display(label),
        context: types::LaunchContext {
            waddle_id,
            source_stanza_id,
        },
        payloads: vec![payload(
            "answer-request",
            vec![
                ("game-id", GAME_ID.to_string()),
                ("question-id", QUESTION_ID.to_string()),
                (
                    "choice-id",
                    id.strip_prefix("answer-").unwrap_or(id).to_string(),
                ),
            ],
            label,
        )],
        fallback: None,
        expires_at: None,
    }
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
    }
}

fn pubsub_template(suffix: &str) -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:waddle:{{waddle-id}}:{suffix}"),
    }
}

fn submissions_node(waddle_id: &types::WaddleId, game_id: &str) -> types::PubsubNode {
    typed_node(waddle_id, &format!("submissions:{game_id}"))
}

fn leaderboard_node(waddle_id: &types::WaddleId) -> types::PubsubNode {
    typed_node(waddle_id, "leaderboard")
}

fn typed_node(waddle_id: &types::WaddleId, suffix: &str) -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:waddle:{}:{suffix}", waddle_id.value),
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
