use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use xmpp_parsers::message::{Body, Message};

use super::{
    detect_links, effective_module_config_json, effective_module_config_with_reader,
    message_hook_effect_launches_match_room, runtime_grants_for_module, sign_launch_token,
    EffectiveModuleConfigError, ExtensionManager, LaunchValidationRequest, MAX_DETECTED_LINKS,
};
use crate::config::{ExtensionConfig, ExtensionModuleConfig};
use crate::types::{
    ActionId, CommandNode, DataFormValue, DisplayText, EnrichmentId, ExtensionCapability,
    ExtensionEffect, ExtensionEnvelope, ExtensionManifest, ExtensionPayload, FormFieldValue,
    LaunchContext, LaunchDescriptor, LaunchId, MessageEnrichment, PayloadNamespace, PluginId,
    RoomJid, Timestamp, UiActionId, WaddleId, XmlAttribute, XmlElement, XmlNode,
};

#[test]
fn detects_urls() {
    let links = detect_links("hello https://github.com/waddle-social/waddle world");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].url, "https://github.com/waddle-social/waddle");
}

#[test]
fn deduplicates_and_caps_links() {
    let links =
        detect_links("https://a.test https://a.test https://b.test https://c.test https://d.test");
    assert_eq!(links.len(), MAX_DETECTED_LINKS);
    assert_eq!(links[0].url, "https://a.test");
    assert_eq!(links[1].url, "https://b.test");
}

#[test]
fn skips_urls_inside_code_and_trims_punctuation() {
    let body = "Use `https://example.com/in-code` and:\nhttps://github.com/waddle-social/waddle).\n```https://skip.me```";
    let links = detect_links(body);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].url, "https://github.com/waddle-social/waddle");
}

#[test]
fn launch_tokens_cover_payload_fields() {
    let key = b"payload-signing-key";
    let manager = ExtensionManager {
        actors: Vec::new(),
        feature_namespaces: Vec::new(),
        route_descriptors: Vec::new(),
        launch_signing_key: Some(key.to_vec()),
    };
    let plugin = PluginId::new("link-board").expect("plugin id");
    let action = ActionId::new("save-link").expect("action id");
    let launch_id = LaunchId::new("save-link-1").expect("launch id");
    let context = LaunchContext {
        waddle_id: WaddleId::new("alice@example.com").expect("waddle id"),
        room: None,
        source_stanza_id: None,
    };
    let namespace = PayloadNamespace::new("urn:waddle:link-board:1").expect("namespace");
    let payload = ExtensionPayload::new(
        namespace.clone(),
        XmlElement::new(
            namespace,
            "link",
            vec![XmlAttribute {
                namespace: None,
                local_name: "url".to_string(),
                value: "https://example.com/a".to_string(),
            }],
            vec![XmlNode::Text("https://example.com/a".to_string())],
        )
        .expect("xml element"),
    )
    .expect("payload");
    let payload_digest = super::launch_payload_digest(&[payload]);
    let token = sign_launch_token(
        key,
        &plugin,
        &action,
        &launch_id,
        &context,
        None,
        &payload_digest,
    );
    let fields = vec![
        FormFieldValue {
            name: UiActionId::new("payload#link#url").expect("field"),
            values: vec![DataFormValue::new("https://example.com/a")],
        },
        FormFieldValue {
            name: UiActionId::new("payload#link").expect("field"),
            values: vec![DataFormValue::new("https://example.com/a")],
        },
    ];
    assert!(
        manager.validates_launch_invocation(LaunchValidationRequest {
            plugin_name: plugin.as_str(),
            action_id: action.as_str(),
            launch_id: &launch_id,
            context: &context,
            fields: &fields,
            expires_at: None,
            launch_token: &token,
        })
    );

    let tampered_fields = vec![FormFieldValue {
        name: UiActionId::new("payload#link#url").expect("field"),
        values: vec![DataFormValue::new("https://example.com/b")],
    }];
    assert!(
        !manager.validates_launch_invocation(LaunchValidationRequest {
            plugin_name: plugin.as_str(),
            action_id: action.as_str(),
            launch_id: &launch_id,
            context: &context,
            fields: &tampered_fields,
            expires_at: None,
            launch_token: &token,
        })
    );

    let room_context = LaunchContext {
        waddle_id: WaddleId::new("alice@example.com").expect("waddle id"),
        room: Some(RoomJid::new("pub@muc.example.com").expect("room jid")),
        source_stanza_id: None,
    };
    assert!(
        !manager.validates_launch_invocation(LaunchValidationRequest {
            plugin_name: plugin.as_str(),
            action_id: action.as_str(),
            launch_id: &launch_id,
            context: &room_context,
            fields: &fields,
            expires_at: None,
            launch_token: &token,
        })
    );
}

