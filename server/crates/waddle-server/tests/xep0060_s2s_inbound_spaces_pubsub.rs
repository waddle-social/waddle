//! XEP-0060 across S2S, inbound, on the Spaces service (XEP-0503 version
//! 0.3.0, "Joining a space"; xsf/xeps
//! 4132c531487fe9450ae90b905b8df950d0d478b2): an account at another server
//! subscribes to a Space of this deployment and is notified of what the
//! Space lists, and changes a Space only as ADR-0020 D8 allows.
//!
//! - §6.1 and §6.2: a remote account subscribes and unsubscribes its own
//!   address, within bounds for the Space, the account and its domain. The
//!   subscription is no grant in any room of the Space.
//! - §7.1: an event notification leaves for a subscriber at another server
//!   only for a subscription that account made itself, only while its
//!   domain is admitted and its standing still lets it read the Space, and
//!   only about a room it may be told of; one stanza for each subscribed
//!   address.
//! - §8: publishing, retracting and everything of the `pubsub#owner`
//!   namespace need a standing that is honoured for the request's stream:
//!   an account of a trusted home on a certificate-authenticated stream.
//!   Owner and publisher affiliations are written only for accounts of
//!   this deployment and of trusted homes.
//!
//! `example.com` and `home-b.test` are the Spaces fixture's deployments
//! (`s2s_spaces_support`): loopback TLS in one process, in-memory SQLite.
//! The peer authenticates by certificate under a private CA, so its
//! streams have `pkix` strength. No PostgreSQL, SpiceDB or second process
//! is involved.
#![cfg(feature = "test-support")]

#[allow(dead_code, unused_imports)]
mod s2s_spaces_support;

use minidom::Element;
use s2s_spaces_support::{
    affiliate, affiliation, ask, channel, configure, descendant, disco, error_condition, federate,
    federated_space, field, form_value, hosting, hosting_with, is_event, item_ids, items, jid,
    of_node, owner_of, owner_subscription, publish, pubsub, request, retract, room, rows, space,
    spaces, subscription, Adjust, Federated, Pki, BOB, BOB_PHONE, FIELD_FEDERATED, MALLORY,
    NS_DISCO_ITEMS, NS_PUBSUB_OWNER, PEER, SPACES,
};
use waddle_server::db::s2s_spaces::{
    SpaceSubscriptions, SubscriptionRecord, SubscriptionRequest, MAX_REMOTE_SUBSCRIBERS_PER_SPACE,
    MAX_SUBSCRIPTIONS_PER_REMOTE_ACCOUNT,
};
use waddle_server::permissions::host_grant::StreamEvidence;
use waddle_server::permissions::{
    CheckPermission, ListRelations, Object, ObjectType, Permission, Subject,
};
use waddle_server::s2s::conn::AuthenticationStrength;
use waddle_server::s2s::route::RouterLimits;
use waddle_server::s2s::verify::TrustProfileKind;
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::stanza::{is_error, is_result, set};
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::pubsub::{Affiliation, NodeConfig};
use xmpp_parsers::stanza_error::DefinedCondition;

const DEV: &str = "dev";
const VICTOR: &str = "victor@example.com";

fn ping() -> Element {
    Element::builder("ping", "urn:xmpp:ping").build()
}

/// A federated public Space listing nothing yet, the room `lounge` open to
/// other servers, and `victor`, who owns both, online.
async fn space_with_owner(federated: &Federated) -> WsXmppClient {
    federated_space(federated, DEV).await;
    channel(federated, "lounge", true).await;
    federated.account("victor").await;
    owner_of(federated, DEV, "lounge", VICTOR).await;
    federated.loopback.client("victor", "desk").await
}

async fn subscribe(federated: &Federated, id: &str, from: &str, address: &str) -> Element {
    ask(
        federated,
        request(from, id, true, subscription("subscribe", DEV, address)),
    )
    .await
}

/// Everything `home-b.test` has received once a ping it sends after the
/// case's last step is answered: the answer travels the queue a
/// notification travels, behind it.
async fn settled(federated: &Federated, id: &str) -> Vec<Element> {
    ask(federated, request(BOB_PHONE, id, false, ping())).await;
    federated.peer_received(|_| true)
}

