//! XEP-0060 across S2S, inbound, on the Spaces service (XEP-0503 version
//! 0.3.0, "Joining a space"; xsf/xeps
//! 4132c531487fe9450ae90b905b8df950d0d478b2): an account at another server
//! subscribes to a Space of this deployment and is notified of what the
//! Space lists, and changes a Space only as ADR-0020 D8 allows.
//!
//! - §6.1 and §6.2: a remote account subscribes and unsubscribes itself,
//!   within bounds for the Space, for its domain in the Space, for the
//!   account and for its domain; the service keeps one subscription for an
//!   account, under its bare JID. Leaving is possible once a Space is no
//!   longer open to the account. The subscription is no grant in any room
//!   of the Space. It ends when the owner removes it, bans the account or
//!   closes the Space to other servers, and lapses after thirty days
//!   without being made again.
//! - §7.1: an event notification leaves for a subscriber at another server
//!   only for a subscription that account made itself, only while its
//!   domain is admitted and its standing still lets it read the Space, and
//!   only about a room it may be told of; one stanza for each subscribed
//!   account. Notifications are decided off the publisher's request, so
//!   each case waits for the Spaces host to settle before it reads them.
//! - §8: publishing, retracting and everything of the `pubsub#owner`
//!   namespace need a standing that is honoured for the request's stream:
//!   an account of a trusted home on a certificate-authenticated stream.
//!   Owner and publisher affiliations are written only for accounts of
//!   this deployment and of trusted homes, and an owner's standing is not
//!   honoured on evidence of `dialback` strength. A Space deleted through
//!   PubSub, by a remote owner as by a session, takes its rooms out of it
//!   and leaves them standing (only the admin command `spaces:delete`
//!   destroys rooms); purging its items takes its rooms out of it too.
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
    of_node, owner_of, owner_subscription, owner_subscription_as, publish, pubsub, request,
    retract, room, rows, space, spaces, subscription, Adjust, Federated, Pki, BOB, BOB_PHONE,
    FIELD_FEDERATED, LOCAL, MALLORY, NS_DISCO_ITEMS, NS_PUBSUB_OWNER, PEER, SPACES,
};
use waddle_server::db::actor::DbExecute;
use waddle_server::db::s2s_spaces::{
    FederatedSpaces, SpaceSubscriptions, SubscriptionRecord, SubscriptionRequest,
    MAX_REMOTE_SUBSCRIBERS_PER_DOMAIN_AND_SPACE, MAX_REMOTE_SUBSCRIBERS_PER_SPACE,
    MAX_REMOTE_SUBSCRIPTIONS, MAX_SUBSCRIPTIONS_PER_REMOTE_ACCOUNT, SUBSCRIPTION_LIFETIME_MS,
};
use waddle_server::permissions::host_grant::StreamEvidence;
use waddle_server::permissions::{
    CheckPermission, ListRelations, ListSubjects, Object, ObjectType, Permission, Relation, Subject,
};
use waddle_server::s2s::conn::AuthenticationStrength;
use waddle_server::s2s::route::RouterLimits;
use waddle_server::s2s::verify::TrustProfileKind;
use waddle_server::space_identity::SpaceNode;
use waddle_server::spaces_metadata::SpaceMetadata;
use waddle_server::test_support::{
    hold_space_subscription_after_record, spaces_publications_settled,
};
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

