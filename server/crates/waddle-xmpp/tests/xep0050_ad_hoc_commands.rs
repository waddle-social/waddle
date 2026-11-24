//! XEP-0050: Ad-Hoc Commands — dedicated conformance suite.
//!
//! The in-crate `xep::xep0050::tests` module covers Action / Status
//! / Note / AllowedActions / Command parse-build round-trips. This
//! file pins the audit-level invariants at the public-API
//! boundary:
//!
//! - §"Protocol Namespace" namespace string,
//! - §2 well-known disco#items node identifier,
//! - §"Discovering Support" disco advertisement on `server_features()`,
//! - §3 command IQ classifier — only `iq/type='set'` with a
//!   namespaced `<command>` payload counts as a request,
//! - §3.5 status / action / note-type enums round-trip on the
//!   wire (`status="executing"` etc must be the literal spec
//!   strings),
//! - the `notes` collection accepts the three §3.5.1 note types
//!   (info / warn / error).

use minidom::Element;
use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::xep::xep0004::{FromElement, ToElement};
use waddle_xmpp::xep::xep0050::{
    is_command_request, parse_command_from_iq, Action as CommandAction,
    AllowedActions as CommandAllowedActions, Command, Note as CommandNote,
    NoteType as CommandNoteType, Status as CommandStatus, NODE_COMMANDS, NS_COMMANDS,
};
use xmpp_parsers::iq::Iq;

// ── §"Protocol Namespace" + §2 node identifier ──────────────────────

#[test]
fn xep0050_namespace_and_node_match_spec() {
    // XEP-0050 §"Protocol Namespace" and §2 both pin the URI
    // `http://jabber.org/protocol/commands` — used for both the
    // disco feature AND the disco#items node where commands are
    // enumerated. The two happen to coincide; pinning both
    // separately so a future split is a deliberate decision.
    assert_eq!(NS_COMMANDS, "http://jabber.org/protocol/commands");
    assert_eq!(NODE_COMMANDS, "http://jabber.org/protocol/commands");
}

// ── §"Discovering Support" advertisement ────────────────────────────

#[test]
fn xep0050_server_features_advertise_commands() {
    // §"Discovering Support": a server that supports ad-hoc
    // commands MUST advertise `http://jabber.org/protocol/commands`
    // in disco#info. Waddle's `CommandRegistry` dispatches
    // commands at the server domain; the advert is mandatory.
    let feats = server_features();
    assert!(
        feats.iter().any(|f| f == &Feature::new(NS_COMMANDS)),
        "server_features() must advertise `http://jabber.org/protocol/commands`"
    );
}

// ── §3 IQ classifier ────────────────────────────────────────────────

fn command_element(node: &str) -> Element {
    Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .build()
}

#[test]
fn xep0050_classifier_accepts_set_with_namespaced_command() {
    // §3: client → server command request is an IQ-set with
    // `<command xmlns='http://jabber.org/protocol/commands' node='…' action='…'/>`.
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "c-1".into(),
        payload: command_element("admin:wipe-room"),
    };
    assert!(is_command_request(&iq));
}

#[test]
fn xep0050_classifier_rejects_get_iq_type() {
    // A `get` carrying the same payload isn't a command request;
    // misclassifying it would let an attacker probe command
    // semantics via a request the spec doesn't define a response
    // for.
    let iq = Iq::Get {
        from: None,
        to: None,
        id: "c-2".into(),
        payload: command_element("anything"),
    };
    assert!(!is_command_request(&iq));
}

#[test]
fn xep0050_classifier_rejects_wrong_namespace_payload() {
    // A `<command>` in some other namespace isn't a XEP-0050
    // request. Accepting it would let foreign-ns payloads reach
    // the command dispatcher.
    let wrong_ns = Element::builder("command", "wrong:ns")
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), "anything")
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "c-3".into(),
        payload: wrong_ns,
    };
    assert!(!is_command_request(&iq));
}

#[test]
fn xep0050_parse_command_rejects_missing_node_attribute() {
    // §3 makes `node=` REQUIRED on a command request. Without it
    // the dispatcher has no key to route on; the parser MUST
    // surface this as an error rather than route an empty-node
    // request.
    let no_node = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "c-4".into(),
        payload: no_node,
    };
    assert!(parse_command_from_iq(&iq).is_err());
}

#[test]
fn xep0050_parse_command_round_trips_node_and_action() {
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "c-5".into(),
        payload: command_element("space:create"),
    };
    let parsed = parse_command_from_iq(&iq).expect("valid command request");
    assert_eq!(parsed.node, "space:create");
    assert_eq!(parsed.action, Some(CommandAction::Execute));
}

