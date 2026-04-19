#![recursion_limit = "512"]

//! Inbox — conversation list + unread counters — dedicated suite.

mod common;

use minidom::Element;
use waddle_xmpp::inbox::storage::{InMemoryInboxStorage, InboxStorage};
use waddle_xmpp::inbox::{ConversationKind, InboxEntry, InboxView};
use waddle_xmpp::xep::xep0430::{
    build_entry_element, build_inbox_query_result, build_mark_read_result, is_inbox_iq,
    parse_entry_element, parse_inbox_query, parse_mark_read, InboxError, NS_INBOX,
};
use xmpp_parsers::iq::{Iq, IqType};

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, RawXmppClient,
    TestServer, DEFAULT_TIMEOUT,
};

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

async fn read_iq_response(client: &mut RawXmppClient) -> std::io::Result<String> {
    let start = std::time::Instant::now();
    let mut response = String::new();
    loop {
        if start.elapsed() > DEFAULT_TIMEOUT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Timeout waiting for IQ response",
            ));
        }
        response.push_str(&client.read(DEFAULT_TIMEOUT).await?);
        if response.contains("</iq>")
            || (response.contains("<iq") && response.contains("/>") && !response.contains("</iq>"))
        {
            return Ok(response);
        }
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
            jid("g@conference.example.com"),
            ConversationKind::MucRoom,
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

    let iq = get_iq(
        Element::builder("query", NS_INBOX)
            .attr("since", "1700000")
            .attr("only-unread", "1")
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
fn inbox_entry_round_trip_muc_and_direct() {
    for kind in [ConversationKind::Direct, ConversationKind::MucRoom] {
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
    assert!(matches!(mr.payload, IqType::Result(None)));
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

#[test]
fn inbox_entry_invalid_integer_reports_raw_value() {
    let bad_last_updated = Element::builder("conversation", NS_INBOX)
        .attr("partner", "x@example.com")
        .attr("kind", "direct")
        .attr("last-stanza-id", "sid")
        .attr("last-updated", "not-a-number")
        .attr("unread", "2")
        .build();
    assert_eq!(
        parse_entry_element(&bad_last_updated),
        Err(InboxError::InvalidInteger("not-a-number".into()))
    );

    let bad_unread = Element::builder("conversation", NS_INBOX)
        .attr("partner", "x@example.com")
        .attr("kind", "direct")
        .attr("last-stanza-id", "sid")
        .attr("last-updated", "10")
        .attr("unread", "NaN")
        .build();
    assert_eq!(
        parse_entry_element(&bad_unread),
        Err(InboxError::InvalidInteger("NaN".into()))
    );
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
                jid("g@conference.example.com"),
                ConversationKind::MucRoom,
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

    store
        .mark_read(&user, &jid("a@example.com"), None)
        .await
        .unwrap();
    assert_eq!(store.total_unread(&user).await.unwrap(), 1);
}

#[tokio::test]
async fn inbox_is_advertised_and_direct_messages_round_trip_over_tcp() {
    init_test_env();

    let server = TestServer::start().await;
    let mut alice = RawXmppClient::connect(server.addr)
        .await
        .expect("alice connect");
    let mut bob = RawXmppClient::connect(server.addr)
        .await
        .expect("bob connect");

    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("alice session");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bob session");
    alice
        .send("<presence xmlns='jabber:client'/>")
        .await
        .expect("alice initial presence");
    bob.send("<presence xmlns='jabber:client'/>")
        .await
        .expect("bob initial presence");

    let disco = disco_info_query(&mut bob, "localhost", "inbox-disco")
        .await
        .expect("disco");
    assert!(
        disco.contains(NS_INBOX),
        "server disco should advertise inbox support: {disco}"
    );

    alice
        .send(
            "<message xmlns='jabber:client' to='bob@localhost' type='chat' id='inbox-dm-1'>\
                <body>Hello over TCP</body>\
             </message>",
        )
        .await
        .expect("send dm");

    let delivered = bob
        .read_until("</message>", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives dm");
    assert!(
        delivered.contains("Hello over TCP"),
        "recipient should receive direct message: {delivered}"
    );
    bob.clear();

    bob.send(
        "<iq xmlns='jabber:client' type='get' to='bob@localhost' id='inbox-get-1'>\
            <query xmlns='urn:xmpp:inbox:0'/>\
         </iq>",
    )
    .await
    .expect("send inbox get");
    let inbox = read_iq_response(&mut bob).await.expect("inbox response");
    assert!(
        inbox.contains("partner=\"alice@localhost\"")
            || inbox.contains("partner='alice@localhost'"),
        "inbox should contain Alice conversation: {inbox}"
    );
    assert!(
        inbox.contains("unread=\"1\"") || inbox.contains("unread='1'"),
        "inbox should show one unread message: {inbox}"
    );
    assert!(
        inbox.contains("Hello over TCP"),
        "inbox preview should include the message body: {inbox}"
    );

    bob.send(
        "<iq xmlns='jabber:client' type='set' to='bob@localhost' id='inbox-set-1'>\
            <mark-read xmlns='urn:xmpp:inbox:0' partner='alice@localhost'/>\
         </iq>",
    )
    .await
    .expect("send mark-read");
    let mark_read = read_iq_response(&mut bob)
        .await
        .expect("mark-read response");
    assert!(
        mark_read.contains("type=\"result\"") || mark_read.contains("type='result'"),
        "mark-read should succeed: {mark_read}"
    );

    bob.send(
        "<iq xmlns='jabber:client' type='get' to='bob@localhost' id='inbox-get-2'>\
            <query xmlns='urn:xmpp:inbox:0' only-unread='true'/>\
         </iq>",
    )
    .await
    .expect("send unread-only inbox get");
    let unread_only = read_iq_response(&mut bob)
        .await
        .expect("unread-only response");
    assert!(
        unread_only.contains("total-unread=\"0\"") || unread_only.contains("total-unread='0'"),
        "mark-read should clear unread totals: {unread_only}"
    );
    assert!(
        !unread_only.contains("<conversation "),
        "unread-only query should be empty after mark-read: {unread_only}"
    );
}

#[tokio::test]
async fn groupchat_messages_project_into_tcp_inbox() {
    init_test_env();

    let server = TestServer::start().await;
    let mut alice = RawXmppClient::connect(server.addr)
        .await
        .expect("alice connect");
    let mut bob = RawXmppClient::connect(server.addr)
        .await
        .expect("bob connect");

    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("alice session");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bob session");

    let room = "inbox-room@muc.localhost";
    join_muc_room(&mut alice, room, "alice")
        .await
        .expect("alice joins");
    join_muc_room(&mut bob, room, "bob")
        .await
        .expect("bob joins");
    let _ = alice
        .read_until("</presence>", DEFAULT_TIMEOUT)
        .await
        .expect("alice sees bob join");
    alice.clear();

    alice
        .send(&format!(
            "<message xmlns='jabber:client' to='{room}' type='groupchat' id='inbox-gc-1'>\
                <body>Hello room</body>\
             </message>"
        ))
        .await
        .expect("send groupchat");
    let _ = alice
        .read_until("</message>", DEFAULT_TIMEOUT)
        .await
        .expect("alice receives echo");
    alice.clear();
    let delivered = bob
        .read_until("</message>", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives room message");
    assert!(
        delivered.contains("Hello room"),
        "room occupant should receive message: {delivered}"
    );
    bob.clear();

    bob.send(
        "<iq xmlns='jabber:client' type='get' to='bob@localhost' id='inbox-room-get'>\
            <query xmlns='urn:xmpp:inbox:0'/>\
         </iq>",
    )
    .await
    .expect("send room inbox get");
    let inbox = read_iq_response(&mut bob)
        .await
        .expect("room inbox response");
    assert!(
        inbox.contains("partner=\"inbox-room@muc.localhost\"")
            || inbox.contains("partner='inbox-room@muc.localhost'"),
        "groupchat inbox entry should target the room JID: {inbox}"
    );
    assert!(
        inbox.contains("kind=\"muc\"") || inbox.contains("kind='muc'"),
        "groupchat inbox entry should be typed as a MUC conversation: {inbox}"
    );
    assert!(
        inbox.contains("unread=\"1\"") || inbox.contains("unread='1'"),
        "room message should increment unread for other occupants: {inbox}"
    );
}
