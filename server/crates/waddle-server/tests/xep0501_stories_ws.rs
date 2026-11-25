//! XEP-0501 Pubsub Stories integration tests over WebSocket.
//!
//! "This profile is defined by the Pubsub type
//! `urn:xmpp:pubsub-social-feed:stories:0` and is hosted on a
//! `urn:xmpp:pubsub-social-feed:stories:0` XEP-0163 node." Every account
//! therefore owns its own stories node on its PEP service: the node is
//! `presence`-gated, carries `pubsub#item_expire` 86400, and notifies the
//! contacts whose clients advertise the `+notify` feature. Items past the
//! node's `pubsub#item_expire` are purged and announced as retractions.

use waddle_ws_test_support as ws_common;

mod pep_caps_support;
use pep_caps_support::{go_available_with_caps, wait_for_event};

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ALICE: &str = "alice@localhost";
const BOB: &str = "bob@localhost";
const COMMUNITY_JID: &str = "community.localhost";
const STORIES_NODE: &str = "urn:xmpp:pubsub-social-feed:stories:0";
const STORIES_NOTIFY: &str = "urn:xmpp:pubsub-social-feed:stories:0+notify";
const NS_SOCIAL_FEED: &str = "urn:xmpp:pubsub-social-feed:1";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_ATOM: &str = "http://www.w3.org/2005/Atom";
const NS_WADDLE_STORIES: &str = "urn:waddle:stories:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn story_entry_xml(
    item_id: &str,
    body: &str,
    media_url: &str,
    media_type: &str,
    author: &str,
    expires: &str,
) -> String {
    format!(
        r#"<entry xmlns="{NS_ATOM}">
          <title type="text">{body}</title>
          <id>{item_id}</id>
          <published>2026-06-01T12:00:00Z</published>
          <updated>2026-06-01T12:00:00Z</updated>
          <content type="text">{body}</content>
          <author><uri>xmpp:{author}</uri></author>
          <link rel="enclosure" href="{media_url}" type="{media_type}"/>
          <expires xmlns="{NS_WADDLE_STORIES}">{expires}</expires>
        </entry>"#
    )
}

fn story_publish_body(item_id: &str, body: &str, author: &str) -> String {
    format!(
        r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{STORIES_NODE}"><item id="{item_id}">{}</item></publish></pubsub>"#,
        story_entry_xml(
            item_id,
            body,
            &format!("https://example.com/{item_id}.jpg"),
            "image/jpeg",
            author,
            "2030-01-01T12:00:00Z",
        )
    )
}

fn frame_has_iq_id(frame: &str, id: &str) -> bool {
    frame.contains("<iq")
        && (frame.contains(&format!("id='{id}'")) || frame.contains(&format!("id=\"{id}\"")))
}

/// Send an IQ (`to` empty addresses the account's own PEP service) and
/// return its reply.
async fn iq(client: &mut WsXmppClient, kind: &str, id: &str, to: &str, body: &str) -> String {
    let to_attr = if to.is_empty() {
        String::new()
    } else {
        format!(r#" to="{to}""#)
    };
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="{kind}" id="{id}"{to_attr}>{body}</iq>"#
        ))
        .await
        .expect("send iq");
    client
        .recv_matching(|frame| frame_has_iq_id(frame, id))
        .await
        .expect("iq response")
}

async fn publish_story(client: &mut WsXmppClient, id: &str, to: &str, story_id: &str) -> String {
    iq(
        client,
        "set",
        id,
        to,
        &story_publish_body(story_id, "Look at this!", "ignored@localhost"),
    )
    .await
}

async fn items(client: &mut WsXmppClient, id: &str, to: &str) -> String {
    iq(
        client,
        "get",
        id,
        to,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{STORIES_NODE}"/></pubsub>"#),
    )
    .await
}

async fn retract_story(client: &mut WsXmppClient, id: &str, to: &str, story_id: &str) -> String {
    iq(
        client,
        "set",
        id,
        to,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><retract node="{STORIES_NODE}"><item id="{story_id}"/></retract></pubsub>"#
        ),
    )
    .await
}

struct World {
    _server: TestServer,
    alice: WsXmppClient,
    /// Subscribed to alice's presence, advertising the stories `+notify`.
    bob: WsXmppClient,
    /// No relationship with alice.
    carol: WsXmppClient,
}

