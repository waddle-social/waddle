use minidom::Element;
use waddle_extensions::types::*;
use xmpp_parsers::message::Message;

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const THIRD_PARTY_NS: &str = "urn:example:task-widget:1";

fn text(value: &str) -> DisplayText {
    DisplayText::new(value).expect("display text should be valid")
}

macro_rules! id {
    ($constructor:path, $value:expr) => {
        $constructor($value).expect("id should be valid")
    };
}

fn artifact() -> ArtifactReference {
    ArtifactReference::new(
        format!("https://artifacts.example.com/sample/sha256/{DIGEST}/thumb.png"),
        DIGEST,
        Some(MediaType::new("image/png").expect("media type should be valid")),
    )
    .expect("artifact reference should be digest pinned")
}

fn sample_namespace() -> PayloadNamespace {
    PayloadNamespace::new(THIRD_PARTY_NS).expect("third-party namespace should be valid")
}

fn sample_payload() -> ExtensionPayload {
    let namespace = sample_namespace();
    ExtensionPayload::new(
        namespace.clone(),
        XmlElement::new(
            namespace,
            "link",
            vec![XmlAttribute {
                namespace: None,
                local_name: "url".to_string(),
                value: "https://example.org/post".to_string(),
            }],
            vec![XmlNode::Element(
                XmlElement::new(
                    sample_namespace(),
                    "title",
                    Vec::new(),
                    vec![XmlNode::Text("Example Post".to_string())],
                )
                .expect("child XML should be valid"),
            )],
        )
        .expect("payload XML should be valid"),
    )
    .expect("extension payload should be valid")
}

fn sample_manifest() -> ExtensionManifest {
    ExtensionManifest {
        id: id!(PluginId::new, "task-widget"),
        name: text("Task Widget"),
        version: id!(PluginVersion::new, "1.0.0"),
        payloads: vec![
            PayloadRule {
                surface: PayloadSurface::MessageEnrichment,
                root: PayloadRoot::new(sample_namespace(), "link").expect("payload root"),
            },
            PayloadRule {
                surface: PayloadSurface::PubSubItem,
                root: PayloadRoot::new(sample_namespace(), "link").expect("payload root"),
            },
        ],
        capabilities: vec![
            ExtensionCapability::MessageEnrich,
            ExtensionCapability::Launch,
            ExtensionCapability::PubSubPublish,
        ],
        commands: vec![CommandDescriptor {
            node: CommandNode::new("urn:waddle:extension:1:task-widget").expect("command node"),
            name: text("Task Widget"),
            scope: CommandScope::Global,
        }],
        routes: Vec::new(),
        pubsub_nodes: vec![id!(
            PubSubNode::new,
            "urn:example:task-widget:1:waddle:{waddle-id}:links"
        )],
        artifact: None,
    }
}

#[test]
fn payload_namespaces_allow_third_party_but_reject_official_xmpp() {
    assert!(PayloadNamespace::new("urn:waddle:sample:1").is_ok());
    assert!(PayloadNamespace::new("https://plugins.example.com/ns/tasks").is_ok());
    assert!(PayloadNamespace::new("urn:example:tasks:1").is_ok());
    // The framework namespace is allowed because every extension publishes
    // PubSub state items wrapped in the framework-defined `<extension-item>`
    // envelope. Manifest payload rules in the framework namespace remain
    // forbidden by `validate_manifest_against_module`, so extensions cannot
    // claim ownership of framework wire shapes.
    let framework =
        PayloadNamespace::new(FRAMEWORK_NAMESPACE).expect("framework namespace is a valid payload namespace");
    assert!(framework.is_framework());
    assert!(matches!(
        PayloadNamespace::new("urn:xmpp:sid:0"),
        Err(FrameworkTypeError::OfficialNamespace(_))
    ));
    assert!(matches!(
        PayloadNamespace::new("http://jabber.org/protocol/pubsub"),
        Err(FrameworkTypeError::OfficialNamespace(_))
    ));
}

#[test]
fn command_nodes_must_be_exact_framework_nodes() {
    assert!(CommandNode::new(INVOKE_COMMAND_NODE).is_ok());
    assert!(matches!(
        CommandNode::new("urn:waddle:extension:10:invoke"),
        Err(FrameworkTypeError::InvalidCommandNode(_))
    ));
    assert!(matches!(
        CommandNode::new("http://jabber.org/protocol/commands"),
        Err(FrameworkTypeError::InvalidCommandNode(_))
    ));
}