fn events_for(received: &[Element], to: &str) -> Vec<Element> {
    received
        .iter()
        .filter(|stanza| is_event(stanza, DEV, to))
        .cloned()
        .collect()
}

fn recorded(strength: AuthenticationStrength) -> StreamEvidence {
    StreamEvidence::for_test(strength, Some(TrustProfileKind::PrivateCa))
}

async fn node_created_at(federated: &Federated, node: &str) -> i64 {
    federated
        .loopback
        .state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces(), node)
        .await
        .expect("the node is read")
        .expect("the Space exists")
        .created_at
        .timestamp_millis()
}

fn subscriptions(federated: &Federated) -> SpaceSubscriptions {
    SpaceSubscriptions::new(
        federated
            .loopback
            .state
            .deps
            .app_state
            .db_pool
            .global()
            .clone(),
    )
}

/// Record a subscription of `address` to `node` through the store, as one
/// made over a `pkix` stream.
async fn record(federated: &Federated, node: &str, created_at: i64, address: &str) {
    let subscriber = jid(address);
    let account = subscriber.to_bare();
    let outcome = subscriptions(federated)
        .record(
            &spaces(),
            node,
            created_at,
            SubscriptionRequest {
                subscriber: &subscriber,
                account: &account,
                evidence: recorded(AuthenticationStrength::Pkix),
                snapshot: uuid::Uuid::new_v4(),
                now_ms: 0,
            },
        )
        .await
        .expect("the subscription is recorded");
    assert_eq!(outcome, SubscriptionRecord::Recorded, "{address}");
}

// --- XEP-0060 §6.1 and §6.2 -------------------------------------------------

#[tokio::test]
async fn a_remote_account_subscribes_and_unsubscribes_its_own_address() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;

    let subscribed = subscribe(&federated, "sub-1", BOB_PHONE, BOB).await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");
    assert_eq!(subscribed.attr("from"), Some(SPACES));
    let answer = descendant(&subscribed, "subscription").expect("the subscription");
    assert_eq!(answer.attr("node"), Some(DEV));
    assert_eq!(answer.attr("jid"), Some(BOB));
    assert_eq!(answer.attr("subscription"), Some("subscribed"));
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);

    // Subscribing again changes nothing; a full JID of the account is a
    // subscription of its own.
    let again = subscribe(&federated, "sub-2", BOB_PHONE, BOB).await;
    assert_eq!(again.attr("type"), Some("result"), "{again:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);
    let device = subscribe(&federated, "sub-3", BOB_PHONE, BOB_PHONE).await;
    assert_eq!(device.attr("type"), Some("result"), "{device:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 2);

    // §6.1.3.1 and §6.2.3.3: only the sender's own address.
    let other = subscribe(&federated, "sub-4", BOB_PHONE, "mallory@home-b.test").await;
    assert_eq!(error_condition(&other).as_deref(), Some("bad-request"));
    let local = subscribe(&federated, "sub-5", BOB_PHONE, VICTOR).await;
    assert_eq!(error_condition(&local).as_deref(), Some("bad-request"));
    let theirs = ask(
        &federated,
        request(
            MALLORY,
            "unsub-1",
            true,
            subscription("unsubscribe", DEV, BOB),
        ),
    )
    .await;
    assert_eq!(error_condition(&theirs).as_deref(), Some("forbidden"));
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 2);

    let left = ask(
        &federated,
        request(
            BOB_PHONE,
            "unsub-2",
            true,
            subscription("unsubscribe", DEV, BOB),
        ),
    )
    .await;
    assert_eq!(left.attr("type"), Some("result"), "{left:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);
    // §6.2.3.2: no such subscription any more.
    let twice = ask(
        &federated,
        request(
            BOB_PHONE,
            "unsub-3",
            true,
            subscription("unsubscribe", DEV, BOB),
        ),
    )
    .await;
    assert_eq!(
        error_condition(&twice).as_deref(),
        Some("unexpected-request")
    );
}

/// ADR-0020 D4: a subscription to a Space is not a grant in its rooms, and
/// nothing is derived from it.
#[tokio::test]
async fn a_subscription_to_a_space_is_no_grant_in_its_rooms() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    let _victor = space_with_owner(&federated).await;
    s2s_spaces_support::list_in(&federated, DEV, "lounge", None).await;

    let subscribed = subscribe(&federated, "grant-1", BOB_PHONE, BOB).await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");
    let read = ask(&federated, request(BOB_PHONE, "grant-2", false, items(DEV))).await;
    assert_eq!(item_ids(&read), [room("lounge").to_string()]);

    let state = &federated.loopback.state;
    let permissions = &state.deps.app_state.permission_actor;
    for object in [
        Object::new(ObjectType::Space, DEV),
        Object::new(ObjectType::Channel, "lounge"),
    ] {
        let relations = permissions
            .ask(ListRelations {
                subject: Subject::user(BOB),
                object: object.clone(),
            })
            .await
            .expect("the relations are read");
        assert!(relations.is_empty(), "{object:?}: {relations:?}");
        for permission in [Permission::View, Permission::Read, Permission::SendMessage] {
            let check = permissions
                .ask(CheckPermission {
                    subject: Subject::user(BOB),
                    permission: permission.clone(),
                    object: object.clone(),
                })
                .await;
            assert!(
                !matches!(check, Ok(ref response) if response.allowed),
                "{object:?} {permission:?}"
            );
        }
    }
    let affiliation = state
        .deps
        .protocol
        .pubsub_storage
        .affiliation(&spaces(), DEV, &jid(BOB).to_bare())
        .await
        .expect("the affiliation is read");
    assert_eq!(affiliation, Affiliation::None);
    assert_eq!(rows(&federated, "s2s_local_room_seats").await, 0);
}