/// Alice, bob (alice's presence subscriber) and carol (a stranger). Bob
/// advertises `urn:xmpp:pubsub-social-feed:stories:0+notify` in his caps.
async fn world(extra_envs: &[(&str, &str)]) -> World {
    let pw = |n: &str| format!("{n}-{}", uuid::Uuid::new_v4());
    let (a, b, c) = (pw("alice"), pw("bob"), pw("carol"));
    let server =
        TestServer::start_with_extra_envs(&[("alice", &a), ("bob", &b), ("carol", &c)], extra_envs);
    let connect = |user: &'static str, pass: String, res: &'static str| {
        let url = server.ws_url();
        async move {
            WsXmppClient::connect_and_auth(&url, DOMAIN, user, &pass, res)
                .await
                .expect("connect")
        }
    };
    let mut alice = connect("alice", a, "alice-r1").await;
    let mut bob = connect("bob", b, "bob-r1").await;
    let carol = connect("carol", c, "carol-r1").await;

    for (client, id) in [(&mut alice, "ri-a"), (&mut bob, "ri-b")] {
        let r = iq(
            client,
            "get",
            id,
            "",
            r#"<query xmlns="jabber:iq:roster"/>"#,
        )
        .await;
        assert!(r.contains("type='result'"), "{r}");
    }
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice presence");
    bob.send(&format!(
        r#"<presence xmlns="jabber:client" type="subscribe" to="{ALICE}"/>"#
    ))
    .await
    .expect("subscribe");
    alice
        .recv_matching(|f| f.contains("type='subscribe'") && f.contains(BOB))
        .await
        .expect("alice receives subscribe");
    alice
        .send(&format!(
            r#"<presence xmlns="jabber:client" type="subscribed" to="{BOB}"/>"#
        ))
        .await
        .expect("approve");
    bob.recv_matching(|f| f.contains("type='subscribed'"))
        .await
        .expect("bob receives approval");
    go_available_with_caps(&mut bob, "bob-stories", &[STORIES_NOTIFY]).await;
    World {
        _server: server,
        alice,
        bob,
        carol,
    }
}

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let resource = format!("xep0501-{}", uuid::Uuid::new_v4());
    let password = server.fixed_account_password().to_string();
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "admin", &password, &resource)
            .await
            .expect("connect and auth");
    (server, client)
}

#[tokio::test]
async fn account_disco_info_advertises_the_stories_features() {
    // "If an entity implements Pubsub Stories, it MUST specify the
    // 'urn:xmpp:pubsub-social-feed:1' and 'urn:xmpp:pubsub-social-feed:stories:0'
    // features in its service discovery information."
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    let reply = iq(
        &mut client,
        "get",
        "disco-own",
        "admin@localhost",
        r#"<query xmlns="http://jabber.org/protocol/disco#info"/>"#,
    )
    .await;
    for feature in [NS_SOCIAL_FEED, STORIES_NODE] {
        assert!(
            reply.contains(&format!("var='{feature}'")),
            "account disco#info missing {feature}: {reply}"
        );
    }

    let _ = client.close().await;
}

