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

fn deployment_status_webhook(state: &str) -> types::ProviderWebhook {
    types::ProviderWebhook {
        waddle_id: types::WaddleId {
            value: "github-deployment-1".to_string(),
        },
        provider: types::ProviderId {
            value: "github".to_string(),
        },
        event_type: types::ProviderEventType {
            value: "deployment_status".to_string(),
        },
        delivery_id: types::ProviderDeliveryId {
            value: "delivery-deployment-1".to_string(),
        },
        payload: types::ProviderPayload {
            fields: vec![
                text_field(&["action"], "created"),
                number_field(&["installation", "id"], "42"),
                number_field(&["repository", "id"], "100"),
                text_field(&["repository", "full_name"], "waddle-social/waddle"),
                text_field(&["deployment", "environment"], "production"),
                text_field(&["deployment", "ref"], "main"),
                text_field(&["deployment", "sha"], "1234567890abcdef"),
                text_field(&["deployment_status", "state"], state),
                text_field(
                    &["deployment_status", "target_url"],
                    "https://dashboard.render.com/deploys/1",
                ),
            ],
        },
    }
}

fn installation_webhook() -> types::ProviderWebhook {
    types::ProviderWebhook {
        waddle_id: types::WaddleId {
            value: "github-delivery-installation".to_string(),
        },
        provider: types::ProviderId {
            value: "github".to_string(),
        },
        event_type: types::ProviderEventType {
            value: "installation".to_string(),
        },
        delivery_id: types::ProviderDeliveryId {
            value: "delivery-installation".to_string(),
        },
        payload: types::ProviderPayload {
            fields: vec![
                text_field(&["action"], "deleted"),
                number_field(&["installation", "id"], "42"),
            ],
        },
    }
}

fn sample_payload(conclusion: &str) -> GitHubPayload {
    GitHubPayload::from_webhook(&sample_webhook(conclusion)).expect("payload")
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

fn sample_route(repository_id: &str, channel: &str) -> Route {
    Route {
        repository_id: repository_id.to_string(),
        channel: channel.to_string(),
        events: vec!["workflow_run".to_string(), "check_run".to_string()],
        installation_id: None,
    }
}

fn admin_command(
    node: &str,
    requester: &str,
    fields: Vec<types::FormFieldValue>,
) -> types::CommandInvocation {
    types::CommandInvocation {
        waddle_id: types::WaddleId {
            value: "command-1".to_string(),
        },
        room: None,
        requester: types::FullJid {
            value: requester.to_string(),
        },
        command_node: types::CommandNode {
            value: node.to_string(),
        },
        session_id: None,
        action: Some(types::CommandAction::Execute),
        form: None,
        fields,
    }
}

fn form_field(name: &str, values: &[&str]) -> types::FormFieldValue {
    types::FormFieldValue {
        name: types::UiActionId {
            value: name.to_string(),
        },
        values: values
            .iter()
            .map(|value| types::DataFormValue {
                value: (*value).to_string(),
            })
            .collect(),
    }
}

#[test]
fn manifest_declares_admin_command_and_routes_node() {
    let manifest = manifest();

    assert_eq!(manifest.id.value, "github");
    assert_eq!(
        manifest
            .profile
            .as_ref()
            .expect("profile")
            .display_name
            .value,
        "GitHub"
    );
    assert_eq!(manifest.payloads[0].root.namespace.value, PLUGIN_NS);
    assert!(manifest.payloads.iter().any(|rule| {
        rule.surface == types::PayloadSurface::MessageEnrichment
            && rule.root.local_name == "github-event"
    }));
    assert!(manifest
        .capabilities
        .contains(&types::ExtensionCapability::MessageEnrich));
    assert!(manifest
        .capabilities
        .contains(&types::ExtensionCapability::HostMessageSend));
    assert!(manifest
        .capabilities
        .contains(&types::ExtensionCapability::Commands));
    assert!(manifest
        .capabilities
        .contains(&types::ExtensionCapability::PubsubPublish));
    assert_eq!(manifest.commands.len(), 1);
    assert_eq!(manifest.commands[0].node.value, COMMAND_NODE);
    assert!(manifest
        .pubsub_nodes
        .iter()
        .any(|node| node.value == ROUTES_NODE));
}

#[test]
fn parses_admin_only_config() {
    let config = parse_config(
        r#"{
            "admins": ["rawkode@waddle.social", "icepuma@waddle.social"]
        }"#,
    )
    .expect("config parses");

    assert_eq!(config.admins.len(), 2);
    assert!(config.admins.contains(&"rawkode@waddle.social".to_string()));
}