/// Everything `home-b.test` has received once the Spaces host has decided
/// every publication of the case and a ping it sends after that is
/// answered: the answer travels the queue a notification travels, behind
/// it.
async fn settled(federated: &Federated, id: &str) -> Vec<Element> {
    spaces_publications_settled(&federated.router).await;
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

/// What a stream of `strength` proves of the peer: a certificate under the
/// private CA for `pkix`, nothing of one for `dialback`.
fn recorded(strength: AuthenticationStrength) -> StreamEvidence {
    let profile =
        matches!(strength, AuthenticationStrength::Pkix).then_some(TrustProfileKind::PrivateCa);
    StreamEvidence::for_test(strength, profile)
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

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn federated_spaces(federated: &Federated) -> FederatedSpaces {
    FederatedSpaces::new(
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

/// Offer the store a subscription of `account` to `node`, created at
/// `created_at`, made at `at` on evidence of `strength`, with `admitted`
/// as the domains admitted now.
async fn offer(
    federated: &Federated,
    node: &str,
    created_at: i64,
    account: &str,
    at: i64,
    strength: AuthenticationStrength,
    admitted: &(dyn Fn(&str) -> bool + Send + Sync),
) -> SubscriptionRecord {
    let account = jid(account).to_bare();
    subscriptions(federated)
        .record(
            &spaces(),
            node,
            created_at,
            SubscriptionRequest {
                account: &account,
                evidence: recorded(strength),
                snapshot: uuid::Uuid::new_v4(),
                now_ms: at,
            },
            admitted,
        )
        .await
        .expect("the store answers")
        .outcome
}

/// Record a subscription of `account` to `node` through the store, as one
/// made now over a `pkix` stream. The node's `federated` setting is turned
/// on for `created_at` first, as a subscription is recorded only for a
/// Space that is open.
async fn record(federated: &Federated, node: &str, created_at: i64, account: &str) {
    assert!(federated_spaces(federated)
        .enable(&spaces(), node, created_at, now_ms())
        .await
        .expect("the setting is written"));
    let outcome = offer(
        federated,
        node,
        created_at,
        account,
        now_ms(),
        AuthenticationStrength::Pkix,
        &|_| true,
    )
    .await;
    assert_eq!(outcome, SubscriptionRecord::Recorded, "{account}");
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

    // The service keeps one subscription for an account, under its bare
    // JID: subscribing again, with the bare JID or with one of the
    // account's full JIDs, is the same subscription and is answered as
    // such.
    for (id, address) in [("sub-2", BOB), ("sub-3", BOB_PHONE)] {
        let again = subscribe(&federated, id, BOB_PHONE, address).await;
        assert_eq!(again.attr("type"), Some("result"), "{again:?}");
        let answer = descendant(&again, "subscription").expect("the subscription");
        assert_eq!(answer.attr("jid"), Some(BOB), "{address}");
        assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);
    }
    let storage = &federated.loopback.state.deps.protocol.pubsub_storage;
    assert_eq!(
        storage
            .list_node_subscriptions(&spaces(), DEV)
            .await
            .expect("the subscriptions are read")
            .len(),
        1
    );

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
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);

    let left = ask(
        &federated,
        request(
            BOB_PHONE,
            "unsub-2",
            true,
            subscription("unsubscribe", DEV, BOB_PHONE),
        ),
    )
    .await;
    assert_eq!(left.attr("type"), Some("result"), "{left:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);
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
    let condition = descendant(&twice, "not-subscribed").expect("the PubSub condition");
    assert_eq!(condition.ns(), s2s_spaces_support::NS_PUBSUB_ERRORS);
}

/// Leaving only reduces what an account receives, so it is possible once
/// the Space is no longer open to the account; the answer is then the one
/// a node that does not exist gets.
#[tokio::test]
async fn an_account_leaves_a_space_that_is_no_longer_open_to_it() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    space(&federated, DEV, NodeConfig::spaces_private()).await;
    federate(&federated, DEV).await;
    affiliate(&federated, DEV, BOB, Affiliation::Member).await;
    let subscribed = subscribe(&federated, "left-1", BOB_PHONE, BOB).await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");
    affiliate(&federated, DEV, BOB, Affiliation::None).await;

    let left = ask(
        &federated,
        request(
            BOB_PHONE,
            "left-2",
            true,
            subscription("unsubscribe", DEV, BOB),
        ),
    )
    .await;
    let missing = ask(
        &federated,
        request(
            BOB_PHONE,
            "left-3",
            true,
            subscription("unsubscribe", "nowhere", BOB),
        ),
    )
    .await;
    assert_eq!(error_condition(&left).as_deref(), Some("item-not-found"));
    assert_eq!(
        left.children().find(|child| child.name() == "error"),
        missing.children().find(|child| child.name() == "error")
    );
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);
    let storage = &federated.loopback.state.deps.protocol.pubsub_storage;
    assert!(storage
        .list_node_subscriptions(&spaces(), DEV)
        .await
        .expect("the subscriptions are read")
        .is_empty());
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
async fn subscriptions_are_bounded_for_an_account_and_for_its_domain_in_a_space() {
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

    // The bound on the subscribers one domain holds in a Space.
    let created_at = node_created_at(&federated, DEV).await;
    for index in 1..MAX_REMOTE_SUBSCRIBERS_PER_DOMAIN_AND_SPACE {
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

/// The bound on a Space's remote subscribers is exact and counts only the
/// rows of domains admitted now: rows of a domain no longer admitted are
/// removed when they would keep an admitted account out. Lapsed rows count
/// for nothing and name no subscriber; subscribing again renews a row.
#[tokio::test]
async fn a_spaces_bound_counts_admitted_and_current_subscriptions_only() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    let created_at = node_created_at(&federated, DEV).await;
    let now = now_ms();
    let only_admitted = |domain: &str| domain != "gone.test";

    // Fill the Space from eight domains, one of which is no longer
    // admitted, within the bound each domain holds in a Space.
    let mut filled = 0;
    'fill: for domain in 0..8 {
        let domain = if domain == 0 {
            "gone.test".to_owned()
        } else {
            format!("d{domain}.test")
        };
        for index in 0..MAX_REMOTE_SUBSCRIBERS_PER_DOMAIN_AND_SPACE {
            if filled == MAX_REMOTE_SUBSCRIBERS_PER_SPACE {
                break 'fill;
            }
            let outcome = offer(
                &federated,
                DEV,
                created_at,
                &format!("guest-{index}@{domain}"),
                now,
                AuthenticationStrength::Pkix,
                &|_| true,
            )
            .await;
            assert_eq!(outcome, SubscriptionRecord::Recorded, "{domain} {index}");
            filled += 1;
        }
    }
    assert_eq!(filled, MAX_REMOTE_SUBSCRIBERS_PER_SPACE);
    // While every domain is admitted, the Space is full.
    let full = offer(
        &federated,
        DEV,
        created_at,
        "carol@d9.test",
        now,
        AuthenticationStrength::Pkix,
        &|_| true,
    )
    .await;
    assert_eq!(full, SubscriptionRecord::Bounds);
    // Once one is not, its rows give way.
    let admitted = offer(
        &federated,
        DEV,
        created_at,
        "carol@d9.test",
        now,
        AuthenticationStrength::Pkix,
        &only_admitted,
    )
    .await;
    assert_eq!(admitted, SubscriptionRecord::Recorded);
    let rows_now = subscriptions(&federated)
        .of_node(&spaces(), DEV, created_at, now)
        .await
        .expect("the subscriptions are read");
    assert!(rows_now
        .iter()
        .all(|row| row.account.domain().as_str() != "gone.test"));

    // A row older than the lifetime names no subscriber and holds no place.
    let later = now + SUBSCRIPTION_LIFETIME_MS + 1;
    assert!(subscriptions(&federated)
        .of_node(&spaces(), DEV, created_at, later)
        .await
        .expect("the subscriptions are read")
        .is_empty());
    let renewed = offer(
        &federated,
        DEV,
        created_at,
        "carol@d9.test",
        later,
        AuthenticationStrength::Pkix,
        &only_admitted,
    )
    .await;
    assert_eq!(renewed, SubscriptionRecord::Recorded);
    let current = subscriptions(&federated)
        .of_node(&spaces(), DEV, created_at, later)
        .await
        .expect("the subscriptions are read");
    assert_eq!(current.len(), 1, "{current:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);
}

/// A subscription is recorded only while the Space's `federated` setting is
/// on, read under the lock that turning it off takes: once it is off, no
/// row is written for it.
#[tokio::test]
async fn no_subscription_is_recorded_for_a_space_closed_to_other_servers() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    let created_at = node_created_at(&federated, DEV).await;
    record(&federated, DEV, created_at, BOB).await;

    federated_spaces(&federated)
        .close(&spaces(), DEV)
        .await
        .expect("the Space is closed");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);
    let late = offer(
        &federated,
        DEV,
        created_at,
        "mallory@home-b.test",
        now_ms(),
        AuthenticationStrength::Pkix,
        &|_| true,
    )
    .await;
    assert_eq!(late, SubscriptionRecord::NotFederated);
    // Nor for a node of the name created at another time.
    federate(&federated, DEV).await;
    let stale = offer(
        &federated,
        DEV,
        created_at + 1,
        "mallory@home-b.test",
        now_ms(),
        AuthenticationStrength::Pkix,
        &|_| true,
    )
    .await;
    assert_eq!(stale, SubscriptionRecord::NotFederated);
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);
    // Open again, the Space takes subscriptions again.
    let reopened = subscribe(&federated, "closed-1", MALLORY, "mallory@home-b.test").await;
    assert_eq!(reopened.attr("type"), Some("result"), "{reopened:?}");
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
    // The account subscribes with one of its full JIDs: the subscription
    // is the account's, and its notification goes to the bare JID.
    let subscribed = subscribe(&federated, "made-2", BOB_PHONE, BOB_PHONE).await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");

    let published = round_trip(&mut victor, set("made-4", SPACES, publish(DEV, "lounge"))).await;
    assert!(is_result(&published), "{published:?}");
    let received = settled(&federated, "made-barrier-1").await;
    // One stanza for the account that subscribed, none for the address
    // that did not.
    assert_eq!(events_for(&received, BOB).len(), 1, "{received:?}");
    assert!(events_for(&received, "mallory@home-b.test").is_empty());
    assert!(!received
        .iter()
        .any(|stanza| stanza.name() == "message" && stanza.attr("to") != Some(BOB)));

    // A subscription that ended receives nothing more.
    let left = ask(
        &federated,
        request(
            BOB_PHONE,
            "made-5",
            true,
            subscription("unsubscribe", DEV, BOB),
        ),
    )
    .await;
    assert_eq!(left.attr("type"), Some("result"), "{left:?}");
    let retracted = round_trip(&mut victor, set("made-7", SPACES, retract(DEV, "lounge"))).await;
    assert!(is_result(&retracted), "{retracted:?}");
    let received = settled(&federated, "made-barrier-2").await;
    assert_eq!(events_for(&received, BOB).len(), 1, "{received:?}");
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
    spaces_publications_settled(&federated.router).await;
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

/// ADR-0020 D8 on what a subscription recorded: an owner's standing of a
/// trusted home is not honoured on evidence of `dialback` strength and
/// contributes nothing, not even the membership it would imply, so it
/// reads no private Space. A membership is honoured on such evidence while
/// host policy does not require `pkix`, as the fixture's does not.
#[tokio::test]
async fn an_owner_subscribed_over_dialback_is_notified_of_nothing_in_a_private_space() {
    let pki = Pki::new();
    let federated = hosting_with(&pki, trusting_the_peer()).await;
    space(&federated, DEV, NodeConfig::spaces_private()).await;
    federate(&federated, DEV).await;
    channel(&federated, "lounge", true).await;
    federated.account("victor").await;
    owner_of(&federated, DEV, "lounge", VICTOR).await;
    let mut victor = federated.loopback.client("victor", "desk").await;
    affiliate(&federated, DEV, BOB, Affiliation::Owner).await;
    affiliate(&federated, DEV, "mallory@home-b.test", Affiliation::Member).await;
    let created_at = node_created_at(&federated, DEV).await;
    let storage = &federated.loopback.state.deps.protocol.pubsub_storage;
    for account in [BOB, "mallory@home-b.test"] {
        storage
            .subscribe(&spaces(), DEV, &jid(account))
            .await
            .expect("the service's subscription");
        let outcome = offer(
            &federated,
            DEV,
            created_at,
            account,
            now_ms(),
            AuthenticationStrength::Dialback,
            &|_| true,
        )
        .await;
        assert_eq!(outcome, SubscriptionRecord::Recorded, "{account}");
    }

    let published = round_trip(&mut victor, set("dial-1", SPACES, publish(DEV, "lounge"))).await;
    assert!(is_result(&published), "{published:?}");
    let received = settled(&federated, "dial-barrier").await;
    assert!(events_for(&received, BOB).is_empty(), "{received:?}");
    assert_eq!(
        events_for(&received, "mallory@home-b.test").len(),
        1,
        "{received:?}"
    );
}

/// The relations whose object is the Space `node`.
async fn space_relations(federated: &Federated, node: &str) -> Vec<(String, Subject)> {
    let permissions = &federated.loopback.state.deps.app_state.permission_actor;
    let mut held = Vec::new();
    for relation in ["owner", "admin", "moderator", "member"] {
        let subjects = permissions
            .ask(ListSubjects {
                object: Object::new(ObjectType::Space, node),
                relation: Relation::new(relation),
            })
            .await
            .expect("the relations are read");
        held.extend(
            subjects
                .into_iter()
                .map(|subject| (relation.to_owned(), subject)),
        );
    }
    held
}

/// The parent relations of the channel `id`.
async fn parents(federated: &Federated, id: &str) -> Vec<Subject> {
    federated
        .loopback
        .state
        .deps
        .app_state
        .permission_actor
        .ask(ListSubjects {
            object: Object::new(ObjectType::Channel, id),
            relation: Relation::new("parent"),
        })
        .await
        .expect("the parents are read")
}

/// A trusted home's owner holds every power a Space's owner holds, over a
/// certificate stream, deletion included. A Space deleted through PubSub
/// goes with everything that named it, and its rooms stay: each is taken
/// out of the Space (its parent grant on it, its link to it and its
/// bookmark in it go) and stands alone. The Space's own relations and
/// metadata, its `federated` setting and the remote subscriptions to it go
/// too, so a Space created again under the name inherits none of it. Only
/// the admin command `spaces:delete` destroys rooms.
#[tokio::test]
async fn a_remote_owners_deletion_unlinks_the_rooms_and_leaves_nothing_of_the_space() {
    let pki = Pki::new();
    let federated = hosting_with(&pki, trusting_the_peer()).await;
    let mut victor = space_with_owner(&federated).await;
    let state = &federated.loopback.state;
    state
        .deps
        .app_state
        .spaces_metadata_store
        .upsert(&SpaceMetadata {
            space_jid: ["dev@", SPACES].concat().parse().expect("the Space's JID"),
            space_node: SpaceNode::from(DEV),
            name: "Development".to_owned(),
            description: None,
            icon_url: None,
            created_at: 0,
            updated_at: 0,
        })
        .await
        .expect("the Space's metadata");
    // The local owner places the room in the Space: its parent grant on the
    // Space and its link to it.
    let placed = round_trip(&mut victor, set("del-1", SPACES, publish(DEV, "lounge"))).await;
    assert!(is_result(&placed), "{placed:?}");
    let links = &state.deps.app_state.channel_space_link_store;
    assert!(links
        .get(&room("lounge"))
        .await
        .expect("the link is read")
        .is_some());
    assert!(!parents(&federated, "lounge").await.is_empty());
    assert!(!space_relations(&federated, DEV).await.is_empty());
    let subscribed = subscribe(&federated, "del-2", MALLORY, "mallory@home-b.test").await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");
    affiliate(&federated, DEV, BOB, Affiliation::Owner).await;
    let channels = rows(&federated, "channels").await;

    let deleted = ask(
        &federated,
        request(
            BOB_PHONE,
            "del-3",
            true,
            pubsub(NS_PUBSUB_OWNER, of_node("delete", NS_PUBSUB_OWNER, DEV)),
        ),
    )
    .await;
    assert_eq!(deleted.attr("type"), Some("result"), "{deleted:?}");

    let storage = &state.deps.protocol.pubsub_storage;
    assert!(storage
        .get_node(&spaces(), DEV)
        .await
        .expect("the node is read")
        .is_none());
    assert_eq!(rows(&federated, "s2s_federated_spaces").await, 0);
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);
    // The room stands alone: its catalog row stays, and nothing places it
    // in a Space any more.
    assert_eq!(rows(&federated, "channels").await, channels);
    assert_eq!(rows(&federated, "channels WHERE id = 'lounge'").await, 1);
    assert!(links
        .get(&room("lounge"))
        .await
        .expect("the link is read")
        .is_none());
    assert!(parents(&federated, "lounge").await.is_empty());
    assert!(space_relations(&federated, DEV).await.is_empty());
    assert!(state
        .deps
        .app_state
        .spaces_metadata_store
        .get_by_node(&SpaceNode::from(DEV))
        .await
        .expect("the metadata is read")
        .is_none());

    // A Space created again under the name starts with nothing, and does
    // not hold the room.
    space(&federated, DEV, NodeConfig::spaces_public()).await;
    assert!(space_relations(&federated, DEV).await.is_empty());
    assert!(storage
        .get_items(&spaces(), DEV, None, &[])
        .await
        .expect("the items are read")
        .is_empty());
    assert!(parents(&federated, "lounge").await.is_empty());
    assert_eq!(
        federated_spaces(&federated)
            .enabled_for(&spaces(), DEV)
            .await
            .expect("the setting is read"),
        None
    );
    let unknown = subscribe(&federated, "del-4", MALLORY, "mallory@home-b.test").await;
    assert_eq!(error_condition(&unknown).as_deref(), Some("item-not-found"));
}

