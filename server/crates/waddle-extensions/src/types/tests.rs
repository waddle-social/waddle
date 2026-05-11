use super::*;

fn manifest_with(capabilities: Vec<ExtensionCapability>) -> ExtensionManifest {
    ExtensionManifest {
        id: PluginId::new("test-extension").expect("plugin id"),
        name: DisplayText::new("Test Extension").expect("display text"),
        version: PluginVersion::new("0.1.0").expect("version"),
        payloads: Vec::new(),
        capabilities,
        commands: Vec::new(),
        routes: Vec::new(),
        pubsub_nodes: Vec::new(),
        artifact: None,
    }
}

#[test]
fn command_form_effect_requires_manifest_and_operator_grant() {
    let manifest = manifest_with(vec![ExtensionCapability::Commands]);
    let effect = ExtensionEffect::CommandForm(DataForm {
        form_type: DataFormType::Form,
        title: None,
        instructions: Vec::new(),
        fields: Vec::new(),
    });
    assert!(!effect.validate_for_manifest_and_grants(&manifest, &HashSet::new()));

    let grants = HashSet::from([ExtensionCapability::Commands]);
    assert!(effect.validate_for_manifest_and_grants(&manifest, &grants));
}

#[test]
fn payload_namespace_accepts_framework_namespace() {
    let namespace = PayloadNamespace::new(FRAMEWORK_NAMESPACE).expect("framework namespace");
    assert!(namespace.is_framework());
    assert_eq!(namespace.as_str(), FRAMEWORK_NAMESPACE);
}

#[test]
fn pubsub_node_declaration_does_not_grant_prefix_or_sibling_nodes() {
    let mut manifest = manifest_with(Vec::new());
    manifest.pubsub_nodes = vec![
        PubSubNode::new("urn:waddle:web-integration:1:github:routes")
            .expect("declared pubsub node"),
    ];

    assert!(manifest.declares_pubsub_node(
        &PubSubNode::new("urn:waddle:web-integration:1:github:routes")
            .expect("declared pubsub node")
    ));
    assert!(!manifest.declares_pubsub_node(
        &PubSubNode::new("urn:waddle:web-integration:1:github").expect("prefix pubsub node")
    ));
    assert!(!manifest.declares_pubsub_node(
        &PubSubNode::new("urn:waddle:web-integration:1:github:routes-extra")
            .expect("sibling pubsub node")
    ));
    assert!(!manifest.declares_pubsub_node(
        &PubSubNode::new("urn:waddle:web-integration:10:github:routes")
            .expect("version sibling pubsub node")
    ));
}

#[test]
fn provider_field_text_preserves_empty_strings() {
    let text = ProviderFieldText::new("").expect("empty provider text is valid");

    assert_eq!(text.as_str(), "");
    assert_eq!(text.into_string(), "");
}

#[test]
fn pubsub_publish_accepts_framework_extension_item_without_manifest_payload_rule() {
    let manifest = ExtensionManifest {
        id: PluginId::new("test-extension").expect("plugin id"),
        name: DisplayText::new("Test Extension").expect("display text"),
        version: PluginVersion::new("0.1.0").expect("version"),
        payloads: Vec::new(),
        capabilities: vec![ExtensionCapability::PubSubPublish],
        commands: Vec::new(),
        routes: Vec::new(),
        pubsub_nodes: vec![PubSubNode::new("urn:waddle:test-extension:1:items").expect("node")],
        artifact: None,
    };
    let framework_namespace = PayloadNamespace::framework();
    let payload = ExtensionPayload::new(
        framework_namespace.clone(),
        XmlElement {
            namespace: framework_namespace,
            local_name: EXTENSION_ITEM_LOCAL_NAME.to_string(),
            attributes: Vec::new(),
            children: Vec::new(),
        },
    )
    .expect("framework extension-item payload");
    let effect = ExtensionEffect::PublishPubSub(PubSubPublish {
        node: PubSubNode::new("urn:waddle:test-extension:1:items").expect("node"),
        item_id: None,
        payload,
    });
    let grants = HashSet::from([ExtensionCapability::PubSubPublish]);
    assert!(effect.validate_for_manifest_and_grants(&manifest, &grants));
}

#[test]
fn pubsub_publish_in_extension_namespace_still_requires_manifest_payload_rule() {
    let manifest = ExtensionManifest {
        id: PluginId::new("test-extension").expect("plugin id"),
        name: DisplayText::new("Test Extension").expect("display text"),
        version: PluginVersion::new("0.1.0").expect("version"),
        payloads: Vec::new(),
        capabilities: vec![ExtensionCapability::PubSubPublish],
        commands: Vec::new(),
        routes: Vec::new(),
        pubsub_nodes: vec![PubSubNode::new("urn:waddle:test-extension:1:items").expect("node")],
        artifact: None,
    };
    let extension_namespace =
        PayloadNamespace::new("urn:waddle:test-extension:1").expect("extension namespace");
    let payload = ExtensionPayload::new(
        extension_namespace.clone(),
        XmlElement {
            namespace: extension_namespace,
            local_name: "custom-item".to_string(),
            attributes: Vec::new(),
            children: Vec::new(),
        },
    )
    .expect("custom payload");
    let effect = ExtensionEffect::PublishPubSub(PubSubPublish {
        node: PubSubNode::new("urn:waddle:test-extension:1:items").expect("node"),
        item_id: None,
        payload,
    });
    let grants = HashSet::from([ExtensionCapability::PubSubPublish]);
    assert!(!effect.validate_for_manifest_and_grants(&manifest, &grants));
}