// ── §3.5 status / action / note-type spec strings ───────────────────

#[test]
fn xep0050_status_enum_serialises_to_spec_strings() {
    // §3.5: the three command states use these literal strings
    // on the wire. Clients dispatch on the textual form; a typo
    // here drops state transitions on the floor.
    assert_eq!(CommandStatus::Executing.as_str(), "executing");
    assert_eq!(CommandStatus::Completed.as_str(), "completed");
    assert_eq!(CommandStatus::Canceled.as_str(), "canceled");
}

#[test]
fn xep0050_action_enum_serialises_to_spec_strings() {
    // §"Action Semantics" enumerates execute/cancel/prev/next/complete.
    // The dispatcher's state machine relies on the exact string
    // mapping.
    assert_eq!(CommandAction::Execute.as_str(), "execute");
    assert_eq!(CommandAction::Cancel.as_str(), "cancel");
    assert_eq!(CommandAction::Prev.as_str(), "prev");
    assert_eq!(CommandAction::Next.as_str(), "next");
    assert_eq!(CommandAction::Complete.as_str(), "complete");
}

#[test]
fn xep0050_note_type_default_is_info() {
    // §3.5.1: when `<note type=…/>` is absent the receiver MUST
    // treat the note as informational. The typed enum's default
    // reflects that fallback.
    assert_eq!(CommandNoteType::default(), CommandNoteType::Info);
    assert_eq!(CommandNote::info("hello").note_type, CommandNoteType::Info);
    assert_eq!(
        CommandNote::warn("careful").note_type,
        CommandNoteType::Warn
    );
    assert_eq!(CommandNote::error("boom").note_type, CommandNoteType::Error);
}

#[test]
fn xep0050_allowed_actions_round_trip() {
    // §"Action Semantics": the `<actions/>` element on a
    // `status="executing"` response declares which of
    // prev/next/complete the client can take. `execute` is
    // implicit (any executing response always permits restart).
    // Round-trip pinning so any future change to the wire shape
    // is a deliberate decision.
    let actions = CommandAllowedActions::new(CommandAction::Next)
        .with_next()
        .with_complete();
    let elem = actions.to_element();
    let round_tripped = CommandAllowedActions::from_element(&elem).expect("parses back");
    assert!(round_tripped.next);
    assert!(round_tripped.complete);
    assert!(!round_tripped.prev);
    assert_eq!(round_tripped.execute_default, CommandAction::Next);
}

// ── §3 status round-trip on full Command ────────────────────────────

#[test]
fn xep0050_command_round_trip_preserves_all_fields() {
    // Build a fully-fleshed command response (executing, with
    // session id + notes), serialise to XML, parse back, and
    // verify every field survived. This is the wire-shape
    // contract for the multi-step state machine.
    let command = Command::new("admin:wipe-room")
        .with_status(CommandStatus::Executing)
        .with_session_id("sess-1")
        .with_note(CommandNote::info("Step 1 of 2"))
        .with_note(CommandNote::warn("This is destructive"));

    let elem = command.to_element();
    let parsed = Command::from_element(&elem).expect("round-trips");
    assert_eq!(parsed.node, "admin:wipe-room");
    assert_eq!(parsed.status, Some(CommandStatus::Executing));
    assert_eq!(parsed.session_id.as_deref(), Some("sess-1"));
    assert_eq!(parsed.notes.len(), 2);
    assert_eq!(parsed.notes[0].note_type, CommandNoteType::Info);
    assert_eq!(parsed.notes[0].text, "Step 1 of 2");
    assert_eq!(parsed.notes[1].note_type, CommandNoteType::Warn);
}

// ── §2.4 request parsing, execute resolution, sessions, lang ────────

mod session_flow {
    use std::sync::{Arc, Mutex};

    use minidom::Element;
    use waddle_xmpp::commands::{CommandContext, CommandRegistry, CommandResult};
    use waddle_xmpp::xep::xep0004::{DataForm, FormType, ToElement};
    use waddle_xmpp::xep::xep0050::{
        Action, AdHocCommandCondition, AllowedActions, Command, NS_COMMANDS,
    };
    use waddle_xmpp::XmppError;
    use xmpp_parsers::iq::Iq;

    const NODE: &str = "test:wizard";

    fn context(command: Command) -> CommandContext {
        CommandContext {
            from: "user@localhost".parse().expect("jid"),
            authenticated_user_id: Some("user".to_string()),
            iq: Iq::Set {
                from: None,
                to: None,
                id: "iq".to_string(),
                payload: Element::builder("query", "urn:test").build(),
            },
            command,
        }
    }