/// Purging a Space's items through PubSub takes its rooms out of it, as
/// retracting each would: no room keeps a parent grant on the Space, or a
/// link to it, that no item names. The rooms themselves stay, and so does
/// what S2S recorded for the Space, which names the node and not its items.
#[tokio::test]
async fn a_remote_owners_purge_takes_the_rooms_out_of_the_space() {
    let pki = Pki::new();
    let federated = hosting_with(&pki, trusting_the_peer()).await;
    let mut victor = space_with_owner(&federated).await;
    let placed = round_trip(&mut victor, set("purge-1", SPACES, publish(DEV, "lounge"))).await;
    assert!(is_result(&placed), "{placed:?}");
    let subscribed = subscribe(&federated, "purge-2", MALLORY, "mallory@home-b.test").await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");
    affiliate(&federated, DEV, BOB, Affiliation::Owner).await;
    let channels = rows(&federated, "channels").await;

    let purged = ask(
        &federated,
        request(
            BOB_PHONE,
            "purge-3",
            true,
            pubsub(NS_PUBSUB_OWNER, of_node("purge", NS_PUBSUB_OWNER, DEV)),
        ),
    )
    .await;
    assert_eq!(purged.attr("type"), Some("result"), "{purged:?}");
    let read = ask(&federated, request(MALLORY, "purge-4", false, items(DEV))).await;
    assert!(item_ids(&read).is_empty(), "{read:?}");
    assert!(parents(&federated, "lounge").await.is_empty());
    assert!(federated
        .loopback
        .state
        .deps
        .app_state
        .channel_space_link_store
        .get(&room("lounge"))
        .await
        .expect("the link is read")
        .is_none());
    assert_eq!(rows(&federated, "channels").await, channels);
    assert_eq!(rows(&federated, "s2s_federated_spaces").await, 1);
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);
}