#[tokio::test]
async fn community_service_no_longer_hosts_stories() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    let disco = iq(
        &mut client,
        "get",
        "disco-community",
        COMMUNITY_JID,
        r#"<query xmlns="http://jabber.org/protocol/disco#info"/>"#,
    )
    .await;
    assert!(
        !disco.contains(&format!("var='{STORIES_NODE}'")),
        "the community service must not advertise stories: {disco}"
    );

    let items = items(&mut client, "community-stories-items", COMMUNITY_JID).await;
    assert!(
        items.contains("type='error'") && items.contains("item-not-found"),
        "the community service has no stories node: {items}"
    );
    let publish = publish_story(
        &mut client,
        "community-stories-publish",
        COMMUNITY_JID,
        "story-community",
    )
    .await;
    assert!(
        publish.contains("type='error'") && publish.contains("item-not-found"),
        "publishing a story to the community service must fail: {publish}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn stories_node_follows_the_xep0501_configuration() {
    // "The pubsub#type MUST be set to urn:xmpp:pubsub-social-feed:stories:0",
    // "The pubsub#access_model SHOULD be set to presence" and "The
    // pubsub#item_expire SHOULD be set, usually 24 hours (86400)."
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    let published = publish_story(&mut client, "own-publish", "", "story-config").await;
    assert!(
        published.contains("type='result'"),
        "a story publishes to the account's own node, creating it: {published}"
    );

    let form = iq(
        &mut client,
        "get",
        "stories-node-config",
        "",
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{STORIES_NODE}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        form.contains("type='result'")
            && form.contains("var='pubsub#type'")
            && form.contains(&format!("<value>{STORIES_NODE}</value>")),
        "stories node must declare the XEP-0501 pubsub#type: {form}"
    );
    assert!(
        form.contains("var='pubsub#access_model'") && form.contains("<value>presence</value>"),
        "stories node access_model must be presence: {form}"
    );
    assert!(
        form.contains("var='pubsub#item_expire'") && form.contains("<value>86400</value>"),
        "stories node item_expire must be 86400: {form}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn unbounded_max_items_is_the_max_token_in_the_node_form() {
    // XEP-0060 §16.4.4: `pubsub#max_items` is an integer or `max`; the form
    // must not leak the internal u32::MAX sentinel.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let published = publish_story(&mut client, "own-publish", "", "story-max").await;
    assert!(published.contains("type='result'"), "{published}");

    let read =
        format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{STORIES_NODE}"/></pubsub>"#);
    let form = iq(&mut client, "get", "max-get-1", "", &read).await;
    assert!(
        form.contains("var='pubsub#max_items'") && !form.contains("4294967295"),
        "unbounded node must not render the numeric sentinel: {form}"
    );
    let compact: String = form.split_whitespace().collect();
    assert!(
        compact.contains("var='pubsub#max_items'><value>max</value>"),
        "unbounded node renders max: {form}"
    );

    let submit = |value: &str| {
        format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{STORIES_NODE}"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/pubsub#node_config</value></field><field var="pubsub#max_items"><value>{value}</value></field></x></configure></pubsub>"#
        )
    };
    let bounded = iq(&mut client, "set", "max-set-5", "", &submit("5")).await;
    assert!(bounded.contains("type='result'"), "{bounded}");
    let form = iq(&mut client, "get", "max-get-2", "", &read).await;
    assert!(form.contains("<value>5</value>"), "{form}");
    let unbounded = iq(&mut client, "set", "max-set-max", "", &submit("max")).await;
    assert!(unbounded.contains("type='result'"), "{unbounded}");
    let form = iq(&mut client, "get", "max-get-3", "", &read).await;
    let compact: String = form.split_whitespace().collect();
    assert!(
        compact.contains("var='pubsub#max_items'><value>max</value>"),
        "max round-trips: {form}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn item_expire_is_configurable_through_the_node_form() {
    // XEP-0060 §16.4.4: `pubsub#item_expire` is a number of seconds, or `max`
    // for no limit other than a server imposed maximum.
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;
    let published = publish_story(&mut client, "own-publish", "", "story-form").await;
    assert!(published.contains("type='result'"), "{published}");

    let configure = |value: &str| {
        format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{STORIES_NODE}"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/pubsub#node_config</value></field><field var="pubsub#item_expire"><value>{value}</value></field></x></configure></pubsub>"#
        )
    };
    let read_form = |id: &'static str| {
        (
            id,
            format!(
                r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{STORIES_NODE}"/></pubsub>"#
            ),
        )
    };

    let set = iq(&mut client, "set", "expire-set", "", &configure("3600")).await;
    assert!(set.contains("type='result'"), "{set}");
    let (id, body) = read_form("expire-get-1");
    let form = iq(&mut client, "get", id, "", &body).await;
    assert!(form.contains("<value>3600</value>"), "{form}");

    let unlimited = iq(&mut client, "set", "expire-max", "", &configure("max")).await;
    assert!(unlimited.contains("type='result'"), "{unlimited}");
    let (id, body) = read_form("expire-get-2");
    let form = iq(&mut client, "get", id, "", &body).await;
    assert!(
        form.contains("var='pubsub#item_expire'") && form.contains("<value>max</value>"),
        "`max` clears the expiry: {form}"
    );

    for bad in ["0", "soon", "-5"] {
        let rejected = iq(&mut client, "set", "expire-bad", "", &configure(bad)).await;
        assert!(
            rejected.contains("type='error'") && rejected.contains("bad-request"),
            "item_expire {bad:?} must be rejected: {rejected}"
        );
    }

    let _ = client.close().await;
}

