use super::*;
use chrono::Datelike;
use jid::Jid;
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::{Message, MessageType};

fn filter_id(s: &str) -> MamFilterStanzaId {
    MamFilterStanzaId::new(s).expect("valid test fixture id")
}

use crate::xep0201::ThreadInfo;
use crate::xep0359::{OriginId, StanzaId};
use crate::CoreError;

fn jid(value: &str) -> Jid {
    value.parse::<Jid>().expect("valid jid literal")
}

/// RFC 6121 §5.2.2 ("Type Attribute"): "If absent, the message is
/// implicitly of type `normal`."
///
/// `xmpp_parsers::message::MessageType::default()` correctly
/// returns [`MessageType::Normal`] (verified by reading
/// `generate_attribute!` `Default = Normal` in
/// `xmpp-parsers-0.21.0/src/message.rs`). This test pins that
/// contract: a future bump of `xmpp-parsers` that changes the
/// default would silently shift the absent-type semantics of every
/// archived row that hits the typed-decode fallback path.
///
/// Pre-#228 commit 8 the `default_message_type() = "chat"` helper
/// in this module was a latent conformance bug: archived rows that
/// went through serde with an absent `message_type` field would
/// hydrate to `"chat"`, violating the RFC. Deleting that helper
/// and typing the field as [`MessageType`] anchors the absent-type
/// semantics to [`MessageType::default()`] (== [`Normal`]), which
/// matches the RFC.
///
/// [`Normal`]: xmpp_parsers::message::MessageType::Normal
#[test]
fn message_type_default_matches_rfc6121_5_2_2() {
    assert_eq!(MessageType::default(), MessageType::Normal);
}

#[test]
fn identifies_mam_query_response_messages() {
    let mut result_message = Message::new(Some(jid("alice@example.com/web")));
    result_message.type_ = MessageType::Normal;
    result_message.payloads.push(
        Element::builder("result", MAM_NS)
            .attr("queryid", "query-1")
            .attr("id", "archive-id-1")
            .append(Element::builder("forwarded", FORWARD_NS).build())
            .build(),
    );
    assert!(is_mam_query_response_message(&result_message));

    let mut fin_message = Message::new(Some(jid("alice@example.com/web")));
    fin_message.type_ = MessageType::Normal;
    fin_message.payloads.push(
        Element::builder("fin", MAM_NS)
            .append(Element::builder("set", RSM_NS).build())
            .build(),
    );
    assert!(is_mam_query_response_message(&fin_message));
}

#[test]
fn ordinary_messages_are_not_mam_query_responses() {
    let mut message = Message::new(Some(jid("alice@example.com")));
    message.type_ = MessageType::Chat;
    message.payloads.push(
        Element::builder("result", "urn:example:not-mam")
            .attr("queryid", "query-1")
            .build(),
    );

    assert!(!is_mam_query_response_message(&message));
}

#[test]
fn body_messages_with_mam_payload_are_not_mam_query_responses() {
    let mut message = Message::new(Some(jid("alice@example.com")));
    message.type_ = MessageType::Chat;
    message
        .bodies
        .insert(String::new(), xmpp_parsers::message::Body("keep me".into()));
    message.payloads.push(
        Element::builder("result", MAM_NS)
            .attr("queryid", "query-1")
            .attr("id", "archive-id-1")
            .append(Element::builder("forwarded", FORWARD_NS).build())
            .build(),
    );

    assert!(!is_mam_query_response_message(&message));
}

