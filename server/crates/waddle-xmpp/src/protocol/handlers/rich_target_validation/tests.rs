use super::*;
use crate::protocol::id_gen::FixedIdGenerator;
use crate::protocol::message_context::MessageContextEnv;
use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy};
use crate::xep::xep0308::build_replace_element;
use crate::xep::xep0424::build_retract_element;
use crate::xep::xep0461::{build_reply_element, ReplyReference};
use crate::Stanza;
use jid::FullJid;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

fn full(s: &str) -> FullJid {
    s.parse().expect("valid full jid")
}

fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
}

fn chat_with_body(from: &str, to: &str, body: &str) -> Message {
    let mut m = Message::new(Some(to.parse().expect("jid")));
    m.from = Some(from.parse().expect("jid"));
    m.type_ = MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    m
}

fn ctx_for<'a>(
    local: &'a FullJid,
    bl: &'a Blocklist,
    occ: &'a MucOccupancy,
    gen: &'a FixedIdGenerator,
    msg: &Message,
) -> MessageContext<'a> {
    let env = MessageContextEnv {
        domain: "example.com",
        full_jid: local,
        blocklist: bl,
        carbons: CarbonsState::Disabled,
        muc_occupancy: occ,
        has_live_transport: true,
        delivery_fanout: &[],
        id_gen: gen,
    };
    MessageContext::derive(env, msg)
}

fn run(local: &FullJid, msg: &mut Message) -> HandlerOutcome {
    let bl = Blocklist::empty();
    let occ = MucOccupancy::empty();
    let gen = FixedIdGenerator("test".to_string());
    let ctx = ctx_for(local, &bl, &occ, &gen, msg);
    RichTargetValidationHandler.handle(msg, &ctx)
}

fn archived_message_from(from: &str, body: &str) -> ArchivedMessage {
    let mut m = Message::new(Some("bob@example.com".parse().expect("jid")));
    m.from = Some(from.parse().expect("jid"));
    m.type_ = MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    let archive_jid: jid::Jid = bare("alice@example.com").into();
    ArchivedMessage {
        stanza_id: StanzaId::new("archive-A1", archive_jid),
        message: Box::new(m),
        tombstoned: false,
    }
}

fn extract_lookup_event(
    outcome: &HandlerOutcome,
) -> (&CallbackId, &BareJid, MamArchiveKind, &MessageRef) {
    let events = match outcome {
        HandlerOutcome::AwaitCallback(events) => events,
        other => panic!("expected AwaitCallback, got {other:?}"),
    };
    assert_eq!(events.len(), 1);
    match &events[0] {
        OutboundEvent::LookupArchivedMessage {
            id,
            archive,
            archive_kind,
            reference,
        } => (id, archive, *archive_kind, reference),
        other => panic!("expected LookupArchivedMessage, got {other:?}"),
    }
}

fn extract_error_payload(outcome: &HandlerOutcome) -> StanzaError {
    let events = match outcome {
        HandlerOutcome::Halt(events) => events,
        other => panic!("expected Halt, got {other:?}"),
    };
    let stanza = match &events[0] {
        OutboundEvent::SendStanza(s) => s,
        other => panic!("expected SendStanza, got {other:?}"),
    };
    let msg = match stanza.as_ref() {
        Stanza::Message(m) => m,
        other => panic!("expected Message, got {other:?}"),
    };
    let elem = msg
        .payloads
        .iter()
        .find(|p| p.name() == "error")
        .expect("error payload");
    StanzaError::try_from(elem.clone()).expect("typed parse")
}

// -----------------------------------------------------------------
// Detection — produces the typed lookup event with the right kind
// -----------------------------------------------------------------

#[test]
fn xep_0308_correction_emits_lookup_with_origin_id() {
    let local = full("alice@example.com/web");
    let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "fixed text");
    msg.payloads.push(build_replace_element("orig-msg-1"));

    let outcome = run(&local, &mut msg);
    let (_id, archive, archive_kind, reference) = extract_lookup_event(&outcome);
    assert_eq!(*archive, bare("alice@example.com"));
    assert_eq!(archive_kind, MamArchiveKind::Personal);
    match reference {
        MessageRef::OriginId { sender, origin_id } => {
            assert_eq!(*sender, bare("alice@example.com"));
            assert_eq!(origin_id.as_str(), "orig-msg-1");
        }
        other => panic!("expected OriginId ref, got {other:?}"),
    }
}