#[test]
fn artifact_references_must_be_digest_pinned_and_immutable() {
    let reference = artifact();
    assert_eq!(reference.sha256.as_str(), DIGEST);

    assert!(matches!(
        ArtifactReference::new(
            "https://artifacts.example.com/canvas/latest/render.png",
            DIGEST,
            None
        ),
        Err(FrameworkTypeError::InvalidArtifactUri(_))
    ));
    assert!(matches!(
        ArtifactReference::new(
            format!("https://artifacts.example.com/canvas/sha256/{DIGEST}/render.png"),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            None,
        ),
        Err(FrameworkTypeError::ArtifactDigestMismatch { .. })
    ));
}

#[test]
fn extension_payload_rejects_mismatched_declared_namespace() {
    let declared = sample_namespace();
    let other = PayloadNamespace::new("urn:waddle:other-sample:1").expect("namespace");
    let root = XmlElement::new(other, "link", Vec::new(), Vec::new()).expect("root");

    assert!(matches!(
        ExtensionPayload::new(declared, root),
        Err(FrameworkTypeError::InvalidPayloadNamespace(_))
    ));
}

#[test]
fn extension_payload_rejects_invalid_xml_and_duplicate_attrs() {
    let namespace = sample_namespace();
    assert!(matches!(
        XmlElement::new(namespace.clone(), "bad:name", Vec::new(), Vec::new()),
        Err(FrameworkTypeError::InvalidXmlName(_))
    ));
    assert!(matches!(
        XmlElement::new(
            namespace,
            "link",
            vec![
                XmlAttribute {
                    namespace: None,
                    local_name: "url".to_string(),
                    value: "https://example.org/1".to_string(),
                },
                XmlAttribute {
                    namespace: None,
                    local_name: "url".to_string(),
                    value: "https://example.org/2".to_string(),
                },
            ],
            Vec::new(),
        ),
        Err(FrameworkTypeError::DuplicateXmlAttribute { .. })
    ));
    assert!(matches!(
        XmlElement::new(
            sample_namespace(),
            "link",
            vec![XmlAttribute {
                namespace: Some(PayloadNamespace::new("urn:example:attrs:1").expect("namespace")),
                local_name: "url".to_string(),
                value: "https://example.org/1".to_string(),
            }],
            Vec::new(),
        ),
        Err(FrameworkTypeError::NamespacedXmlAttributeUnsupported)
    ));
}

#[test]
fn framework_envelope_builds_generic_payload_and_fallback_ui() {
    let envelope = ExtensionEnvelope::new(vec![MessageEnrichment {
        id: id!(EnrichmentId::new, "enrich-1"),
        plugin: id!(PluginId::new, "task-widget"),
        capability: ExtensionCapability::MessageEnrich,
        payload_namespace: sample_namespace(),
        created_at: id!(Timestamp::new, "2026-04-27T10:00:00Z"),
        source: Some(MessageSource {
            stanza_id: id!(StanzaId::new, "archive-id-456"),
            body_range: Some(BodyRange::new(5, 29).expect("range should be valid")),
        }),
        ui: vec![UiView {
            id: id!(UiViewId::new, "fallback"),
            title: Some(text("Example Post")),
            blocks: vec![
                UiBlock::Text(TextBlock {
                    text: text("Generic fallback"),
                    style: TextStyle::Body,
                }),
                UiBlock::Action(ActionBlock {
                    launch_id: id!(LaunchId::new, "save-link"),
                    label: text("Save"),
                }),
            ],
        }],
        payloads: vec![sample_payload()],
        launches: vec![LaunchDescriptor {
            id: id!(LaunchId::new, "save-link"),
            plugin: id!(PluginId::new, "task-widget"),
            action: id!(ActionId::new, "save-link"),
            command_node: CommandNode::invoke(),
            label: text("Save link"),
            context: LaunchContext {
                waddle_id: id!(WaddleId::new, "waddle-123"),
                room: None,
                source_stanza_id: Some(id!(StanzaId::new, "archive-id-456")),
            },
            payloads: Vec::new(),
            fallback: None,
            expires_at: None,
            token: None,
        }],
    }]);

    let element = envelope.to_minidom();
    assert!(element.is("extensions", FRAMEWORK_NAMESPACE));
    assert_eq!(element.attr("version"), Some("1"));

    let enrichment = element
        .get_child("enrichment", FRAMEWORK_NAMESPACE)
        .expect("enrichment child should exist");
    assert_eq!(enrichment.attr("plugin"), Some("task-widget"));
    assert_eq!(enrichment.attr("payload-ns"), Some(THIRD_PARTY_NS));

    let payload = enrichment
        .get_child("payload", FRAMEWORK_NAMESPACE)
        .expect("payload child should exist");
    assert!(payload.get_child("view", FRAMEWORK_NAMESPACE).is_some());
    assert!(payload.get_child("link", THIRD_PARTY_NS).is_some());

    let launch = enrichment
        .get_child("launch", FRAMEWORK_NAMESPACE)
        .expect("launch child should exist");
    assert_eq!(launch.attr("command-node"), Some(INVOKE_COMMAND_NODE));
    assert!(launch.get_child("payload", FRAMEWORK_NAMESPACE).is_none());
}