/// What ends a remote account's subscription besides its own
/// `<unsubscribe/>`: its owner removing it (XEP-0060 §8.8.2), a ban
/// (§8.9.2), and the Space being closed to other servers, which also
/// removes the service's records of every remote subscriber, an owner's
/// record for one included.
#[tokio::test]
async fn an_owner_ends_remote_subscriptions_by_removing_banning_or_closing() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    let mut victor = space_with_owner(&federated).await;
    for (id, from, address) in [
        ("end-1", BOB_PHONE, BOB),
        ("end-2", MALLORY, "mallory@home-b.test"),
    ] {
        let subscribed = subscribe(&federated, id, from, address).await;
        assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");
    }
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 2);

    let removed = round_trip(
        &mut victor,
        set("end-3", SPACES, owner_subscription_as(DEV, BOB, "none")),
    )
    .await;
    assert!(is_result(&removed), "{removed:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);

    let banned = round_trip(
        &mut victor,
        set(
            "end-4",
            SPACES,
            affiliation(DEV, "mallory@home-b.test", "outcast"),
        ),
    )
    .await;
    assert!(is_result(&banned), "{banned:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);

    // Bob subscribes again, and the owner records a subscription for an
    // address at another server; closing the Space ends both.
    let again = subscribe(&federated, "end-5", BOB_PHONE, BOB).await;
    assert_eq!(again.attr("type"), Some("result"), "{again:?}");
    let recorded = round_trip(
        &mut victor,
        set(
            "end-6",
            SPACES,
            owner_subscription(DEV, "carol@home-b.test"),
        ),
    )
    .await;
    assert!(is_result(&recorded), "{recorded:?}");
    let closed = round_trip(
        &mut victor,
        set(
            "end-7",
            SPACES,
            configure(DEV, vec![field(FIELD_FEDERATED, "0")]),
        ),
    )
    .await;
    assert!(is_result(&closed), "{closed:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);
    assert_eq!(rows(&federated, "s2s_federated_spaces").await, 0);
    let storage = &federated.loopback.state.deps.protocol.pubsub_storage;
    let remaining = storage
        .list_node_subscriptions(&spaces(), DEV)
        .await
        .expect("the subscriptions are read");
    assert!(
        remaining
            .iter()
            .all(|row| row.subscriber.to_bare().domain().as_str() == "example.com"),
        "{remaining:?}"
    );
}

/// Two deletions of one Space asked at once, by a local owner's session and
/// by a remote owner, run one after the other under the Space's deletion
/// lock: one deletes the Space and the other deletes nothing, and the room
/// the Space held stands alone. The remote request that comes second finds
/// no such node. The session's request is decided by the requester's Owner
/// affiliation before it waits for the lock, so one that comes second
/// finds no such node, or, when the Space and its affiliations were gone
/// before it was decided, is refused as a non-owner's.
#[tokio::test]
async fn two_deletions_of_one_space_at_once_delete_it_once() {
    let pki = Pki::new();
    let federated = hosting_with(&pki, trusting_the_peer()).await;
    let mut victor = space_with_owner(&federated).await;
    let placed = round_trip(&mut victor, set("twice-1", SPACES, publish(DEV, "lounge"))).await;
    assert!(is_result(&placed), "{placed:?}");
    affiliate(&federated, DEV, BOB, Affiliation::Owner).await;
    let channels = rows(&federated, "channels").await;
    let delete = || pubsub(NS_PUBSUB_OWNER, of_node("delete", NS_PUBSUB_OWNER, DEV));

    let (local, remote) = tokio::join!(
        round_trip(&mut victor, set("twice-2", SPACES, delete())),
        ask(&federated, request(BOB_PHONE, "twice-3", true, delete())),
    );
    let local_deleted = is_result(&local);
    let remote_deleted = remote.attr("type") == Some("result");
    assert!(local_deleted != remote_deleted, "{local:?} {remote:?}");
    if local_deleted {
        assert_eq!(
            error_condition(&remote).as_deref(),
            Some("item-not-found"),
            "{remote:?}"
        );
    } else {
        assert!(
            is_error(&local, DefinedCondition::ItemNotFound)
                || is_error(&local, DefinedCondition::Forbidden),
            "{local:?}"
        );
    }
    assert!(federated
        .loopback
        .state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces(), DEV)
        .await
        .expect("the node is read")
        .is_none());
    assert_eq!(rows(&federated, "channels").await, channels);
    assert!(parents(&federated, "lounge").await.is_empty());
    assert!(federated
        .loopback
        .state
        .deps
        .app_state
        .channel_space_link_store
        .get(&room("lounge"))
        .await
        .expect("the link is read")
        .is_none());
}

/// A banned account's subscription is refused and leaves no row: in a
/// public Space with `forbidden`, and in a private one as a node that does
/// not exist. A ban written while a subscription is being recorded is
/// covered by
/// [`a_ban_written_while_a_subscription_is_recorded_removes_its_row`].
#[tokio::test]
async fn a_banned_accounts_subscription_is_refused_and_leaves_no_row() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    affiliate(&federated, DEV, "mallory@home-b.test", Affiliation::Outcast).await;
    let refused = subscribe(&federated, "banned-1", MALLORY, "mallory@home-b.test").await;
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("forbidden"),
        "{refused:?}"
    );
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);

    space(&federated, "inner", NodeConfig::spaces_private()).await;
    federate(&federated, "inner").await;
    affiliate(
        &federated,
        "inner",
        "mallory@home-b.test",
        Affiliation::Outcast,
    )
    .await;
    let hidden = ask(
        &federated,
        request(
            MALLORY,
            "banned-2",
            true,
            subscription("subscribe", "inner", "mallory@home-b.test"),
        ),
    )
    .await;
    assert_eq!(
        error_condition(&hidden).as_deref(),
        Some("item-not-found"),
        "{hidden:?}"
    );
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 0);
}