#[tokio::test]
async fn subscriptions_are_bounded_for_an_account_and_for_a_space() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;

    // The account's bound, spent on other Spaces.
    for index in 0..MAX_SUBSCRIPTIONS_PER_REMOTE_ACCOUNT {
        record(&federated, &format!("other-{index}"), 1, BOB).await;
    }
    let refused = subscribe(&federated, "bound-1", BOB_PHONE, BOB).await;
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("resource-constraint")
    );
    assert_eq!(refused.attr("type"), Some("error"));
    let storage = &federated.loopback.state.deps.protocol.pubsub_storage;
    assert!(storage
        .list_node_subscriptions(&spaces(), DEV)
        .await
        .expect("the subscriptions are read")
        .is_empty());
    // Another account of the domain is not held to it.
    let other = subscribe(&federated, "bound-2", MALLORY, "mallory@home-b.test").await;
    assert_eq!(other.attr("type"), Some("result"), "{other:?}");

    // The Space's bound on subscribers at other servers.
    let created_at = node_created_at(&federated, DEV).await;
    for index in 1..MAX_REMOTE_SUBSCRIBERS_PER_SPACE {
        record(
            &federated,
            DEV,
            created_at,
            &format!("guest-{index}@home-b.test"),
        )
        .await;
    }
    let full = subscribe(
        &federated,
        "bound-3",
        "carol@home-b.test/desk",
        "carol@home-b.test",
    )
    .await;
    assert_eq!(
        error_condition(&full).as_deref(),
        Some("resource-constraint")
    );
    // A subscriber the Space already holds is recorded again whatever the
    // bound.
    let held = subscribe(&federated, "bound-4", MALLORY, "mallory@home-b.test").await;
    assert_eq!(held.attr("type"), Some("result"), "{held:?}");
}

/// One account's requests beyond its share are refused without spending
/// what the other accounts of its domain may ask.
#[tokio::test]
async fn an_accounts_requests_beyond_its_share_are_refused_and_no_one_elses() {
    let pki = Pki::new();
    let federated = hosting_with(
        &pki,
        Adjust {
            limits: RouterLimits {
                space_request_rate_per_account: 1,
                space_request_burst_per_account: 2,
                ..RouterLimits::default()
            },
            ..Adjust::default()
        },
    )
    .await;
    federated_space(&federated, DEV).await;

    for id in ["rate-1", "rate-2"] {
        let answer = ask(&federated, request(BOB_PHONE, id, false, ping())).await;
        assert_eq!(answer.attr("type"), Some("result"), "{answer:?}");
    }
    let limited = ask(&federated, request(BOB_PHONE, "rate-3", false, ping())).await;
    assert_eq!(
        error_condition(&limited).as_deref(),
        Some("resource-constraint")
    );
    let other = ask(&federated, request(MALLORY, "rate-4", false, ping())).await;
    assert_eq!(other.attr("type"), Some("result"), "{other:?}");
}