#[test]
fn parses_mam_query_with_form_and_rsm() {
    let iq = Iq {
        from: None,
        to: None,
        id: "mam-1".to_string(),
        payload: IqType::Set(
            Element::builder("query", MAM_NS)
                .attr("queryid", "query-1")
                .append(
                    Element::builder("x", DATA_FORMS_NS)
                        .attr("type", "submit")
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "start")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("2024-01-15T10:30:00Z")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "with")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("juliet@example.com")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "before-id")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("msg-20")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "after-id")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("msg-2")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "ids")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("msg-5")
                                        .build(),
                                )
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("msg-7")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", FULLTEXT_MAM_FIELD)
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("release notes")
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .append(
                    Element::builder("set", RSM_NS)
                        .append(Element::builder("max", RSM_NS).append("10").build())
                        .append(Element::builder("after", RSM_NS).append("msg-9").build())
                        .build(),
                )
                .build(),
        ),
    };

    let (query_id, query) = parse_mam_query(&iq).expect("valid MAM query");

    assert_eq!(query_id, "query-1");
    assert_eq!(query.max, Some(10));
    assert_eq!(query.after_id.as_deref(), Some("msg-9"));
    assert_eq!(query.filter_before_id.as_deref(), Some("msg-20"));
    assert_eq!(query.filter_after_id.as_deref(), Some("msg-2"));
    assert_eq!(query.ids, vec!["msg-5", "msg-7"]);
    assert_eq!(
        query.with.as_ref().map(Jid::to_string).as_deref(),
        Some("juliet@example.com")
    );
    assert_eq!(
        query.fulltext.as_ref().map(RichText::as_str),
        Some("release notes")
    );
    let start = query.start.expect("start filter");
    assert_eq!(start.year(), 2024);
    assert_eq!(start.month(), 1);
    assert_eq!(start.day(), 15);
}

#[test]
fn parses_waddle_mam_thread_field() {
    let iq = Iq {
        from: None,
        to: None,
        id: "mam-thread".to_string(),
        payload: IqType::Set(
            Element::builder("query", MAM_NS)
                .append(
                    Element::builder("x", DATA_FORMS_NS)
                        .attr("type", "submit")
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", WADDLE_MAM_THREAD_FIELD)
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("thread-root")
                                        .build(),
                                )
                                .build(),
                        )
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "FORM_TYPE")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append(MAM_NS)
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .build(),
        ),
    };

    let (_, query) = parse_mam_query(&iq).expect("valid MAM query");

    assert_eq!(
        query.thread_id.as_ref().map(ThreadId::as_str),
        Some("thread-root")
    );
}

#[test]
fn rejects_unsupported_mam_form_fields() {
    let iq = Iq {
        from: None,
        to: None,
        id: "mam-thread".to_string(),
        payload: IqType::Set(
            Element::builder("query", MAM_NS)
                .append(
                    Element::builder("x", DATA_FORMS_NS)
                        .attr("type", "submit")
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "{urn:xmpp:mam:2}thread")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("wrong-thread")
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .build(),
        ),
    };

    let err = parse_mam_query(&iq).expect_err("unsupported MAM field");
    assert!(matches!(err, CoreError::NotImplemented));
}

#[test]
fn rejects_legacy_bare_fulltext_mam_form_field() {
    let iq = Iq {
        from: None,
        to: None,
        id: "mam-legacy-fulltext".to_string(),
        payload: IqType::Set(
            Element::builder("query", MAM_NS)
                .append(
                    Element::builder("x", DATA_FORMS_NS)
                        .attr("type", "submit")
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "fulltext")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("release notes")
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .build(),
        ),
    };

    let err = parse_mam_query(&iq).expect_err("unsupported MAM field");
    assert!(matches!(err, CoreError::NotImplemented));
}

#[test]
fn builds_mam_query_form_with_waddle_thread_field() {
    let iq = Iq {
        from: Some("juliet@example.com/chamber".parse().expect("from jid")),
        to: Some("room@muc.example.com".parse().expect("to jid")),
        id: "mam-form".to_string(),
        payload: IqType::Get(Element::builder("query", MAM_NS).build()),
    };

    assert!(is_mam_query_form_request(&iq));
    let result = build_query_form_iq(&iq);
    let IqType::Result(Some(query)) = result.payload else {
        panic!("expected query form result");
    };
    let form = query.get_child("x", DATA_FORMS_NS).expect("form");
    let fields = form
        .children()
        .filter_map(|field| field.attr("var"))
        .collect::<Vec<_>>();

    assert!(fields.contains(&"FORM_TYPE"));
    assert!(fields.contains(&"with"));
    assert!(fields.contains(&"start"));
    assert!(fields.contains(&"end"));
    assert!(fields.contains(&"before-id"));
    assert!(fields.contains(&"after-id"));
    assert!(fields.contains(&"ids"));
    assert!(fields.contains(&WADDLE_MAM_THREAD_FIELD));
    assert!(fields.contains(&FULLTEXT_MAM_FIELD));
    assert!(!fields.contains(&"fulltext"));

    let ids_field = form
        .children()
        .find(|field| field.attr("var") == Some("ids"))
        .expect("ids field");
    let validate = ids_field
        .get_child("validate", XDATA_VALIDATE_NS)
        .expect("ids field validate element");
    assert_eq!(validate.attr("datatype"), Some("xs:string"));
    assert!(validate.get_child("open", XDATA_VALIDATE_NS).is_some());
}