/// Make the remote subscription of `account` lapsed, as if it was last
/// made at the epoch.
async fn lapse(federated: &Federated, account: &str) {
    federated
        .loopback
        .state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: "UPDATE s2s_space_subscriptions SET subscribed_at_ms = 0 \
                  WHERE subscriber_jid = ?"
                .to_owned(),
            params: vec![account.into()],
        })
        .await
        .expect("the subscription lapses");
}

/// The bare JIDs of the PubSub service's subscription records of `node`.
async fn service_subscribers(federated: &Federated, node: &str) -> Vec<String> {
    federated
        .loopback
        .state
        .deps
        .protocol
        .pubsub_storage
        .list_node_subscriptions(&spaces(), node)
        .await
        .expect("the subscriptions are read")
        .into_iter()
        .map(|subscription| subscription.subscriber.to_bare().to_string())
        .collect()
}

/// A lapsed subscription that is removed as another is recorded takes the
/// PubSub service's own record of it along; an account that subscribes
/// again renews its own lapsed subscription and stays subscribed.
#[tokio::test]
async fn a_lapsed_subscription_removed_takes_the_services_record_along() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    let made = subscribe(&federated, "lapse-1", MALLORY, "mallory@home-b.test").await;
    assert_eq!(made.attr("type"), Some("result"), "{made:?}");
    lapse(&federated, "mallory@home-b.test").await;

    let other = subscribe(&federated, "lapse-2", BOB_PHONE, BOB).await;
    assert_eq!(other.attr("type"), Some("result"), "{other:?}");
    let subscribers = service_subscribers(&federated, DEV).await;
    assert!(
        !subscribers.iter().any(|jid| jid == "mallory@home-b.test"),
        "{subscribers:?}"
    );
    assert!(subscribers.iter().any(|jid| jid == BOB), "{subscribers:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);

    lapse(&federated, BOB).await;
    let renewed = subscribe(&federated, "lapse-3", BOB_PHONE, BOB).await;
    assert_eq!(renewed.attr("type"), Some("result"), "{renewed:?}");
    let subscribers = service_subscribers(&federated, DEV).await;
    assert!(subscribers.iter().any(|jid| jid == BOB), "{subscribers:?}");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);
}