#[test]
fn message_hook_launches_must_stay_in_source_room() {
    let source_room = RoomJid::new("pub@muc.example.com").expect("room jid");

    assert!(message_hook_effect_launches_match_room(
        &enrich_effect_with_launch_room(Some("pub@muc.example.com")),
        Some(&source_room),
    ));
    assert!(!message_hook_effect_launches_match_room(
        &enrich_effect_with_launch_room(Some("other@muc.example.com")),
        Some(&source_room),
    ));
    assert!(!message_hook_effect_launches_match_room(
        &enrich_effect_with_launch_room(None),
        Some(&source_room),
    ));
}

fn enrich_effect_with_launch_room(room: Option<&str>) -> ExtensionEffect {
    let namespace = PayloadNamespace::new("urn:waddle:decision-polls:1").expect("namespace");
    ExtensionEffect::EnrichMessage(ExtensionEnvelope::new(vec![MessageEnrichment {
        id: EnrichmentId::new("enrichment-1").expect("enrichment id"),
        plugin: PluginId::new("decision-polls").expect("plugin id"),
        capability: ExtensionCapability::MessageEnrich,
        payload_namespace: namespace,
        created_at: Timestamp::new("2026-04-27T12:00:00Z").expect("timestamp"),
        source: None,
        ui: Vec::new(),
        payloads: Vec::new(),
        launches: vec![LaunchDescriptor {
            id: LaunchId::new("vote-yes").expect("launch id"),
            plugin: PluginId::new("decision-polls").expect("plugin id"),
            action: ActionId::new("vote").expect("action id"),
            command_node: CommandNode::invoke(),
            label: DisplayText::new("Vote yes").expect("label"),
            context: LaunchContext {
                waddle_id: WaddleId::new("waddle-1").expect("waddle id"),
                room: room.map(|value| RoomJid::new(value).expect("room jid")),
                source_stanza_id: None,
            },
            payloads: Vec::new(),
            fallback: None,
            expires_at: None,
            token: None,
        }],
    }]))
}

#[test]
fn merges_secret_file_values_into_effective_config() {
    let mut config_secret_files = BTreeMap::new();
    config_secret_files.insert(
        "github_token".to_string(),
        "/secrets/github-token".to_string(),
    );
    config_secret_files.insert(
        "webhook_secret".to_string(),
        "/secrets/webhook-secret".to_string(),
    );

    let module = ExtensionModuleConfig {
        name: "example-extension".to_string(),
        registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tag: None,
        namespace: "urn:example:extension:1".to_string(),
        config: json!({
            "github_token": "from-config",
            "log_level": "debug"
        }),
        capability_grants: Vec::new(),
        allowed_http_origins: Vec::new(),
        provider_room_grants: Vec::new(),
        config_secret_files,
        local_path: None,
    };

    let merged = effective_module_config_with_reader(&module, |path| match path.to_str() {
        Some("/secrets/github-token") => Ok("from-secret-file".to_string()),
        Some("/secrets/webhook-secret") => Ok("webhook-value".to_string()),
        other => panic!("unexpected path: {other:?}"),
    })
    .expect("config should merge");

    assert_eq!(
        merged,
        json!({
            "github_token": "from-secret-file",
            "log_level": "debug",
            "webhook_secret": "webhook-value"
        })
    );
}

#[test]
fn rejects_non_object_config_when_secret_files_are_enabled() {
    let mut config_secret_files = BTreeMap::new();
    config_secret_files.insert(
        "github_token".to_string(),
        "/secrets/github-token".to_string(),
    );

    let module = ExtensionModuleConfig {
        name: "example-extension".to_string(),
        registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tag: None,
        namespace: "urn:example:extension:1".to_string(),
        config: json!(["not", "an", "object"]),
        capability_grants: Vec::new(),
        allowed_http_origins: Vec::new(),
        provider_room_grants: Vec::new(),
        config_secret_files,
        local_path: None,
    };

    let error = effective_module_config_with_reader(&module, |_| Ok(String::new()))
        .expect_err("non-object config should fail");
    assert!(matches!(
        error,
        EffectiveModuleConfigError::NonObjectBaseConfig { extension }
        if extension == "example-extension"
    ));
}

#[test]
fn reads_secret_files_from_disk_when_building_effective_config() {
    let artifact_dir = TestArtifacts::new();
    let secret_path = artifact_dir.write("github-token", "file-secret\n");

    let mut config_secret_files = BTreeMap::new();
    config_secret_files.insert(
        "github_token".to_string(),
        secret_path.to_string_lossy().into_owned(),
    );

    let module = ExtensionModuleConfig {
        name: "example-extension".to_string(),
        registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tag: None,
        namespace: "urn:example:extension:1".to_string(),
        config: json!({}),
        capability_grants: Vec::new(),
        allowed_http_origins: Vec::new(),
        provider_room_grants: Vec::new(),
        config_secret_files,
        local_path: None,
    };

    let config_json =
        effective_module_config_json(&module).expect("secret file should be read from disk");
    assert_eq!(config_json, r#"{"github_token":"file-secret\n"}"#);
}

#[tokio::test]
async fn from_config_fails_fast_when_configured_actor_cannot_load() {
    let config = ExtensionConfig {
        enabled: true,
        cache_dir: "/var/lib/waddle/extensions".to_string(),
        modules: vec![ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
            digest: None,
            tag: Some("latest".to_string()),
            namespace: "urn:example:extension:1".to_string(),
            config: json!({}),
            capability_grants: Vec::new(),
            allowed_http_origins: Vec::new(),
            provider_room_grants: Vec::new(),
            config_secret_files: Default::default(),
            local_path: Some("missing-example-extension-test.wasm".to_string()),
        }],
    };

    let error = ExtensionManager::from_config(config)
        .await
        .expect_err("configured extension load should fail fast");
    assert!(error
        .to_string()
        .contains("failed to resolve extension WASM path"));
}

