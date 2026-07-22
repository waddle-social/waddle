//! Dedicated test suite for the `urn:waddle:extension:1` command
//! surface (XEP-0050 / XEP-0004 / XEP-0128 shapes): service
//! qualification, command-list filtering, metadata forms under both
//! FORM_TYPEs, invoke/submit wire shapes with session + room context,
//! and typed result parsing including notes, actions, and forms.

use minidom::Element;

use crate::discovery::{DiscoDataField, DiscoDataForm, DiscoInfoResult, DiscoItem};
use crate::xep::xep0050::{AdHocAction, AdHocStatus, NS_COMMANDS, NS_DATA_FORMS};

use super::*;

const CLIENT_NS: &str = "jabber:client";

fn info(features: &[&str], forms: Vec<DiscoDataForm>) -> DiscoInfoResult {
    DiscoInfoResult {
        jid: "extensions.waddle.test".to_string(),
        node: None,
        identities: vec![],
        features: features.iter().map(|f| f.to_string()).collect(),
        forms,
    }
}

fn metadata_form(form_type: &str, fields: &[(&str, &str)]) -> DiscoDataForm {
    DiscoDataForm {
        form_type: Some(form_type.to_string()),
        fields: fields
            .iter()
            .map(|(var, value)| DiscoDataField {
                var: var.to_string(),
                values: vec![value.to_string()],
            })
            .collect(),
    }
}

fn command_response(inner: &str) -> Element {
    format!("<iq xmlns='{CLIENT_NS}' type='result'>{inner}</iq>")
        .parse()
        .expect("test IQ parses")
}

// ── Service qualification ────────────────────────────────────────────

#[test]
fn service_qualifies_only_with_both_features() {
    assert!(is_extension_service(&info(
        &[NS_WADDLE_EXTENSION_1, NS_COMMANDS],
        vec![],
    )));
    assert!(!is_extension_service(&info(
        &[NS_WADDLE_EXTENSION_1],
        vec![]
    )));
    assert!(!is_extension_service(&info(&[NS_COMMANDS], vec![])));
    assert!(!is_extension_service(&info(&[], vec![])));
}

#[test]
fn candidates_prefer_discovered_components_then_fallback_then_domain() {
    let candidates = extension_service_candidates(
        "waddle.test",
        &[
            "muc.waddle.test".to_string(),
            "extensions.waddle.test".to_string(),
        ],
    );
    assert_eq!(
        candidates,
        vec![
            "muc.waddle.test".to_string(),
            "extensions.waddle.test".to_string(),
            "waddle.test".to_string(),
        ],
    );
}

#[test]
fn fallback_service_jid_is_the_extensions_subdomain() {
    assert_eq!(
        fallback_extension_service_jid("waddle.test"),
        "extensions.waddle.test",
    );
}

// ── Command-list filtering ───────────────────────────────────────────

#[test]
fn command_refs_drop_the_invoke_node_and_nodeless_items() {
    let refs = extension_command_refs(
        vec![
            DiscoItem {
                jid: "extensions.waddle.test".to_string(),
                name: Some("Decision Polls".to_string()),
                node: Some("urn:waddle:extension:1:decision-polls".to_string()),
            },
            DiscoItem {
                jid: "extensions.waddle.test".to_string(),
                name: None,
                node: Some(INVOKE_COMMAND_NODE.to_string()),
            },
            DiscoItem {
                jid: "extensions.waddle.test".to_string(),
                name: Some("No Node".to_string()),
                node: None,
            },
        ],
        "extensions.waddle.test",
    );
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].node, "urn:waddle:extension:1:decision-polls");
    assert_eq!(refs[0].name, "Decision Polls");
}

#[test]
fn command_refs_default_the_name_to_the_node() {
    let refs = extension_command_refs(
        vec![DiscoItem {
            jid: String::new(),
            name: None,
            node: Some("urn:waddle:extension:1:stargate-quotes".to_string()),
        }],
        "extensions.waddle.test",
    );
    assert_eq!(refs[0].service_jid, "extensions.waddle.test");
    assert_eq!(refs[0].name, "urn:waddle:extension:1:stargate-quotes");
}

// ── Metadata forms ───────────────────────────────────────────────────

#[test]
fn metadata_parses_from_the_per_command_form_type() {
    let metadata = parse_extension_command_metadata(&info(
        &[],
        vec![metadata_form(
            EXTENSION_COMMAND_FORM_TYPE,
            &[
                ("waddle#command_scope", "channel"),
                ("waddle#composer_prefix", "poll"),
            ],
        )],
    ));
    assert_eq!(metadata.scope, Some(ExtensionCommandScope::Channel));
    assert_eq!(metadata.composer_prefix.as_deref(), Some("poll"));
    assert_eq!(metadata.inline_field, None);
    assert!(!metadata.composer_execute);
}