#[test]
fn disco_form_advertises_stanza_id_filter_field() {
    let iq = Iq {
        from: None,
        to: None,
        id: "disco".to_string(),
        payload: IqType::Get(Element::builder("query", MAM_NS).build()),
    };

    let response = build_query_form_iq(&iq);
    let IqType::Result(Some(query)) = response.payload else {
        panic!("expected query form result");
    };
    let form = query.get_child("x", DATA_FORMS_NS).expect("form");
    let fields = form
        .children()
        .filter_map(|field| field.attr("var"))
        .collect::<Vec<_>>();

    assert!(
        fields.contains(&STANZA_ID_FILTER_FIELD),
        "expected disco form to advertise {STANZA_ID_FILTER_FIELD} (got {fields:?})"
    );
}

#[test]
fn parses_last_page_rsm_before() {
    let iq = Iq {
        from: None,
        to: None,
        id: "mam-2".to_string(),
        payload: IqType::Set(
            Element::builder("query", MAM_NS)
                .append(
                    Element::builder("set", RSM_NS)
                        .append(Element::builder("before", RSM_NS).build())
                        .build(),
                )
                .build(),
        ),
    };

    let (_, query) = parse_mam_query(&iq).expect("valid MAM query");

    assert_eq!(query.before_id, Some(String::new()));
}

#[test]
fn rejects_invalid_datetime() {
    let iq = Iq {
        from: None,
        to: None,
        id: "mam-3".to_string(),
        payload: IqType::Set(
            Element::builder("query", MAM_NS)
                .append(
                    Element::builder("x", DATA_FORMS_NS)
                        .append(
                            Element::builder("field", DATA_FORMS_NS)
                                .attr("var", "start")
                                .append(
                                    Element::builder("value", DATA_FORMS_NS)
                                        .append("not-a-date")
                                        .build(),
                                )
                                .build(),
                        )
                        .build(),
                )
                .build(),
        ),
    };

    let err = parse_mam_query(&iq).expect_err("invalid MAM query");
    assert!(matches!(err, CoreError::BadRequest(_)));
}

#[test]
fn builds_result_message_from_legacy_fields() {
    let archived = ArchivedMessage {
        id: "msg-123".to_string(),
        thread: Some(ThreadInfo::root(
            ThreadId::new("thread-1").expect("non-empty thread id"),
        )),
        reply: Some(ArchivedReply {
            id: RichMessageId::new("parent-1").expect("non-empty reply id"),
            to: Some("alice@example.com".parse::<Jid>().expect("valid jid")),
        }),
        origin_id: Some(OriginId::new("origin-1")),
        body: Some("Hello, world!".to_string()),
        ..ArchivedMessage::for_test(
            jid("user@example.com/nick"),
            jid("room@conference.example.com"),
        )
    };

    let msg = build_result_messages("query-1", &jid("user@example.com"), &[archived]);
    let result = msg[0]
        .payloads
        .iter()
        .find(|p| p.name() == "result" && p.ns() == MAM_NS)
        .expect("result payload");
    let forwarded = result
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
        .expect("forwarded element");
    let inner_msg = forwarded
        .children()
        .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
        .expect("inner message");

    assert!(inner_msg.children().any(|c| c.name() == "thread"));
    assert!(inner_msg
        .children()
        .any(|c| c.name() == "reply" && c.ns() == REPLY_NS));
    assert!(inner_msg
        .children()
        .any(|c| c.name() == "origin-id" && c.ns() == STANZA_ID_NS));
}

fn nested_thread_archived_for_replay(stanza_xml: Option<String>) -> ArchivedMessage {
    ArchivedMessage {
        id: "msg-thread-nested".to_string(),
        body: Some("nested reply".to_string()),
        stanza_id: Some(StanzaId::new(
            "wire-id-1",
            "bob@example.com".parse::<Jid>().expect("valid jid"),
        )),
        thread: Some(ThreadInfo::child(
            ThreadId::new("child-thread").expect("non-empty thread id"),
            ThreadId::new("root-thread").expect("non-empty parent id"),
        )),
        message_type: MessageType::Chat,
        stanza_xml,
        ..ArchivedMessage::for_test(jid("alice@example.com/web"), jid("bob@example.com"))
    }
}