// --- XEP-0060 §7.1 ------------------------------------------------------------

#[tokio::test]
async fn a_remote_subscriber_is_notified_of_the_rooms_it_may_be_told_of() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    let mut victor = space_with_owner(&federated).await;
    channel(&federated, "backroom", false).await;
    s2s_spaces_support::relate(
        &federated,
        Object::new(ObjectType::Channel, "backroom"),
        "owner",
        VICTOR,
    )
    .await;
    let subscribed = subscribe(&federated, "ev-sub", BOB_PHONE, BOB).await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");

    let published = round_trip(&mut victor, set("ev-pub-1", SPACES, publish(DEV, "lounge"))).await;
    assert!(is_result(&published), "{published:?}");
    let event = federated
        .peer_receives_matching(|stanza| is_event(stanza, DEV, BOB))
        .await;
    assert_eq!(event.attr("type"), Some("headline"));
    assert_eq!(item_ids(&event), [room("lounge").to_string()]);
    let item = descendant(&event, "item").expect("the item");
    assert_eq!(item.attr("publisher"), None);
    assert!(descendant(item, "conference").is_some(), "{event:?}");
    assert!(!String::from(&event).contains(VICTOR));

    // A room that is not open to other servers is not told, when it is
    // listed and when it is taken out again.
    let hidden = round_trip(
        &mut victor,
        set("ev-pub-2", SPACES, publish(DEV, "backroom")),
    )
    .await;
    assert!(is_result(&hidden), "{hidden:?}");
    let unlisted = round_trip(
        &mut victor,
        set("ev-ret-1", SPACES, retract(DEV, "backroom")),
    )
    .await;
    assert!(is_result(&unlisted), "{unlisted:?}");
    let received = settled(&federated, "ev-barrier-1").await;
    assert_eq!(events_for(&received, BOB).len(), 1, "{received:?}");
    assert!(!received
        .iter()
        .any(|stanza| String::from(stanza).contains("backroom")));

    // Taking a told room out of the Space is told.
    let retracted = round_trip(&mut victor, set("ev-ret-2", SPACES, retract(DEV, "lounge"))).await;
    assert!(is_result(&retracted), "{retracted:?}");
    let retraction = federated
        .peer_receives_matching(|stanza| {
            is_event(stanza, DEV, BOB) && descendant(stanza, "retract").is_some()
        })
        .await;
    let retract = descendant(&retraction, "retract").expect("the retraction");
    assert_eq!(retract.attr("id"), Some(room("lounge").as_str()));
}