#[tokio::test]
async fn story_publish_requires_one_enclosure_story_payload() {
    let _guard = TEST_SERIAL.lock().await;
    let (_server, mut client) = setup().await;

    let missing_payload = iq(
        &mut client,
        "set",
        "story-missing-payload",
        "",
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{STORIES_NODE}"><item id="missing-payload"/></publish></pubsub>"#
        ),
    )
    .await;
    assert!(
        missing_payload.contains("type='error'") && missing_payload.contains("payload-required"),
        "story publish without an Atom entry must be payload-required: {missing_payload}"
    );

    let cases = [
        (
            "no-enclosure",
            r#"<title type="text">no media</title><id>no-enclosure</id><updated>2026-06-01T12:00:00Z</updated>"#,
        ),
        (
            "blank-href",
            r#"<title type="text">blank</title><id>blank-href</id><updated>2026-06-01T12:00:00Z</updated><link rel="enclosure" href="   " type="image/jpeg"/>"#,
        ),
        (
            "missing-updated",
            r#"<title type="text">t</title><id>missing-updated</id><link rel="enclosure" href="https://example.com/p.jpg" type="image/jpeg"/>"#,
        ),
        (
            "bad-published",
            r#"<title type="text">t</title><id>bad-published</id><updated>2026-06-01T12:00:00Z</updated><published>not-a-date</published><link rel="enclosure" href="https://example.com/p.jpg" type="image/jpeg"/>"#,
        ),
        (
            "two-enclosures",
            r#"<title type="text">t</title><id>two-enclosures</id><updated>2026-06-01T12:00:00Z</updated><link rel="enclosure" href="https://example.com/a.jpg" type="image/jpeg"/><link rel="enclosure" href="https://example.com/b.jpg" type="image/jpeg"/>"#,
        ),
    ];
    for (id, entry) in cases {
        let reply = iq(
            &mut client,
            "set",
            &format!("story-{id}"),
            "",
            &format!(
                r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{STORIES_NODE}"><item id="{id}"><entry xmlns="{NS_ATOM}">{entry}</entry></item></publish></pubsub>"#
            ),
        )
        .await;
        assert!(
            reply.contains("type='error'") && reply.contains("invalid-payload"),
            "{id}: a stories item MUST have exactly one enclosure and the required Atom fields: {reply}"
        );
    }

    let _ = client.close().await;
}

#[tokio::test]
async fn story_publish_round_trips_and_stamps_the_session_author() {
    let _guard = TEST_SERIAL.lock().await;
    let mut world = world(&[]).await;

    let story_id = format!("story-{}", uuid::Uuid::new_v4());
    let published = iq(
        &mut world.alice,
        "set",
        "story-publish",
        "",
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{STORIES_NODE}"><item id="{story_id}">{}</item></publish></pubsub>"#,
            story_entry_xml(
                &story_id,
                "From the new office",
                "https://example.com/member-photo.jpg",
                "image/jpeg",
                "spoofed@localhost",
                "2030-01-01T12:00:00Z",
            )
        ),
    )
    .await;
    assert!(published.contains("type='result'"), "{published}");

    let own = items(&mut world.alice, "own-items", "").await;
    for needle in [
        story_id.as_str(),
        "From the new office",
        "https://example.com/member-photo.jpg",
        "2030-01-01T12:00:00Z",
        ALICE,
    ] {
        assert!(own.contains(needle), "own items lost {needle}: {own}");
    }
    assert!(
        !own.contains("spoofed@localhost"),
        "the service trusted a spoofed story author: {own}"
    );

    let _ = world.alice.close().await;
    let _ = world.bob.close().await;
    let _ = world.carol.close().await;
}

#[tokio::test]
async fn only_presence_subscribers_read_a_contacts_stories() {
    // `pubsub#access_model` is `presence`: only entities subscribed to the
    // owner's presence retrieve items (XEP-0060 §4.5.1, XEP-0163 §4).
    let _guard = TEST_SERIAL.lock().await;
    let mut world = world(&[]).await;
    let story_id = format!("story-{}", uuid::Uuid::new_v4());
    let published = publish_story(&mut world.alice, "pub", "", &story_id).await;
    assert!(published.contains("type='result'"), "{published}");

    let bob_items = items(&mut world.bob, "bob-items", ALICE).await;
    assert!(
        bob_items.contains("type='result'") && bob_items.contains(&story_id),
        "a presence subscriber reads the stories: {bob_items}"
    );

    let carol_items = items(&mut world.carol, "carol-items", ALICE).await;
    assert!(
        carol_items.contains("type='error'")
            && carol_items.contains("presence-subscription-required"),
        "a stranger must not read the stories: {carol_items}"
    );

    let _ = world.alice.close().await;
    let _ = world.bob.close().await;
    let _ = world.carol.close().await;
}

