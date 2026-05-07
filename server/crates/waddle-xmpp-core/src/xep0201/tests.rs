use super::*;

#[test]
fn parses_root_thread() {
    let xml = "<message xmlns='jabber:client'><thread>root-1</thread></message>"
        .parse::<Element>()
        .expect("valid xml");
    let info = parse_thread_info(&xml).expect("thread");
    assert_eq!(info.id.as_str(), "root-1");
    assert_eq!(info.parent, None);
}

#[test]
fn parses_child_thread() {
    let xml = "<message xmlns='jabber:client'><thread parent='root-1'>child-2</thread></message>"
        .parse::<Element>()
        .expect("valid xml");
    let info = parse_thread_info(&xml).expect("thread");
    assert_eq!(info.id.as_str(), "child-2");
    assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root-1"));
}

#[test]
fn empty_thread_returns_none() {
    let xml = "<message xmlns='jabber:client'><thread></thread></message>"
        .parse::<Element>()
        .expect("valid xml");
    assert_eq!(parse_thread_info(&xml), None);
}

#[test]
fn missing_thread_returns_none() {
    let xml = "<message xmlns='jabber:client'/>"
        .parse::<Element>()
        .expect("valid xml");
    assert_eq!(parse_thread_info(&xml), None);
}

#[test]
fn explicit_empty_namespace_thread_is_not_stanza_thread() {
    let xml = Element::builder("message", "jabber:client")
        .append(
            Element::builder(THREAD_ELEMENT, "")
                .attr("parent", "root-1")
                .append("not-stanza-thread")
                .build(),
        )
        .build();

    assert_eq!(parse_thread_info(&xml), None);
}

#[test]
fn parent_only_with_empty_id_is_rejected() {
    // XEP-0201: `parent` is meaningful only as a back-reference from a
    // thread that has its own id. A `<thread parent='X'/>` with no id is
    // ill-formed; this helper rejects it so the write path never persists
    // a parent without a thread id.
    let xml = "<message xmlns='jabber:client'><thread parent='root-1'></thread></message>"
        .parse::<Element>()
        .expect("valid xml");
    assert_eq!(parse_thread_info(&xml), None);
}

fn tid(value: &str) -> ThreadId {
    ThreadId::new(value).expect("non-empty")
}

#[test]
fn build_element_with_parent() {
    let info = ThreadInfo::child(tid("child-a"), tid("root-a"));
    let elem = build_thread_element(&info, "jabber:client");
    assert_eq!(elem.name(), THREAD_ELEMENT);
    assert_eq!(elem.ns(), "jabber:client");
    assert_eq!(elem.attr("parent"), Some("root-a"));
    assert_eq!(elem.text(), "child-a");
}

#[test]
fn build_element_without_parent() {
    let info = ThreadInfo::root(tid("root-a"));
    let elem = build_thread_element(&info, "jabber:client");
    assert_eq!(elem.attr("parent"), None);
    assert_eq!(elem.text(), "root-a");
}

#[test]
fn install_thread_element_inherits_message_ns() {
    let mut xml = "<message xmlns='jabber:client'/>"
        .parse::<Element>()
        .expect("valid xml");
    install_thread_element(&mut xml, &ThreadInfo::child(tid("c"), tid("r")));
    let thread = xml
        .children()
        .find(|c| c.name() == THREAD_ELEMENT)
        .expect("thread child");
    assert_eq!(thread.ns(), "jabber:client");
}

#[test]
fn install_thread_element_strips_existing() {
    let mut xml = "<message xmlns='jabber:client'><thread>old</thread></message>"
        .parse::<Element>()
        .expect("valid xml");
    install_thread_element(&mut xml, &ThreadInfo::child(tid("new"), tid("root")));
    let count = xml
        .children()
        .filter(|c| c.name() == THREAD_ELEMENT)
        .count();
    assert_eq!(count, 1);
    let info = parse_thread_info(&xml).expect("thread after install");
    assert_eq!(info.id.as_str(), "new");
    assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root"));
}

#[test]
fn install_thread_element_preserves_unrelated_namespaced_thread_payload() {
    let mut xml = "<message xmlns='jabber:client'><thread xmlns='urn:example:other:0' kind='extension'>keep me</thread><thread>old</thread></message>"
        .parse::<Element>()
        .expect("valid xml");
    install_thread_element(&mut xml, &ThreadInfo::child(tid("new"), tid("root")));

    assert!(xml.children().any(|c| {
        c.name() == THREAD_ELEMENT && c.ns() == "urn:example:other:0" && c.text() == "keep me"
    }));
    let count = xml
        .children()
        .filter(|c| is_thread_element_for_stanza(c, "jabber:client"))
        .count();
    assert_eq!(count, 1);
    let info = parse_thread_info(&xml).expect("thread after install");
    assert_eq!(info.id.as_str(), "new");
    assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root"));
}

#[test]
fn install_thread_element_preserves_explicit_empty_namespace_thread_payload() {
    let mut xml = Element::builder("message", "jabber:client")
        .append(
            Element::builder(THREAD_ELEMENT, "")
                .attr("kind", "extension")
                .append("keep me")
                .build(),
        )
        .append(
            Element::builder(THREAD_ELEMENT, "jabber:client")
                .append("old")
                .build(),
        )
        .build();

    install_thread_element(&mut xml, &ThreadInfo::child(tid("new"), tid("root")));

    assert!(xml
        .children()
        .any(|c| c.name() == THREAD_ELEMENT && c.ns().is_empty() && c.text() == "keep me"));
    let count = xml
        .children()
        .filter(|c| is_thread_element_for_stanza(c, "jabber:client"))
        .count();
    assert_eq!(count, 1);
    let info = parse_thread_info(&xml).expect("thread after install");
    assert_eq!(info.id.as_str(), "new");
    assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root"));
}