#[tokio::test]
async fn a_notification_leaves_only_for_a_subscription_the_remote_account_made() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    let mut victor = space_with_owner(&federated).await;

    // The owner records a subscription for an address at another server
    // (XEP-0060 §8.8.2): the service holds it, and nothing is sent for it.
    let added = round_trip(
        &mut victor,
        set(
            "made-1",
            SPACES,
            owner_subscription(DEV, "mallory@home-b.test"),
        ),
    )
    .await;
    assert!(is_result(&added), "{added:?}");
    let subscribed = subscribe(&federated, "made-2", BOB_PHONE, BOB).await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");
    let device = subscribe(&federated, "made-3", BOB_PHONE, BOB_PHONE).await;
    assert_eq!(device.attr("type"), Some("result"), "{device:?}");

    let published = round_trip(&mut victor, set("made-4", SPACES, publish(DEV, "lounge"))).await;
    assert!(is_result(&published), "{published:?}");
    let received = settled(&federated, "made-barrier-1").await;
    // One stanza for each address the account subscribed, none for the
    // address it did not.
    assert_eq!(events_for(&received, BOB).len(), 1, "{received:?}");
    assert_eq!(events_for(&received, BOB_PHONE).len(), 1, "{received:?}");
    assert!(events_for(&received, "mallory@home-b.test").is_empty());
    assert!(!received.iter().any(|stanza| stanza.name() == "message"
        && stanza.attr("to") != Some(BOB)
        && stanza.attr("to") != Some(BOB_PHONE)));

    // A subscription that ended receives nothing more.
    for (id, address) in [("made-5", BOB), ("made-6", BOB_PHONE)] {
        let left = ask(
            &federated,
            request(
                BOB_PHONE,
                id,
                true,
                subscription("unsubscribe", DEV, address),
            ),
        )
        .await;
        assert_eq!(left.attr("type"), Some("result"), "{left:?}");
    }
    let retracted = round_trip(&mut victor, set("made-7", SPACES, retract(DEV, "lounge"))).await;
    assert!(is_result(&retracted), "{retracted:?}");
    let received = settled(&federated, "made-barrier-2").await;
    assert_eq!(events_for(&received, BOB).len(), 1, "{received:?}");
    assert_eq!(events_for(&received, BOB_PHONE).len(), 1, "{received:?}");
}

/// A subscriber's standing is decided again when a notification is handed
/// over: a ban, or a lost membership of a private Space, ends what it
/// receives although its subscription stands.
#[tokio::test]
async fn a_subscriber_that_may_no_longer_read_the_space_is_notified_of_nothing() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    space(&federated, DEV, NodeConfig::spaces_private()).await;
    federate(&federated, DEV).await;
    channel(&federated, "lounge", true).await;
    federated.account("victor").await;
    owner_of(&federated, DEV, "lounge", VICTOR).await;
    let mut victor = federated.loopback.client("victor", "desk").await;
    affiliate(&federated, DEV, BOB, Affiliation::Member).await;
    affiliate(&federated, DEV, "mallory@home-b.test", Affiliation::Member).await;
    for (id, from, address) in [
        ("lost-1", BOB_PHONE, BOB),
        ("lost-2", MALLORY, "mallory@home-b.test"),
    ] {
        let subscribed = subscribe(&federated, id, from, address).await;
        assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");
    }
    // Bob's membership is withdrawn and Mallory is banned, through the
    // store, so that the subscription rows stay as they are.
    affiliate(&federated, DEV, BOB, Affiliation::None).await;
    affiliate(&federated, DEV, "mallory@home-b.test", Affiliation::Outcast).await;

    let published = round_trip(&mut victor, set("lost-3", SPACES, publish(DEV, "lounge"))).await;
    assert!(is_result(&published), "{published:?}");
    let received = settled(&federated, "lost-barrier").await;
    assert!(events_for(&received, BOB).is_empty(), "{received:?}");
    assert!(events_for(&received, "mallory@home-b.test").is_empty());
}

/// ADR-0020 D1 where a notification is handed over: nothing leaves for a
/// domain the snapshot the replica runs does not admit, whatever admitted
/// the subscription.
#[tokio::test]
async fn nothing_leaves_for_a_subscriber_whose_domain_is_no_longer_admitted() {
    let pki = Pki::new();
    let federated = hosting_with(
        &pki,
        Adjust {
            effect_snapshot: Some(|spec| {
                spec.allow = Vec::new();
                spec.private = Vec::new();
                spec.deny = vec![PEER];
            }),
            ..Adjust::default()
        },
    )
    .await;
    let mut victor = space_with_owner(&federated).await;
    // A subscription made while the domain was admitted.
    federated
        .loopback
        .state
        .deps
        .protocol
        .pubsub_storage
        .subscribe(&spaces(), DEV, &jid(BOB))
        .await
        .expect("the service's subscription");
    record(&federated, DEV, node_created_at(&federated, DEV).await, BOB).await;

    let published = round_trip(&mut victor, set("gone-1", SPACES, publish(DEV, "lounge"))).await;
    assert!(is_result(&published), "{published:?}");
    // The barrier is refused, and answered, by the same decision.
    let barrier = ask(
        &federated,
        request(BOB_PHONE, "gone-barrier", false, ping()),
    )
    .await;
    assert_eq!(error_condition(&barrier).as_deref(), Some("forbidden"));
    assert!(federated
        .peer_received(|stanza| stanza.name() == "message")
        .is_empty());
}