/// When the bound on the deployment's remote subscriptions is reached, a
/// sweep of the whole store reads at most 4,096 remote domains, starting
/// after the last domain the sweep before it read and wrapping round: a
/// domain no longer admitted whose name sorts after the first 4,096 is
/// reached by the next sweep, and its rows give way, listed for the caller
/// to remove the PubSub service's records of them. The cursor is the
/// process's own; each case runs in a process of its own.
#[tokio::test]
async fn sweeps_of_the_whole_store_read_every_remote_domain_in_turn() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    let created_at = node_created_at(&federated, DEV).await;
    let now = now_ms();
    // The deployment's bound in rows, eight to a domain over 512 Spaces:
    // 8,191 domains `dNNNNN.test`, then `zz-gone.test`, which is no longer
    // admitted and sorts after all of them.
    let bound = i64::try_from(MAX_REMOTE_SUBSCRIPTIONS).expect("the bound fits");
    federated
        .loopback
        .state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: "WITH RECURSIVE seq(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM seq WHERE i < ?) \
                  INSERT INTO s2s_space_subscriptions \
                  (service_jid, node_name, subscriber_jid, account_jid, subscriber_domain, \
                   node_created_at_ms, strength, trust_profile, snapshot_generation, \
                   subscribed_at_ms) \
                  SELECT 'spaces.example.com', 'seed-' || (i % 512), \
                  'guest-' || i || '@' || domain, 'guest-' || i || '@' || domain, domain, \
                  0, 'pkix', NULL, '00000000-0000-0000-0000-000000000000', ? \
                  FROM (SELECT i, CASE WHEN i >= ? - 8 THEN 'zz-gone.test' \
                  ELSE 'd' || printf('%05d', i / 8) || '.test' END AS domain FROM seq)"
                .to_owned(),
            params: vec![(bound - 1).into(), now.into(), bound.into()],
        })
        .await
        .expect("the store is filled");
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, bound);
    let admitted = |domain: &str| domain != "zz-gone.test";
    let carol = jid("carol@new.test").to_bare();
    let store = subscriptions(&federated);
    let service = spaces();
    let carol_subscribes = || {
        store.record(
            &service,
            DEV,
            created_at,
            SubscriptionRequest {
                account: &carol,
                evidence: recorded(AuthenticationStrength::Pkix),
                snapshot: uuid::Uuid::new_v4(),
                now_ms: now,
            },
            &admitted,
        )
    };

    // The first sweep reads the first 4,096 domains, every one admitted.
    let first = carol_subscribes().await.expect("the store answers");
    assert_eq!(first.outcome, SubscriptionRecord::Bounds);
    assert!(first.removed.is_empty());
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, bound);

    // The next starts after them and reaches `zz-gone.test`.
    let second = carol_subscribes().await.expect("the store answers");
    assert_eq!(second.outcome, SubscriptionRecord::Recorded);
    let mut removed: Vec<String> = second
        .removed
        .iter()
        .map(|gone| gone.account.domain().as_str().to_owned())
        .collect();
    removed.dedup();
    assert_eq!(removed, ["zz-gone.test"]);
    assert_eq!(second.removed.len(), 8);
    assert_eq!(rows(&federated, "s2s_space_subscriptions").await, bound - 7);
}

