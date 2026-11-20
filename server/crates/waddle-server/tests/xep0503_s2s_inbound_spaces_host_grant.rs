//! XEP-0503 version 0.3.0 across S2S, inbound, where an enrolled host-grant
//! scope is involved (ADR-0020 D5, D8 and D10; xsf/xeps
//! 4132c531487fe9450ae90b905b8df950d0d478b2).
//!
//! - A Space lists its rooms, and a room an enrolled scope manages is told
//!   to a remote asker only where the room's own host would describe it:
//!   to a principal its canonical grants admit to view it, decided on the
//!   evidence of the stream that carried the request. A stranger is not
//!   told of it, in the Space's items, in its node's `disco#items` or in
//!   the room's own discovery.
//! - The Space node of an enrolled scope is served to nobody: whatever its
//!   `federated` setting says, it answers as a node that does not exist, in
//!   every operation a remote principal may ask of a node, with the error a
//!   node that does not exist gets.
//!
//! - On PostgreSQL, a sweep of the subscription store at the deployment's
//!   bound passes over rows other transactions hold, and a Space's
//!   deletion lock holds off another deletion of that Space only.
//!
//! The deployment is the revocation fixture's on PostgreSQL, with one
//! replica that runs an S2S snapshot admitting `home-b.test` and serves its
//! Spaces to it. The cases need PostgreSQL only. Without it they return
//! early, as every PostgreSQL case of the workspace does; a lane that sets
//! `WADDLE_TEST_REQUIRE_POSTGRES=1` fails them instead, since the fixture
//! reads the test database through
//! `waddle_ws_test_support::require_test_postgres_url`, which panics there.
//! No SpiceDB or second process is involved.
#![cfg(feature = "test-support")]

// The shared fixtures serve several suites; this one uses a subset.
#[allow(dead_code)]
mod host_grant_revocation_support;
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;
#[allow(dead_code)]
#[path = "s2s_inbound_muc_support/managed.rs"]
mod managed;
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

use host_grant_support::{served, subject, OWNER};
use jid::BareJid;
use managed::{Managed, GUEST_PHONE, STRANGER_PHONE};
use minidom::Element;
use s2s_inbound_muc_support::{error_condition, jid};
use waddle_server::db::s2s_spaces::{
    lock_space_deletion, FederatedSpaces, SpaceSubscriptionGrant, SpaceSubscriptions,
    SubscriptionRecord, SubscriptionRequest, MAX_REMOTE_SUBSCRIPTIONS,
};
use waddle_server::permissions::host_grant::{
    BindSpace, FenceAcknowledgement, GrantOperationId, OperatorRevoke, ScopeKey,
    SetSpaceAffiliations, SpaceAffiliationChange, SpaceBookmarkAction, SpaceBookmarkChange,
    SpaceBookmarkVersion, SpaceMembership, StreamEvidence,
};
use waddle_server::s2s::conn::AuthenticationStrength;
use waddle_server::s2s::route::SpacePublication;
use waddle_server::test_support::{host_spaces_for_remote_principals, notify_space_publication};
use waddle_xmpp::pubsub::{build_pubsub_event, NodeConfig, PubSubEvent, PubSubItem};
use waddle_xmpp::xep::xep0402::{build_bookmark_element, Bookmark};
use xmpp_parsers::iq::Iq;

/// The Spaces service of the fixture's deployment.
const SPACES: &str = "spaces.example.com";
/// A Space no scope enrolls.
const LOBBY: &str = "lobby";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NS_DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";

fn spaces() -> BareJid {
    SPACES.parse().expect("the Spaces service")
}

/// The fixture's managed room on a replica that also serves its Spaces to
/// `home-b.test`.
async fn serving_spaces(label: &str) -> Option<Managed> {
    let managed = Managed::start(label, |spec| spec.local.push(SPACES)).await?;
    host_spaces_for_remote_principals(&managed.hosted.state, &managed.hosted.router);
    Some(managed)
}