#[test]
fn metadata_parses_from_the_service_form_type() {
    let metadata = parse_extension_command_metadata(&info(
        &[],
        vec![metadata_form(
            NS_WADDLE_EXTENSION_1,
            &[
                ("waddle#command_scope", "global"),
                ("waddle#composer_prefix", "ai"),
                ("waddle#inline_field", "prompt"),
            ],
        )],
    ));
    assert_eq!(metadata.scope, Some(ExtensionCommandScope::Global));
    assert_eq!(metadata.composer_prefix.as_deref(), Some("ai"));
    assert_eq!(metadata.inline_field.as_deref(), Some("prompt"));
}

#[test]
fn metadata_ignores_unrelated_form_types() {
    let metadata = parse_extension_command_metadata(&info(
        &[],
        vec![metadata_form(
            "urn:waddle:extension:1:routes",
            &[("waddle#composer_prefix", "bogus")],
        )],
    ));
    assert_eq!(metadata, ExtensionCommandMetadata::default());
}

#[test]
fn metadata_composer_execute_accepts_case_insensitive_true_only() {
    let parse = |value: &str| {
        parse_extension_command_metadata(&info(
            &[],
            vec![metadata_form(
                EXTENSION_COMMAND_FORM_TYPE,
                &[("waddle#composer_execute", value)],
            )],
        ))
        .composer_execute
    };
    assert!(parse("true"));
    assert!(parse("TRUE"));
    assert!(!parse("1"));
    assert!(!parse("false"));
}

#[test]
fn descriptor_defaults_to_global_scope_without_metadata() {
    let descriptor = extension_command_descriptor(
        ExtensionCommandItemRef {
            service_jid: "extensions.waddle.test".to_string(),
            node: "urn:waddle:extension:1:github".to_string(),
            name: "GitHub".to_string(),
        },
        ExtensionCommandMetadata::default(),
    );
    assert_eq!(descriptor.scope, ExtensionCommandScope::Global);
    assert_eq!(descriptor.composer_prefix, None);
    assert!(!descriptor.composer_execute);
}

// ── Invoke / submit wire shapes ──────────────────────────────────────

fn command_child(iq: &Element) -> &Element {
    iq.get_child("command", NS_COMMANDS).expect("command child")
}

fn form_field_values(form: &Element, var: &str) -> Vec<String> {
    form.children()
        .filter(|child| child.name() == "field" && child.attr("var") == Some(var))
        .flat_map(|field| {
            field
                .children()
                .filter(|child| child.name() == "value")
                .map(Element::text)
        })
        .collect()
}

#[test]
fn invoke_iq_without_room_carries_no_form() {
    let iq = build_extension_invoke_iq(
        "extensions.waddle.test",
        "urn:waddle:extension:1:stargate-quotes",
        None,
    );
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), Some("extensions.waddle.test"));
    let command = command_child(&iq);
    assert_eq!(
        command.attr("node"),
        Some("urn:waddle:extension:1:stargate-quotes"),
    );
    assert_eq!(command.attr("action"), Some("execute"));
    assert_eq!(command.attr("sessionid"), None);
    assert!(command.get_child("x", NS_DATA_FORMS).is_none());
}

#[test]
fn invoke_iq_with_room_submits_the_room_jid_field_without_form_type() {
    let iq = build_extension_invoke_iq(
        "extensions.waddle.test",
        "urn:waddle:extension:1:decision-polls",
        Some("general@muc.waddle.test"),
    );
    let form = command_child(&iq)
        .get_child("x", NS_DATA_FORMS)
        .expect("submit form");
    assert_eq!(form.attr("type"), Some("submit"));
    assert_eq!(
        form_field_values(form, ROOM_JID_FIELD),
        vec!["general@muc.waddle.test".to_string()],
    );
    assert!(form_field_values(form, "FORM_TYPE").is_empty());
}

#[test]
fn submit_iq_threads_the_session_and_appends_the_room_jid() {
    let iq = build_extension_submit_iq(
        "extensions.waddle.test",
        "urn:waddle:extension:1:decision-polls",
        Some("session-1"),
        AdHocAction::Complete,
        &[ExtensionSubmitField {
            var: "question".to_string(),
            values: vec!["Lunch?".to_string()],
        }],
        Some("general@muc.waddle.test"),
    );
    let command = command_child(&iq);
    assert_eq!(command.attr("sessionid"), Some("session-1"));
    assert_eq!(command.attr("action"), Some("complete"));
    let form = command.get_child("x", NS_DATA_FORMS).expect("submit form");
    assert_eq!(
        form_field_values(form, "question"),
        vec!["Lunch?".to_string()],
    );
    assert_eq!(
        form_field_values(form, ROOM_JID_FIELD),
        vec!["general@muc.waddle.test".to_string()],
    );
}