    fn request(xml: &str) -> Command {
        let iq = Iq::Set {
            from: None,
            to: None,
            id: "iq".into(),
            payload: xml.parse().expect("xml"),
        };
        waddle_xmpp::xep::xep0050::parse_command_from_iq(&iq).expect("request parses")
    }

    /// Two-stage wizard: stage 1 advertises `execute='complete'`; stage 2
    /// records the action and form type the handler saw, then completes
    /// unless the form is empty (then a correctable bad-payload).
    async fn wizard() -> (CommandRegistry, Arc<Mutex<Vec<(Option<Action>, FormType)>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let registry = CommandRegistry::new();
        let log = Arc::clone(&seen);
        registry
            .register(NODE, "Wizard", move |ctx: CommandContext| {
                let log = Arc::clone(&log);
                async move {
                    let Some(form) = ctx.command.form.as_ref() else {
                        return CommandResult::Executing {
                            form: DataForm::new(FormType::Form),
                            session_id: ctx.command.session_id.unwrap_or_default(),
                            notes: vec![],
                            actions: Some(AllowedActions::new(Action::Complete).with_complete()),
                        };
                    };
                    log.lock()
                        .expect("lock")
                        .push((ctx.command.action, form.form_type));
                    if form.fields.is_empty() {
                        return CommandResult::Error(XmppError::ad_hoc_command(
                            AdHocCommandCondition::BadPayload,
                            None,
                        ));
                    }
                    CommandResult::Completed {
                        session_id: None,
                        form: None,
                        notes: vec![],
                    }
                }
            })
            .await;
        (registry, seen)
    }

    async fn start(registry: &CommandRegistry) -> String {
        match registry
            .dispatch(context(Command::new(NODE).with_action(Action::Execute)))
            .await
        {
            CommandResult::Executing { session_id, .. } => session_id,
            _ => panic!("expected executing"),
        }
    }

    fn filled(form_type: FormType) -> DataForm {
        DataForm::new(form_type).add_field(waddle_xmpp::xep::xep0004::Field::text_single("a", "b"))
    }

    #[tokio::test]
    async fn execute_on_continuing_session_resolves_to_advertised_execute_action() {
        // §2.4: action 'execute' is equivalent to the `execute` attribute of
        // the advertised <actions/> ('complete' here).
        let (registry, seen) = wizard().await;
        let session = start(&registry).await;
        let mut cmd = Command::new(NODE)
            .with_action(Action::Execute)
            .with_session_id(&session);
        cmd.form = Some(filled(FormType::Submit));
        let result = registry.dispatch(context(cmd)).await;
        assert!(matches!(result, CommandResult::Completed { .. }));
        assert_eq!(seen.lock().expect("lock")[0].0, Some(Action::Complete));
        assert!(registry.get_session(&session).await.is_none());
    }

    #[tokio::test]
    async fn absent_action_on_continuing_session_means_execute() {
        let (registry, seen) = wizard().await;
        let session = start(&registry).await;
        let mut cmd = Command::new(NODE).with_session_id(&session);
        cmd.form = Some(filled(FormType::Submit));
        let result = registry.dispatch(context(cmd)).await;
        assert!(matches!(result, CommandResult::Completed { .. }));
        assert_eq!(seen.lock().expect("lock")[0].0, Some(Action::Complete));
    }

    #[tokio::test]
    async fn execute_without_advertised_actions_defaults_to_next() {
        let registry = CommandRegistry::new();
        let seen = Arc::new(Mutex::new(None));
        let log = Arc::clone(&seen);
        registry
            .register(NODE, "Plain", move |ctx: CommandContext| {
                let log = Arc::clone(&log);
                async move {
                    if ctx.command.session_id.is_some()
                        && ctx.command.action != Some(Action::Execute)
                    {
                        *log.lock().expect("lock") = ctx.command.action;
                    }
                    CommandResult::Executing {
                        form: DataForm::new(FormType::Form),
                        session_id: ctx.command.session_id.unwrap_or_default(),
                        notes: vec![],
                        actions: None,
                    }
                }
            })
            .await;
        let session = start(&registry).await;
        registry
            .dispatch(context(
                Command::new(NODE)
                    .with_action(Action::Execute)
                    .with_session_id(session),
            ))
            .await;
        assert_eq!(*seen.lock().expect("lock"), Some(Action::Next));
    }

