//! Extension-command FFI test suite: invoke/submit wire shapes as
//! produced by the exact FFI construction path, reply → typed-result
//! mapping (statuses, sessions, actions, forms, notes, malformed
//! payloads), and the exported methods' typed failure behavior
//! (not-connected, invalid JID) without a live connection.

use std::sync::Arc;

use jid::BareJid;
use minidom::Element;

use waddle_xmpp_client::error::{StanzaError, StanzaErrorType};
use waddle_xmpp_client::extension_commands::ExtensionSubmitField;
use waddle_xmpp_client::xep::xep0050::{NS_COMMANDS, NS_DATA_FORMS};
use waddle_xmpp_client::ClientError;

use crate::extension_commands::{
    adhoc_action_from_ffi, extension_invoke_stanza, extension_submit_stanza, map_command_reply,
};
use crate::{
    WaddleAdhocAction, WaddleAdhocStatus, WaddleClient, WaddleClientEvent, WaddleConfig,
    WaddleError, WaddleEventListener, WaddleExtensionFieldType, WaddleExtensionFormField,
    WaddleExtensionNoteType,
};

struct NoopListener;

impl WaddleEventListener for NoopListener {
    fn on_event(&self, _event: WaddleClientEvent) {}
}

fn test_client() -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
        },
        listener: Arc::new(Box::new(NoopListener) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
        inbox_query_gate: tokio::sync::Mutex::new(()),
    })
}

fn bare(value: &str) -> BareJid {
    value.parse().expect("test bare JID parses")
}

fn reply(inner: &str) -> Result<Element, ClientError> {
    Ok(
        format!("<iq xmlns='jabber:client' type='result'>{inner}</iq>")
            .parse()
            .expect("test IQ parses"),
    )
}

// ── Stanza factories ─────────────────────────────────────────────────

#[test]
fn invoke_stanza_carries_node_action_and_room_field() {
    let iq = extension_invoke_stanza(
        &bare("extensions.waddle.test"),
        "urn:waddle:extension:1:decision-polls",
        Some(&bare("general@muc.waddle.test")),
    );
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("extensions.waddle.test"));
    let command = iq.get_child("command", NS_COMMANDS).expect("command");
    assert_eq!(
        command.attr("node"),
        Some("urn:waddle:extension:1:decision-polls"),
    );
    assert_eq!(command.attr("action"), Some("execute"));
    let form = command.get_child("x", NS_DATA_FORMS).expect("submit form");
    let field = form
        .children()
        .find(|child| child.attr("var") == Some("waddle#room_jid"))
        .expect("room field");
    assert_eq!(
        field
            .get_child("value", NS_DATA_FORMS)
            .map(Element::text)
            .as_deref(),
        Some("general@muc.waddle.test"),
    );
}

#[test]
fn invoke_stanza_without_room_has_no_form() {
    let iq = extension_invoke_stanza(
        &bare("extensions.waddle.test"),
        "urn:waddle:extension:1:stargate-quotes",
        None,
    );
    let command = iq.get_child("command", NS_COMMANDS).expect("command");
    assert!(command.get_child("x", NS_DATA_FORMS).is_none());
}

#[test]
fn submit_stanza_threads_session_action_and_fields() {
    let iq = extension_submit_stanza(
        &bare("extensions.waddle.test"),
        "urn:waddle:extension:1:ai-chatbot",
        Some("session-9"),
        adhoc_action_from_ffi(WaddleAdhocAction::Complete),
        &[ExtensionSubmitField {
            var: "prompt".to_string(),
            values: vec!["hello".to_string()],
        }],
        Some(&bare("general@muc.waddle.test")),
    );
    let command = iq.get_child("command", NS_COMMANDS).expect("command");
    assert_eq!(command.attr("sessionid"), Some("session-9"));
    assert_eq!(command.attr("action"), Some("complete"));
    let form = command.get_child("x", NS_DATA_FORMS).expect("submit form");
    let vars: Vec<_> = form
        .children()
        .filter_map(|child| child.attr("var").map(str::to_string))
        .collect();
    assert_eq!(
        vars,
        vec!["prompt".to_string(), "waddle#room_jid".to_string()],
    );
}