#[test]
fn empty_config_parses_to_empty_admin_list() {
    let config = parse_config("").expect("empty config parses");
    assert!(config.admins.is_empty());
}

#[test]
fn failure_webhook_builds_alert_text() {
    let payload = sample_payload("failure");
    let alert = alert_for_payload(&payload).expect("alert");

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
    let payload = sample_payload("success");

    assert!(alert_for_payload(&payload).is_none());
}

#[test]
fn cancelled_webhook_does_not_alert() {
    let payload = sample_payload("cancelled");

    assert!(alert_for_payload(&payload).is_none());
}

#[test]
fn deployment_status_payload_uses_state_and_deployment_fields() {
    let payload =
        GitHubPayload::from_webhook(&deployment_status_webhook("failure")).expect("payload");

    assert_eq!(payload.conclusion, "failure");
    assert_eq!(payload.name, "production");
    assert_eq!(payload.branch.as_deref(), Some("main"));
    assert_eq!(payload.revision.as_deref(), Some("1234567890abcdef"));
    assert_eq!(
        payload.url.as_deref(),
        Some("https://dashboard.render.com/deploys/1"),
    );
}

#[test]
fn deployment_status_payload_skips_empty_url_fields() {
    let mut webhook = deployment_status_webhook("failure");
    webhook.payload.fields.push(text_field(
        &["deployment_status", "log_url"],
        "https://github.com/waddle-social/waddle/deployments/1/logs",
    ));
    webhook.payload.fields.retain(|field| {
        !field
            .path
            .iter()
            .map(|segment| segment.value.as_str())
            .eq(["deployment_status", "target_url"])
    });
    webhook
        .payload
        .fields
        .push(text_field(&["deployment_status", "target_url"], ""));

    let payload = GitHubPayload::from_webhook(&webhook).expect("payload");

    assert_eq!(
        payload.url.as_deref(),
        Some("https://github.com/waddle-social/waddle/deployments/1/logs")
    );
    assert!(alert_for_payload(&payload).is_some());
}

#[test]
fn deployment_status_pending_does_not_alert() {
    let payload =
        GitHubPayload::from_webhook(&deployment_status_webhook("pending")).expect("payload");

    assert!(alert_for_payload(&payload).is_none());
}

#[test]
fn deployment_status_error_does_not_alert_for_now() {
    let payload =
        GitHubPayload::from_webhook(&deployment_status_webhook("error")).expect("payload");

    assert!(alert_for_payload(&payload).is_none());
}

#[test]
fn provider_webhook_sends_to_route_loaded_from_pubsub() {
    let _guard = test_lock().lock().expect("test lock");
    test_state::reset();
    test_state::set_route_fixtures(vec![sample_route("100", "dev@muc.waddle.social")]);

    let effects = handle_provider_webhook(sample_webhook("failure")).expect("handled");

    assert_eq!(effects.len(), 1);
    assert!(matches!(effects[0], types::ExtensionEffect::Noop));
    let messages = test_state::sent_room_messages()
        .lock()
        .expect("messages lock");
    assert_eq!(messages.len(), 1);
    match &messages[0].target {
        types::MessageTarget::Muc(room) => assert_eq!(room.value, "dev@muc.waddle.social"),
        types::MessageTarget::Direct(_) => panic!("expected room message"),
    }
    assert!(messages[0].body.value.contains("failure"));
    let envelope = messages[0].extensions.as_ref().expect("extension envelope");
    assert_eq!(envelope.version, 1);
    let enrichment = envelope.enrichments.first().expect("enrichment");
    assert_eq!(enrichment.plugin.value, "github");
    assert_eq!(
        enrichment.capability,
        types::ExtensionCapability::MessageEnrich
    );
    assert_eq!(enrichment.payload_namespace.value, PLUGIN_NS);
    assert_eq!(enrichment.payloads.len(), 1);
    let payload = &enrichment.payloads[0];
    assert_eq!(payload.root.local_name, "github-event");
    let attrs = match &payload.tokens[0] {
        types::XmlToken::StartElement(element) => &element.attributes,
        _ => panic!("expected start element"),
    };
    assert_eq!(find_attr(attrs, "event-type"), Some("workflow_run"));
    assert_eq!(find_attr(attrs, "repository"), Some("waddle-social/waddle"));
    assert_eq!(find_attr(attrs, "conclusion"), Some("failure"));
}