/// A ban written while a remote account's subscription is being recorded,
/// after the Space was read for the request and before the row committed,
/// is read again once the row is committed: the subscription is refused as
/// it is when the ban was there first, and the row and the service's own
/// subscription are gone, in a public Space and in a private one.
#[tokio::test]
async fn a_ban_written_while_a_subscription_is_recorded_removes_its_row() {
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    federated_space(&federated, DEV).await;
    space(&federated, "inner", NodeConfig::spaces_private()).await;
    federate(&federated, "inner").await;
    affiliate(&federated, "inner", BOB, Affiliation::Member).await;
    let bob = jid(BOB).to_bare();

    for (node, condition) in [(DEV, "forbidden"), ("inner", "item-not-found")] {
        let gate = hold_space_subscription_after_record(node, bob.clone());
        let id = format!("ban-race-{node}");
        let (answer, ()) = tokio::join!(
            ask(
                &federated,
                request(BOB_PHONE, &id, true, subscription("subscribe", node, BOB)),
            ),
            async {
                gate.wait_until_reached().await;
                // The row committed before the ban was written.
                assert_eq!(rows(&federated, "s2s_space_subscriptions").await, 1);
                affiliate(&federated, node, BOB, Affiliation::Outcast).await;
                gate.release();
            }
        );
        assert_eq!(
            error_condition(&answer).as_deref(),
            Some(condition),
            "{node}: {answer:?}"
        );
        assert_eq!(
            rows(&federated, "s2s_space_subscriptions").await,
            0,
            "{node}"
        );
        assert!(
            service_subscribers(&federated, node).await.is_empty(),
            "{node}"
        );
    }
}

/// Subscribe the local account `localpart` to the Space `node` from a
/// session of its own, which then receives the Space's notifications.
async fn local_subscriber(federated: &Federated, localpart: &str, node: &str) -> WsXmppClient {
    let account = federated.account(localpart).await;
    let mut client = federated.loopback.client(localpart, "desk").await;
    let subscribed = round_trip(
        &mut client,
        set(
            "local-sub",
            SPACES,
            subscription("subscribe", node, account.as_str()),
        ),
    )
    .await;
    assert!(is_result(&subscribed), "{subscribed:?}");
    client
}

