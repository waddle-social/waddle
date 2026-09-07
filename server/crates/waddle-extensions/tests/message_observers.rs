use std::path::Path;

use waddle_extensions::{
    ExtensionCapability, ExtensionConfig, ExtensionManager, ExtensionModuleConfig,
};
use xmpp_parsers::message::{Lang, Message};

fn message() -> Message {
    let mut message = Message::new(None);
    message.bodies.insert(Lang(String::new()), "hello".into());
    message
}

fn config(capability: ExtensionCapability) -> ExtensionConfig {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/message_hook.wat");
    ExtensionConfig {
        modules: vec![ExtensionModuleConfig {
            name: "message-hook-fixture".into(),
            registry: String::new(),
            digest: None,
            tag: None,
            namespace: "urn:test:message-hook".into(),
            config: serde_json::json!(u8::from(capability == ExtensionCapability::MessageObserve)),
            capability_grants: vec![capability],
            allowed_http_origins: Vec::new(),
            provider_room_grants: Vec::new(),
            config_secret_files: Default::default(),
            local_path: Some(fixture.to_string_lossy().into_owned()),
        }],
        ..ExtensionConfig::default()
    }
}

#[tokio::test]
async fn message_observers_granted_observer_is_eligible() {
    let manager = ExtensionManager::from_config(config(ExtensionCapability::MessageObserve))
        .await
        .expect("observer fixture loads");
    assert!(manager.has_message_observers(&message()));
}

#[tokio::test]
async fn message_observers_ungranted_observer_is_rejected_at_startup() {
    let mut config = config(ExtensionCapability::MessageObserve);
    config.modules[0].capability_grants.clear();
    let error = ExtensionManager::from_config(config)
        .await
        .expect_err("ungranted observer must not load");
    assert!(error
        .to_string()
        .contains("requires explicit operator grant"));
}

#[tokio::test]
async fn message_observers_enrichment_only_is_ineligible() {
    let manager = ExtensionManager::from_config(config(ExtensionCapability::MessageEnrich))
        .await
        .expect("enrichment fixture loads");
    assert!(!manager.has_message_observers(&message()));
}

#[tokio::test]
async fn message_observers_empty_manager_is_ineligible() {
    let manager = ExtensionManager::from_config(ExtensionConfig::default())
        .await
        .expect("empty manager loads");
    assert!(!manager.has_message_observers(&message()));
}

#[tokio::test]
async fn message_observers_disabled_manager_is_ineligible() {
    let mut config = config(ExtensionCapability::MessageObserve);
    config.enabled = false;
    let manager = ExtensionManager::from_config(config)
        .await
        .expect("disabled manager loads");
    assert!(!manager.has_message_observers(&message()));
}

#[tokio::test]
async fn message_observers_bodyless_and_whitespace_messages_are_ineligible() {
    let manager = ExtensionManager::from_config(config(ExtensionCapability::MessageObserve))
        .await
        .expect("observer fixture loads");
    assert!(!manager.has_message_observers(&Message::new(None)));
    let mut message = message();
    message.bodies.insert(Lang(String::new()), " \t\n".into());
    assert!(!manager.has_message_observers(&message));
}

#[tokio::test]
async fn message_observers_non_default_language_is_eligible() {
    let manager = ExtensionManager::from_config(config(ExtensionCapability::MessageObserve))
        .await
        .expect("observer fixture loads");
    let mut message = Message::new(None);
    message.bodies.insert(Lang("nb".into()), "hei".into());
    assert!(manager.has_message_observers(&message));
}
