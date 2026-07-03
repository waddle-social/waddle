use super::*;
use crate::xep::xep0004::{DataForm, Field, FormType, FromElement, ToElement};
use minidom::Element;
use xmpp_parsers::iq::Iq;

// --- Action ---

#[test]
fn test_action_from_str() {
    assert_eq!("execute".parse::<Action>().ok(), Some(Action::Execute));
    assert_eq!("next".parse::<Action>().ok(), Some(Action::Next));
    assert_eq!("prev".parse::<Action>().ok(), Some(Action::Prev));
    assert_eq!("complete".parse::<Action>().ok(), Some(Action::Complete));
    assert_eq!("cancel".parse::<Action>().ok(), Some(Action::Cancel));
    assert!("invalid".parse::<Action>().is_err());
}

#[test]
fn test_action_as_str() {
    assert_eq!(Action::Execute.as_str(), "execute");
    assert_eq!(Action::Next.as_str(), "next");
    assert_eq!(Action::Prev.as_str(), "prev");
    assert_eq!(Action::Complete.as_str(), "complete");
    assert_eq!(Action::Cancel.as_str(), "cancel");
}

#[test]
fn test_action_display() {
    assert_eq!(format!("{}", Action::Execute), "execute");
}

// --- Status ---

#[test]
fn test_status_from_str() {
    assert_eq!("executing".parse::<Status>().ok(), Some(Status::Executing));
    assert_eq!("completed".parse::<Status>().ok(), Some(Status::Completed));
    assert_eq!("canceled".parse::<Status>().ok(), Some(Status::Canceled));
    assert!("bogus".parse::<Status>().is_err());
}

#[test]
fn test_status_as_str() {
    assert_eq!(Status::Executing.as_str(), "executing");
    assert_eq!(Status::Completed.as_str(), "completed");
    assert_eq!(Status::Canceled.as_str(), "canceled");
}

// --- NoteType ---

#[test]
fn test_note_type_from_str() {
    assert_eq!("info".parse::<NoteType>().ok(), Some(NoteType::Info));
    assert_eq!("warn".parse::<NoteType>().ok(), Some(NoteType::Warn));
    assert_eq!("error".parse::<NoteType>().ok(), Some(NoteType::Error));
    assert!("unknown".parse::<NoteType>().is_err());
}

#[test]
fn test_note_type_default() {
    assert_eq!(NoteType::default(), NoteType::Info);
}

// --- Note ---

#[test]
fn test_note_constructors() {
    let n = Note::info("hello");
    assert_eq!(n.note_type, NoteType::Info);
    assert_eq!(n.text, "hello");

    let n = Note::warn("careful");
    assert_eq!(n.note_type, NoteType::Warn);

    let n = Note::error("oops");
    assert_eq!(n.note_type, NoteType::Error);
}

#[test]
fn test_note_roundtrip() {
    let note = Note::warn("be careful");
    let elem = note.to_element();
    let parsed = Note::from_element(&elem).expect("parse note");
    assert_eq!(parsed.note_type, NoteType::Warn);
    assert_eq!(parsed.text, "be careful");
}

#[test]
fn test_note_default_type_when_absent() {
    let elem = Element::builder("note", NS_COMMANDS).build();
    let parsed = Note::from_element(&elem).expect("parse note");
    assert_eq!(parsed.note_type, NoteType::Info);
}

// --- AllowedActions ---

#[test]
fn test_allowed_actions_roundtrip() {
    let actions = AllowedActions::new(Action::Next)
        .with_prev()
        .with_next()
        .with_complete();

    let elem = actions.to_element();
    let parsed = AllowedActions::from_element(&elem).expect("parse actions");

    assert_eq!(parsed.execute_default, Action::Next);
    assert!(parsed.prev);
    assert!(parsed.next);
    assert!(parsed.complete);
}

#[test]
fn test_allowed_actions_minimal() {
    let actions = AllowedActions::new(Action::Complete);
    let elem = actions.to_element();
    let parsed = AllowedActions::from_element(&elem).expect("parse actions");

    assert_eq!(parsed.execute_default, Action::Complete);
    assert!(!parsed.prev);
    assert!(!parsed.next);
    assert!(!parsed.complete);
}

