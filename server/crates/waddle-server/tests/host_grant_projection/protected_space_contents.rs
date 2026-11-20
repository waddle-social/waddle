use std::time::Duration;

use jid::BareJid;
use minidom::{rxml::xml_ncname, Element};
use waddle_server::permissions::host_grant::{
    FenceAcknowledgement, GrantOperationId, HostGrantAuthority, Reconciliation, RoomAffiliation,
    SetAffiliation,
};
use waddle_ws_test_support::{
    iq::round_trip,
    stanza::{get, is_result, pubsub_items},
    WsXmppClient,
};
use waddle_xmpp::pubsub::{PubSubItem, NS_PUBSUB};
use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};
use xmpp_parsers::iq::Iq;

use super::{bare, host_grant_revocation_support::Replica, subject, Live, OWNER, PEER};

const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NS_DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";

fn discovery(node: &str, namespace: &str) -> Element {
    Element::builder("query", namespace)
        .attr(xml_ncname!("node").to_owned(), node)
        .build()
}

async fn assert_visible_children(
    client: &mut WsXmppClient,
    service: &BareJid,
    node: &str,
    ids: &[&str],
    phase: &str,
) {
    let requests = [
        ("items", pubsub_items(node), "id"),
        ("children", discovery(node, NS_DISCO_ITEMS), "name"),
    ];
    for (request, payload, item_key) in requests {
        let iq = round_trip(
            client,
            get(
                &format!("space-{request}-{phase}"),
                service.as_str(),
                payload,
            ),
        )
        .await;
        let Iq::Result {
            payload: Some(payload),
            ..
        } = iq
        else {
            panic!("Space {request} {phase}: {iq:?}");
        };
        let listing = if request == "items" {
            assert!(payload.is("pubsub", NS_PUBSUB), "{payload:?}");
            payload
                .get_child("items", NS_PUBSUB)
                .expect("items payload")
        } else {
            assert!(payload.is("query", NS_DISCO_ITEMS), "{payload:?}");
            &payload
        };
        assert_eq!(listing.attr("node"), Some(node));
        let exposed: Vec<_> = listing
            .children()
            .map(|item| {
                if request == "items" {
                    assert!(item.is("item", NS_PUBSUB), "{item:?}");
                } else {
                    assert!(item.is("item", NS_DISCO_ITEMS), "{item:?}");
                    assert_eq!(item.attr("jid"), Some(service.as_str()));
                    assert_eq!(item.attr("node"), None);
                }
                item.attr(item_key).expect("room bookmark ID")
            })
            .collect();
        assert_eq!(exposed, ids, "{request} {phase}");
    }
}

pub(super) async fn assert_canonical_space_children(
    live: &Live,
    authority: &HostGrantAuthority,
    replica: &Replica,
    member: &mut WsXmppClient,
) {
    let fixture = &live.fixture;
    let service = bare("spaces.example.com");
    let node = fixture.scope.space_node.as_str();
    let allowed_room = &fixture.scope.room;
    let hidden_scope = fixture.enroll_another().await;
    let hidden_room = &hidden_scope.room;
    assert!(matches!(
        authority
            .reconcile(hidden_scope.lifecycle)
            .await
            .expect("bootstrap hidden room"),
        Reconciliation::Activated(_) | Reconciliation::AlreadyActive(_)
    ));
    let hide = SetAffiliation {
        operation: GrantOperationId::generate(),
        lifecycle: hidden_scope.lifecycle,
        expected_generation: fixture.state(&hidden_scope).await.generation,
        initiator: subject(OWNER),
        room: hidden_room.clone(),
        subject: subject(PEER),
        affiliation: RoomAffiliation::None,
    };
    authority
        .set_affiliation(&hide)
        .await
        .expect("hide second room");
    assert!(matches!(
        authority
            .reconcile(hidden_scope.lifecycle)
            .await
            .expect("project hidden room revoke"),
        Reconciliation::Activated(_) | Reconciliation::AlreadyActive(_)
    ));

    let storage = &replica.state.deps.protocol.pubsub_storage;
    for room in [allowed_room, hidden_room] {
        let bookmark = PubSubItem::new(
            Some(room.to_string()),
            Some(build_bookmark_element(&Bookmark::new(room.clone()))),
        );
        storage
            .publish_item(&service, node, &bookmark, None, false)
            .await
            .expect("store room bookmark in protected Space");
    }
    assert_visible_children(member, &service, node, &[allowed_room.as_str()], "admitted").await;

    let revoke = fixture.command(OWNER, PEER, RoomAffiliation::None).await;
    let receipt = authority
        .set_affiliation(&revoke)
        .await
        .expect("revoke room grant");
    assert_eq!(
        fixture
            .store
            .await_revocation_fence(
                revoke.lifecycle,
                receipt.record().generation,
                Duration::from_secs(10),
            )
            .await
            .expect("room revocation fence"),
        FenceAcknowledgement::Acknowledged { replicas: 2 }
    );
    assert!(matches!(
        authority
            .reconcile(revoke.lifecycle)
            .await
            .expect("project room revoke"),
        Reconciliation::Activated(_) | Reconciliation::AlreadyActive(_)
    ));

    let discovered = round_trip(
        member,
        get(
            "space-disco-after-room-revoke",
            service.as_str(),
            discovery(node, NS_DISCO_INFO),
        ),
    )
    .await;
    assert!(
        is_result(&discovered),
        "Space membership survives: {discovered:?}"
    );
    assert_visible_children(member, &service, node, &[], "room-revoked").await;
}
