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

struct AiAssistantCanvas;

bindings::export!(AiAssistantCanvas with_types_in bindings);

const PLUGIN_ID: &str = "ai-assistant-canvas";
const PLUGIN_NAME: &str = "AI Assistant Canvas";
const PLUGIN_NS: &str = "urn:waddle:ai-assistant-canvas:1";
const INVOKE_NODE: &str = "urn:waddle:extension:1:invoke";
const COMMAND_NODE: &str = "urn:waddle:extension:1:ai-assistant-canvas";
const VERSION: &str = "0.1.0";
const CANVAS_ID: &str = "canvas-1";
const RENDER_ID: &str = "render-1";

impl exports::waddle::extension::lifecycle::Guest for AiAssistantCanvas {
    fn init(_config: String) -> Result<types::ExtensionManifest, String> {
        Ok(manifest())
    }
}

impl exports::waddle::extension::framework::Guest for AiAssistantCanvas {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::Command(command) => {
                let artifact = artifact_reference();
                vec![
                    types::ExtensionEffect::ReferenceArtifact(artifact.clone()),
                    types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
                        node: renders_node(&command.waddle_id),
                        item_id: Some(types::PubsubItemId {
                            value: RENDER_ID.to_string(),
                        }),
                        payload: canvas_payload(artifact.clone()),
                    }),
                    visible_message(canvas_message(command.waddle_id, artifact)),
                ]
            }
            types::ExtensionEvent::Launch(launch) => {
                let artifact = artifact_reference();
                vec![
                    types::ExtensionEffect::ReferenceArtifact(artifact.clone()),
                    types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
                        node: canvases_node(&launch.context.waddle_id),
                        item_id: Some(types::PubsubItemId {
                            value: CANVAS_ID.to_string(),
                        }),
                        payload: payload(
                            "canvas",
                            vec![
                                ("canvas-id", CANVAS_ID.to_string()),
                                ("render-id", RENDER_ID.to_string()),
                            ],
                            "Canvas remix queued",
                        ),
                    }),
                    types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
                        node: renders_node(&launch.context.waddle_id),
                        item_id: Some(types::PubsubItemId {
                            value: format!("{}-remix", launch.launch_id.value),
                        }),
                        payload: canvas_payload(artifact.clone()),
                    }),
                    visible_message(canvas_message(launch.context.waddle_id, artifact)),
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
            payload_rule(types::PayloadSurface::MessageEnrichment, "canvas"),
            payload_rule(types::PayloadSurface::LaunchPayload, "remix-source"),
            payload_rule(types::PayloadSurface::PubsubItem, "canvas"),
            payload_rule(types::PayloadSurface::PubsubItem, "render"),
        ],
        capabilities: vec![
            types::ExtensionCapability::Commands,
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::Launch,
            types::ExtensionCapability::ArtifactReference,
            types::ExtensionCapability::PubsubPublish,
            types::ExtensionCapability::UiDeclarative,
        ],
        commands: vec![
            command_descriptor(COMMAND_NODE, "Create AI Canvas"),
            command_descriptor(INVOKE_NODE, "Run AI Canvas action"),
        ],
        pubsub_nodes: vec![pubsub_template("canvases"), pubsub_template("renders")],
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
                value: "canvas-message".to_string(),
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

fn canvas_message(
    waddle_id: types::WaddleId,
    artifact: types::ArtifactReference,
) -> VisibleMessage {
    VisibleMessage {
        ui: vec![view(
            "canvas",
            PLUGIN_NAME,
            "Immutable canvas artifact reference created.",
            Some(artifact.clone()),
        )],
        payloads: vec![canvas_payload(artifact)],
        launches: vec![types::LaunchDescriptor {
            id: types::LaunchId {
                value: "remix-canvas".to_string(),
            },
            plugin: types::PluginId {
                value: PLUGIN_ID.to_string(),
            },
            action: types::ActionId {
                value: "remix".to_string(),
            },
            command_node: types::CommandNode {
                value: INVOKE_NODE.to_string(),
            },
            label: display("Remix"),
            context: types::LaunchContext {
                waddle_id,
                source_stanza_id: None,
            },
            payloads: vec![payload(
                "remix-source",
                vec![("canvas-id", CANVAS_ID.to_string())],
                "Remix canvas",
            )],
            fallback: None,
            expires_at: None,
        }],
    }
}

fn view(
    id: &str,
    title: &str,
    text: &str,
    artifact: Option<types::ArtifactReference>,
) -> types::UiView {
    let mut blocks = vec![types::UiBlock::Text(types::TextBlock {
        text: display(text),
        style: types::TextStyle::Body,
    })];
    if let Some(artifact) = artifact {
        blocks.push(types::UiBlock::Image(types::ImageBlock {
            artifact,
            alt: display("Generated canvas"),
        }));
        blocks.push(types::UiBlock::Action(types::ActionBlock {
            launch_id: types::LaunchId {
                value: "remix-canvas".to_string(),
            },
            label: display("Remix"),
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

fn artifact_reference() -> types::ArtifactReference {
    types::ArtifactReference {
        uri: types::ArtifactUri {
            value: "https://artifacts.waddle.social/sha256/789abc789abc789abc789abc789abc789abc789abc789abc789abc789abc789a/canvas.png".to_string(),
        },
        sha256: types::Sha256Digest {
            value: "789abc789abc789abc789abc789abc789abc789abc789abc789abc789abc789a".to_string(),
        },
        media_type: Some(types::MediaType {
            value: "image/png".to_string(),
        }),
    }
}

fn canvas_payload(artifact: types::ArtifactReference) -> types::ExtensionPayload {
    payload(
        "canvas",
        vec![
            ("canvas-id", CANVAS_ID.to_string()),
            ("render-id", RENDER_ID.to_string()),
            ("artifact-uri", artifact.uri.value),
            ("artifact-sha256", artifact.sha256.value),
            (
                "media-type",
                artifact
                    .media_type
                    .map(|media_type| media_type.value)
                    .unwrap_or_else(|| "image/png".to_string()),
            ),
        ],
        "Canvas generated",
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
    }
}

fn pubsub_template(suffix: &str) -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:waddle:{{waddle-id}}:{suffix}"),
    }
}

fn canvases_node(waddle_id: &types::WaddleId) -> types::PubsubNode {
    typed_node(waddle_id, "canvases")
}

fn renders_node(waddle_id: &types::WaddleId) -> types::PubsubNode {
    typed_node(waddle_id, "renders")
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