#[test]
fn test_allowed_actions_default_execute() {
    // No execute attribute => defaults to Execute
    let elem = Element::builder("actions", NS_COMMANDS)
        .append(Element::builder("next", NS_COMMANDS).build())
        .build();
    let parsed = AllowedActions::from_element(&elem).expect("parse actions");
    assert_eq!(parsed.execute_default, Action::Execute);
    assert!(parsed.next);
}

// --- Command ---

#[test]
fn test_command_builder() {
    let cmd = Command::new("http://example.com/cmd")
        .with_session_id("sess-1")
        .with_action(Action::Execute)
        .with_status(Status::Executing)
        .with_note(Note::info("Step 1"))
        .with_actions(AllowedActions::new(Action::Complete).with_complete())
        .with_form(DataForm::new(FormType::Form).add_field(Field::text_single("name", "")));

    assert_eq!(cmd.node, "http://example.com/cmd");
    assert_eq!(cmd.session_id.as_deref(), Some("sess-1"));
    assert_eq!(cmd.action, Some(Action::Execute));
    assert_eq!(cmd.status, Some(Status::Executing));
    assert_eq!(cmd.notes.len(), 1);
    assert!(cmd.actions.is_some());
    assert!(cmd.form.is_some());
}

#[test]
fn test_command_roundtrip_minimal() {
    let cmd = Command::new("test-node").with_action(Action::Execute);
    let elem = cmd.to_element();
    let parsed = Command::from_element(&elem).expect("parse command");

    assert_eq!(parsed.node, "test-node");
    assert_eq!(parsed.action, Some(Action::Execute));
    assert!(parsed.session_id.is_none());
    assert!(parsed.status.is_none());
    assert!(parsed.actions.is_none());
    assert!(parsed.notes.is_empty());
    assert!(parsed.form.is_none());
}

#[test]
fn test_command_roundtrip_full() {
    let cmd = Command::new("my-command")
        .with_session_id("abc-123")
        .with_status(Status::Executing)
        .with_actions(AllowedActions::new(Action::Next).with_next().with_prev())
        .with_note(Note::info("Please fill this form"))
        .with_form(DataForm::new(FormType::Form).add_field(Field::text_single("username", "")));

    let elem = cmd.to_element();
    let parsed = Command::from_element(&elem).expect("parse command");

    assert_eq!(parsed.node, "my-command");
    assert_eq!(parsed.session_id.as_deref(), Some("abc-123"));
    assert_eq!(parsed.status, Some(Status::Executing));
    assert!(parsed.actions.is_some());
    let actions = parsed.actions.as_ref().expect("actions");
    assert_eq!(actions.execute_default, Action::Next);
    assert!(actions.next);
    assert!(actions.prev);
    assert!(!actions.complete);
    assert_eq!(parsed.notes.len(), 1);
    assert_eq!(parsed.notes[0].text, "Please fill this form");
    assert!(parsed.form.is_some());
}

#[test]
fn test_command_missing_node_error() {
    let elem = Element::builder("command", NS_COMMANDS).build();
    let result = Command::from_element(&elem);
    assert!(result.is_err());
}

#[test]
fn test_command_wrong_element_error() {
    let elem = Element::builder("query", "jabber:iq:last").build();
    let result = Command::from_element(&elem);
    assert!(matches!(result, Err(CommandError::NotACommand)));
}

// --- IQ helpers ---

#[test]
fn test_is_command_request() {
    let cmd_elem = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "test-cmd")
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .build();
    let iq = Iq::Set {
        from: Some("alice@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "cmd-1".to_string(),
        payload: cmd_elem,
    };

    assert!(is_command_request(&iq));
}

#[test]
fn test_is_command_request_false_for_get() {
    let cmd_elem = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "test-cmd")
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "cmd-2".to_string(),
        payload: cmd_elem,
    };

    assert!(!is_command_request(&iq));
}

#[test]
fn test_is_command_request_false_for_wrong_ns() {
    let elem = Element::builder("command", "wrong:ns")
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "test")
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "cmd-3".to_string(),
        payload: elem,
    };

    assert!(!is_command_request(&iq));
}

#[test]
fn test_parse_command_from_iq() {
    let cmd_elem = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "my-node")
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .build();
    let iq = Iq::Set {
        from: Some("alice@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "parse-1".to_string(),
        payload: cmd_elem,
    };

    let cmd = parse_command_from_iq(&iq).expect("parse command from IQ");
    assert_eq!(cmd.node, "my-node");
    assert_eq!(cmd.action, Some(Action::Execute));
}