/// Create the public Space `node` and turn its `federated` setting on.
async fn federated_space(managed: &Managed, node: &str) {
    let state = &managed.hosted.state;
    let storage = &state.deps.protocol.pubsub_storage;
    let (stored, _) = storage
        .get_or_create_node(&spaces(), node)
        .await
        .expect("the Space node");
    storage
        .update_node_config(&spaces(), node, &NodeConfig::spaces_public())
        .await
        .expect("the Space's configuration");
    let enabled = FederatedSpaces::new(state.deps.app_state.db_pool.global().clone())
        .enable(
            &spaces(),
            node,
            stored.created_at.timestamp_millis(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .expect("the setting is written");
    assert!(enabled);
}

fn items(from: &str, id: &str, node: &str) -> Iq {
    Iq::Get {
        from: Some(jid(from)),
        to: Some(jid(SPACES)),
        id: id.to_owned(),
        payload: Element::builder("pubsub", NS_PUBSUB)
            .append(
                Element::builder("items", NS_PUBSUB)
                    .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                    .build(),
            )
            .build(),
    }
}

/// The `disco#items` request of `from` for the Space `node`'s children.
fn children(from: &str, id: &str, node: &str) -> Iq {
    Iq::Get {
        from: Some(jid(from)),
        to: Some(jid(SPACES)),
        id: id.to_owned(),
        payload: Element::builder("query", NS_DISCO_ITEMS)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
            .build(),
    }
}

/// The `name` attributes of a `disco#items` result's items.
fn item_names(answer: &Element) -> Vec<String> {
    answer
        .children()
        .filter(|child| child.is("query", NS_DISCO_ITEMS))
        .flat_map(Element::children)
        .filter(|child| child.name() == "item")
        .filter_map(|item| item.attr("name").map(ToOwned::to_owned))
        .collect()
}

async fn ask(managed: &Managed, iq: Iq) -> Element {
    let id = iq.id().to_owned();
    managed.hosted.peer_sends_to(SPACES, iq).await;
    managed.hosted.answered(&id).await
}

fn item_ids(answer: &Element) -> Vec<String> {
    let mut ids = Vec::new();
    for child in answer.children() {
        if child.name() == "item" {
            ids.extend(child.attr("id").map(ToOwned::to_owned));
        }
        ids.extend(item_ids(child));
    }
    ids
}

#[tokio::test]
async fn a_managed_rooms_bookmark_is_told_only_to_a_principal_its_grants_admit() {
    let Some(managed) = serving_spaces("s2s_spaces_managed").await else {
        return;
    };
    let room = managed.room();
    federated_space(&managed, LOBBY).await;
    // A bookmark of the managed room that a Space no scope enrolls still
    // holds, as one published before the room was enrolled.
    let bookmark = PubSubItem::new(
        Some(room.to_string()),
        Some(build_bookmark_element(
            &Bookmark::new(room.clone()).with_name("managed"),
        )),
    );
    managed
        .hosted
        .state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces(), LOBBY, &bookmark, None, false)
        .await
        .expect("the stored bookmark");

    // The guest holds a membership of the room: its grants admit it to
    // view the room on the evidence of its stream.
    let told = ask(&managed, items(GUEST_PHONE, "managed-1", LOBBY)).await;
    assert_eq!(told.attr("type"), Some("result"), "{told:?}");
    assert_eq!(item_ids(&told), [room.to_string()]);

    // A stranger reads the Space, which is public, and is not told of the
    // room.
    let untold = ask(&managed, items(STRANGER_PHONE, "managed-2", LOBBY)).await;
    assert_eq!(untold.attr("type"), Some("result"), "{untold:?}");
    assert!(item_ids(&untold).is_empty(), "{untold:?}");

    // The node's `disco#items` tells the same: the room's JID as an item
    // name to the guest, and nothing to the stranger.
    let listed = ask(&managed, children(GUEST_PHONE, "managed-4", LOBBY)).await;
    assert_eq!(listed.attr("type"), Some("result"), "{listed:?}");
    assert_eq!(item_names(&listed), [room.to_string()]);
    let unlisted = ask(&managed, children(STRANGER_PHONE, "managed-5", LOBBY)).await;
    assert_eq!(unlisted.attr("type"), Some("result"), "{unlisted:?}");
    assert!(item_names(&unlisted).is_empty(), "{unlisted:?}");

    // Nor is anybody while the room is not open to other servers.
    managed.deployment.replicas[0]
        .federate(&managed.deployment.grants.scope.channel_id, false)
        .await;
    let closed = ask(&managed, items(GUEST_PHONE, "managed-3", LOBBY)).await;
    assert_eq!(closed.attr("type"), Some("result"), "{closed:?}");
    assert!(item_ids(&closed).is_empty(), "{closed:?}");
    let closed = ask(&managed, children(GUEST_PHONE, "managed-6", LOBBY)).await;
    assert_eq!(closed.attr("type"), Some("result"), "{closed:?}");
    assert!(item_names(&closed).is_empty(), "{closed:?}");
    managed.deployment.close().await;
}

/// An element `name` in `namespace` naming `node`.
fn naming(name: &str, namespace: &str, node: &str) -> minidom::element::ElementBuilder {
    Element::builder(name, namespace).attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
}

/// A `pubsub` element of `namespace` holding `child`.
fn pubsub(namespace: &str, child: Element) -> Element {
    Element::builder("pubsub", namespace).append(child).build()
}

/// Every operation a remote principal may ask of the node `node`, each
/// with whether it is a `set`: discovery, reading, subscribing and leaving,
/// placing and removing a room, and each request of the owner namespace.
fn operations(node: &str, account: &str) -> Vec<(&'static str, bool, Element)> {
    let room = "lounge@muc.example.com";
    vec![
        (
            "disco-info",
            false,
            naming("query", NS_DISCO_INFO, node).build(),
        ),
        (
            "disco-items",
            false,
            naming("query", NS_DISCO_ITEMS, node).build(),
        ),
        (
            "items",
            false,
            pubsub(NS_PUBSUB, naming("items", NS_PUBSUB, node).build()),
        ),
        (
            "subscribe",
            true,
            pubsub(
                NS_PUBSUB,
                naming("subscribe", NS_PUBSUB, node)
                    .attr(minidom::rxml::xml_ncname!("jid").to_owned(), account)
                    .build(),
            ),
        ),
        (
            "unsubscribe",
            true,
            pubsub(
                NS_PUBSUB,
                naming("unsubscribe", NS_PUBSUB, node)
                    .attr(minidom::rxml::xml_ncname!("jid").to_owned(), account)
                    .build(),
            ),
        ),
        (
            "publish",
            true,
            pubsub(
                NS_PUBSUB,
                naming("publish", NS_PUBSUB, node)
                    .append(
                        Element::builder("item", NS_PUBSUB)
                            .attr(minidom::rxml::xml_ncname!("id").to_owned(), room)
                            .build(),
                    )
                    .build(),
            ),
        ),
        (
            "retract",
            true,
            pubsub(
                NS_PUBSUB,
                naming("retract", NS_PUBSUB, node)
                    .append(
                        Element::builder("item", NS_PUBSUB)
                            .attr(minidom::rxml::xml_ncname!("id").to_owned(), room)
                            .build(),
                    )
                    .build(),
            ),
        ),
        (
            "configure",
            false,
            pubsub(
                NS_PUBSUB_OWNER,
                naming("configure", NS_PUBSUB_OWNER, node).build(),
            ),
        ),
        (
            "delete",
            true,
            pubsub(
                NS_PUBSUB_OWNER,
                naming("delete", NS_PUBSUB_OWNER, node).build(),
            ),
        ),
        (
            "purge",
            true,
            pubsub(
                NS_PUBSUB_OWNER,
                naming("purge", NS_PUBSUB_OWNER, node).build(),
            ),
        ),
        (
            "affiliations",
            false,
            pubsub(
                NS_PUBSUB_OWNER,
                naming("affiliations", NS_PUBSUB_OWNER, node).build(),
            ),
        ),
        (
            "subscriptions",
            false,
            pubsub(
                NS_PUBSUB_OWNER,
                naming("subscriptions", NS_PUBSUB_OWNER, node).build(),
            ),
        ),
    ]
}

/// `payload` from `from` to the Spaces service, as a `get` or a `set`.
fn request(from: &str, id: String, set: bool, payload: Element) -> Iq {
    let (from, to) = (Some(jid(from)), Some(jid(SPACES)));
    if set {
        Iq::Set {
            from,
            to,
            id,
            payload,
        }
    } else {
        Iq::Get {
            from,
            to,
            id,
            payload,
        }
    }
}

#[tokio::test]
async fn the_space_of_an_enrolled_scope_is_served_to_nobody() {
    let Some(managed) = serving_spaces("s2s_spaces_enrolled").await else {
        return;
    };
    let node = managed.deployment.grants.scope.space_node.clone();
    // Even with a PubSub node of that name whose setting is on.
    federated_space(&managed, &node).await;
    let account = jid(GUEST_PHONE).to_bare().to_string();

    // Every operation is answered as for a node that does not exist, with
    // the very error such a node gets.
    let enrolled = operations(&node, &account);
    let missing = operations("nowhere", &account);
    for ((name, set, asked), (_, _, control)) in enrolled.into_iter().zip(missing) {
        let answer = ask(
            &managed,
            request(GUEST_PHONE, format!("enrolled-{name}"), set, asked),
        )
        .await;
        let expected = ask(
            &managed,
            request(GUEST_PHONE, format!("missing-{name}"), set, control),
        )
        .await;
        assert_eq!(
            error_condition(&answer).as_deref(),
            Some("item-not-found"),
            "{name}: {answer:?}"
        );
        assert_eq!(
            answer.children().find(|child| child.name() == "error"),
            expected.children().find(|child| child.name() == "error"),
            "{name}"
        );
    }
    let listed = ask(
        &managed,
        Iq::Get {
            from: Some(jid(GUEST_PHONE)),
            to: Some(jid(SPACES)),
            id: "enrolled-listing".to_owned(),
            payload: Element::builder("query", NS_DISCO_ITEMS).build(),
        },
    )
    .await;
    assert_eq!(listed.attr("type"), Some("result"), "{listed:?}");
    assert!(item_ids(&listed).is_empty(), "{listed:?}");
    managed.deployment.close().await;
}

#[tokio::test]
async fn protected_space_disco_requires_a_canonical_remote_grant() {
    let Some(managed) = serving_spaces("s2s_protected_space_disco").await else {
        return;
    };
    let fixture = &managed.deployment.grants;
    let node = fixture.scope.space_node.as_str();
    let storage = &managed.hosted.state.deps.protocol.pubsub_storage;
    storage
        .get_or_create_node(&spaces(), node)
        .await
        .expect("create node");
    storage
        .update_node_config(&spaces(), node, &NodeConfig::spaces_private())
        .await
        .expect("whitelisted Space");
    let command = BindSpace {
        operation: GrantOperationId::generate(),
        lifecycle: fixture.scope.lifecycle,
        expected_generation: fixture.state(&fixture.scope).await.generation,
        owner: subject(OWNER),
        federated: true,
    };
    let bound = fixture
        .store
        .bind_space(&command, &served())
        .await
        .expect("bind Space");
    fixture.activate_current(&fixture.scope).await;

    let member = fixture
        .store
        .set_space_affiliations(
            &SetSpaceAffiliations {
                operation: GrantOperationId::generate(),
                lifecycle: command.lifecycle,
                expected_generation: bound.record().generation,
                initiator: subject(OWNER),
                changes: vec![SpaceAffiliationChange {
                    subject: subject(jid(GUEST_PHONE).to_bare().as_str()),
                    affiliation: SpaceMembership::Member,
                }],
            },
            &fixture.policy,
        )
        .await
        .expect("grant Space membership");
    fixture.activate_current(&fixture.scope).await;

    let created_at = storage
        .get_node(&spaces(), node)
        .await
        .expect("read node")
        .expect("Space exists")
        .created_at
        .timestamp_millis();
    let subscriber = jid(GUEST_PHONE).to_bare();
    let subscriptions = SpaceSubscriptions::new(fixture.ingress.db.clone());
    let now_ms = chrono::Utc::now().timestamp_millis();
    let subscription = SubscriptionRequest {
        account: &subscriber,
        evidence: StreamEvidence::for_test(AuthenticationStrength::Pkix, None),
        snapshot: uuid::Uuid::new_v4(),
        now_ms,
    };
    let token = SpaceSubscriptionGrant {
        lifecycle: command.lifecycle,
        incarnation: member.record().authority_incarnation,
        generation: member.record().generation,
    };
    let stale = SpaceSubscriptionGrant {
        generation: token.generation.next().expect("next epoch"),
        ..token
    };
    assert_eq!(
        subscriptions
            .record_protected(&spaces(), node, created_at, subscription, &|_| true, stale)
            .await
            .expect("stale subscription decision")
            .outcome,
        SubscriptionRecord::NotFederated
    );
    assert_eq!(
        subscriptions
            .record_protected(&spaces(), node, created_at, subscription, &|_| true, token)
            .await
            .expect("canonical subscription")
            .outcome,
        SubscriptionRecord::Recorded
    );
    let stored = subscriptions
        .of_node(&spaces(), node, created_at, now_ms)
        .await
        .expect("read the subscription");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].grant, Some(token));

    for (from, expected) in [(GUEST_PHONE, "result"), (STRANGER_PHONE, "error")] {
        let answer = ask(
            &managed,
            request(
                from,
                format!("protected-disco-{expected}"),
                false,
                naming("query", NS_DISCO_INFO, node).build(),
            ),
        )
        .await;
        assert_eq!(answer.attr("type"), Some(expected), "{answer:?}");
        if expected == "error" {
            assert_eq!(error_condition(&answer).as_deref(), Some("item-not-found"));
        } else {
            assert_eq!(
                answer
                    .get_child("query", NS_DISCO_INFO)
                    .and_then(|query| query.attr("node")),
                Some(node),
            );
        }
    }
    let room = managed.room();
    let bookmark = PubSubItem::new(
        Some(room.to_string()),
        Some(build_bookmark_element(
            &Bookmark::new(room.clone()).with_name("managed"),
        )),
    );
    let scope = fixture.state(&fixture.scope).await;
    let change = SpaceBookmarkChange {
        key: ScopeKey::new(spaces(), node).expect("Space key"),
        lifecycle: command.lifecycle,
        expected_incarnation: scope.authority_incarnation,
        expected_generation: scope.generation,
        initiator: subject(OWNER),
        room: room.clone(),
        action: SpaceBookmarkAction::Publish {
            payload: bookmark.payload.clone().expect("bookmark payload"),
        },
    };
    let written = fixture
        .store
        .write_space_bookmark(&change, &fixture.policy)
        .await
        .expect("store the canonical bookmark");
    let version = written
        .versions
        .into_iter()
        .next()
        .expect("publication version");
    let visible = ask(&managed, items(GUEST_PHONE, "protected-items-member", node)).await;
    assert_eq!(visible.attr("type"), Some("result"), "{visible:?}");
    assert_eq!(item_ids(&visible), [room.to_string()]);
    let hidden = ask(
        &managed,
        items(STRANGER_PHONE, "protected-items-stranger", node),
    )
    .await;
    assert_eq!(error_condition(&hidden).as_deref(), Some("item-not-found"));
    let listed = ask(
        &managed,
        children(GUEST_PHONE, "protected-children-member", node),
    )
    .await;
    assert_eq!(listed.attr("type"), Some("result"), "{listed:?}");
    assert_eq!(item_names(&listed), [room.to_string()]);
    let unlisted = ask(
        &managed,
        children(STRANGER_PHONE, "protected-children-stranger", node),
    )
    .await;
    assert_eq!(
        error_condition(&unlisted).as_deref(),
        Some("item-not-found")
    );
    for (from, expected) in [(GUEST_PHONE, "result"), (STRANGER_PHONE, "error")] {
        let subscription = naming("subscribe", NS_PUBSUB, node)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                jid(from).to_bare().to_string(),
            )
            .build();
        let reply = ask(
            &managed,
            request(
                from,
                format!("protected-subscribe-{expected}"),
                true,
                pubsub(NS_PUBSUB, subscription),
            ),
        )
        .await;
        assert_eq!(reply.attr("type"), Some(expected), "{reply:?}");
        if expected == "result" {
            let subscription = reply
                .get_child("pubsub", NS_PUBSUB)
                .and_then(|pubsub| pubsub.get_child("subscription", NS_PUBSUB))
                .expect("subscription result");
            assert_eq!(subscription.attr("subscription"), Some("subscribed"));
            assert_eq!(subscription.attr("jid"), Some(subscriber.as_str()));
        } else {
            assert_eq!(error_condition(&reply).as_deref(), Some("item-not-found"));
        }
    }
    subscriptions
        .remove(&spaces(), node, &subscriber)
        .await
        .expect("legacy removal finishes");
    assert!(!subscriptions
        .remove_protected(&spaces(), node, &subscriber, stale)
        .await
        .expect("stale removal finishes"));
    let still_subscribed = subscriptions
        .of_node(&spaces(), node, created_at, now_ms)
        .await
        .expect("read current preference");
    assert_eq!(still_subscribed.len(), 1);
    assert_eq!(still_subscribed[0].grant, Some(token));

    let key = ScopeKey::new(spaces(), node).expect("qualified Space");
    let gates = &managed.hosted.state.deps.app_state.host_grants;
    let delivery = gates
        .space_subscription_event(&key, &still_subscribed[0])
        .await
        .expect("the subscription's current affiliation can authorize delivery");
    assert_eq!(
        delivery.admit(|affiliation| affiliation),
        Some(waddle_xmpp::pubsub::Affiliation::Member)
    );
    let old_subscription = waddle_server::db::s2s_spaces::SpaceSubscription {
        grant: Some(stale),
        ..still_subscribed[0].clone()
    };
    assert!(gates
        .space_subscription_event(&key, &old_subscription)
        .await
        .is_none());

    let publication = |id: &str, version: &SpaceBookmarkVersion| {
        let mut event = build_pubsub_event(
            &jid(SPACES),
            &jid(GUEST_PHONE),
            &PubSubEvent::new(node, vec![bookmark.clone()]),
        );
        event.id = Some(xmpp_parsers::message::Id(id.to_owned()));
        SpacePublication {
            service: spaces(),
            node: node.to_owned(),
            node_created_at_ms: created_at,
            event,
            bookmark_version: Some(version.clone()),
        }
    };
    assert_eq!(
        notify_space_publication(
            &managed.hosted.state,
            &managed.hosted.router,
            &publication("protected-space-event", &version),
        )
        .await,
        1
    );
    let event = managed.hosted.answered("protected-space-event").await;
    assert_eq!(event.name(), "message");
    assert_eq!(event.attr("to"), Some(subscriber.as_str()));

    let latest = fixture
        .store
        .write_space_bookmark(&change, &fixture.policy)
        .await
        .expect("republish the same bookmark")
        .versions
        .into_iter()
        .next()
        .expect("replacement version");
    assert_ne!(latest.operation, version.operation);
    for offered in [
        publication("superseded-space-event", &version),
        publication("latest-space-event", &latest),
    ] {
        assert_eq!(
            notify_space_publication(&managed.hosted.state, &managed.hosted.router, &offered).await,
            1,
            "the transport must decide the queued version",
        );
    }
    let current = managed.hosted.answered("latest-space-event").await;
    assert_eq!(current.attr("to"), Some(subscriber.as_str()));
    assert!(managed
        .hosted
        .captured
        .stanzas()
        .iter()
        .all(|stanza| stanza.attr("id") != Some("superseded-space-event")));

    let mut retract = change.clone();
    retract.action = SpaceBookmarkAction::Retract;
    let retracted = fixture
        .store
        .write_space_bookmark(&retract, &fixture.policy)
        .await
        .expect("retract bookmark")
        .versions
        .remove(0);
    assert!(!retracted.present);
    let mut retraction = publication("current-space-retraction", &retracted);
    retraction.event = waddle_xmpp_core::build_pubsub_retract_event(
        &jid(SPACES),
        &jid(GUEST_PHONE),
        node,
        room.as_str(),
    );
    retraction.event.id = Some(xmpp_parsers::message::Id(
        "current-space-retraction".to_owned(),
    ));
    assert_eq!(
        notify_space_publication(&managed.hosted.state, &managed.hosted.router, &retraction).await,
        1
    );
    managed.hosted.answered("current-space-retraction").await;

    let latest = fixture
        .store
        .write_space_bookmark(&change, &fixture.policy)
        .await
        .expect("restore bookmark")
        .versions
        .remove(0);
    retraction.event.id = Some(xmpp_parsers::message::Id(
        "superseded-space-retraction".to_owned(),
    ));
    for offered in [retraction, publication("restored-space-event", &latest)] {
        assert_eq!(
            notify_space_publication(&managed.hosted.state, &managed.hosted.router, &offered).await,
            1
        );
    }
    managed.hosted.answered("restored-space-event").await;
    assert!(managed
        .hosted
        .captured
        .stanzas()
        .iter()
        .all(|stanza| stanza.attr("id") != Some("superseded-space-retraction")));

    // A concurrent removal holds the row before the event is queued. The
    // connection's locked recheck must see the deletion, even though the
    // notification worker admitted the pre-commit subscription snapshot.
    let mut removal = fixture
        .ingress
        .db
        .begin()
        .await
        .expect("removal transaction");
    let mut rows = removal
        .query(
            "SELECT 1 FROM s2s_space_subscriptions WHERE subscriber_jid = ? FOR UPDATE",
            waddle_server::db_params![subscriber.as_str()],
        )
        .await
        .expect("lock preference");
    assert!(rows.next().await.expect("subscription row").is_some());
    drop(rows);
    assert_eq!(
        notify_space_publication(
            &managed.hosted.state,
            &managed.hosted.router,
            &publication("removed-space-event", &latest),
        )
        .await,
        1
    );
    removal
        .execute(
            "DELETE FROM s2s_space_subscriptions WHERE subscriber_jid = ?",
            waddle_server::db_params![subscriber.as_str()],
        )
        .await
        .expect("delete locked preference");
    removal.commit().await.expect("commit removal");
    let barrier = ask(&managed, items(GUEST_PHONE, "post-event-barrier", node)).await;
    assert_eq!(barrier.attr("type"), Some("result"));
    assert!(managed
        .hosted
        .captured
        .stanzas()
        .iter()
        .all(|stanza| stanza.attr("id") != Some("removed-space-event")));
    let subscription = SubscriptionRequest {
        evidence: still_subscribed[0].evidence,
        snapshot: still_subscribed[0].snapshot,
        ..subscription
    };
    assert_eq!(
        subscriptions
            .record_protected(&spaces(), node, created_at, subscription, &|_| true, token)
            .await
            .expect("subscribe again")
            .outcome,
        SubscriptionRecord::Recorded
    );

    // Hold a queued notification until the room's scope acknowledges a
    // revoke. The Space subscription itself remains in place.
    let mut held = fixture.ingress.db.begin().await.expect("hold preference");
    let mut rows = held
        .query(
            "SELECT 1 FROM s2s_space_subscriptions WHERE subscriber_jid = ? FOR UPDATE",
            waddle_server::db_params![subscriber.as_str()],
        )
        .await
        .expect("lock preference before enqueue");
    assert!(rows.next().await.expect("held preference").is_some());
    drop(rows);
    assert_eq!(
        notify_space_publication(
            &managed.hosted.state,
            &managed.hosted.router,
            &publication("revoked-room-event", &latest),
        )
        .await,
        1
    );
    let revoked = fixture
        .store
        .operator_revoke(
            &OperatorRevoke {
                operation: GrantOperationId::generate(),
                lifecycle: command.lifecycle,
                room: room.clone(),
                subject: subject(subscriber.as_str()),
                ban: false,
            },
            &served(),
        )
        .await
        .expect("revoke the remote member's room view");
    let acknowledged = fixture
        .store
        .await_revocation_fence(
            command.lifecycle,
            revoked.record().generation,
            std::time::Duration::from_secs(10),
        )
        .await
        .expect("revocation fence");
    assert_eq!(
        acknowledged,
        FenceAcknowledgement::Acknowledged { replicas: 1 }
    );
    assert!(
        delivery.admit(|_| ()).is_none(),
        "the retained generation is stale"
    );
    held.rollback().await.expect("release the preference lock");
    let barrier = ask(
        &managed,
        items(GUEST_PHONE, "post-revoke-event-barrier", node),
    )
    .await;
    assert_eq!(barrier.attr("type"), Some("result"), "{barrier:?}");
    assert!(item_ids(&barrier).is_empty());
    assert!(managed
        .hosted
        .captured
        .stanzas()
        .iter()
        .all(|stanza| stanza.attr("id") != Some("revoked-room-event")));
    assert_eq!(
        notify_space_publication(
            &managed.hosted.state,
            &managed.hosted.router,
            &publication("post-revoke-room-event", &latest),
        )
        .await,
        0,
    );

    for (attempt, expected) in [("first", "result"), ("repeat", "error")] {
        let unsubscribe = naming("unsubscribe", NS_PUBSUB, node)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                subscriber.as_str(),
            )
            .attr(minidom::rxml::xml_ncname!("subid").to_owned(), "unused")
            .build();
        let reply = ask(
            &managed,
            request(
                GUEST_PHONE,
                format!("protected-unsubscribe-{attempt}"),
                true,
                pubsub(NS_PUBSUB, unsubscribe),
            ),
        )
        .await;
        assert_eq!(reply.attr("type"), Some(expected), "{reply:?}");
        if expected == "error" {
            assert_eq!(
                error_condition(&reply).as_deref(),
                Some("unexpected-request")
            );
        }
    }
    assert!(subscriptions
        .of_node(&spaces(), node, created_at, now_ms)
        .await
        .expect("read after unsubscribe")
        .is_empty());
    let membership = |generation, affiliation| SetSpaceAffiliations {
        operation: GrantOperationId::generate(),
        lifecycle: command.lifecycle,
        expected_generation: generation,
        initiator: subject(OWNER),
        changes: vec![SpaceAffiliationChange {
            subject: subject(subscriber.as_str()),
            affiliation,
        }],
    };
    let current = fixture.state(&fixture.scope).await.generation;
    let revoked = fixture
        .store
        .set_space_affiliations(&membership(current, SpaceMembership::None), &fixture.policy)
        .await
        .expect("revoke Space membership");
    let generation = revoked.record().generation;
    let acknowledged = fixture
        .store
        .await_revocation_fence(
            command.lifecycle,
            generation,
            std::time::Duration::from_secs(10),
        )
        .await
        .expect("Space revocation fence");
    assert_eq!(
        acknowledged,
        FenceAcknowledgement::Acknowledged { replicas: 1 }
    );
    fixture.activate_current(&fixture.scope).await;
    let denied = ask(
        &managed,
        request(
            GUEST_PHONE,
            "protected-disco-revoked".to_owned(),
            false,
            naming("query", NS_DISCO_INFO, node).build(),
        ),
    )
    .await;
    assert_eq!(error_condition(&denied).as_deref(), Some("item-not-found"));

    fixture
        .store
        .set_space_affiliations(
            &membership(generation, SpaceMembership::Member),
            &fixture.policy,
        )
        .await
        .expect("regrant Space membership");
    fixture.activate_current(&fixture.scope).await;
    let restored = ask(
        &managed,
        request(
            GUEST_PHONE,
            "protected-disco-regranted".to_owned(),
            false,
            naming("query", NS_DISCO_INFO, node).build(),
        ),
    )
    .await;
    assert_eq!(restored.attr("type"), Some("result"), "{restored:?}");
    assert_eq!(
        subscriptions
            .record_protected(&spaces(), node, created_at, subscription, &|_| true, token)
            .await
            .expect("the old affiliation cannot subscribe")
            .outcome,
        SubscriptionRecord::NotFederated
    );
    managed.deployment.close().await;
}