#[test]
fn xep_0308_malformed_correction_emits_bad_request() {
    let local = full("alice@example.com/web");
    let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "fixed text");
    msg.payloads.push(
        xmpp_parsers::message_correct::Replace {
            id: xmpp_parsers::message::Id(String::new()),
        }
        .into(),
    );

    let outcome = run(&local, &mut msg);
    let parsed = extract_error_payload(&outcome);
    assert_eq!(parsed.type_, ErrorType::Modify);
    assert_eq!(parsed.defined_condition, DefinedCondition::BadRequest);
}

#[test]
fn xep_0308_malformed_groupchat_correction_is_skipped_user_side() {
    let local = full("alice@example.com/web");
    let mut msg = chat_with_body(
        "alice@example.com/web",
        "room@conf.example.com",
        "fixed text",
    );
    msg.type_ = MessageType::Groupchat;
    msg.payloads.push(
        xmpp_parsers::message_correct::Replace {
            id: xmpp_parsers::message::Id(String::new()),
        }
        .into(),
    );

    let outcome = run(&local, &mut msg);
    assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
}

#[test]
fn xep_0424_retraction_emits_lookup_with_stanza_id() {
    let local = full("alice@example.com/web");
    let mut msg = chat_with_body(
        "alice@example.com/web",
        "bob@example.com",
        "I take that back",
    );
    msg.payloads.push(build_retract_element("stanza-X"));

    let outcome = run(&local, &mut msg);
    let (_id, _archive, _archive_kind, reference) = extract_lookup_event(&outcome);
    match reference {
        MessageRef::StanzaId { stanza_id } => {
            let expected_by: jid::Jid = bare("alice@example.com").into();
            assert_eq!(stanza_id.by, expected_by);
            assert_eq!(stanza_id.as_str(), "stanza-X");
        }
        other => panic!("expected StanzaId ref, got {other:?}"),
    }
}

#[test]
fn xep_0461_reply_emits_lookup_with_stanza_id() {
    let local = full("alice@example.com/web");
    let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "agreed");
    msg.payloads.push(build_reply_element(
        &ReplyReference::new("stanza-Y").with_to("bob@example.com".parse().expect("valid jid")),
    ));

    let outcome = run(&local, &mut msg);
    let (_id, _archive, _archive_kind, reference) = extract_lookup_event(&outcome);
    match reference {
        MessageRef::StanzaId { stanza_id } => {
            assert_eq!(stanza_id.as_str(), "stanza-Y");
        }
        other => panic!("expected StanzaId ref, got {other:?}"),
    }
}

#[test]
fn message_without_rich_target_continues() {
    let local = full("alice@example.com/web");
    let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
    let outcome = run(&local, &mut msg);
    assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
}

#[test]
fn recipient_pass_skips_rich_target_validation() {
    let local = full("bob@example.com/desk");
    let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "fixed text");
    msg.payloads.push(build_replace_element("orig-msg-1"));
    let outcome = run(&local, &mut msg);
    // Validation is the sender's job — recipient pass does nothing.
    assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
}

#[test]
fn groupchat_rich_target_is_skipped_user_side() {
    // Groupchat rich-targets reference messages in the room's
    // archive, not the user's. The user-side handler must not
    // validate against ctx.full_jid.to_bare() — that would miss
    // every legitimate MUC retraction. The room chain (PR5) owns
    // this validation against the room's archive.
    let local = full("alice@example.com/web");
    let mut msg = chat_with_body(
        "alice@example.com/web",
        "room@conf.example.com",
        "I take that back",
    );
    msg.type_ = MessageType::Groupchat;
    msg.payloads.push(build_retract_element("stanza-X"));
    let outcome = run(&local, &mut msg);
    assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
}

// -----------------------------------------------------------------
// Completion — XEP rule branches
// -----------------------------------------------------------------