// --- XEP-0060 §8 and ADR-0020 D8 -----------------------------------------------

/// The requests that change a Space, each with whether it is a `set`.
fn changes(tag: &str) -> Vec<(String, bool, Element)> {
    let own = |name: &str| pubsub(NS_PUBSUB_OWNER, of_node(name, NS_PUBSUB_OWNER, DEV));
    [
        ("publish", true, publish(DEV, "lounge")),
        ("retract", true, retract(DEV, "lounge")),
        ("configure", false, own("configure")),
        (
            "configure-set",
            true,
            configure(DEV, vec![field("pubsub#access_model", "whitelist")]),
        ),
        ("affiliations", false, own("affiliations")),
        ("affiliations-set", true, affiliation(DEV, BOB, "owner")),
        ("subscriptions", false, own("subscriptions")),
        (
            "subscriptions-set",
            true,
            owner_subscription(DEV, "mallory@home-b.test"),
        ),
        ("purge", true, own("purge")),
        ("delete", true, own("delete")),
    ]
    .into_iter()
    .map(|(kind, set, payload)| ([tag, "-", kind].concat(), set, payload))
    .collect()
}

/// An Owner affiliation of an account whose home is not a trusted home is
/// not honoured: it changes nothing in the Space.
#[tokio::test]
async fn an_owner_at_a_home_that_is_not_trusted_changes_nothing() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    let _victor = space_with_owner(&federated).await;
    s2s_spaces_support::list_in(&federated, DEV, "lounge", None).await;
    affiliate(&federated, DEV, BOB, Affiliation::Owner).await;
    owner_of(&federated, DEV, "lounge", BOB).await;

    for (id, set, payload) in changes("untrusted") {
        let answer = ask(&federated, request(BOB_PHONE, &id, set, payload)).await;
        assert_eq!(
            error_condition(&answer).as_deref(),
            Some("forbidden"),
            "{id}"
        );
    }
    let storage = &federated.loopback.state.deps.protocol.pubsub_storage;
    let stored = storage
        .get_node(&spaces(), DEV)
        .await
        .expect("the node is read")
        .expect("the Space still exists");
    assert_eq!(
        stored.config.access_model,
        waddle_xmpp::pubsub::AccessModel::Open
    );
    assert_eq!(
        storage
            .get_items(&spaces(), DEV, None, &[])
            .await
            .expect("the items are read")
            .len(),
        1
    );
    assert!(storage
        .list_node_subscriptions(&spaces(), DEV)
        .await
        .expect("the subscriptions are read")
        .is_empty());
}

fn trusting_the_peer() -> Adjust {
    Adjust {
        local: |spec| spec.trusted_homes = vec![PEER],
        ..Adjust::default()
    }
}