#[test]
fn submit_iq_never_duplicates_an_explicit_room_jid_field() {
    let iq = build_extension_submit_iq(
        "extensions.waddle.test",
        "urn:waddle:extension:1:decision-polls",
        Some("session-1"),
        AdHocAction::Complete,
        &[ExtensionSubmitField {
            var: ROOM_JID_FIELD.to_string(),
            values: vec!["general@muc.waddle.test".to_string()],
        }],
        Some("general@muc.waddle.test"),
    );
    let form = command_child(&iq)
        .get_child("x", NS_DATA_FORMS)
        .expect("submit form");
    assert_eq!(
        form_field_values(form, ROOM_JID_FIELD),
        vec!["general@muc.waddle.test".to_string()],
    );
}

#[test]
fn submit_iq_multi_value_fields_serialize_every_value() {
    let iq = build_extension_submit_iq(
        "extensions.waddle.test",
        "urn:waddle:extension:1:decision-polls",
        Some("session-1"),
        AdHocAction::Complete,
        &[ExtensionSubmitField {
            var: "options".to_string(),
            values: vec!["Pizza".to_string(), "Sushi".to_string()],
        }],
        None,
    );
    let form = command_child(&iq)
        .get_child("x", NS_DATA_FORMS)
        .expect("submit form");
    assert_eq!(
        form_field_values(form, "options"),
        vec!["Pizza".to_string(), "Sushi".to_string()],
    );
}

#[test]
fn cancel_and_prev_strip_the_form_entirely() {
    for action in [AdHocAction::Cancel, AdHocAction::Prev] {
        let iq = build_extension_submit_iq(
            "extensions.waddle.test",
            "urn:waddle:extension:1:decision-polls",
            Some("session-1"),
            action,
            &[ExtensionSubmitField {
                var: "question".to_string(),
                values: vec!["Lunch?".to_string()],
            }],
            Some("general@muc.waddle.test"),
        );
        assert!(command_child(&iq).get_child("x", NS_DATA_FORMS).is_none());
    }
}

// ── Result parsing ───────────────────────────────────────────────────

#[test]
fn result_parsing_extracts_status_session_notes_and_form() {
    let iq = command_response(
        "<command xmlns='http://jabber.org/protocol/commands' \
         node='urn:waddle:extension:1:decision-polls' sessionid='s-1' status='executing'>\
         <actions execute='complete'><complete/></actions>\
         <note type='info'>Pick your options.</note>\
         <x xmlns='jabber:x:data' type='form'>\
         <title>New Poll</title>\
         <instructions>Fill in the poll.</instructions>\
         <field var='question' type='text-single' label='Question'><required/></field>\
         <field var='visibility' type='list-single' label='Visibility'>\
         <value>channel</value>\
         <option label='Channel'><value>channel</value></option>\
         <option><value>private</value></option>\
         </field>\
         <field var='anonymous' type='boolean'><value>0</value></field>\
         </x>\
         </command>",
    );
    let result = parse_extension_command_result(&iq).expect("parses");
    assert_eq!(result.status, AdHocStatus::Executing);
    assert_eq!(result.session_id.as_deref(), Some("s-1"));
    assert_eq!(
        result.actions,
        vec![AdHocAction::Complete, AdHocAction::Cancel],
    );
    assert_eq!(
        result.notes,
        vec![ExtensionCommandNote {
            note_type: ExtensionNoteType::Info,
            value: "Pick your options.".to_string(),
        }],
    );
    let form = result.form.expect("form");
    assert_eq!(form.title.as_deref(), Some("New Poll"));
    assert_eq!(form.instructions.as_deref(), Some("Fill in the poll."));
    assert_eq!(form.fields.len(), 3);
    let question = &form.fields[0];
    assert_eq!(question.var, "question");
    assert_eq!(question.field_type, ExtensionFieldType::TextSingle);
    assert!(question.required);
    let visibility = &form.fields[1];
    assert_eq!(visibility.field_type, ExtensionFieldType::ListSingle);
    assert_eq!(visibility.values, vec!["channel".to_string()]);
    assert_eq!(
        visibility.options,
        vec![
            ExtensionFormOption {
                label: Some("Channel".to_string()),
                value: "channel".to_string(),
            },
            ExtensionFormOption {
                label: None,
                value: "private".to_string(),
            },
        ],
    );
    assert_eq!(form.fields[2].field_type, ExtensionFieldType::Boolean);
    assert!(form.fields.iter().all(|field| !field.blocked));
}