#[test]
fn test_parse_command_from_iq_error_on_get() {
    let elem = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "x")
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "parse-2".to_string(),
        payload: elem,
    };

    assert!(parse_command_from_iq(&iq).is_err());
}

#[test]
fn test_build_command_result() {
    let cmd_elem = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "test")
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .build();
    let iq = Iq::Set {
        from: Some("alice@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "res-1".to_string(),
        payload: cmd_elem,
    };

    let response_cmd = Command::new("test")
        .with_session_id("sess-1")
        .with_status(Status::Completed)
        .with_note(Note::info("Done"));

    let result = build_command_result(&iq, &response_cmd);

    assert_eq!(result.id(), "res-1");
    assert_eq!(result.from(), iq.to());
    assert_eq!(result.to(), iq.from());
    match result {
        Iq::Result {
            payload: Some(elem),
            ..
        } => {
            assert_eq!(elem.name(), "command");
            assert_eq!(elem.ns(), NS_COMMANDS);
            assert_eq!(elem.attr("status"), Some("completed"));
            assert_eq!(elem.attr("sessionid"), Some("sess-1"));
        }
        _ => panic!("Expected Result with command payload"),
    }
}

// --- §4.4 command-specific error conditions ---

#[test]
fn ad_hoc_command_condition_element_names_match_xep0050_4_4() {
    use super::AdHocCommandCondition as C;
    assert_eq!(C::MalformedAction.element_name(), "malformed-action");
    assert_eq!(C::BadAction.element_name(), "bad-action");
    assert_eq!(C::BadLocale.element_name(), "bad-locale");
    assert_eq!(C::BadPayload.element_name(), "bad-payload");
    assert_eq!(C::BadSessionId.element_name(), "bad-sessionid");
    assert_eq!(C::SessionExpired.element_name(), "session-expired");
}

#[test]
fn ad_hoc_command_condition_stanza_mapping_matches_xep0050_4_4() {
    use super::AdHocCommandCondition as C;
    use crate::error::{StanzaErrorCondition, StanzaErrorType};

    // All §4.4 conditions are modify/bad-request except session-expired,
    // which the table maps to cancel/not-allowed.
    for cond in [
        C::MalformedAction,
        C::BadAction,
        C::BadLocale,
        C::BadPayload,
        C::BadSessionId,
    ] {
        assert_eq!(
            cond.stanza_error(),
            (StanzaErrorType::Modify, StanzaErrorCondition::BadRequest),
            "{cond:?} must map to modify/bad-request",
        );
    }
    assert_eq!(
        C::SessionExpired.stanza_error(),
        (StanzaErrorType::Cancel, StanzaErrorCondition::NotAllowed),
    );
}

// --- Disco helpers ---

#[test]
fn test_is_commands_disco_items() {
    use crate::disco::items::DISCO_ITEMS_NS;

    let query = Element::builder("query", DISCO_ITEMS_NS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NODE_COMMANDS)
        .build();
    let iq = Iq::Get {
        from: Some("alice@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "disco-items-1".to_string(),
        payload: query,
    };

    assert!(is_commands_disco_items(&iq));
}

#[test]
fn test_is_commands_disco_items_false_for_other_node() {
    use crate::disco::items::DISCO_ITEMS_NS;

    let query = Element::builder("query", DISCO_ITEMS_NS)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "some-other-node",
        )
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "disco-items-2".to_string(),
        payload: query,
    };

    assert!(!is_commands_disco_items(&iq));
}

#[test]
fn test_is_commands_disco_info() {
    use crate::disco::info::DISCO_INFO_NS;

    let query = Element::builder("query", DISCO_INFO_NS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NODE_COMMANDS)
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "disco-info-1".to_string(),
        payload: query,
    };

    assert!(is_commands_disco_info(&iq));
}

#[test]
fn test_is_command_node_disco_info() {
    use crate::disco::info::DISCO_INFO_NS;

    let query = Element::builder("query", DISCO_INFO_NS)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "my-command-node",
        )
        .build();
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "disco-info-2".to_string(),
        payload: query,
    };

    assert!(is_command_node_disco_info(&iq, "my-command-node"));
    assert!(!is_command_node_disco_info(&iq, "other-node"));
}