fn replay_inner_thread(msg: &Message) -> &Element {
    let result = msg
        .payloads
        .iter()
        .find(|p| p.name() == "result" && p.ns() == MAM_NS)
        .expect("result payload");
    let forwarded = result
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
        .expect("forwarded element");
    let inner_msg = forwarded
        .children()
        .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
        .expect("inner message");
    inner_msg
        .children()
        .find(|c| c.name() == "thread")
        .expect("thread child on replay")
}

#[test]
fn xep_0201_typed_replay_emits_thread_parent() {
    // Typed reconstruction path: stanza_xml is None and rich is None
    // so the projection falls through to `build_typed_inner_message`
    // (with `rich` defaulted) — actually this falls into the legacy
    // path since `rich.is_none()`. Use a row that exercises the
    // typed path by setting a rich payload that doesn't carry its
    // own `<thread/>`.
    let archived = ArchivedMessage {
        rich: Some(ArchivedRichMessage {
            payload: None,
            reply: None,
            references: vec![],
            mentions: vec![],
        }),
        ..nested_thread_archived_for_replay(None)
    };
    let msgs = build_result_messages("q1", &jid("user@example.com"), &[archived]);
    let thread = replay_inner_thread(&msgs[0]);
    assert_eq!(thread.text().trim(), "child-thread");
    assert_eq!(thread.attr("parent"), Some("root-thread"));
}

#[test]
fn xep_0201_legacy_replay_emits_thread_parent() {
    // Legacy reconstruction path: stanza_xml is None AND rich is
    // None — `build_legacy_inner_message` rebuilds purely from
    // scalar columns.
    let archived = nested_thread_archived_for_replay(None);
    let msgs = build_result_messages("q2", &jid("user@example.com"), &[archived]);
    let thread = replay_inner_thread(&msgs[0]);
    assert_eq!(thread.text().trim(), "child-thread");
    assert_eq!(thread.attr("parent"), Some("root-thread"));
}

#[test]
fn xep_0201_groupchat_stanza_xml_replay_reinstalls_thread_and_strips_to() {
    let archived = ArchivedMessage {
        id: "archive-threaded-reply".to_string(),
        body: Some("threaded reply".to_string()),
        stanza_id: Some(StanzaId::new(
            "wire-id-2",
            "room@conference.example.com"
                .parse::<Jid>()
                .expect("valid jid"),
        )),
        thread: Some(ThreadInfo::child(
            ThreadId::new("root-thread").expect("non-empty thread id"),
            ThreadId::new("parent-thread").expect("non-empty parent id"),
        )),
        message_type: MessageType::Groupchat,
        stanza_xml: Some(
            "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='bob@example.com/web' type='groupchat' id='wire-id-2'><body>threaded reply</body><thread>stale-thread</thread><reply xmlns='urn:xmpp:reply:0' id='root-thread'/></message>"
                .to_string(),
        ),
        ..ArchivedMessage::for_test(
            jid("room@conference.example.com/alice"),
            jid("room@conference.example.com"),
        )
    };

    let msgs = build_result_messages("q-stanza-xml", &jid("bob@example.com/web"), &[archived]);
    let result = msgs[0]
        .payloads
        .iter()
        .find(|p| p.name() == "result" && p.ns() == MAM_NS)
        .expect("result payload");
    let forwarded = result
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
        .expect("forwarded element");
    let inner_msg = forwarded
        .children()
        .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
        .expect("inner message");
    let thread = inner_msg
        .children()
        .find(|c| c.name() == "thread")
        .expect("thread child on replay");

    assert_eq!(inner_msg.attr("to"), None);
    assert_eq!(thread.text().trim(), "root-thread");
    assert_eq!(thread.attr("parent"), Some("parent-thread"));
    assert!(inner_msg
        .children()
        .any(|c| c.name() == "reply" && c.ns() == REPLY_NS));
}