    #[tokio::test]
    async fn execute_resolving_to_a_disallowed_action_is_bad_action() {
        let registry = CommandRegistry::new();
        registry
            .register(NODE, "Odd", |ctx: CommandContext| async move {
                CommandResult::Executing {
                    form: DataForm::new(FormType::Form),
                    session_id: ctx.command.session_id.unwrap_or_default(),
                    notes: vec![],
                    // default `complete` but `complete` is not offered
                    actions: Some(AllowedActions::new(Action::Complete).with_next()),
                }
            })
            .await;
        let session = start(&registry).await;
        let result = registry
            .dispatch(context(
                Command::new(NODE)
                    .with_action(Action::Execute)
                    .with_session_id(session),
            ))
            .await;
        match result {
            CommandResult::Error(XmppError::AdHocCommand { condition, .. }) => {
                assert_eq!(condition, AdHocCommandCondition::BadAction)
            }
            _ => panic!("expected bad-action"),
        }
    }

    #[tokio::test]
    async fn correctable_error_keeps_session_for_resubmission() {
        // §2.4: a session ends on completion or cancel; a bad-payload is
        // correctable with the same sessionid.
        let (registry, _) = wizard().await;
        let session = start(&registry).await;
        let mut bad = Command::new(NODE)
            .with_action(Action::Complete)
            .with_session_id(&session);
        bad.form = Some(DataForm::new(FormType::Submit));
        match registry.dispatch(context(bad)).await {
            CommandResult::Error(XmppError::AdHocCommand { condition, .. }) => {
                assert_eq!(condition, AdHocCommandCondition::BadPayload)
            }
            _ => panic!("expected bad-payload"),
        }
        assert!(registry.get_session(&session).await.is_some());

        let mut good = Command::new(NODE)
            .with_action(Action::Complete)
            .with_session_id(&session);
        good.form = Some(filled(FormType::Submit));
        assert!(matches!(
            registry.dispatch(context(good)).await,
            CommandResult::Completed { .. }
        ));
        assert!(registry.get_session(&session).await.is_none());
    }

    #[tokio::test]
    async fn non_correctable_error_ends_session() {
        let registry = CommandRegistry::new();
        registry
            .register(NODE, "Deny", |ctx: CommandContext| async move {
                if ctx.command.action == Some(Action::Execute) {
                    CommandResult::Executing {
                        form: DataForm::new(FormType::Form),
                        session_id: ctx.command.session_id.unwrap_or_default(),
                        notes: vec![],
                        actions: Some(AllowedActions::new(Action::Complete).with_complete()),
                    }
                } else {
                    CommandResult::Error(XmppError::forbidden(None))
                }
            })
            .await;
        let session = start(&registry).await;
        let result = registry
            .dispatch(context(
                Command::new(NODE)
                    .with_action(Action::Complete)
                    .with_session_id(&session),
            ))
            .await;
        assert!(matches!(result, CommandResult::Error(_)));
        assert!(registry.get_session(&session).await.is_none());
    }

    #[tokio::test]
    async fn cancel_form_is_treated_as_submit() {
        // §2.4: "Responders SHOULD consider any <x type='cancel'/> to be
        // <x type='submit'/>."
        let (registry, seen) = wizard().await;
        let session = start(&registry).await;
        let mut cmd = Command::new(NODE)
            .with_action(Action::Complete)
            .with_session_id(&session);
        cmd.form = Some(filled(FormType::Cancel));
        let result = registry.dispatch(context(cmd)).await;
        assert!(matches!(result, CommandResult::Completed { .. }));
        assert_eq!(seen.lock().expect("lock")[0].1, FormType::Submit);
    }

    #[test]
    fn request_parser_ignores_requester_status_actions_and_notes() {
        // §2.4: "If specified by the requester, the responder MUST ignore it."
        for status in ["execute", "bogus", "completed"] {
            let cmd = request(&format!(
                "<command xmlns='{NS_COMMANDS}' node='n' status='{status}'>\
                 <actions execute='bogus'><next/></actions><note type='nonsense'>x</note>\
                 </command>"
            ));
            assert_eq!(cmd.node, "n");
            assert_eq!(cmd.status, None);
            assert!(cmd.actions.is_none());
            assert!(cmd.notes.is_empty());
        }
    }

    #[test]
    fn response_parser_stays_strict_about_status() {
        let elem: Element = format!("<command xmlns='{NS_COMMANDS}' node='n' status='bogus'/>")
            .parse()
            .expect("xml");
        assert!(<Command as waddle_xmpp::xep::xep0004::FromElement>::from_element(&elem).is_err());
    }

    #[test]
    fn command_lang_round_trips_as_xml_lang() {
        // §2.4: "The responder SHOULD specify the language/locale with the
        // every command session's response."
        let elem = Command::new("n").with_lang("en").to_element();
        assert_eq!(
            elem.attr_ns(&minidom::rxml::Namespace::XML, "lang"),
            Some("en")
        );
        assert_eq!(request(&String::from(&elem)).lang.as_deref(), Some("en"));
    }
}
