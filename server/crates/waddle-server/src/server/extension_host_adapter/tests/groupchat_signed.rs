use super::{groupchat_ingress::GroupchatFixture, groupchat_receipts};
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{ConfiguredPluginGrants, ExtensionGrantRepository};
use std::sync::Arc;
use waddle_extensions::{
    ExtensionCapability, ExtensionConfig, ExtensionManager, ExtensionModuleConfig, PluginId,
};

async fn signed_manager() -> Arc<ExtensionManager> {
    // The shared `message-hook-fixture` component's `init` declares whichever
    // capabilities its `config` names — a bare decimal WIT enum ordinal for
    // one capability, or a JSON array of ordinals for several (see the
    // fixture's own `capabilities_from_config`). WIT enum indices used here:
    // MessageEnrich=0, HostMessageSend=8, Launch=11.
    let path = std::path::Path::new(
        &std::env::var_os("CARGO_MANIFEST_DIR").expect("test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("../../../../../waddle-extensions/tests/fixtures/message_hook.wasm");
    let manager = ExtensionManager::from_config(ExtensionConfig {
        enabled: true,
        modules: vec![ExtensionModuleConfig {
            name: "message-hook-fixture".into(),
            namespace: "urn:test:message-hook".into(),
            registry: Default::default(),
            digest: None,
            tag: None,
            config: serde_json::json!([0, 8, 11]),
            capability_grants: vec![
                ExtensionCapability::MessageEnrich,
                ExtensionCapability::Launch,
                ExtensionCapability::HostMessageSend,
            ],
            allowed_http_origins: vec![],
            provider_room_grants: vec![],
            config_secret_files: Default::default(),
            local_path: Some(path.display().to_string()),
        }],
        ..Default::default()
    })
    .await
    .expect("real signed-envelope plugin")
    .with_launch_signing_key(b"groupchat-test-signing-key");
    Arc::new(manager)
}

async fn signed_replay(f: IngressFixture) {
    let mut fixture = GroupchatFixture::new(&f).await;
    Arc::get_mut(&mut fixture.adapter.state)
        .expect("unique state")
        .deps
        .protocol
        .extension_manager = signed_manager().await;
    let plugin = PluginId::new("message-hook-fixture").expect("fixture plugin");
    let mut invocation = fixture.invocation();
    invocation.plugin_id = plugin.clone();
    invocation.actor_jid = fixture
        .adapter
        .plugin_actor_jid(&plugin)
        .expect("plugin actor");
    let mut tx = f.uow.begin().await.expect("grant tx");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[ConfiguredPluginGrants {
            plugin: plugin.clone(),
            can_send: true,
            provider_rooms: vec![fixture.room.clone()],
        }],
    )
    .await
    .expect("fixture grant");
    tx.commit().await.expect("grant commit");
    let mut request = fixture.request("signed-replay");
    let mut envelope = super::envelope_with_launch_room(Some(fixture.room.to_string().as_str()));
    envelope.enrichments[0].plugin = plugin.clone();
    envelope.enrichments[0].payload_namespace =
        waddle_extensions::types::PayloadNamespace::new("urn:test:message-hook")
            .expect("namespace");
    envelope.enrichments[0].launches[0].plugin = plugin;
    assert!(fixture
        .adapter
        .state
        .deps
        .protocol
        .extension_manager
        .validate_envelope_for_plugin(&invocation.plugin_id, &envelope));
    request.extensions = Some(envelope);
    let first_time = chrono::Utc::now();
    crate::server::routes::interpret::TEST_SIGNING_TIME
        .scope(
            first_time,
            fixture.adapter.send_message(&invocation, request.clone()),
        )
        .await
        .expect("first signed send");
    let wire = fixture.drain();
    let message = super::groupchat_ingress::groupchat_message(&wire);
    let signed = message
        .payloads
        .iter()
        .find(|p| p.is("extensions", waddle_extensions::FRAMEWORK_NAMESPACE))
        .expect("signed envelope persisted on wire");
    let launch = signed
        .get_child("enrichment", waddle_extensions::FRAMEWORK_NAMESPACE)
        .expect("enrichment")
        .get_child("launch", waddle_extensions::FRAMEWORK_NAMESPACE)
        .expect("signed launch");
    assert!(launch.attr("token").is_some());
    let expiry =
        chrono::DateTime::parse_from_rfc3339(launch.attr("expires-at").expect("generated expiry"))
            .expect("expiry");
    assert!(expiry > first_time);
    assert_eq!(f.count("ingress_messages").await, 1);
    let before = groupchat_receipts::intents(&f).await;
    let candidates = f.count("notification_candidates").await;
    let later = first_time + chrono::Duration::hours(2);
    assert!(later > expiry);
    crate::server::routes::interpret::TEST_SIGNING_TIME
        .scope(later, fixture.adapter.send_message(&invocation, request))
        .await
        .expect("same unsigned origin aliases beyond old signing expiry");
    assert_eq!(f.count("ingress_messages").await, 1, "no AliasConflict row");
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(f.count("notification_candidates").await, candidates);
    assert_eq!(groupchat_receipts::intents(&f).await, before);
    assert!(fixture.drain().is_empty());
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_signed_expiry_replay_sqlite() {
    signed_replay(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_signed_expiry_replay_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_signed").await {
        signed_replay(f).await;
    }
}