#[test]
fn submit_stanza_cancel_drops_the_form() {
    let iq = extension_submit_stanza(
        &bare("extensions.waddle.test"),
        "urn:waddle:extension:1:ai-chatbot",
        Some("session-9"),
        adhoc_action_from_ffi(WaddleAdhocAction::Cancel),
        &[ExtensionSubmitField {
            var: "prompt".to_string(),
            values: vec!["hello".to_string()],
        }],
        None,
    );
    let command = iq.get_child("command", NS_COMMANDS).expect("command");
    assert_eq!(command.attr("action"), Some("cancel"));
    assert!(command.get_child("x", NS_DATA_FORMS).is_none());
}

// ── Reply mapping ────────────────────────────────────────────────────

#[test]
fn map_command_reply_extracts_the_typed_result() {
    let result = map_command_reply(reply(
        "<command xmlns='http://jabber.org/protocol/commands' node='n' sessionid='s-1' \
         status='executing'>\
         <actions><complete/></actions>\
         <note type='warn'>Careful.</note>\
         <x xmlns='jabber:x:data' type='form'>\
         <field var='prompt' type='text-single'><required/></field>\
         </x>\
         </command>",
    ))
    .expect("maps");
    assert_eq!(result.status, WaddleAdhocStatus::Executing);
    assert_eq!(result.session_id.as_deref(), Some("s-1"));
    assert_eq!(
        result.actions,
        vec![WaddleAdhocAction::Complete, WaddleAdhocAction::Cancel],
    );
    assert_eq!(result.notes.len(), 1);
    assert_eq!(result.notes[0].note_type, WaddleExtensionNoteType::Warn);
    let form = result.form.expect("form");
    assert_eq!(form.fields.len(), 1);
    assert_eq!(form.fields[0].var, "prompt");
    assert_eq!(
        form.fields[0].field_type,
        WaddleExtensionFieldType::TextSingle,
    );
    assert!(form.fields[0].required);
}

#[test]
fn map_command_reply_flags_a_missing_command_payload() {
    assert_eq!(
        map_command_reply(reply("")),
        Err(WaddleError::MalformedResponse),
    );
}

#[test]
fn map_command_reply_passes_stanza_errors_through() {
    let error = ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Auth,
        condition: "forbidden".to_string(),
        text: Some("not yours".to_string()),
        application_condition: None,
    });
    assert_eq!(
        map_command_reply(Err(error)),
        Err(WaddleError::Stanza {
            condition: "forbidden".to_string(),
            text: Some("not yours".to_string()),
        }),
    );
}

// ── Exported method failure behavior ─────────────────────────────────

#[tokio::test]
async fn verbs_fail_typed_when_not_connected() {
    let client = test_client();
    assert_eq!(
        client.discover_extension_commands().await,
        Err(WaddleError::NotConnected),
    );
    assert_eq!(
        client
            .invoke_extension_command(
                "extensions.waddle.test".to_string(),
                "urn:waddle:extension:1:ai-chatbot".to_string(),
                None,
            )
            .await,
        Err(WaddleError::NotConnected),
    );
    assert_eq!(
        client
            .submit_extension_command_form(
                "extensions.waddle.test".to_string(),
                "urn:waddle:extension:1:ai-chatbot".to_string(),
                Some("s-1".to_string()),
                vec![WaddleExtensionFormField {
                    var: "prompt".to_string(),
                    values: vec!["hi".to_string()],
                }],
                WaddleAdhocAction::Complete,
                None,
            )
            .await,
        Err(WaddleError::NotConnected),
    );
}

#[tokio::test]
async fn verbs_reject_invalid_jids_and_empty_nodes_before_sending() {
    let client = test_client();
    assert_eq!(
        client
            .invoke_extension_command("not a jid".to_string(), "n".to_string(), None)
            .await,
        Err(WaddleError::InvalidJid),
    );
    assert_eq!(
        client
            .invoke_extension_command(
                "extensions.waddle.test".to_string(),
                "n".to_string(),
                Some("not a jid".to_string()),
            )
            .await,
        Err(WaddleError::InvalidJid),
    );
    assert_eq!(
        client
            .invoke_extension_command("extensions.waddle.test".to_string(), "  ".to_string(), None,)
            .await,
        Err(WaddleError::InvalidArgument),
    );
    assert_eq!(
        client
            .submit_extension_command_form(
                "extensions.waddle.test".to_string(),
                String::new(),
                None,
                vec![],
                WaddleAdhocAction::Cancel,
                None,
            )
            .await,
        Err(WaddleError::InvalidArgument),
    );
}