#[test]
fn declarative_text_and_extension_payload_text_are_not_parsed_as_xml() {
    let payload = ExtensionPayload::new(
        sample_namespace(),
        XmlElement::new(
            sample_namespace(),
            "note",
            Vec::new(),
            vec![XmlNode::Text("<script/> is literal text".to_string())],
        )
        .expect("XML should be valid"),
    )
    .expect("payload should be valid");

    let element = payload.to_minidom();
    assert_eq!(element.children().count(), 0);
    assert_eq!(element.text(), "<script/> is literal text");
}

#[test]
fn extension_effect_validation_is_manifest_authoritative() {
    let manifest = sample_manifest();
    let enrichment = MessageEnrichment {
        id: id!(EnrichmentId::new, "enrich-1"),
        plugin: id!(PluginId::new, "task-widget"),
        capability: ExtensionCapability::MessageEnrich,
        payload_namespace: sample_namespace(),
        created_at: id!(Timestamp::new, "2026-04-27T10:00:00Z"),
        source: None,
        ui: Vec::new(),
        payloads: vec![sample_payload()],
        launches: vec![LaunchDescriptor {
            id: id!(LaunchId::new, "save-link"),
            plugin: id!(PluginId::new, "task-widget"),
            action: id!(ActionId::new, "save-link"),
            command_node: CommandNode::invoke(),
            label: text("Save link"),
            context: LaunchContext {
                waddle_id: id!(WaddleId::new, "waddle-123"),
                room: None,
                source_stanza_id: None,
            },
            payloads: Vec::new(),
            fallback: None,
            expires_at: None,
            token: None,
        }],
    };

    assert!(
        ExtensionEffect::EnrichMessage(ExtensionEnvelope::new(vec![enrichment.clone()]))
            .validate_for_manifest(&manifest)
    );

    let mut wrong_plugin = enrichment;
    wrong_plugin.plugin = id!(PluginId::new, "other-widget");
    assert!(
        !ExtensionEffect::EnrichMessage(ExtensionEnvelope::new(vec![wrong_plugin]))
            .validate_for_manifest(&manifest)
    );

    assert!(ExtensionEffect::PublishPubSub(PubSubPublish {
        node: id!(
            PubSubNode::new,
            "urn:example:task-widget:1:waddle:room-1:links"
        ),
        item_id: None,
        payload: sample_payload(),
    })
    .validate_for_manifest(&manifest));
}

#[test]
fn framework_public_api_stays_generic() {
    let public_vocabulary = [
        std::any::type_name::<ExtensionManifest>(),
        std::any::type_name::<ExtensionPayload>(),
        std::any::type_name::<LaunchDescriptor>(),
        std::any::type_name::<CommandDescriptor>(),
    ]
    .join(" ");

    for leaked_sample_type in [
        "SaveLinkRequest",
        "QuizQuestion",
        "PollVote",
        "CanvasRequest",
    ] {
        assert!(
            !public_vocabulary.contains(leaked_sample_type),
            "{leaked_sample_type} must not leak into the shared extension API"
        );
    }
}

#[test]
fn framework_envelope_detection_is_exact() {
    let mut message = Message::new(None);
    message
        .payloads
        .push(Element::builder("extensions", FRAMEWORK_NAMESPACE).build());
    assert!(message_has_framework_envelope(&message));

    let mut official_message = Message::new(None);
    official_message
        .payloads
        .push(Element::builder("extensions", "urn:xmpp:sid:0").build());
    assert!(!message_has_framework_envelope(&official_message));
}