#[test]
fn xep_0201_replay_omits_thread_when_id_missing_even_with_parent() {
    // RFC 6121 §5.2.5 incoherence guard: parent without id MUST NOT
    // emit a `<thread/>` element on replay. This locks the rule
    // against future regressions where parent-only state could be
    // smuggled past the projection.
    let archived = ArchivedMessage {
        id: "msg-incoherent".to_string(),
        body: Some("body".to_string()),
        // The collapsed `thread: Option<ThreadInfo>` field makes
        // "parent without id" unrepresentable; the closest legal
        // analog of the previous parent-only state is `None`,
        // which (still) emits no `<thread/>` element. This test
        // continues to pin that emission rule for the no-thread
        // case.
        thread: None,
        message_type: MessageType::Chat,
        stanza_xml: None,
        rich: None,
        ..ArchivedMessage::for_test(jid("alice@example.com/web"), jid("bob@example.com"))
    };
    let msgs = build_result_messages("q3", &jid("user@example.com"), &[archived]);
    let result = msgs[0]
        .payloads
        .iter()
        .find(|p| p.name() == "result" && p.ns() == MAM_NS)
        .expect("result payload");
    let forwarded = result
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
        .expect("forwarded element");
    let inner_msg = forwarded
        .children()
        .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
        .expect("inner message");
    assert!(
        !inner_msg.children().any(|c| c.name() == "thread"),
        "no thread element should be emitted when thread_id is None"
    );
}

#[test]
fn preserves_archived_stanza_payload() {
    let archived = ArchivedMessage {
        id: "msg-124".to_string(),
        body: None,
        message_type: MessageType::Groupchat,
        stanza_xml: Some(
            "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='reaction-1'><reactions xmlns='urn:xmpp:reactions:0' id='msg-1'><reaction>👍</reaction></reactions></message>".to_string(),
        ),
        ..ArchivedMessage::for_test(
            jid("room@conference.example.com/alice"),
            jid("room@conference.example.com"),
        )
    };

    let msg = build_result_messages("query-2", &jid("user@example.com"), &[archived]);
    let result = msg[0]
        .payloads
        .iter()
        .find(|p| p.name() == "result" && p.ns() == MAM_NS)
        .expect("result payload");
    let forwarded = result
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
        .expect("forwarded element");
    let inner_msg = forwarded
        .children()
        .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
        .expect("inner message");
    let reactions = inner_msg
        .children()
        .find(|c| c.name() == "reactions" && c.ns() == "urn:xmpp:reactions:0")
        .expect("reactions payload");

    assert_eq!(inner_msg.attr("id"), Some("reaction-1"));
    assert_eq!(reactions.attr("id"), Some("msg-1"));
}

#[test]
fn builds_fin_iq_with_rsm_metadata() {
    let original = Iq {
        from: Some(jid("romeo@example.com/orchard")),
        to: Some(jid("juliet@example.com/balcony")),
        id: "iq-1".to_string(),
        payload: IqType::Get(Element::builder("query", MAM_NS).build()),
    };
    let result = MamResult {
        messages: Vec::new(),
        complete: true,
        first_id: Some("msg-1".to_string()),
        last_id: Some("msg-2".to_string()),
        count: Some(2),
    };

    let fin = build_fin_iq(&original, &result);
    let payload = match fin.payload {
        IqType::Result(Some(payload)) => payload,
        other => panic!("unexpected fin payload: {:?}", other),
    };
    let set = payload
        .children()
        .find(|child| child.name() == "set" && child.ns() == RSM_NS)
        .expect("rsm set");

    assert_eq!(payload.attr("complete"), Some("true"));
    assert_eq!(
        set.get_child("first", RSM_NS).map(|child| child.text()),
        Some("msg-1".to_string())
    );
    assert_eq!(
        set.get_child("last", RSM_NS).map(|child| child.text()),
        Some("msg-2".to_string())
    );
    assert_eq!(
        set.get_child("count", RSM_NS).map(|child| child.text()),
        Some("2".to_string())
    );
}