/// A trusted home's owner administers a Space over its certificate
/// stream: the `pubsub#owner` requests are decided by its Owner
/// affiliation, as a session's are.
#[tokio::test]
async fn a_trusted_homes_owner_administers_a_space() {
    let pki = Pki::new();
    let federated = hosting_with(&pki, trusting_the_peer()).await;
    let _victor = space_with_owner(&federated).await;
    s2s_spaces_support::list_in(&federated, DEV, "lounge", None).await;
    // A member of a trusted home holds no privilege.
    affiliate(&federated, DEV, BOB, Affiliation::Member).await;
    for (id, set, payload) in changes("member") {
        let answer = ask(&federated, request(BOB_PHONE, &id, set, payload)).await;
        assert_eq!(
            error_condition(&answer).as_deref(),
            Some("forbidden"),
            "{id}"
        );
    }

    affiliate(&federated, DEV, BOB, Affiliation::Owner).await;
    let own = |name: &str| pubsub(NS_PUBSUB_OWNER, of_node(name, NS_PUBSUB_OWNER, DEV));
    let form = ask(
        &federated,
        request(BOB_PHONE, "own-1", false, own("configure")),
    )
    .await;
    assert_eq!(form.attr("type"), Some("result"), "{form:?}");
    assert_eq!(
        form_value(&form, "pubsub#access_model").as_deref(),
        Some("open")
    );
    // Whether the Space is open to other servers is not a remote owner's
    // to read or to change.
    assert_eq!(form_value(&form, FIELD_FEDERATED), None);
    let closing = ask(
        &federated,
        request(
            BOB_PHONE,
            "own-2",
            true,
            configure(DEV, vec![field(FIELD_FEDERATED, "0")]),
        ),
    )
    .await;
    assert_eq!(closing.attr("type"), Some("result"), "{closing:?}");
    assert_eq!(rows(&federated, "s2s_federated_spaces").await, 1);

    let listed = ask(
        &federated,
        request(BOB_PHONE, "own-3", false, own("affiliations")),
    )
    .await;
    assert_eq!(listed.attr("type"), Some("result"), "{listed:?}");
    // D8 at issuance, whoever asks: a privileged affiliation for an account
    // of a home that is neither served nor trusted is not written.
    let refused = ask(
        &federated,
        request(
            BOB_PHONE,
            "own-4",
            true,
            affiliation(DEV, "eve@elsewhere.test", "publisher"),
        ),
    )
    .await;
    assert_eq!(error_condition(&refused).as_deref(), Some("not-allowed"));
    let member = ask(
        &federated,
        request(
            BOB_PHONE,
            "own-5",
            true,
            affiliation(DEV, "eve@elsewhere.test", "member"),
        ),
    )
    .await;
    assert_eq!(member.attr("type"), Some("result"), "{member:?}");

    // Placing a room is decided by the Space's `owner` relationship and by
    // authority over the room, which this account does not hold: its
    // PubSub affiliation places nothing.
    let placed = ask(
        &federated,
        request(BOB_PHONE, "own-6", true, publish(DEV, "lounge")),
    )
    .await;
    assert_eq!(error_condition(&placed).as_deref(), Some("forbidden"));

    let deleted = ask(&federated, request(BOB_PHONE, "own-7", true, own("delete"))).await;
    assert_eq!(deleted.attr("type"), Some("result"), "{deleted:?}");
    assert_eq!(rows(&federated, "s2s_federated_spaces").await, 0);
    let gone = ask(
        &federated,
        request(BOB_PHONE, "own-8", false, disco(NS_DISCO_ITEMS, None)),
    )
    .await;
    assert!(descendant(&gone, "item").is_none(), "{gone:?}");
}

/// D8 at issuance for a session's request: a Space's owner writes the
/// owner and publisher affiliations only for accounts of this deployment
/// and of trusted homes.
#[tokio::test]
async fn a_local_owner_grants_privileged_affiliations_to_trusted_homes_only() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    let mut victor = space_with_owner(&federated).await;
    federated.account("wendy").await;
    let storage = &federated.loopback.state.deps.protocol.pubsub_storage;

    for (id, role) in [("grant-1", "owner"), ("grant-2", "publisher")] {
        let refused = round_trip(&mut victor, set(id, SPACES, affiliation(DEV, BOB, role))).await;
        assert!(
            is_error(&refused, DefinedCondition::NotAllowed),
            "{refused:?}"
        );
    }
    assert_eq!(
        storage
            .affiliation(&spaces(), DEV, &jid(BOB).to_bare())
            .await
            .expect("the affiliation is read"),
        Affiliation::None
    );
    for (id, account, role) in [
        ("grant-3", BOB, "member"),
        ("grant-4", BOB, "outcast"),
        ("grant-5", "wendy@example.com", "owner"),
    ] {
        let granted = round_trip(
            &mut victor,
            set(id, SPACES, affiliation(DEV, account, role)),
        )
        .await;
        assert!(is_result(&granted), "{role}: {granted:?}");
    }

    let trusting = hosting_with(&pki, trusting_the_peer()).await;
    let mut victor = space_with_owner(&trusting).await;
    let granted = round_trip(
        &mut victor,
        set("grant-6", SPACES, affiliation(DEV, BOB, "owner")),
    )
    .await;
    assert!(is_result(&granted), "{granted:?}");
}
