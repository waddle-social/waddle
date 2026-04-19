//! Inbox — conversation list + unread counters — dedicated suite.

use minidom::Element;
use waddle_xmpp::inbox::storage::{InMemoryInboxStorage, InboxStorage};
use waddle_xmpp::inbox::{ConversationKind, InboxEntry, InboxView};
use waddle_xmpp::xep::xep0430::{
    build_entry_element, build_inbox_query_result, build_mark_read_result, is_inbox_iq,
    parse_entry_element, parse_inbox_query, parse_mark_read, InboxError, NS_INBOX,
};
use xmpp_parsers::iq::{Iq, IqType};

fn jid(s: &str) -> jid::BareJid {
    s.parse().unwrap()
}

fn get_iq(child: Element) -> Iq {
    Iq {
        from: Some("me@example.com/r".parse().unwrap()),
        to: Some("me@example.com".parse().unwrap()),
        id: "ib-1".into(),
        payload: IqType::Get(child),
    }
}

fn set_iq(child: Element) -> Iq {
    Iq {
        from: Some("me@example.com/r".parse().unwrap()),
        to: Some("me@example.com".parse().unwrap()),
        id: "ib-2".into(),
        payload: IqType::Set(child),
    }
}

#[test]
fn inbox_view_observe_and_snapshot_order() {
    let mut view = InboxView::new();
    view.observe_message(
        InboxEntry::new(jid("a@example.com"), ConversationKind::Direct, "s1", 10),
        false,
    );
    view.observe_message(
        InboxEntry::new(
            jid("g@mix.example.com"),
            ConversationKind::MixChannel,
            "s2",
            30,
        ),
        false,
    );
    view.observe_message(
        InboxEntry::new(jid("b@example.com"), ConversationKind::Direct, "s3", 20),
        false,
    );
    let snap = view.snapshot();
    assert_eq!(snap[0].last_stanza_id, "s2");
    assert_eq!(snap[1].last_stanza_id, "s3");
    assert_eq!(snap[2].last_stanza_id, "s1");
}

#[test]
fn inbox_view_unread_counting() {
    let mut view = InboxView::new();
    view.observe_message(
        InboxEntry::new(jid("a@example.com"), ConversationKind::Direct, "s1", 1),
        true,
    );
    view.observe_message(
        InboxEntry::new(jid("a@example.com"), ConversationKind::Direct, "s2", 2),
        true,
    );
    assert_eq!(view.total_unread(), 2);
    view.mark_read(&jid("a@example.com"));
    assert_eq!(view.total_unread(), 0);
}

#[test]
fn inbox_query_defaults_and_explicit() {
    let iq = get_iq(Element::builder("query", NS_INBOX).build());
    let parsed = parse_inbox_query(&iq).unwrap();
    assert_eq!(parsed.since, None);
    assert!(!parsed.only_unread);

    let iq = get_iq(
        Element::builder("query", NS_INBOX)
            .attr("since", "1700000")
            .attr("only-unread", "true")
            .build(),
    );
    let parsed = parse_inbox_query(&iq).unwrap();
    assert_eq!(parsed.since, Some(1_700_000));
    assert!(parsed.only_unread);
}

#[test]
fn inbox_mark_read_requires_partner() {
    let iq = set_iq(Element::builder("mark-read", NS_INBOX).build());
    assert_eq!(
        parse_mark_read(&iq),
        Err(InboxError::MissingAttribute("partner"))
    );
}

#[test]
fn inbox_entry_round_trip_mix_and_direct() {
    for kind in [ConversationKind::Direct, ConversationKind::MixChannel] {
        let entry = InboxEntry::new(jid("x@example.com"), kind, "sid", 10)
            .with_unread(2)
            .with_preview("hi");
        let elem = build_entry_element(&entry);
        let parsed = parse_entry_element(&elem).unwrap();
        assert_eq!(parsed, entry);
    }
}

#[test]
fn inbox_query_result_and_mark_read_result() {
    let entry =
        InboxEntry::new(jid("x@example.com"), ConversationKind::Direct, "s", 1).with_unread(3);
    let iq = get_iq(Element::builder("query", NS_INBOX).build());
    let result = build_inbox_query_result(&iq, std::slice::from_ref(&entry), 3);
    match result.payload {
        IqType::Result(Some(e)) => {
            assert_eq!(e.attr("total-unread"), Some("3"));
            assert_eq!(e.children().count(), 1);
        }
        _ => panic!("expected result"),
    }
    let mr = build_mark_read_result(&iq);
    matches!(mr.payload, IqType::Result(None));
}

#[test]
fn inbox_iq_recognition() {
    assert!(is_inbox_iq(&get_iq(
        Element::builder("query", NS_INBOX).build()
    )));
    assert!(is_inbox_iq(&set_iq(
        Element::builder("mark-read", NS_INBOX)
            .attr("partner", "a@example.com")
            .build()
    )));
    assert!(!is_inbox_iq(&get_iq(
        Element::builder("query", "other").build()
    )));
}

#[tokio::test]
async fn inbox_in_memory_storage_end_to_end() {
    let store = InMemoryInboxStorage::new();
    let user = jid("me@example.com");
    store
        .upsert(
            &user,
            InboxEntry::new(jid("a@example.com"), ConversationKind::Direct, "s1", 100)
                .with_preview("yo"),
            true,
        )
        .await
        .unwrap();
    store
        .upsert(
            &user,
            InboxEntry::new(
                jid("g@mix.example.com"),
                ConversationKind::MixChannel,
                "s2",
                200,
            ),
            true,
        )
        .await
        .unwrap();
    let snapshot = store.list(&user).await.unwrap();
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot[0].last_stanza_id, "s2");
    assert_eq!(store.total_unread(&user).await.unwrap(), 2);

    store.mark_read(&user, &jid("a@example.com")).await.unwrap();
    assert_eq!(store.total_unread(&user).await.unwrap(), 1);
}