#[test]
fn stanza_xml_preferred_over_rich_payload() {
    let archived = ArchivedMessage {
        id: "msg-priority".to_string(),
        body: Some("typed body".to_string()),
        message_type: MessageType::Groupchat,
        stanza_xml: Some(
            "<message xmlns='jabber:client' from='room@conference.example.com/alice' type='groupchat' id='live-1'><body>live body with extension payload</body><item xmlns='urn:example:task-widget:1' id='task-123' status='open'/></message>".to_string(),
        ),
        rich: Some(ArchivedRichMessage {
            payload: None,
            reply: None,
            references: vec![],
            mentions: vec![],
        }),
        ..ArchivedMessage::for_test(
            jid("room@conference.example.com/alice"),
            jid("room@conference.example.com"),
        )
    };

    let msg = build_result_messages("query-priority", &jid("user@example.com"), &[archived]);
    let result = msg[0]
        .payloads
        .iter()
        .find(|p| p.name() == "result" && p.ns() == MAM_NS)
        .expect("result payload");
    let forwarded = result
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
        .expect("forwarded element");
    let inner_msg = forwarded
        .children()
        .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
        .expect("inner message");

    let body = inner_msg
        .get_child("body", CLIENT_NS)
        .expect("body element");
    assert_eq!(body.text(), "live body with extension payload");

    let extension_payload = inner_msg
        .children()
        .find(|c| c.name() == "item" && c.ns() == "urn:example:task-widget:1")
        .expect("extension payload");
    assert_eq!(extension_payload.attr("id"), Some("task-123"));
}

#[test]
fn stanza_xml_preserves_reply_fallback() {
    let archived = ArchivedMessage {
        id: "msg-fallback".to_string(),
        body: Some("> Alice wrote:\n> Hello!\nI agree!".to_string()),
        message_type: MessageType::Groupchat,
        stanza_xml: Some(
            "<message xmlns='jabber:client' from='room@conference.example.com/bob' type='groupchat' id='reply-1'><body>&gt; Alice wrote:\n&gt; Hello!\nI agree!</body><reply xmlns='urn:xmpp:reply:0' id='orig-1' to='room@conference.example.com/alice'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start='0' end='22'/></fallback></message>".to_string(),
        ),
        reply: Some(ArchivedReply {
            id: RichMessageId::new("orig-1").expect("non-empty reply id"),
            to: Some(jid("room@conference.example.com/alice")),
        }),
        rich: Some(ArchivedRichMessage {
            payload: None,
            reply: Some(ArchivedReply {
                id: RichMessageId("orig-1".to_string()),
                to: Some(jid("room@conference.example.com/alice")),
            }),
            references: vec![],
            mentions: vec![],
        }),
        ..ArchivedMessage::for_test(
            jid("room@conference.example.com/bob"),
            jid("room@conference.example.com"),
        )
    };

    let msg = build_result_messages("query-fallback", &jid("user@example.com"), &[archived]);
    let result = msg[0]
        .payloads
        .iter()
        .find(|p| p.name() == "result" && p.ns() == MAM_NS)
        .expect("result payload");
    let forwarded = result
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
        .expect("forwarded element");
    let inner_msg = forwarded
        .children()
        .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
        .expect("inner message");

    let reply = inner_msg
        .children()
        .find(|c| c.name() == "reply" && c.ns() == "urn:xmpp:reply:0")
        .expect("reply element");
    assert_eq!(reply.attr("id"), Some("orig-1"));

    let fallback = inner_msg
        .children()
        .find(|c| c.name() == "fallback" && c.ns() == "urn:xmpp:fallback:0")
        .expect("fallback element");
    assert_eq!(fallback.attr("for"), Some("urn:xmpp:reply:0"));
    let fallback_body = fallback
        .get_child("body", "urn:xmpp:fallback:0")
        .expect("fallback body element");
    assert_eq!(fallback_body.attr("start"), Some("0"));
    assert_eq!(fallback_body.attr("end"), Some("22"));
}

#[test]
fn stanza_xml_strips_to_for_groupchat() {
    let archived = ArchivedMessage {
        id: "msg-strip-to".to_string(),
        body: Some("Hello!".to_string()),
        message_type: MessageType::Groupchat,
        stanza_xml: Some(
            "<message xmlns='jabber:client' from='room@conference.example.com/alice' to='room@conference.example.com' type='groupchat' id='msg-1'><body>Hello!</body></message>".to_string(),
        ),
        ..ArchivedMessage::for_test(
            jid("room@conference.example.com/alice"),
            jid("room@conference.example.com"),
        )
    };

    let msg = build_result_messages("query-strip-to", &jid("user@example.com"), &[archived]);
    let result = msg[0]
        .payloads
        .iter()
        .find(|p| p.name() == "result" && p.ns() == MAM_NS)
        .expect("result payload");
    let forwarded = result
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
        .expect("forwarded element");
    let inner_msg = forwarded
        .children()
        .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
        .expect("inner message");

    assert!(
        inner_msg.attr("to").is_none(),
        "groupchat MAM replay should not have a 'to' attribute, got: {:?}",
        inner_msg.attr("to")
    );
}