#[test]
fn result_parsing_defaults_valueless_booleans_to_false() {
    let iq = command_response(
        "<command xmlns='http://jabber.org/protocol/commands' \
         node='urn:waddle:extension:1:consent' sessionid='s-1' status='executing'>\
         <x xmlns='jabber:x:data' type='form'>\
         <field var='consent' type='boolean'><required/></field>\
         </x>\
         </command>",
    );
    let result = parse_extension_command_result(&iq).expect("parses");
    let form = result.form.expect("form");
    // Wasm-client parity: required booleans submit `false` untouched
    // instead of tripping required-field gating.
    assert_eq!(form.fields[0].values, vec!["0".to_string()]);
    assert!(form.fields[0].required);
}

#[test]
fn result_parsing_blocks_text_private_and_secret_named_fields() {
    let iq = command_response(
        "<command xmlns='http://jabber.org/protocol/commands' \
         node='urn:waddle:extension:1:hostile' sessionid='s-1' status='executing'>\
         <x xmlns='jabber:x:data' type='form'>\
         <field var='passphrase' type='text-private' label='Passphrase'/>\
         <field var='payload#api_key' type='text-single' label='API key'/>\
         <field var='bot-token' type='text-single'/>\
         <field var='Secret' type='text-single'/>\
         <field var='prompt' type='text-single'/>\
         <field var='tokenizer' type='text-single'/>\
         </x>\
         </command>",
    );
    let result = parse_extension_command_result(&iq).expect("parses");
    let form = result.form.expect("form");
    let blocked: Vec<&str> = form
        .fields
        .iter()
        .filter(|field| field.blocked)
        .map(|field| field.var.as_str())
        .collect();
    assert_eq!(
        blocked,
        vec!["passphrase", "payload#api_key", "bot-token", "Secret"]
    );
}

#[test]
fn result_parsing_defaults_note_type_to_info_and_maps_severities() {
    let iq = command_response(
        "<command xmlns='http://jabber.org/protocol/commands' node='n' status='completed'>\
         <note>Done.</note>\
         <note type='warn'>Careful.</note>\
         <note type='error'>Broken.</note>\
         <note type='info'>   </note>\
         </command>",
    );
    let result = parse_extension_command_result(&iq).expect("parses");
    assert_eq!(result.status, AdHocStatus::Completed);
    assert_eq!(
        result
            .notes
            .iter()
            .map(|note| note.note_type)
            .collect::<Vec<_>>(),
        vec![
            ExtensionNoteType::Info,
            ExtensionNoteType::Warn,
            ExtensionNoteType::Error,
        ],
    );
}

#[test]
fn executing_without_actions_implies_complete_and_cancel() {
    let iq = command_response(
        "<command xmlns='http://jabber.org/protocol/commands' node='n' sessionid='s-1' \
         status='executing'/>",
    );
    let result = parse_extension_command_result(&iq).expect("parses");
    assert_eq!(
        result.actions,
        vec![AdHocAction::Complete, AdHocAction::Cancel],
    );
    assert!(result.form.is_none());
    assert!(result.notes.is_empty());
}

#[test]
fn executing_with_actions_keeps_only_the_advertised_set_plus_cancel() {
    let iq = command_response(
        "<command xmlns='http://jabber.org/protocol/commands' node='n' sessionid='s-1' \
         status='executing'><actions execute='next'><next/><prev/></actions></command>",
    );
    let result = parse_extension_command_result(&iq).expect("parses");
    assert_eq!(
        result.actions,
        vec![AdHocAction::Next, AdHocAction::Prev, AdHocAction::Cancel],
    );
}

#[test]
fn completed_result_has_no_implied_actions() {
    let iq = command_response(
        "<command xmlns='http://jabber.org/protocol/commands' node='n' status='completed'/>",
    );
    let result = parse_extension_command_result(&iq).expect("parses");
    assert!(result.actions.is_empty());
}

#[test]
fn result_parsing_rejects_missing_command_and_unknown_status() {
    let no_command = command_response("");
    assert_eq!(
        parse_extension_command_result(&no_command),
        Err(ExtensionResponseError::MissingCommand),
    );
    let bogus_status = command_response(
        "<command xmlns='http://jabber.org/protocol/commands' node='n' status='bogus'/>",
    );
    assert_eq!(
        parse_extension_command_result(&bogus_status),
        Err(ExtensionResponseError::InvalidStatus),
    );
}