#[test]
fn test_build_command_items() {
    let query = Element::builder("query", "http://jabber.org/protocol/disco#items")
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NODE_COMMANDS)
        .build();
    let iq = Iq::Get {
        from: Some("alice@example.com".parse().unwrap()),
        to: Some("example.com".parse().unwrap()),
        id: "items-1".to_string(),
        payload: query,
    };

    let commands = vec![("cmd-1", "First Command"), ("cmd-2", "Second Command")];

    let result = build_command_items(&iq, &commands, "example.com");
    match result {
        Iq::Result {
            payload: Some(elem),
            ..
        } => {
            assert_eq!(elem.name(), "query");
            assert_eq!(elem.attr("node"), Some(NODE_COMMANDS));
            let items: Vec<_> = elem.children().collect();
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].attr("node"), Some("cmd-1"));
            assert_eq!(items[0].attr("name"), Some("First Command"));
            assert_eq!(items[0].attr("jid"), Some("example.com"));
            assert_eq!(items[1].attr("node"), Some("cmd-2"));
            assert_eq!(items[1].attr("name"), Some("Second Command"));
        }
        _ => panic!("Expected Result with query payload"),
    }
}

// --- Multi-step command scenario ---

#[test]
fn test_multi_step_command_flow() {
    // Step 1: Client sends execute
    let request = Command::new("change-password").with_action(Action::Execute);
    let request_elem = request.to_element();
    let parsed_request = Command::from_element(&request_elem).expect("parse request");
    assert_eq!(parsed_request.action, Some(Action::Execute));

    // Step 2: Server responds with form
    let response = Command::new("change-password")
        .with_session_id("session-abc")
        .with_status(Status::Executing)
        .with_actions(AllowedActions::new(Action::Complete).with_complete())
        .with_form(DataForm::new(FormType::Form).add_field(Field::text_single("new-password", "")));
    let response_elem = response.to_element();
    let parsed_response = Command::from_element(&response_elem).expect("parse response");
    assert_eq!(parsed_response.status, Some(Status::Executing));
    assert!(parsed_response.form.is_some());

    // Step 3: Client submits filled form
    let submit = Command::new("change-password")
        .with_session_id("session-abc")
        .with_action(Action::Complete)
        .with_form(
            DataForm::new(FormType::Submit).add_field(Field::text_single("new-password", "s3cret")),
        );
    let submit_elem = submit.to_element();
    let parsed_submit = Command::from_element(&submit_elem).expect("parse submit");
    assert_eq!(parsed_submit.action, Some(Action::Complete));
    assert_eq!(parsed_submit.session_id.as_deref(), Some("session-abc"));

    // Step 4: Server responds completed
    let completed = Command::new("change-password")
        .with_session_id("session-abc")
        .with_status(Status::Completed)
        .with_note(Note::info("Password changed successfully"));
    let completed_elem = completed.to_element();
    let parsed_completed = Command::from_element(&completed_elem).expect("parse completed");
    assert_eq!(parsed_completed.status, Some(Status::Completed));
    assert_eq!(parsed_completed.notes.len(), 1);
    assert_eq!(
        parsed_completed.notes[0].text,
        "Password changed successfully"
    );
}

// --- Cancel flow ---

#[test]
fn test_cancel_command_flow() {
    let cancel = Command::new("some-command")
        .with_session_id("sess-xyz")
        .with_action(Action::Cancel);
    let elem = cancel.to_element();
    let parsed = Command::from_element(&elem).expect("parse cancel");
    assert_eq!(parsed.action, Some(Action::Cancel));

    let canceled_response = Command::new("some-command")
        .with_session_id("sess-xyz")
        .with_status(Status::Canceled)
        .with_note(Note::info("Command canceled"));
    let resp_elem = canceled_response.to_element();
    let parsed_resp = Command::from_element(&resp_elem).expect("parse canceled response");
    assert_eq!(parsed_resp.status, Some(Status::Canceled));
}

// --- Multiple notes ---

#[test]
fn test_command_with_multiple_notes() {
    let cmd = Command::new("multi-note")
        .with_status(Status::Completed)
        .with_note(Note::info("Step completed"))
        .with_note(Note::warn("But check logs"));

    let elem = cmd.to_element();
    let parsed = Command::from_element(&elem).expect("parse");
    assert_eq!(parsed.notes.len(), 2);
    assert_eq!(parsed.notes[0].note_type, NoteType::Info);
    assert_eq!(parsed.notes[1].note_type, NoteType::Warn);
}