#[test]
fn stanza_xml_strips_to_for_groupchat_on_raw_element_fallback() {
    let stanza_xml = "<message xmlns='jabber:client' from='room@conf.example.com/alice' to='room@conf.example.com' type='groupchat' id='msg-x'><body>test</body><custom xmlns='urn:example:unknown'/></message>";
    let archived = ArchivedMessage {
        id: "msg-strip-raw".to_string(),
        body: Some("test".to_string()),
        message_type: MessageType::Groupchat,
        stanza_xml: Some(stanza_xml.to_string()),
        ..ArchivedMessage::for_test(
            jid("room@conf.example.com/alice"),
            jid("room@conf.example.com"),
        )
    };

    let msg = build_result_messages("query-strip-raw", &jid("user@example.com"), &[archived]);
    let result = msg[0]
        .payloads
        .iter()
        .find(|p| p.name() == "result" && p.ns() == MAM_NS)
        .expect("result payload");
    let forwarded = result
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == FORWARD_NS)
        .expect("forwarded element");
    let inner_msg = forwarded
        .children()
        .find(|c| c.name() == "message" && c.ns() == CLIENT_NS)
        .expect("inner message");

    assert!(
        inner_msg.attr("to").is_none(),
        "groupchat MAM replay via raw element fallback should not have a 'to' attribute, got: {:?}",
        inner_msg.attr("to")
    );
}

fn build_mam_iq_with_form_fields(fields: &[(&str, Vec<&str>)]) -> Iq {
    let mut form = Element::builder("x", DATA_FORMS_NS).attr("type", "submit");
    form = form.append(
        Element::builder("field", DATA_FORMS_NS)
            .attr("var", "FORM_TYPE")
            .attr("type", "hidden")
            .append(
                Element::builder("value", DATA_FORMS_NS)
                    .append(MAM_NS)
                    .build(),
            )
            .build(),
    );
    for (var, values) in fields {
        let mut field = Element::builder("field", DATA_FORMS_NS).attr("var", *var);
        for value in values {
            field = field.append(
                Element::builder("value", DATA_FORMS_NS)
                    .append(*value)
                    .build(),
            );
        }
        form = form.append(field.build());
    }
    let query = Element::builder("query", MAM_NS)
        .append(form.build())
        .build();
    Iq {
        from: None,
        to: None,
        id: "q1".to_string(),
        payload: IqType::Set(query),
    }
}

#[test]
fn parses_stanza_id_filter_field() {
    let iq =
        build_mam_iq_with_form_fields(&[(STANZA_ID_FILTER_FIELD, vec!["stanza-A", "stanza-B"])]);
    let (_, query) = parse_mam_query(&iq).expect("parses");
    assert_eq!(
        query.stanza_ids,
        vec![filter_id("stanza-A"), filter_id("stanza-B")]
    );
}

#[test]
fn rejects_oversize_stanza_id_value() {
    let oversized = "x".repeat(MAX_FILTER_STANZA_ID_LEN + 1);
    let iq = build_mam_iq_with_form_fields(&[(STANZA_ID_FILTER_FIELD, vec![oversized.as_str()])]);
    let err = parse_mam_query(&iq).expect_err("must reject");
    assert!(matches!(err, CoreError::BadRequest(_)));
}

#[test]
fn rejects_too_many_stanza_ids() {
    let values: Vec<String> = (0..=MAX_FILTER_STANZA_IDS)
        .map(|i| format!("s{i}"))
        .collect();
    let refs: Vec<&str> = values.iter().map(String::as_str).collect();
    let iq = build_mam_iq_with_form_fields(&[(STANZA_ID_FILTER_FIELD, refs)]);
    let err = parse_mam_query(&iq).expect_err("must reject");
    assert!(matches!(err, CoreError::BadRequest(_)));
}