#[test]
fn thread_feature_constant() {
    assert_eq!(NS_THREAD_FEATURE, "urn:xmpp:threads:0");
}

#[test]
fn set_and_get_thread_id_round_trip() {
    let mut msg = Message::new(None::<jid::Jid>);
    assert_eq!(thread_id_from_message(&msg), None);
    set_thread_id(&mut msg, "thread-root-1");
    assert_eq!(
        thread_id_from_message(&msg).as_deref(),
        Some("thread-root-1")
    );
}

#[test]
fn set_thread_id_overwrites() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_thread_id(&mut msg, "first");
    set_thread_id(&mut msg, "second");
    assert_eq!(thread_id_from_message(&msg).as_deref(), Some("second"));
}

#[test]
fn thread_info_from_message_recovers_parent_from_payload_form() {
    // Post-`reattach_thread_parent` invariant: parent attribute lives in
    // `msg.payloads` as a raw element rather than `msg.thread`, because
    // `xmpp_parsers::Thread(String)` drops it at parse time.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(
        Element::builder(THREAD_ELEMENT, "jabber:client")
            .attr("parent", "root-1")
            .append("child-2")
            .build(),
    );
    let info = thread_info_from_message(&msg).expect("thread info");
    assert_eq!(info.id.as_str(), "child-2");
    assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root-1"));
}

#[test]
fn thread_info_from_message_falls_back_to_typed_field_when_no_payload() {
    // No payload thread element; the typed field is the only source.
    // Parent is unrecoverable in this branch by design — `xmpp_parsers`
    // dropped it at parse time and `reattach_thread_parent` was not run.
    let mut msg = Message::new(None::<jid::Jid>);
    set_thread_id(&mut msg, "abc");
    let info = thread_info_from_message(&msg).expect("thread info");
    assert_eq!(info.id.as_str(), "abc");
    assert_eq!(info.parent, None);
}

#[test]
fn thread_info_from_message_payload_takes_precedence_over_typed_field() {
    // If both forms are present (transient pre-reattach state), the
    // payload form wins because it carries the parent attribute.
    let mut msg = Message::new(None::<jid::Jid>);
    set_thread_id(&mut msg, "stale-id");
    msg.payloads.push(
        Element::builder(THREAD_ELEMENT, "jabber:client")
            .attr("parent", "root-1")
            .append("authoritative-id")
            .build(),
    );
    let info = thread_info_from_message(&msg).expect("thread info");
    assert_eq!(info.id.as_str(), "authoritative-id");
    assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root-1"));
}

#[test]
fn thread_info_from_message_ignores_empty_namespace_payload() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_thread_id(&mut msg, "typed-thread");
    msg.payloads.push(
        Element::builder(THREAD_ELEMENT, "")
            .attr("parent", "foreign-root")
            .append("foreign-thread")
            .build(),
    );

    let info = thread_info_from_message(&msg).expect("typed thread");
    assert_eq!(info.id.as_str(), "typed-thread");
    assert_eq!(info.parent, None);
}

#[test]
fn thread_info_from_message_ignores_wrong_stanza_namespace_payload() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_thread_id(&mut msg, "typed-thread");
    msg.payloads.push(
        Element::builder(THREAD_ELEMENT, SERVER_STANZA_NS)
            .attr("parent", "foreign-root")
            .append("foreign-thread")
            .build(),
    );

    let info = thread_info_from_message(&msg).expect("typed thread");
    assert_eq!(info.id.as_str(), "typed-thread");
    assert_eq!(info.parent, None);
}

#[test]
fn thread_info_from_message_can_read_server_stanza_namespace() {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(
        Element::builder(THREAD_ELEMENT, SERVER_STANZA_NS)
            .attr("parent", "server-root")
            .append("server-child")
            .build(),
    );

    assert_eq!(thread_info_from_message(&msg), None);
    let info =
        thread_info_from_message_in_stanza_ns(&msg, SERVER_STANZA_NS).expect("server thread");
    assert_eq!(info.id.as_str(), "server-child");
    assert_eq!(
        info.parent.as_ref().map(ThreadId::as_str),
        Some("server-root")
    );
}

#[test]
fn thread_id_from_message_ignores_empty_namespace_payload() {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(
        Element::builder(THREAD_ELEMENT, "")
            .append("foreign-thread")
            .build(),
    );

    assert_eq!(thread_id_from_message(&msg), None);
}

#[test]
fn thread_info_from_message_payload_with_empty_id_is_rejected() {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(
        Element::builder(THREAD_ELEMENT, "jabber:client")
            .attr("parent", "root-1")
            .build(),
    );
    assert_eq!(thread_info_from_message(&msg), None);
}

#[test]
fn thread_info_from_message_returns_none_when_no_thread_at_all() {
    let msg = Message::new(None::<jid::Jid>);
    assert_eq!(thread_info_from_message(&msg), None);
}
