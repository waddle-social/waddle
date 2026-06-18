use super::{
    handle_command_with_rng, manifest, parse_quotes_json, quote_body, quote_catalog,
    select_quote_with_rng, take_sent_room_messages, types, COMMAND_NAME, COMMAND_NODE,
    COMMAND_RESULT_TEXT, PLUGIN_NS,
};
use serde_json::Value;

#[test]
fn manifest_registers_channel_stargate_command() {
    let manifest = manifest();

    assert_eq!(manifest.id.value, "stargate-quotes");
    assert_eq!(manifest.commands.len(), 1);
    assert_eq!(manifest.commands[0].node.value, COMMAND_NODE);
    assert_eq!(manifest.commands[0].name.value, COMMAND_NAME);
    assert_eq!(
        manifest.commands[0].composer_prefix.as_deref(),
        Some("stargate")
    );
    assert!(matches!(
        manifest.commands[0].scope,
        types::CommandScope::Channel
    ));
    assert_eq!(
        manifest.capabilities,
        vec![
            types::ExtensionCapability::Commands,
            types::ExtensionCapability::HostMessageSend,
            types::ExtensionCapability::MessageEnrich,
        ]
    );
}

#[test]
fn quote_catalog_is_typed_and_covers_all_series() {
    let quotes = quote_catalog().expect("quote catalog parses");

    assert!(quotes.iter().any(|quote| quote.series == "Stargate SG-1"));
    assert!(quotes
        .iter()
        .any(|quote| quote.series == "Stargate Atlantis"));
    assert!(quotes
        .iter()
        .any(|quote| quote.series == "Stargate Universe"));
    assert!(quotes.len() >= 36);
    assert!(quotes.iter().all(|quote| !quote.role.is_empty()
        && !quote.quote.is_empty()
        && !quote.series.is_empty()));
}

#[test]
fn quote_catalog_rejects_invalid_json() {
    assert!(parse_quotes_json(r#"{"not":"a list"}"#).is_err());
}

#[test]
fn quote_catalog_json_only_contains_rendered_fields() {
    let values: Value = serde_json::from_str(include_str!("quotes.json")).expect("json parses");
    let quotes = values.as_array().expect("quote catalog is a list");

    assert!(quotes.iter().all(|quote| {
        let fields = quote.as_object().expect("quote is object");
        fields.len() == 3
            && fields.contains_key("role")
            && fields.contains_key("quote")
            && fields.contains_key("series")
    }));
}

#[test]
fn quote_body_renders_plain_quote_with_role_and_series() {
    let quotes = quote_catalog().expect("quote catalog parses");
    let body = quote_body(&quotes[0]);

    assert_eq!(
        body,
        format!(
            "{}\n\n{}, {}",
            quotes[0].quote, quotes[0].role, quotes[0].series
        )
    );
    assert!(!body.starts_with('"'));
    assert!(!body.contains("\n\n- "));
}

#[test]
fn random_selection_rejects_low_values_that_would_bias_modulo() {
    let quotes = quote_catalog().expect("quote catalog parses");
    let mut values = [0, quotes.len() as u64].into_iter();
    let quote = select_quote_with_rng(&quotes, || values.next().expect("next random value"))
        .expect("quote selected");

    assert_eq!(quote, &quotes[0]);
}

#[test]
fn room_command_posts_plain_quote_and_returns_completion() {
    let _ = take_sent_room_messages();
    let command = command_invocation(Some("sgc@muc.example.com"));
    let quotes = quote_catalog().expect("quote catalog parses");
    let expected = select_quote_with_rng(&quotes, || 42).expect("expected quote");

    let effects = handle_command_with_rng(command, || 42).expect("command succeeds");

    assert_eq!(effects.len(), 1);
    let types::ExtensionEffect::EnrichMessage(envelope) = &effects[0] else {
        panic!("expected command completion enrichment");
    };
    assert_eq!(envelope.enrichments[0].payload_namespace.value, PLUGIN_NS);
    let types::UiBlock::Text(text) = &envelope.enrichments[0].ui[0].blocks[0] else {
        panic!("expected text block");
    };
    assert_eq!(text.text.value, COMMAND_RESULT_TEXT);

    let sent = take_sent_room_messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].body.value, quote_body(expected));
    assert!(sent[0].extensions.is_none());
    assert!(matches!(
        &sent[0].target,
        types::MessageTarget::Muc(room) if room.value == "sgc@muc.example.com"
    ));
}

#[test]
fn command_without_room_warns_and_sends_nothing() {
    let _ = take_sent_room_messages();
    let effects = handle_command_with_rng(command_invocation(None), || 42).expect("command runs");

    assert_eq!(effects.len(), 1);
    let types::ExtensionEffect::HostWarning(message) = &effects[0] else {
        panic!("expected warning");
    };
    assert_eq!(
        message.value,
        "Stargate quotes require an active channel.".to_string()
    );
    assert!(take_sent_room_messages().is_empty());
}

fn command_invocation(room: Option<&str>) -> types::CommandInvocation {
    types::CommandInvocation {
        waddle_id: types::WaddleId {
            value: "test-waddle".to_string(),
        },
        room: room.map(|value| types::RoomJid {
            value: value.to_string(),
        }),
        requester: types::FullJid {
            value: "sam@example.com/alpha".to_string(),
        },
        command_node: types::CommandNode {
            value: COMMAND_NODE.to_string(),
        },
        session_id: None,
        action: Some(types::CommandAction::Execute),
        form: None,
        fields: vec![],
    }
}