#[test]
fn provider_webhook_ignores_non_matching_repository() {
    let _guard = test_lock().lock().expect("test lock");
    test_state::reset();
    test_state::set_route_fixtures(vec![sample_route("999", "dev@muc.waddle.social")]);

    let effects = handle_provider_webhook(sample_webhook("failure")).expect("handled");

    assert!(effects.is_empty());
    assert!(test_state::sent_room_messages()
        .lock()
        .expect("messages lock")
        .is_empty());
}

#[test]
fn provider_webhook_ignores_installation_scoped_event_without_repository() {
    let _guard = test_lock().lock().expect("test lock");
    test_state::reset();
    test_state::set_route_fixtures(vec![sample_route("100", "dev@muc.waddle.social")]);

    let effects = handle_provider_webhook(installation_webhook()).expect("handled");

    assert!(effects.is_empty());
    assert!(test_state::sent_room_messages()
        .lock()
        .expect("messages lock")
        .is_empty());
}

#[test]
fn configure_route_command_returns_form_when_fields_missing() {
    let _guard = test_lock().lock().expect("test lock");
    test_state::reset();
    test_state::set_config_fixture(GitHubConfig {
        admins: vec!["rawkode@waddle.social".to_string()],
    });

    let command = admin_command(COMMAND_NODE, "rawkode@waddle.social/abc", vec![]);
    let effects = handle_command(command, current_config().expect("config")).expect("handled");

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        types::ExtensionEffect::CommandForm(form) => {
            assert!(form.fields.iter().any(|f| {
                f.name.value == FIELD_FORM_TYPE
                    && f.field_type == types::FormFieldType::Hidden
                    && f.values
                        .first()
                        .is_some_and(|value| value.value == CONFIGURE_ROUTE_FORM_TYPE)
            }));
            assert!(form
                .fields
                .iter()
                .any(|f| f.name.value == FIELD_REPOSITORY_ID));
            assert!(form.fields.iter().any(|f| f.name.value == FIELD_CHANNEL));
            assert!(form.fields.iter().any(|f| f.name.value == FIELD_EVENTS));
        }
        other => panic!("expected CommandForm, got {other:?}"),
    }
}

#[test]
fn configure_route_command_writes_publish_effect_on_submit() {
    let _guard = test_lock().lock().expect("test lock");
    test_state::reset();
    test_state::set_config_fixture(GitHubConfig {
        admins: vec!["rawkode@waddle.social".to_string()],
    });

    let command = admin_command(
        COMMAND_NODE,
        "rawkode@waddle.social/abc",
        vec![
            form_field(FIELD_FORM_TYPE, &[CONFIGURE_ROUTE_FORM_TYPE]),
            form_field(FIELD_REPOSITORY_ID, &["1009269194"]),
            form_field(FIELD_CHANNEL, &["chat@muc.waddle.social"]),
            form_field(FIELD_EVENTS, &["workflow_run", "check_run"]),
        ],
    );
    let effects = handle_command(command, current_config().expect("config")).expect("handled");

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        types::ExtensionEffect::PublishPubsub(publish) => {
            assert_eq!(publish.node.value, ROUTES_NODE);
            assert_eq!(
                publish.item_id.as_ref().expect("item id").value,
                "1009269194"
            );
            assert_eq!(publish.payload.root.local_name, ROUTE_ELEMENT);
            assert_eq!(publish.payload.root.namespace.value, PLUGIN_NS);
            let attrs = match &publish.payload.tokens[0] {
                types::XmlToken::StartElement(element) => &element.attributes,
                _ => panic!("expected start element"),
            };
            assert_eq!(find_attr(attrs, ATTR_REPOSITORY_ID), Some("1009269194"));
            assert_eq!(
                find_attr(attrs, ATTR_CHANNEL),
                Some("chat@muc.waddle.social")
            );
            assert_eq!(
                find_attr(attrs, ATTR_EVENTS),
                Some("workflow_run,check_run")
            );
        }
        other => panic!("expected PublishPubsub, got {other:?}"),
    }
}

