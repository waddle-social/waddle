//! Wire fixtures for host-owned XEP-0030/0503 data and private account preferences.
use jid::BareJid;
use minidom::Element;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::ready;
use waddle_xmpp_client_community::remote_address::Address;
use waddle_xmpp_client_community::remote_bookmark;
use waddle_xmpp_client_community::remote_community::{self, Error};
use waddle_xmpp_client_community::remote_preferences::{self, WaddleSavedSpace};

const HOST: &str = "spaces.remote.test";
const SPACE: &str = "xmpp:spaces.remote.test?;node=music";
const OWN: &str = "alice@home.test";
const PS: &str = "http://jabber.org/protocol/pubsub";
fn reply(from: &str, content: &str) -> Element {
    format!("<iq xmlns='jabber:client' type='result' from='{from}'>{content}</iq>")
        .parse()
        .unwrap()
}
fn info(node: &str) -> String {
    format!("<query xmlns='http://jabber.org/protocol/disco#info' node='{node}'><identity category='pubsub' type='leaf'/><x xmlns='jabber:x:data' type='result'><field var='FORM_TYPE' type='hidden'><value>http://jabber.org/protocol/pubsub#meta-data</value></field><field var='pubsub#type'><value>urn:xmpp:spaces:0</value></field></x></query>")
}
#[test]
fn rfc5122_normalizes_addresses_and_rejects_non_peer_or_occupant_addresses() {
    assert_eq!(
        Address::parse("XmPp:STAGE@muc.Remote.Test.?join")
            .unwrap()
            .text(),
        "stage@muc.remote.test"
    );
    assert_eq!(
        Address::parse("xmpp:Bücher.remote.test?;node=café%2B%20%26")
            .unwrap()
            .text(),
        "xmpp:xn--bcher-kva.remote.test?;node=caf%C3%A9%2B%20%26"
    );
    for bad in [
        "room@muc.remote.test/nick",
        "xmpp:remote.test?;node=%zz",
        "xmpp:remote.test?;node=%00",
        "xmpp:a@remote.test?;node=music",
        "127.0.0.1",
        "[::1]",
        "s2s",
        "0x7f000001",
        "https://remote.test",
        "service.52",
    ] {
        assert!(Address::parse(bad).is_none(), "{bad}");
    }
}
#[tokio::test]
async fn xep0503_lists_only_canonical_room_bookmarks_and_discards_external_media() {
    let payload=format!("<pubsub xmlns='{PS}'><items node='music'><item id='urn:xmpp:spaces:avatar:metadata:0'><metadata xmlns='urn:xmpp:avatar:metadata'><info url='https://tracker.test/entry'/></metadata></item><item id='STAGE@muc.remote.test'><conference xmlns='urn:xmpp:bookmarks:1' autojoin='true' name='Stage'/></item><item id='stage@muc.remote.test'><conference xmlns='urn:xmpp:bookmarks:1'/></item></items></pubsub>");
    let replies = RefCell::new(VecDeque::from([
        reply(HOST, &info("music")),
        reply(HOST, &payload),
    ]));
    let result = remote_community::describe(
        |iq| {
            assert_eq!(iq.attr("to"), Some(HOST));
            assert_eq!(iq.attr("type"), Some("get"));
            ready(Ok::<_, ()>(replies.borrow_mut().pop_front().unwrap()))
        },
        SPACE,
    )
    .await
    .unwrap();
    assert_eq!(result.kind, "space");
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].address, "stage@muc.remote.test");
    assert!(!format!("{result:?}").contains("tracker"));
}
#[tokio::test]
async fn xep0030_rejects_missing_sender_and_mismatched_hosts_or_nodes() {
    for result in [
        "<iq xmlns='jabber:client' type='result'/>".parse().unwrap(),
        reply("other.test", &info("music")),
    ] {
        assert!(matches!(
            remote_community::describe(|_| ready(Ok::<_, ()>(result.clone())), SPACE).await,
            Err(Error::Untrusted)
        ));
    }
    assert!(matches!(
        remote_community::describe(
            |_| ready(Ok::<_, ()>(reply(HOST, &info("different")))),
            SPACE
        )
        .await,
        Err(Error::Unexpected)
    ));
}
#[tokio::test]
async fn xep0060_subscription_confirms_actor_node_and_pending_state() {
    let own = OWN.parse::<BareJid>().unwrap();
    let send = |iq: Element| {
        let sub = iq
            .get_child("pubsub", PS)
            .unwrap()
            .get_child("subscribe", PS)
            .unwrap();
        assert_eq!(sub.attr("jid"), Some(OWN));
        assert_eq!(sub.attr("node"), Some("music"));
        ready(Ok::<_,()>(reply(HOST,&format!("<pubsub xmlns='{PS}'><subscription node='music' jid='{OWN}' subscription='pending' subid='sid'/></pubsub>"))))
    };
    let result = remote_community::subscribe(send, &own, SPACE, true, None)
        .await
        .unwrap();
    assert_eq!(result.state, "pending");
    assert_eq!(result.subid.as_deref(), Some("sid"));
    let wrong=reply(HOST,&format!("<pubsub xmlns='{PS}'><subscription node='music' jid='mallory@home.test' subscription='subscribed'/></pubsub>"));
    assert!(matches!(
        remote_community::subscribe(
            |_| ready(Ok::<_, ()>(wrong.clone())),
            &own,
            SPACE,
            true,
            None
        )
        .await,
        Err(Error::Untrusted)
    ));
}
#[tokio::test]
async fn xep0223_private_pref_and_xep0402_leave_do_not_share_occupancy() {
    let own = OWN.parse::<BareJid>().unwrap();
    let pref = WaddleSavedSpace {
        address: SPACE.into(),
        name: None,
        follow: true,
        subid: Some("sid".into()),
        revision: None,
    };
    remote_preferences::update(
        |iq| {
            assert!(iq.attr("to").is_none());
            let ps = iq.get_child("pubsub", PS).unwrap();
            let form = ps
                .get_child("publish-options", PS)
                .unwrap()
                .get_child("x", "jabber:x:data")
                .unwrap();
            assert!(form
                .children()
                .any(|f| f.attr("var") == Some("pubsub#access_model")
                    && f.get_child("value", "jabber:x:data").unwrap().text() == "whitelist"));
            let item = ps
                .get_child("publish", PS)
                .unwrap()
                .get_child("item", PS)
                .unwrap();
            assert_eq!(item.attr("id"), Some(SPACE));
            assert_eq!(
                item.get_child("space", "urn:waddle:joined-spaces:0")
                    .unwrap()
                    .attr("subid"),
                Some("sid")
            );
            ready(Ok::<_, ()>(reply(OWN, "")))
        },
        &own,
        &pref,
        false,
    )
    .await
    .unwrap();
    let stored=reply(OWN,&format!("<pubsub xmlns='{PS}'><items node='urn:xmpp:bookmarks:1'><item id='stage@muc.remote.test'><conference xmlns='urn:xmpp:bookmarks:1' autojoin='true' name='My Stage'><nick>MyNick</nick><extensions><notify xmlns='urn:xmpp:notification-settings:0'/></extensions></conference></item></items></pubsub>"));
    remote_bookmark::update(
        |iq| {
            let item = iq
                .get_child("pubsub", PS)
                .unwrap()
                .get_child("publish", PS)
                .unwrap()
                .get_child("item", PS)
                .unwrap();
            let c = item
                .get_child("conference", "urn:xmpp:bookmarks:1")
                .unwrap();
            assert_eq!(c.attr("autojoin"), None);
            assert_eq!(c.attr("name"), Some("My Stage"));
            assert_eq!(
                c.get_child("nick", "urn:xmpp:bookmarks:1").unwrap().text(),
                "MyNick"
            );
            assert!(c
                .get_child("extensions", "urn:xmpp:bookmarks:1")
                .unwrap()
                .get_child("notify", "urn:xmpp:notification-settings:0")
                .is_some());
            ready(Ok::<_, ()>(reply(OWN, "")))
        },
        &own,
        "stage@muc.remote.test",
        None,
        false,
        false,
        Some(&stored),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn xep0049_partial_copy_preserves_document_and_an_authoritative_pep_unfollow_survives_retry()
{
    use waddle_xmpp_client_community::remote_legacy;
    let own: BareJid = OWN.parse().unwrap();
    let legacy="<query xmlns='jabber:iq:private'><spaces xmlns='urn:waddle:joined-spaces:0'><space service='spaces.remote.test' node='disabled' follow='true'/><space service='spaces.remote.test' node='music' follow='true'/><space service='spaces.remote.test' node='last' follow='true'/></spaces></query>";
    let stored = RefCell::new(vec![WaddleSavedSpace {
        address: "xmpp:spaces.remote.test?;node=disabled".into(),
        follow: false,
        revision: Some("off".into()),
        subid: None,
        name: None,
    }]);
    let cleared = std::cell::Cell::new(false);
    let fail = std::cell::Cell::new(true);
    let send = |iq: Element| {
        assert!(iq.attr("to").is_none());
        let result = if iq.get_child("query", "jabber:iq:private").is_some() {
            if iq.attr("type") == Some("set") {
                cleared.set(true);
                assert_eq!(stored.borrow().len(), 3);
            }
            Ok(reply(OWN, legacy))
        } else {
            let item = iq
                .get_child("pubsub", PS)
                .unwrap()
                .get_child("publish", PS)
                .unwrap()
                .get_child("item", PS)
                .unwrap();
            let address = item.attr("id").unwrap();
            assert!(
                !address.ends_with("disabled"),
                "an existing PEP unfollow must never be overwritten"
            );
            if address.ends_with("last") && fail.get() {
                Err(())
            } else {
                stored.borrow_mut().push(WaddleSavedSpace {
                    address: address.into(),
                    follow: true,
                    revision: Some("copied".into()),
                    subid: None,
                    name: None,
                });
                Ok(reply(OWN, ""))
            }
        };
        ready(result)
    };
    let read = || ready(Ok::<_, ()>(stored.borrow().clone()));
    let before = stored.borrow().clone();
    let first = remote_legacy::migrate(send, read, &own, before).await;
    assert!(!cleared.get());
    assert_eq!(first.len(), 2);
    assert!(!first[0].follow);
    fail.set(false);
    let before = stored.borrow().clone();
    let final_list = remote_legacy::migrate(send, read, &own, before).await;
    assert!(cleared.get());
    assert_eq!(final_list.len(), 3);
    assert!(!final_list[0].follow);
}

#[tokio::test]
async fn xep0049_rejects_another_accounts_document_before_pep_write_or_clear() {
    use waddle_xmpp_client_community::remote_legacy;
    let own: BareJid = OWN.parse().unwrap();
    let calls = std::cell::Cell::new(0);
    let result=remote_legacy::migrate(|iq|{
        calls.set(calls.get()+1);
        assert_eq!(iq.attr("type"),Some("get"));
        ready(Ok::<_,()>(reply("other@home.test","<query xmlns='jabber:iq:private'><spaces xmlns='urn:waddle:joined-spaces:0'><space service='example.org' node='bad' follow='true'/></spaces></query>")))
    },||ready(Ok(vec![])),&own,vec![]).await;
    assert!(result.is_empty());
    assert_eq!(calls.get(), 1);
}