#[tokio::test]
async fn disabled_config_does_not_require_cache_dir() {
    let manager = ExtensionManager::from_config(ExtensionConfig {
        enabled: false,
        cache_dir: String::new(),
        modules: Vec::new(),
    })
    .await
    .expect("disabled extension manager should not validate unused cache dir");

    assert!(manager.feature_namespaces().is_empty());
}

#[test]
fn advertised_feature_namespaces_reject_official_namespaces() {
    let module = ExtensionModuleConfig {
        name: "bad-advertiser".to_string(),
        registry: "ghcr.io/waddle-social/waddle/extensions/bad-advertiser".to_string(),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tag: None,
        namespace: "urn:waddle:bad:1".to_string(),
        config: json!({}),
        capability_grants: Vec::new(),
        allowed_http_origins: Vec::new(),
        provider_room_grants: Vec::new(),
        config_secret_files: Default::default(),
        local_path: None,
    };
    let mut namespaces = Vec::new();

    super::push_feature_namespace(&module, &mut namespaces, "urn:xmpp:mam:2");
    super::push_feature_namespace(&module, &mut namespaces, "jabber:iq:roster");
    super::push_feature_namespace(
        &module,
        &mut namespaces,
        "http://jabber.org/protocol/disco#info",
    );
    super::push_feature_namespace(&module, &mut namespaces, "https://example.com/not-waddle");
    super::push_feature_namespace(&module, &mut namespaces, "urn:example:extension:1");
    super::push_feature_namespace(&module, &mut namespaces, "urn:example:extension:1");

    assert_eq!(
        namespaces,
        vec!["https://example.com/not-waddle", "urn:example:extension:1"]
    );
}

#[test]
fn runtime_grants_are_host_configured_and_manifest_bounded() {
    let module = ExtensionModuleConfig {
        name: "example-extension".to_string(),
        registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
        digest: Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        tag: None,
        namespace: "urn:example:extension:1".to_string(),
        config: json!({}),
        capability_grants: vec![
            ExtensionCapability::Commands,
            ExtensionCapability::OutboundHttpRequest,
            ExtensionCapability::HostMessageSend,
        ],
        allowed_http_origins: Vec::new(),
        provider_room_grants: Vec::new(),
        config_secret_files: Default::default(),
        local_path: None,
    };
    let manifest = ExtensionManifest {
        id: PluginId::new("example-extension").expect("static plugin id is valid"),
        name: DisplayText::new("Example Extension").expect("static display text is valid"),
        version: crate::types::PluginVersion::new("0.1.0").expect("static plugin version is valid"),
        payloads: Vec::new(),
        capabilities: vec![
            ExtensionCapability::Commands,
            ExtensionCapability::OutboundHttpRequest,
        ],
        commands: Vec::new(),
        routes: Vec::new(),
        pubsub_nodes: Vec::new(),
        artifact: None,
    };

    let grants = runtime_grants_for_module(&module, &manifest);

    assert!(grants.contains(&ExtensionCapability::Commands));
    assert!(grants.contains(&ExtensionCapability::OutboundHttpRequest));
    assert!(!grants.contains(&ExtensionCapability::HostMessageSend));
    assert!(!grants.contains(&ExtensionCapability::HostMamRead));
}

#[tokio::test]
async fn enrich_message_does_not_fallback_without_loaded_actor() {
    let manager = ExtensionManager {
        actors: Vec::new(),
        feature_namespaces: vec!["urn:example:extension:1".to_string()],
        route_descriptors: Vec::new(),
        launch_signing_key: None,
    };

    let mut msg = Message::new(None);
    msg.bodies.insert(
        String::new(),
        Body("https://github.com/waddle-social/waddle".to_string()),
    );

    assert_eq!(manager.enrich_message(&mut msg).await, 0);
    assert!(msg.payloads.is_empty());
}

struct TestArtifacts {
    root: PathBuf,
}

impl TestArtifacts {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should move forward")
            .as_nanos();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-artifacts")
            .join(format!("manager-{nonce}-{}", std::process::id()));
        fs::create_dir_all(&root).expect("artifact directory should be created");
        Self { root }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, contents).expect("artifact file should be written");
        path
    }
}

impl Drop for TestArtifacts {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