#[test]
fn configure_route_command_rejects_non_admin() {
    let _guard = test_lock().lock().expect("test lock");
    test_state::reset();
    test_state::set_config_fixture(GitHubConfig {
        admins: vec!["rawkode@waddle.social".to_string()],
    });

    let command = admin_command(
        COMMAND_NODE,
        "stranger@elsewhere.org/x",
        vec![
            form_field(FIELD_FORM_TYPE, &[CONFIGURE_ROUTE_FORM_TYPE]),
            form_field(FIELD_REPOSITORY_ID, &["1009269194"]),
            form_field(FIELD_CHANNEL, &["chat@muc.waddle.social"]),
            form_field(FIELD_EVENTS, &["workflow_run"]),
        ],
    );
    let err = handle_command(command, current_config().expect("config")).expect_err("should deny");
    assert_eq!(err.code, types::ExtensionErrorCode::Denied);
}

#[test]
fn configure_route_command_rejects_missing_form_type_on_submit() {
    let _guard = test_lock().lock().expect("test lock");
    test_state::reset();
    test_state::set_config_fixture(GitHubConfig {
        admins: vec!["rawkode@waddle.social".to_string()],
    });

    let command = admin_command(
        COMMAND_NODE,
        "rawkode@waddle.social/abc",
        vec![
            form_field(FIELD_REPOSITORY_ID, &["1009269194"]),
            form_field(FIELD_CHANNEL, &["chat@muc.waddle.social"]),
            form_field(FIELD_EVENTS, &["workflow_run"]),
        ],
    );
    let err =
        handle_command(command, current_config().expect("config")).expect_err("should reject");
    assert_eq!(err.code, types::ExtensionErrorCode::InvalidRequest);
}

#[test]
fn configure_route_command_rejects_wrong_form_type_on_submit() {
    let _guard = test_lock().lock().expect("test lock");
    test_state::reset();
    test_state::set_config_fixture(GitHubConfig {
        admins: vec!["rawkode@waddle.social".to_string()],
    });

    let command = admin_command(
        COMMAND_NODE,
        "rawkode@waddle.social/abc",
        vec![
            form_field(
                FIELD_FORM_TYPE,
                &["urn:waddle:web-integration:1:github:other"],
            ),
            form_field(FIELD_REPOSITORY_ID, &["1009269194"]),
            form_field(FIELD_CHANNEL, &["chat@muc.waddle.social"]),
            form_field(FIELD_EVENTS, &["workflow_run"]),
        ],
    );
    let err =
        handle_command(command, current_config().expect("config")).expect_err("should reject");
    assert_eq!(err.code, types::ExtensionErrorCode::InvalidRequest);
}

#[test]
fn configure_route_command_rejects_non_numeric_repository_id() {
    let _guard = test_lock().lock().expect("test lock");
    test_state::reset();
    test_state::set_config_fixture(GitHubConfig {
        admins: vec!["rawkode@waddle.social".to_string()],
    });

    let command = admin_command(
        COMMAND_NODE,
        "rawkode@waddle.social/abc",
        vec![
            form_field(FIELD_FORM_TYPE, &[CONFIGURE_ROUTE_FORM_TYPE]),
            form_field(FIELD_REPOSITORY_ID, &["not-a-number"]),
            form_field(FIELD_CHANNEL, &["chat@muc.waddle.social"]),
            form_field(FIELD_EVENTS, &["workflow_run"]),
        ],
    );
    let err =
        handle_command(command, current_config().expect("config")).expect_err("should reject");
    assert_eq!(err.code, types::ExtensionErrorCode::InvalidRequest);
}

#[test]
fn route_payload_round_trip() {
    let original = Route {
        repository_id: "1009269194".to_string(),
        channel: "chat@muc.waddle.social".to_string(),
        events: vec!["workflow_run".to_string(), "check_run".to_string()],
        installation_id: Some("42".to_string()),
    };
    let payload = original.to_payload();
    let parsed = Route::from_payload(&payload).expect("round trip");
    assert_eq!(parsed, original);
}