#[tokio::test]
async fn nobody_else_publishes_or_retracts_on_the_authors_node() {
    let _guard = TEST_SERIAL.lock().await;
    let mut world = world(&[]).await;
    let story_id = format!("story-{}", uuid::Uuid::new_v4());
    let published = publish_story(&mut world.alice, "pub", "", &story_id).await;
    assert!(published.contains("type='result'"), "{published}");

    let intruder = publish_story(&mut world.bob, "bob-pub", ALICE, "story-intruder").await;
    assert!(
        intruder.contains("type='error'"),
        "a contact must not publish onto the author's stories node: {intruder}"
    );
    let intruder_retract = retract_story(&mut world.bob, "bob-retract", ALICE, &story_id).await;
    assert!(
        intruder_retract.contains("type='error'") && intruder_retract.contains("forbidden"),
        "a contact must not retract the author's story: {intruder_retract}"
    );
    let still_there = items(&mut world.alice, "still-there", "").await;
    assert!(still_there.contains(&story_id), "{still_there}");

    let _ = world.alice.close().await;
    let _ = world.bob.close().await;
    let _ = world.carol.close().await;
}

#[tokio::test]
async fn contacts_with_the_stories_notify_feature_are_notified_of_publish_and_retract() {
    // XEP-0501 §Discovery: "If an entity wish to receive notifications from
    // a contact when a Story is published it should also declares it in its
    // service discovery followed by '+notify'". `pubsub#notify_retract` is
    // true (XEP-0472 base profile), so a retraction is announced too.
    let _guard = TEST_SERIAL.lock().await;
    let mut world = world(&[]).await;
    let story_id = format!("story-{}", uuid::Uuid::new_v4());
    let published = publish_story(&mut world.alice, "pub", "", &story_id).await;
    assert!(published.contains("type='result'"), "{published}");

    let event = wait_for_event(&mut world.bob, STORIES_NODE, Duration::from_secs(3))
        .await
        .expect("bob is notified of the new story");
    assert!(
        event.contains(&story_id) && event.contains(ALICE),
        "notification names the story and its author: {event}"
    );

    let retracted = retract_story(&mut world.alice, "retract", "", &story_id).await;
    assert!(retracted.contains("type='result'"), "{retracted}");
    let retract_event = wait_for_event(&mut world.bob, STORIES_NODE, Duration::from_secs(3))
        .await
        .expect("bob is notified of the retraction");
    assert!(
        retract_event.contains("<retract") && retract_event.contains(&story_id),
        "{retract_event}"
    );

    let _ = world.alice.close().await;
    let _ = world.bob.close().await;
    let _ = world.carol.close().await;
}

#[tokio::test]
async fn expired_stories_are_purged_and_retractions_are_announced() {
    // "The pubsub#item_expire SHOULD be set, usually 24 hours (86400)." The
    // service purges an item once it is older than the node's item_expire
    // and notifies subscribers like any retraction.
    let _guard = TEST_SERIAL.lock().await;
    let mut world = world(&[("WADDLE_PUBSUB_ITEM_EXPIRY_SWEEP_SECS", "1")]).await;
    let first = publish_story(&mut world.alice, "pub-first", "", "story-expiring").await;
    assert!(first.contains("type='result'"), "{first}");
    let _ = wait_for_event(&mut world.bob, STORIES_NODE, Duration::from_secs(3))
        .await
        .expect("bob is notified of the new story");

    let configured = iq(
        &mut world.alice,
        "set",
        "expire-set",
        "",
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{STORIES_NODE}"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/pubsub#node_config</value></field><field var="pubsub#item_expire"><value>1</value></field></x></configure></pubsub>"#
        ),
    )
    .await;
    assert!(configured.contains("type='result'"), "{configured}");

    let retract_event = loop {
        let event = wait_for_event(&mut world.bob, STORIES_NODE, Duration::from_secs(10))
            .await
            .expect("bob is notified that the story expired");
        if event.contains("<retract") {
            break event;
        }
    };
    assert!(retract_event.contains("story-expiring"), "{retract_event}");

    let after = items(&mut world.alice, "after-expiry", "").await;
    assert!(
        after.contains("type='result'") && !after.contains("story-expiring"),
        "the expired story must be gone: {after}"
    );

    let _ = world.alice.close().await;
    let _ = world.bob.close().await;
    let _ = world.carol.close().await;
}

#[tokio::test]
async fn unexpired_stories_survive_the_sweep() {
    let _guard = TEST_SERIAL.lock().await;
    let mut world = world(&[("WADDLE_PUBSUB_ITEM_EXPIRY_SWEEP_SECS", "1")]).await;
    let published = publish_story(&mut world.alice, "pub", "", "story-fresh").await;
    assert!(published.contains("type='result'"), "{published}");

    tokio::time::sleep(Duration::from_secs(3)).await;

    let after = items(&mut world.alice, "after-sweeps", "").await;
    assert!(
        after.contains("story-fresh"),
        "a story inside its item_expire window must survive the sweep: {after}"
    );

    let _ = world.alice.close().await;
    let _ = world.bob.close().await;
    let _ = world.carol.close().await;
}
