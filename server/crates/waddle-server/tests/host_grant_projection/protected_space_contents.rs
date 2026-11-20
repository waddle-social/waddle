use std::time::Duration;

use jid::BareJid;
use minidom::{rxml::xml_ncname, Element};
use waddle_server::permissions::host_grant::{
    FenceAcknowledgement, GrantOperationId, HostGrantAuthority, Reconciliation, RoomAffiliation,
    SetAffiliation,
};
use waddle_ws_test_support::{
    iq::round_trip,
    stanza::{
        get, is_error, is_result, pubsub_items, pubsub_publish, pubsub_retract, result_payload, set,
    },
    WsXmppClient,
};
use waddle_xmpp::pubsub::{PubSubItem, NS_PUBSUB};
use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};
use xmpp_parsers::{iq::Iq, stanza_error::DefinedCondition};

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
    owner: &mut WsXmppClient,
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

    assert_protected_bookmark_writes(
        replica,
        owner,
        member,
        &service,
        node,
        allowed_room,
        hidden_room,
    )
    .await;
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

async fn assert_protected_bookmark_writes(
    replica: &Replica,
    owner: &mut WsXmppClient,
    member: &mut WsXmppClient,
    service: &BareJid,
    node: &str,
    allowed: &BareJid,
    other: &BareJid,
) {
    let bookmark = |room: &BareJid, name: &str| {
        build_bookmark_element(&Bookmark::new(room.clone()).with_name(name))
    };
    let publish = |id: &str, room: &BareJid, name: &str| {
        set(
            id,
            service.as_str(),
            pubsub_publish(node, room.as_str(), bookmark(room, name)),
        )
    };
    let retract =
        |id: &str, room: &BareJid| set(id, service.as_str(), pubsub_retract(node, room.as_str()));
    let name = "Published by the canonical Space owner";
    let storage = &replica.state.deps.protocol.pubsub_storage;
    let denied = round_trip(
        member,
        publish("space-publish-member", allowed, "Untrusted item"),
    )
    .await;
    assert!(is_error(&denied, DefinedCondition::Forbidden), "{denied:?}");
    let foreign = round_trip(owner, publish("space-publish-other-scope", other, name)).await;
    assert!(
        is_error(&foreign, DefinedCondition::Forbidden),
        "{foreign:?}"
    );
    assert!(
        storage
            .get_items(service, node, None, &[])
            .await
            .expect("read stored items")
            .is_empty(),
        "denied publications must leave the node empty"
    );

    let published = round_trip(owner, publish("space-publish-owner", allowed, name)).await;
    let confirmation = result_payload(&published)
        .and_then(|payload| payload.get_child("publish", NS_PUBSUB))
        .expect("publication confirmation");
    assert_eq!(confirmation.attr("node"), Some(node));
    assert_eq!(
        confirmation
            .get_child("item", NS_PUBSUB)
            .and_then(|item| item.attr("id")),
        Some(allowed.as_str())
    );
    let read = round_trip(
        member,
        get(
            "space-read-published-bookmark",
            service.as_str(),
            pubsub_items(node),
        ),
    )
    .await;
    let item = result_payload(&read)
        .and_then(|payload| payload.get_child("items", NS_PUBSUB))
        .and_then(|items| items.get_child("item", NS_PUBSUB))
        .expect("published bookmark read through WebSocket");
    assert_eq!(item.attr("id"), Some(allowed.as_str()));
    assert_eq!(item.children().next(), Some(&bookmark(allowed, name)));

    storage
        .publish_item(
            service,
            node,
            &PubSubItem::new(
                Some(other.to_string()),
                Some(bookmark(other, "Existing bookmark")),
            ),
            None,
            false,
        )
        .await
        .expect("retain a distinct pre-existing bookmark");
    let denied = round_trip(member, retract("space-retract-member", allowed)).await;
    assert!(is_error(&denied, DefinedCondition::Forbidden), "{denied:?}");
    let foreign = round_trip(owner, retract("space-retract-other-scope", other)).await;
    assert!(
        is_error(&foreign, DefinedCondition::Forbidden),
        "{foreign:?}"
    );
    assert_visible_children(
        member,
        service,
        node,
        &[allowed.as_str()],
        "before-exact-retract",
    )
    .await;

    let removed = round_trip(owner, retract("space-retract-owner", allowed)).await;
    assert!(is_result(&removed), "{removed:?}");
    let missing = round_trip(owner, retract("space-retract-absent", allowed)).await;
    assert!(
        is_error(&missing, DefinedCondition::ItemNotFound),
        "{missing:?}"
    );
    let remaining = storage
        .get_items(service, node, None, &[])
        .await
        .expect("read remaining items");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, other.as_str());
    assert_visible_children(member, service, node, &[], "after-exact-retract").await;

    let republished = round_trip(owner, publish("space-republish-owner", allowed, name)).await;
    assert!(is_result(&republished), "{republished:?}");
}
