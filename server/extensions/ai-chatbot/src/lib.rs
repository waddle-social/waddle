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

struct AiChatbot;

bindings::export!(AiChatbot with_types_in bindings);

const PLUGIN_ID: &str = "ai-chatbot";
const PLUGIN_NAME: &str = "AI Chatbot";
const PLUGIN_NS: &str = "urn:waddle:ai-chatbot:1";
const INVOKE_NODE: &str = "urn:waddle:extension:1:invoke";
const COMMAND_NODE: &str = "urn:waddle:extension:1:ai-chatbot";
const VERSION: &str = "0.1.0";

impl exports::waddle::extension::lifecycle::Guest for AiChatbot {
    fn init(_config: String) -> Result<types::ExtensionManifest, String> {
        Ok(manifest())
    }
}

impl exports::waddle::extension::framework::Guest for AiChatbot {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::MessageHook(hook)
                if hook.body.value.contains("@waddle") || hook.body.value.contains("/ai") =>
            {
                vec![visible_message(answer(
                    &hook.body.value,
                    hook.context.waddle_id,
                    hook.context.stanza_id,
                ))]
            }
            types::ExtensionEvent::Command(command) => {
                let prompt = command
                    .fields
                    .first()
                    .and_then(|field| field.values.first())
                    .map(|value| value.value.as_str())
                    .unwrap_or("Ask me from chat with /ask.");
                vec![visible_message(answer(
                    prompt,
                    command.waddle_id,
                    None,
                ))]
            }
            types::ExtensionEvent::Launch(launch) => {
                let prompt = field_value(&launch.fields, "payload#assistant-followup#question")
                    .or_else(|| field_value(&launch.fields, "payload#assistant-followup"))
                    .or_else(|| field_value(&launch.fields, "payload#question"))
                    .unwrap_or("Continue the previous answer.");
                vec![visible_message(answer(
                    prompt,
                    launch.context.waddle_id,
                    launch.context.source_stanza_id,
                ))]
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
            payload_rule(types::PayloadSurface::MessageEnrichment, "assistant-answer"),
            payload_rule(types::PayloadSurface::LaunchPayload, "assistant-followup"),
        ],
        capabilities: vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::Commands,
            types::ExtensionCapability::Launch,
            types::ExtensionCapability::MessageObserve,
        ],
        commands: vec![
            command_descriptor(COMMAND_NODE, "Ask AI Chatbot"),
            command_descriptor(INVOKE_NODE, "Run AI Chatbot action"),
        ],
        pubsub_nodes: vec![],
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
                value: "assistant-message".to_string(),
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

fn answer(
    prompt: &str,
    waddle_id: types::WaddleId,
    source_stanza_id: Option<types::StanzaId>,
) -> VisibleMessage {
    VisibleMessage {
        ui: vec![types::UiView {
            id: types::UiViewId {
                value: "ai-answer".to_string(),
            },
            title: Some(display(PLUGIN_NAME)),
            blocks: vec![
                types::UiBlock::Text(types::TextBlock {
                    text: display("I can help summarize the recent thread, draft replies, or turn this into an action list."),
                    style: types::TextStyle::Body,
                }),
                types::UiBlock::Text(types::TextBlock {
                    text: display(prompt),
                    style: types::TextStyle::Muted,
                }),
                types::UiBlock::Action(types::ActionBlock {
                    launch_id: launch_id("ask-followup"),
                    label: display("Ask follow-up"),
                }),
            ],
        }],
        payloads: vec![payload(
            "assistant-answer",
            vec![
                ("run-id", "run-ai-chatbot-1".to_string()),
                ("profile", "default".to_string()),
                ("context-source", "mam".to_string()),
            ],
            "Assistant response generated",
        )],
        launches: vec![types::LaunchDescriptor {
            id: launch_id("ask-followup"),
            plugin: plugin_id(),
            action: types::ActionId {
                value: "ask-followup".to_string(),
            },
            command_node: types::CommandNode {
                value: INVOKE_NODE.to_string(),
            },
            label: display("Ask follow-up"),
            context: types::LaunchContext {
                waddle_id,
                source_stanza_id,
            },
            payloads: vec![payload(
                "assistant-followup",
                vec![("question", prompt.to_string())],
                prompt,
            )],
            fallback: None,
            expires_at: None,
        }],
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

fn field_value<'a>(fields: &'a [types::FormFieldValue], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|field| field.name.value == name)
        .and_then(|field| field.values.first())
        .map(|value| value.value.as_str())
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