// --- The Spaces stores on PostgreSQL -----------------------------------------
//
// The fixture's deployment runs on PostgreSQL, where the subscription store
// and a Space's deletion take advisory locks and a sweep of the whole store
// passes over rows other transactions hold. SQLite runs one writer at a
// time and the other Spaces suites cover it.

/// Fill the store with as many current remote subscriptions as the
/// deployment's bound and two lapsed ones, over 512 Spaces: the rows
/// `guest-0` and `guest-1` of `d0000.test` have lapsed, seventeen rows go
/// to each domain `dNNNN.test` (3,855 of them) and the last sixteen to
/// `zz-gone.test`, so that one sweep reads every domain. The number of
/// rows, the last sixteen of which are `zz-gone.test`'s.
async fn fill_the_store(managed: &Managed, now: i64) -> i64 {
    let rows = i64::try_from(MAX_REMOTE_SUBSCRIPTIONS).expect("the bound fits") + 2;
    managed
        .hosted
        .state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("a connection")
        .execute(
            "INSERT INTO s2s_space_subscriptions \
             (service_jid, node_name, subscriber_jid, account_jid, subscriber_domain, \
              node_created_at_ms, strength, trust_profile, snapshot_generation, \
              subscribed_at_ms) \
             SELECT 'spaces.example.com', 'seed-' || (i % 512), \
             'guest-' || i || '@' || domain, 'guest-' || i || '@' || domain, domain, \
             0, 'pkix', NULL, '00000000-0000-0000-0000-000000000000', \
             CASE WHEN i < 2 THEN 0 ELSE ? END \
             FROM (SELECT i, CASE WHEN i >= ? - 16 THEN 'zz-gone.test' \
             ELSE 'd' || lpad((i / 17)::text, 4, '0') || '.test' END AS domain \
             FROM generate_series(0, ? - 1) AS i) AS seeded",
            vec![now.into(), rows.into(), rows.into()],
        )
        .await
        .expect("the store is filled");
    rows
}