#[test]
fn xep_0424_retraction_target_not_found_emits_item_not_found() {
    let original = chat_with_body(
        "alice@example.com/web",
        "bob@example.com",
        "I take that back",
    );
    let outcome = RichTargetValidationHandler::handle_completion(
        RichTargetKind::Retraction,
        &original,
        None,
        &bare("alice@example.com"),
    );
    let parsed = extract_error_payload(&outcome);
    assert_eq!(parsed.type_, ErrorType::Cancel);
    assert_eq!(parsed.defined_condition, DefinedCondition::ItemNotFound);
}

#[test]
fn xep_0461_reply_target_not_found_emits_item_not_found() {
    let original = chat_with_body("alice@example.com/web", "bob@example.com", "agreed");
    let outcome = RichTargetValidationHandler::handle_completion(
        RichTargetKind::Reply,
        &original,
        None,
        &bare("alice@example.com"),
    );
    let parsed = extract_error_payload(&outcome);
    assert_eq!(parsed.defined_condition, DefinedCondition::ItemNotFound);
}

#[test]
fn xep_0308_correction_by_non_author_emits_not_acceptable() {
    // The corrector is alice but the archived message was sent by
    // mallory — XEP-0308 §7.1 forbids cross-author correction.
    let original = chat_with_body("alice@example.com/web", "bob@example.com", "edit");
    let archived = archived_message_from("mallory@example.com/web", "original");
    let outcome = RichTargetValidationHandler::handle_completion(
        RichTargetKind::Correction,
        &original,
        Some(&archived),
        &bare("alice@example.com"),
    );
    let parsed = extract_error_payload(&outcome);
    assert_eq!(parsed.defined_condition, DefinedCondition::NotAcceptable);
}

#[test]
fn xep_0424_retraction_by_non_author_emits_not_acceptable() {
    let original = chat_with_body("alice@example.com/web", "bob@example.com", "retract");
    let archived = archived_message_from("mallory@example.com/web", "victim");
    let outcome = RichTargetValidationHandler::handle_completion(
        RichTargetKind::Retraction,
        &original,
        Some(&archived),
        &bare("alice@example.com"),
    );
    let parsed = extract_error_payload(&outcome);
    assert_eq!(parsed.defined_condition, DefinedCondition::NotAcceptable);
}

#[test]
fn xep_0424_retraction_of_already_tombstoned_message_emits_bad_request() {
    let original = chat_with_body("alice@example.com/web", "bob@example.com", "retract");
    let mut archived = archived_message_from("alice@example.com/web", "original");
    archived.tombstoned = true;
    let outcome = RichTargetValidationHandler::handle_completion(
        RichTargetKind::Retraction,
        &original,
        Some(&archived),
        &bare("alice@example.com"),
    );
    let parsed = extract_error_payload(&outcome);
    assert_eq!(parsed.defined_condition, DefinedCondition::BadRequest);
    assert_eq!(parsed.type_, ErrorType::Modify);
}

#[test]
fn xep_0308_correction_by_same_author_continues() {
    let original = chat_with_body("alice@example.com/web", "bob@example.com", "edit");
    let archived = archived_message_from("alice@example.com/web", "original");
    let outcome = RichTargetValidationHandler::handle_completion(
        RichTargetKind::Correction,
        &original,
        Some(&archived),
        &bare("alice@example.com"),
    );
    assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
}

#[test]
fn xep_0424_retraction_by_same_author_continues() {
    let original = chat_with_body("alice@example.com/web", "bob@example.com", "retract");
    let archived = archived_message_from("alice@example.com/web", "original");
    let outcome = RichTargetValidationHandler::handle_completion(
        RichTargetKind::Retraction,
        &original,
        Some(&archived),
        &bare("alice@example.com"),
    );
    assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
}

#[test]
fn xep_0461_reply_existence_is_sufficient_regardless_of_author() {
    // §3.3 — a reply doesn't impose author-equality.
    let original = chat_with_body("alice@example.com/web", "bob@example.com", "agreed");
    let archived = archived_message_from("mallory@example.com/web", "thread root");
    let outcome = RichTargetValidationHandler::handle_completion(
        RichTargetKind::Reply,
        &original,
        Some(&archived),
        &bare("alice@example.com"),
    );
    assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
}