/// The ids of the items a `retract` notification of `node` names among
/// `received`.
fn retracted(received: &[Element], node: &str) -> Vec<String> {
    received
        .iter()
        .filter(|stanza| stanza.name() == "message" && stanza.attr("from") == Some(SPACES))
        .flat_map(Element::children)
        .filter(|child| child.is("event", s2s_spaces_support::NS_PUBSUB_EVENT))
        .flat_map(Element::children)
        .filter(|items| items.name() == "items" && items.attr("node") == Some(node))
        .flat_map(Element::children)
        .filter(|child| child.name() == "retract")
        .filter_map(|retract| retract.attr("id").map(ToOwned::to_owned))
        .collect()
}

/// A room that leaves a Space is told to the Space's subscribers here and
/// at other servers (XEP-0060 §7.2.2.1; a Space node's
/// `pubsub#notify_retract` is on, XEP-0503): when its bookmark is
/// retracted, and when a publish places it in another Space, which moves
/// it out of this one.
#[tokio::test]
async fn a_room_leaving_a_space_is_told_to_its_subscribers_here_and_at_other_servers() {
    const OPS: &str = "ops";
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    let mut victor = space_with_owner(&federated).await;
    federated_space(&federated, OPS).await;
    affiliate(&federated, OPS, VICTOR, Affiliation::Owner).await;
    s2s_spaces_support::relate(
        &federated,
        Object::new(ObjectType::Space, OPS),
        "owner",
        VICTOR,
    )
    .await;
    let subscribed = subscribe(&federated, "leave-sub", BOB_PHONE, BOB).await;
    assert_eq!(subscribed.attr("type"), Some("result"), "{subscribed:?}");
    let mut walter = local_subscriber(&federated, "walter", DEV).await;
    let lounge = room("lounge").to_string();

    // A retract of the room's bookmark.
    for (id, payload) in [
        ("leave-pub-1", publish(DEV, "lounge")),
        ("leave-ret-1", retract(DEV, "lounge")),
    ] {
        let answer = round_trip(&mut victor, set(id, SPACES, payload)).await;
        assert!(is_result(&answer), "{id}: {answer:?}");
    }
    let here = federated.client_settle(&mut walter).await;
    assert_eq!(
        retracted(&here, DEV),
        std::slice::from_ref(&lounge),
        "{here:?}"
    );
    let received = settled(&federated, "leave-barrier-1").await;
    assert_eq!(
        retracted(&events_for(&received, BOB), DEV),
        std::slice::from_ref(&lounge),
        "{received:?}"
    );

    // A publish that places the room in another Space moves it out of this
    // one.
    let mark = federated.peer_received(|_| true).len();
    for (id, payload) in [
        ("leave-pub-2", publish(DEV, "lounge")),
        ("leave-pub-3", publish(OPS, "lounge")),
    ] {
        let answer = round_trip(&mut victor, set(id, SPACES, payload)).await;
        assert!(is_result(&answer), "{id}: {answer:?}");
    }
    let here = federated.client_settle(&mut walter).await;
    assert_eq!(
        retracted(&here, DEV),
        std::slice::from_ref(&lounge),
        "{here:?}"
    );
    let received = settled(&federated, "leave-barrier-2").await;
    let since: Vec<Element> = received.into_iter().skip(mark).collect();
    assert_eq!(
        retracted(&events_for(&since, BOB), DEV),
        [lounge],
        "{since:?}"
    );
}

/// The admin command that deletes a channel tells the subscribers of each
/// Space it took the room out of, as a retract of the room's bookmark
/// there would. A subscriber at another server is not told of a deleted
/// room: whether the room was open to other servers can no longer be read
/// when its notification is decided.
#[tokio::test]
async fn deleting_a_channel_tells_the_spaces_local_subscribers_it_left() {
    const NODE_CHANNELS_DELETE: &str = "urn:waddle:admin:channels:delete:0";
    const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
    const NS_DATA: &str = "jabber:x:data";
    let pki = Pki::new();
    let federated = hosting(&pki).await;
    let mut victor = space_with_owner(&federated).await;
    let state = &federated.loopback.state;
    waddle_server::admin::channels::register(
        &state.deps.protocol.command_registry,
        std::sync::Arc::clone(&state.deps.app_state),
        std::sync::Arc::clone(state),
        std::sync::Arc::clone(&state.deps.protocol.connection_registry),
        std::sync::Arc::clone(&state.deps.protocol.sm_session_registry),
        None,
    )
    .await;
    let mut walter = local_subscriber(&federated, "walter", DEV).await;
    let published = round_trip(&mut victor, set("del-pub", SPACES, publish(DEV, "lounge"))).await;
    assert!(is_result(&published), "{published:?}");
    let lounge = room("lounge").to_string();

    let form_field = |var: &str, value: &str| {
        Element::builder("field", NS_DATA)
            .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
            .append(Element::builder("value", NS_DATA).append(value).build())
            .build()
    };
    let command = Element::builder("command", NS_COMMANDS)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            NODE_CHANNELS_DELETE,
        )
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .append(
            Element::builder("x", NS_DATA)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                .append(form_field("FORM_TYPE", NODE_CHANNELS_DELETE))
                .append(form_field("channel_jid", &lounge))
                .append(form_field("confirm", "yes"))
                .build(),
        )
        .build();
    let deleted = round_trip(&mut victor, set("del-run", LOCAL, command)).await;
    assert!(is_result(&deleted), "{deleted:?}");

    let here = federated.client_settle(&mut walter).await;
    assert_eq!(retracted(&here, DEV), [lounge], "{here:?}");
}