/// At the deployment's bound a subscription sweeps the whole store on
/// PostgreSQL: the lapsed rows first, then the rows of a domain that is no
/// longer admitted, passing over a row another transaction holds rather
/// than waiting for it, and lists what it removed.
#[tokio::test]
async fn a_sweep_of_the_whole_store_passes_over_rows_another_transaction_holds() {
    let Some(managed) = serving_spaces("s2s_spaces_sweep").await else {
        return;
    };
    federated_space(&managed, LOBBY).await;
    let db = managed.hosted.state.deps.app_state.db_pool.global();
    let created_at = managed
        .hosted
        .state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces(), LOBBY)
        .await
        .expect("the node is read")
        .expect("the Space exists")
        .created_at
        .timestamp_millis();
    let now = chrono::Utc::now().timestamp_millis();
    let filled = fill_the_store(&managed, now).await;

    // Another transaction holds one of `zz-gone.test`'s rows.
    let mut holder = db.begin().await.expect("a transaction");
    holder
        .query(
            "SELECT 1 FROM s2s_space_subscriptions WHERE subscriber_jid = ? FOR UPDATE",
            vec![format!("guest-{}@zz-gone.test", filled - 1).into()],
        )
        .await
        .expect("the row is held");

    let carol = jid("carol@new.test").to_bare();
    let recording = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        SpaceSubscriptions::new(db.clone()).record(
            &spaces(),
            LOBBY,
            created_at,
            SubscriptionRequest {
                account: &carol,
                evidence: StreamEvidence::for_test(AuthenticationStrength::Pkix, None),
                snapshot: uuid::Uuid::new_v4(),
                now_ms: now,
            },
            &|domain| domain != "zz-gone.test",
        ),
    )
    .await
    .expect("the sweep waits for no row")
    .expect("the store answers");
    holder.rollback().await.expect("the row is released");

    assert_eq!(recording.outcome, SubscriptionRecord::Recorded);
    let mut removed: Vec<String> = recording
        .removed
        .iter()
        .map(|gone| gone.account.to_string())
        .collect();
    removed.sort();
    let mut expected: Vec<String> = ["guest-0@d0000.test", "guest-1@d0000.test"]
        .into_iter()
        .map(str::to_owned)
        .chain((filled - 16..filled - 1).map(|i| format!("guest-{i}@zz-gone.test")))
        .collect();
    expected.sort();
    assert_eq!(removed, expected);
    managed.deployment.close().await;
}

/// On PostgreSQL a Space's deletion lock holds off another deletion of the
/// Space, on any replica, until it is released, and no deletion of another
/// Space.
#[tokio::test]
async fn a_spaces_deletion_lock_holds_off_another_deletion_of_it_only() {
    let Some(managed) = serving_spaces("s2s_spaces_deletion_lock").await else {
        return;
    };
    let db = managed.hosted.state.deps.app_state.db_pool.global();
    let first = lock_space_deletion(db, &spaces(), LOBBY)
        .await
        .expect("the lock is taken");
    let other = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        lock_space_deletion(db, &spaces(), "other"),
    )
    .await
    .expect("another Space's deletion is not held off")
    .expect("the lock is taken");
    other.release().await;

    let service = spaces();
    let mut second = Box::pin(lock_space_deletion(db, &service, LOBBY));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), &mut second)
            .await
            .is_err(),
        "a second deletion of the Space waits"
    );
    first.release().await;
    let second = tokio::time::timeout(std::time::Duration::from_secs(10), second)
        .await
        .expect("the second deletion runs once the first is released")
        .expect("the lock is taken");
    second.release().await;
    managed.deployment.close().await;
}
