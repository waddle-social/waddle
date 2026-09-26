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
    let fixture = Path::new(
        &std::env::var_os("CARGO_MANIFEST_DIR").expect("test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("tests/fixtures/message_hook.wat");
    ExtensionConfig {
        modules: vec![ExtensionModuleConfig {
            room_observation: None,
            runtime_limits: Default::default(),
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
    assert_eq!(
        manager.message_observer_plugins(&message()),
        vec!["message-hook-fixture".parse_plugin_id()]
    );
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
    assert!(manager.message_observer_plugins(&message()).is_empty());
}

#[tokio::test]
async fn message_observers_empty_manager_is_ineligible() {
    let manager = ExtensionManager::from_config(ExtensionConfig::default())
        .await
        .expect("empty manager loads");
    assert!(manager.message_observer_plugins(&message()).is_empty());
}

#[tokio::test]
async fn message_observers_disabled_manager_is_ineligible() {
    let mut config = config(ExtensionCapability::MessageObserve);
    config.enabled = false;
    let manager = ExtensionManager::from_config(config)
        .await
        .expect("disabled manager loads");
    assert!(manager.message_observer_plugins(&message()).is_empty());
}

#[tokio::test]
async fn message_observers_bodyless_and_whitespace_messages_are_ineligible() {
    let manager = ExtensionManager::from_config(config(ExtensionCapability::MessageObserve))
        .await
        .expect("observer fixture loads");
    assert!(manager
        .message_observer_plugins(&Message::new(None))
        .is_empty());
    let mut message = message();
    message.bodies.insert(Lang(String::new()), " \t\n".into());
    assert!(manager.message_observer_plugins(&message).is_empty());
}

#[tokio::test]
async fn message_observers_non_default_language_is_eligible() {
    let manager = ExtensionManager::from_config(config(ExtensionCapability::MessageObserve))
        .await
        .expect("observer fixture loads");
    let mut message = Message::new(None);
    message.bodies.insert(Lang("nb".into()), "hei".into());
    assert_eq!(manager.message_observer_plugins(&message).len(), 1);
}

trait PluginFixture {
    fn parse_plugin_id(self) -> waddle_extensions::PluginId;
}

impl PluginFixture for &str {
    fn parse_plugin_id(self) -> waddle_extensions::PluginId {
        waddle_extensions::PluginId::new(self).expect("fixture plugin")
    }
}

#[tokio::test]
async fn message_observer_invokes_only_the_selected_eligible_plugin() {
    let manager = ExtensionManager::from_config(config(ExtensionCapability::MessageObserve))
        .await
        .expect("observer fixture loads");
    let plugin = "message-hook-fixture".parse_plugin_id();
    assert!(manager
        .process_message_observer(
            &plugin,
            &message(),
            waddle_extensions::WaddleId::new("local").expect("waddle"),
            None,
        )
        .await
        .is_some());
    assert!(manager
        .process_message_observer(
            &"missing-plugin".parse_plugin_id(),
            &message(),
            waddle_extensions::WaddleId::new("local").expect("waddle"),
            None,
        )
        .await
        .is_none());
}

fn subscribed_config() -> ExtensionConfig {
    let mut config = config(ExtensionCapability::MessageObserve);
    config.modules[0].room_observation = Some(waddle_extensions::RoomObservationConfig {
        generation: waddle_extensions::ObservationGeneration::new(1).expect("generation"),
        scope: waddle_extensions::RoomObservationScope::Rooms(vec!["room@muc.example.com"
            .parse()
            .expect("room")]),
        max_concurrent: 2,
    });
    config
}

fn room_source() -> waddle_extensions::RoomMessageSource {
    use waddle_extensions::*;
    RoomMessageSource {
        room: "room@muc.example.com".parse().expect("room"),
        stanza_id: StanzaId::new("original").expect("stanza"),
        revision_stanza_id: StanzaId::new("original").expect("revision stanza"),
        origin_id: Some(OriginId::new("sender-origin").expect("origin")),
        sender: "alice@example.com".parse().expect("sender"),
        revision: MessageRevision::new(0),
        body_digest: Sha256Digest::new(
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        )
        .expect("digest"),
        observed_at: Timestamp::new("2026-09-26T12:00:00Z").expect("time"),
    }
}

#[tokio::test]
async fn durable_observations_are_explicit_and_excluded_from_legacy_hooks() {
    let manager = ExtensionManager::from_config(subscribed_config())
        .await
        .expect("manager");
    let source = room_source();
    assert!(manager.message_observer_plugins(&message()).is_empty());
    assert!(manager
        .room_observation_subscriptions(&"other@muc.example.com".parse().expect("room"))
        .is_empty());
    let selected = manager.room_observation_subscriptions(&source.room);
    assert_eq!(selected.len(), 1);
    let outcome = manager
        .observe_room_message(
            &selected[0],
            source,
            waddle_extensions::DisplayText::new("hello").expect("body"),
        )
        .await;
    assert!(matches!(
        outcome,
        waddle_extensions::RoomObservationOutcome::Completed(_)
    ));
}

#[tokio::test]
async fn frozen_observations_reject_changed_config_and_missing_origin() {
    use waddle_extensions::*;
    let manager = ExtensionManager::from_config(subscribed_config())
        .await
        .expect("manager");
    let source = room_source();
    let selected = manager.room_observation_subscriptions(&source.room);
    let mut stale = selected[0].clone();
    stale.generation = ObservationGeneration::new(2).expect("generation");
    assert_eq!(
        manager
            .observe_room_message(
                &stale,
                source.clone(),
                DisplayText::new("hello").expect("body")
            )
            .await,
        RoomObservationOutcome::NotApplicable(ObservationSkip::SubscriptionUnavailable)
    );
    let mut missing = source.clone();
    missing.origin_id = None;
    assert_eq!(
        manager
            .observe_room_message(
                &selected[0],
                missing,
                DisplayText::new("hello").expect("body")
            )
            .await,
        RoomObservationOutcome::NotApplicable(ObservationSkip::MissingOriginId)
    );
    assert_eq!(
        manager
            .observe_room_message(
                &selected[0],
                source,
                DisplayText::new("changed").expect("body")
            )
            .await,
        RoomObservationOutcome::PermanentFailure(ObservationFailure::SourceMismatch)
    );
}

#[tokio::test]
async fn frozen_identity_changes_when_execution_configuration_changes() {
    let first = ExtensionManager::from_config(subscribed_config())
        .await
        .expect("first");
    let mut changed = subscribed_config();
    changed.modules[0].runtime_limits.http_timeout_ms = 4_000;
    let second = ExtensionManager::from_config(changed)
        .await
        .expect("second");
    assert_ne!(
        first.configured_room_observers()[0].identity,
        second.configured_room_observers()[0].identity
    );
}
