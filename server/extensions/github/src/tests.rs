use super::*;

fn test_lock() -> &'static std::sync::Mutex<()> {
    static TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    TEST_LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

fn sample_webhook(conclusion: &str) -> types::ProviderWebhook {
    types::ProviderWebhook {
        waddle_id: types::WaddleId {
            value: "github-delivery-1".to_string(),
        },
        provider: types::ProviderId {
            value: "github".to_string(),
        },
        event_type: types::ProviderEventType {
            value: "workflow_run".to_string(),
        },
        delivery_id: types::ProviderDeliveryId {
            value: "delivery-1".to_string(),
        },
        payload: types::ProviderPayload {
            fields: vec![
                text_field(&["action"], "completed"),
                number_field(&["installation", "id"], "42"),
                number_field(&["repository", "id"], "100"),
                text_field(&["repository", "full_name"], "waddle-social/waddle"),
                text_field(&["workflow_run", "name"], "ci"),
                text_field(&["workflow_run", "conclusion"], conclusion),
                text_field(&["workflow_run", "head_branch"], "main"),
                text_field(&["workflow_run", "head_sha"], "1234567890abcdef"),
                text_field(
                    &["workflow_run", "html_url"],
                    "https://github.com/waddle-social/waddle/actions/runs/1",
                ),
            ],
        },
    }
}

fn text_field(path: &[&str], value: &str) -> types::ProviderField {
    provider_field(path, types::ProviderFieldValue::Text(text(value)))
}

fn number_field(path: &[&str], value: &str) -> types::ProviderField {
    provider_field(path, types::ProviderFieldValue::Number(number(value)))
}

fn provider_field(path: &[&str], value: types::ProviderFieldValue) -> types::ProviderField {
    types::ProviderField {
        path: path
            .iter()
            .map(|segment| types::ProviderFieldName {
                value: (*segment).to_string(),
            })
            .collect(),
        value,
    }
}

fn text(value: &str) -> types::ProviderFieldText {
    types::ProviderFieldText {
        value: value.to_string(),
    }
}

fn number(value: &str) -> types::ProviderFieldNumber {
    types::ProviderFieldNumber {
        value: value.to_string(),
    }
}

#[test]
fn manifest_declares_web_integration_namespace_and_message_send() {
    let manifest = manifest();

    assert_eq!(manifest.id.value, "github");
    assert!(manifest
        .capabilities
        .contains(&types::ExtensionCapability::HostMessageSend));
    assert_eq!(manifest.payloads[0].root.namespace.value, PLUGIN_NS);
}

#[test]
fn parses_route_config() {
    let config = parse_config(
        r#"{
            "routes": [{
                "installation_id": "42",
                "repository_id": "100",
                "channel": "dev@muc.waddle.local",
                "events": ["workflow_run"]
            }]
        }"#,
    )
    .expect("config parses");

    assert_eq!(config.routes.len(), 1);
    assert_eq!(config.routes[0].installation_id.as_deref(), Some("42"));
    assert_eq!(config.routes[0].repository_id, "100");
    assert_eq!(config.routes[0].channel.value, "dev@muc.waddle.local");
    assert_eq!(config.routes[0].events, vec!["workflow_run"]);
}

#[test]
fn failure_webhook_builds_alert_text() {
    let alert = alert_for_webhook(&sample_webhook("failure")).expect("alert");

    assert!(alert
        .body
        .contains("GitHub waddle-social/waddle: ci completed with failure"));
    assert!(alert.body.contains("on main"));
    assert!(alert.body.contains("1234567"));
    assert!(alert
        .body
        .contains("https://github.com/waddle-social/waddle/actions/runs/1"));
}

#[test]
fn successful_webhook_does_not_alert() {
    assert!(alert_for_webhook(&sample_webhook("success")).is_none());
}

#[test]
fn provider_webhook_sends_to_matching_route() {
    let _guard = test_lock().lock().expect("test lock");
    sent_room_messages().lock().expect("messages lock").clear();
    let config = parse_config(
        r#"{
            "routes": [{
                "installation_id": "42",
                "repository_id": "100",
                "channel": "dev@muc.waddle.local",
                "events": ["workflow_run"]
            }]
        }"#,
    )
    .expect("config parses");

    let effects = handle_provider_webhook(sample_webhook("timed_out"), config).expect("handled");

    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], types::ExtensionEffect::Noop));
    let messages = sent_room_messages().lock().expect("messages lock");
    assert_eq!(messages.len(), 1);
    match &messages[0].target {
        types::MessageTarget::Muc(room) => assert_eq!(room.value, "dev@muc.waddle.local"),
        types::MessageTarget::Direct(_) => panic!("expected room message"),
    }
    assert!(messages[0].body.value.contains("timed_out"));
}

#[test]
fn provider_webhook_ignores_non_matching_route() {
    let _guard = test_lock().lock().expect("test lock");
    sent_room_messages().lock().expect("messages lock").clear();
    let config = parse_config(
        r#"{
            "routes": [{
                "installation_id": "42",
                "repository_id": "999",
                "channel": "dev@muc.waddle.local",
                "events": ["workflow_run"]
            }]
        }"#,
    )
    .expect("config parses");

    let effects = handle_provider_webhook(sample_webhook("failure"), config).expect("handled");

    assert!(effects.is_empty());
    assert!(sent_room_messages()
        .lock()
        .expect("messages lock")
        .is_empty());
}
