//! WebSocket sessions with the account coordinator and the durable
//! stream-management authority selected
//! (`WADDLE_XMPP_ACCOUNT_AUTHORITY=coordinator`,
//! `WADDLE_XMPP_DURABLE_SM=durable`) on single-node SQLite.
//!
//! Every connection reports its binding to the coordinator, and routing
//! reads the coordinator's slots. These tests observe the wire and the
//! `account_resource_bindings` rows the coordinator commits.

#![cfg(feature = "test-support")]

use std::time::Duration;

use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::SM_NS;

const ACCOUNTS: &[(&str, &str)] = &[("alice", "coordinator-alice"), ("bob", "coordinator-bob")];
const SELECTORS: &[(&str, &str)] = &[
    ("WADDLE_XMPP_ACCOUNT_AUTHORITY", "coordinator"),
    ("WADDLE_XMPP_DURABLE_SM", "durable"),
];
const CLIENT_NS: &str = "jabber:client";

/// `account_resource_bindings.state` values.
const STATE_ACTIVE: i64 = 3;
const STATE_DETACHED: i64 = 4;

struct Fixture {
    server: TestServer,
    pool: sqlx::SqlitePool,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("test directory");
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("database.sqlite").display()
    );
    let server = TestServer::start_persistent_with_extra_envs(&db_url, ACCOUNTS, SELECTORS);
    let pool = sqlx::SqlitePool::connect(&db_url)
        .await
        .expect("observe SQLite");
    Fixture {
        server,
        pool,
        _dir: dir,
    }
}

async fn authenticate(server: &TestServer, account: usize) -> WsXmppClient {
    let mut client = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("connect");
    client
        .authenticate("localhost", ACCOUNTS[account].0, ACCOUNTS[account].1)
        .await
        .expect("authenticate");
    client
}

async fn bound(server: &TestServer, account: usize, resource: &str) -> WsXmppClient {
    let mut client = authenticate(server, account).await;
    client.bind(resource).await.expect("bind");
    client
}

async fn send_element(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize");
    client
        .send(&String::from_utf8(bytes).expect("UTF-8"))
        .await
        .expect("send");
}

macro_rules! nc {
    ($name:literal) => {
        minidom::rxml::xml_ncname!($name).to_owned()
    };
}

fn chat(to: &str, id: &str, body: &str) -> Element {
    Element::builder("message", CLIENT_NS)
        .attr(nc!("to"), to)
        .attr(nc!("type"), "chat")
        .attr(nc!("id"), id)
        .append(Element::builder("body", CLIENT_NS).append(body).build())
        .build()
}

fn available(priority: i8, status: &str) -> Element {
    Element::builder("presence", CLIENT_NS)
        .append(
            Element::builder("priority", CLIENT_NS)
                .append(priority.to_string())
                .build(),
        )
        .append(Element::builder("status", CLIENT_NS).append(status).build())
        .build()
}

async fn recv_message(client: &mut WsXmppClient, id: &str) -> Element {
    let frame = client
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.is("message", CLIENT_NS) && element.attr("id") == Some(id)
            })
        })
        .await
        .unwrap_or_else(|error| panic!("message {id} did not arrive: {error}"));
    frame.parse().expect("server XML")
}

async fn expect_no_message(client: &mut WsXmppClient, id: &str) {
    let arrived = client
        .recv_matching_within(Duration::from_millis(750), |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.is("message", CLIENT_NS) && element.attr("id") == Some(id)
            })
        })
        .await;
    assert!(arrived.is_err(), "message {id} must not be delivered here");
}

#[derive(Debug, sqlx::FromRow)]
struct BindingRow {
    full_jid: String,
    state: i64,
    binding_ref: Option<String>,
    presence_available: i64,
    presence_priority: i64,
    sm_session_id: Option<String>,
    offline_flushed: i64,
    subscriptions_flushed: i64,
    carbons_enabled: i64,
    roster_interested: i64,
}

async fn binding(pool: &sqlx::SqlitePool, full_jid: &str) -> Option<BindingRow> {
    sqlx::query_as(
        "SELECT full_jid, state, binding_ref, presence_available, presence_priority, \
         sm_session_id, offline_flushed, subscriptions_flushed, carbons_enabled, \
         roster_interested \
         FROM account_resource_bindings WHERE full_jid = ?",
    )
    .bind(full_jid)
    .fetch_optional(pool)
    .await
    .expect("read account bindings")
}

async fn wait_for_binding<F: Fn(Option<&BindingRow>) -> bool>(
    pool: &sqlx::SqlitePool,
    full_jid: &str,
    accept: F,
) -> Option<BindingRow> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let row = binding(pool, full_jid).await;
            if accept(row.as_ref()) {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("binding of {full_jid} did not reach the expected state"))
}

fn holds_binding(row: Option<&BindingRow>, state: i64) -> bool {
    row.is_some_and(|row| row.state == state && row.binding_ref.is_some())
}

fn is_free(row: Option<&BindingRow>) -> bool {
    row.is_none_or(|row| row.binding_ref.is_none())
}

async fn go_available(
    client: &mut WsXmppClient,
    pool: &sqlx::SqlitePool,
    full_jid: &str,
    priority: i8,
) {
    send_element(client, available(priority, "here")).await;
    wait_for_binding(pool, full_jid, |row| {
        row.is_some_and(|row| {
            row.presence_available == 1 && row.presence_priority == i64::from(priority)
        })
    })
    .await;
}

#[tokio::test]
async fn bind_records_the_binding_and_a_bare_jid_message_follows_the_coordinator() {
    let fixture = fixture().await;
    let mut alice = bound(&fixture.server, 0, "laptop").await;
    let row = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        holds_binding(row, STATE_ACTIVE)
    })
    .await
    .expect("binding row");
    assert_eq!(row.full_jid, "alice@localhost/laptop");
    go_available(&mut alice, &fixture.pool, "alice@localhost/laptop", 0).await;

    let mut bob = bound(&fixture.server, 1, "desk").await;
    send_element(&mut bob, chat("alice@localhost", "bare-1", "hello")).await;
    let delivered = recv_message(&mut alice, "bare-1").await;
    assert_eq!(delivered.attr("from"), Some("bob@localhost/desk"));
}

#[tokio::test]
async fn a_priority_change_moves_bare_jid_delivery_to_the_higher_resource() {
    let fixture = fixture().await;
    let mut laptop = bound(&fixture.server, 0, "laptop").await;
    let mut phone = bound(&fixture.server, 0, "phone").await;
    go_available(&mut laptop, &fixture.pool, "alice@localhost/laptop", 5).await;
    go_available(&mut phone, &fixture.pool, "alice@localhost/phone", 1).await;

    let mut bob = bound(&fixture.server, 1, "desk").await;
    send_element(&mut bob, chat("alice@localhost", "prio-1", "to the laptop")).await;
    recv_message(&mut laptop, "prio-1").await;
    expect_no_message(&mut phone, "prio-1").await;

    go_available(&mut phone, &fixture.pool, "alice@localhost/phone", 9).await;
    send_element(&mut bob, chat("alice@localhost", "prio-2", "to the phone")).await;
    recv_message(&mut phone, "prio-2").await;
    expect_no_message(&mut laptop, "prio-2").await;
}

#[tokio::test]
async fn a_second_resource_holds_its_own_slot() {
    let fixture = fixture().await;
    let _laptop = bound(&fixture.server, 0, "laptop").await;
    let _phone = bound(&fixture.server, 0, "phone").await;
    let laptop = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        holds_binding(row, STATE_ACTIVE)
    })
    .await
    .expect("laptop row");
    let phone = wait_for_binding(&fixture.pool, "alice@localhost/phone", |row| {
        holds_binding(row, STATE_ACTIVE)
    })
    .await
    .expect("phone row");
    assert_ne!(laptop.binding_ref, phone.binding_ref);
}

#[tokio::test]
async fn binding_the_same_full_jid_retires_the_old_binding_and_closes_its_stream() {
    let fixture = fixture().await;
    let mut old = bound(&fixture.server, 0, "laptop").await;
    let first = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        holds_binding(row, STATE_ACTIVE)
    })
    .await
    .expect("first binding");

    let _new = bound(&fixture.server, 0, "laptop").await;
    old.recv_matching_within(Duration::from_secs(10), |frame| frame.contains("conflict"))
        .await
        .expect("the replaced connection is closed with a conflict");
    let second = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.state == STATE_ACTIVE && row.binding_ref != first.binding_ref)
    })
    .await
    .expect("second binding");
    assert!(second.binding_ref.is_some());
}

#[tokio::test]
async fn detach_and_resume_keep_the_slot_and_do_not_flush_offline_messages_again() {
    let fixture = fixture().await;
    let mut alice = bound(&fixture.server, 0, "laptop").await;
    send_element(
        &mut alice,
        Element::builder("enable", SM_NS)
            .attr(nc!("resume"), "true")
            .build(),
    )
    .await;
    let enabled: Element = alice
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.is("enabled", SM_NS))
        })
        .await
        .expect("enabled")
        .parse()
        .expect("server XML");
    let previd = enabled.attr("id").expect("resume token").to_owned();
    go_available(&mut alice, &fixture.pool, "alice@localhost/laptop", 0).await;
    let attached = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.sm_session_id.is_some() && row.offline_flushed == 1)
    })
    .await
    .expect("attached row");

    drop(alice);
    let detached = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        holds_binding(row, STATE_DETACHED)
    })
    .await
    .expect("detached row");
    assert_eq!(detached.sm_session_id, attached.sm_session_id);
    assert_eq!(detached.binding_ref, attached.binding_ref);

    let mut resumed = authenticate(&fixture.server, 0).await;
    send_element(
        &mut resumed,
        Element::builder("resume", SM_NS)
            .attr(nc!("previd"), previd)
            .attr(nc!("h"), "0")
            .build(),
    )
    .await;
    resumed
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.is("resumed", SM_NS))
        })
        .await
        .expect("resumed");
    let active = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        holds_binding(row, STATE_ACTIVE)
    })
    .await
    .expect("resumed row");
    assert_eq!(active.sm_session_id, attached.sm_session_id);
    assert_ne!(active.binding_ref, attached.binding_ref);
    assert_eq!(active.offline_flushed, 1);
    assert_eq!(active.subscriptions_flushed, attached.subscriptions_flushed);

    // A message stored while nobody was routable must not be flushed a
    // second time by a presence the resumed session sends.
    send_element(&mut resumed, available(0, "back")).await;
    let after = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        holds_binding(row, STATE_ACTIVE)
    })
    .await
    .expect("row after presence");
    assert_eq!(after.offline_flushed, 1);
}

#[tokio::test]
async fn closing_a_stream_releases_the_session_and_then_the_binding() {
    let fixture = fixture().await;
    let mut alice = bound(&fixture.server, 0, "laptop").await;
    send_element(
        &mut alice,
        Element::builder("enable", SM_NS)
            .attr(nc!("resume"), "true")
            .build(),
    )
    .await;
    alice
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.is("enabled", SM_NS))
        })
        .await
        .expect("enabled");
    go_available(&mut alice, &fixture.pool, "alice@localhost/laptop", 0).await;
    let attached = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.sm_session_id.is_some())
    })
    .await
    .expect("attached row");
    let session = attached.sm_session_id.expect("session reference");

    alice.close().await.expect("close the stream");
    wait_for_binding(&fixture.pool, "alice@localhost/laptop", is_free).await;
    let referencing: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM account_resource_bindings WHERE sm_session_id = ?",
    )
    .bind(&session)
    .fetch_one(&fixture.pool)
    .await
    .expect("count references");
    assert_eq!(referencing, 0, "no resource row names the closed session");
    let state: i64 =
        sqlx::query_scalar("SELECT state FROM sm_durable_sessions WHERE session_id = ?")
            .bind(&session)
            .fetch_one(&fixture.pool)
            .await
            .expect("the session row outlives the binding");
    assert_eq!(state, 4, "the session is terminal");
}

#[tokio::test]
async fn a_probe_is_answered_from_the_coordinator_state() {
    let fixture = fixture().await;
    let mut laptop = bound(&fixture.server, 0, "laptop").await;
    send_element(&mut laptop, available(3, "at the desk")).await;
    wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.presence_available == 1 && row.presence_priority == 3)
    })
    .await;

    let mut phone = bound(&fixture.server, 0, "phone").await;
    send_element(
        &mut phone,
        Element::builder("presence", CLIENT_NS)
            .attr(nc!("type"), "probe")
            .attr(nc!("to"), "alice@localhost")
            .build(),
    )
    .await;
    let answer: Element = phone
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.is("presence", CLIENT_NS)
                    && element.attr("from") == Some("alice@localhost/laptop")
            })
        })
        .await
        .expect("probe answer")
        .parse()
        .expect("server XML");
    assert_eq!(
        answer
            .get_child("status", CLIENT_NS)
            .map(minidom::Element::text),
        Some("at the desk".to_owned())
    );
    assert_eq!(
        answer
            .get_child("priority", CLIENT_NS)
            .map(minidom::Element::text),
        Some("3".to_owned())
    );
}

const CARBONS_NS: &str = "urn:xmpp:carbons:2";
const ROSTER_NS: &str = "jabber:iq:roster";

fn unavailable() -> Element {
    Element::builder("presence", CLIENT_NS)
        .attr(nc!("type"), "unavailable")
        .build()
}

fn iq(kind: &str, id: &str, payload: Element) -> Element {
    Element::builder("iq", CLIENT_NS)
        .attr(nc!("type"), kind)
        .attr(nc!("id"), id)
        .append(payload)
        .build()
}

async fn recv_element<F: Fn(&Element) -> bool>(
    client: &mut WsXmppClient,
    what: &str,
    accept: F,
) -> Element {
    client
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| accept(&element))
        })
        .await
        .unwrap_or_else(|error| panic!("{what} did not arrive: {error}"))
        .parse()
        .expect("server XML")
}

async fn enable_stream_management(client: &mut WsXmppClient) -> String {
    send_element(
        client,
        Element::builder("enable", SM_NS)
            .attr(nc!("resume"), "true")
            .build(),
    )
    .await;
    recv_element(client, "enabled", |element| element.is("enabled", SM_NS))
        .await
        .attr("id")
        .expect("resume token")
        .to_owned()
}

#[tokio::test]
async fn an_unavailable_resource_no_longer_receives_bare_jid_messages() {
    let fixture = fixture().await;
    let mut laptop = bound(&fixture.server, 0, "laptop").await;
    let mut phone = bound(&fixture.server, 0, "phone").await;
    go_available(&mut laptop, &fixture.pool, "alice@localhost/laptop", 5).await;
    go_available(&mut phone, &fixture.pool, "alice@localhost/phone", 1).await;

    send_element(&mut laptop, unavailable()).await;
    let row = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.presence_available == 0)
    })
    .await
    .expect("laptop row");
    assert_eq!(row.state, STATE_ACTIVE, "the binding outlives its presence");

    let mut bob = bound(&fixture.server, 1, "desk").await;
    send_element(&mut bob, chat("alice@localhost", "gone-1", "to the phone")).await;
    recv_message(&mut phone, "gone-1").await;
    expect_no_message(&mut laptop, "gone-1").await;
}

#[tokio::test]
async fn a_negative_priority_resource_gets_the_message_once_it_turns_non_negative() {
    let fixture = fixture().await;
    let mut alice = bound(&fixture.server, 0, "laptop").await;
    go_available(&mut alice, &fixture.pool, "alice@localhost/laptop", -1).await;

    let mut bob = bound(&fixture.server, 1, "desk").await;
    send_element(&mut bob, chat("alice@localhost", "neg-1", "stored")).await;
    expect_no_message(&mut alice, "neg-1").await;
    let before = binding(&fixture.pool, "alice@localhost/laptop")
        .await
        .expect("row");
    assert_eq!(before.offline_flushed, 0);

    go_available(&mut alice, &fixture.pool, "alice@localhost/laptop", 0).await;
    recv_message(&mut alice, "neg-1").await;
    let after = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.offline_flushed == 1)
    })
    .await
    .expect("row");
    assert_eq!(after.presence_priority, 0);
}

#[tokio::test]
async fn carbons_follow_the_coordinator_snapshot() {
    let fixture = fixture().await;
    let mut laptop = bound(&fixture.server, 0, "laptop").await;
    let mut phone = bound(&fixture.server, 0, "phone").await;
    go_available(&mut laptop, &fixture.pool, "alice@localhost/laptop", 0).await;
    go_available(&mut phone, &fixture.pool, "alice@localhost/phone", 0).await;
    send_element(
        &mut phone,
        iq(
            "set",
            "carbons-1",
            Element::builder("enable", CARBONS_NS).build(),
        ),
    )
    .await;
    recv_element(&mut phone, "carbons result", |element| {
        element.is("iq", CLIENT_NS) && element.attr("id") == Some("carbons-1")
    })
    .await;
    wait_for_binding(&fixture.pool, "alice@localhost/phone", |row| {
        row.is_some_and(|row| row.carbons_enabled == 1)
    })
    .await;

    let mut bob = bound(&fixture.server, 1, "desk").await;
    send_element(
        &mut bob,
        chat("alice@localhost/laptop", "carbon-1", "to the laptop"),
    )
    .await;
    recv_message(&mut laptop, "carbon-1").await;
    let received = recv_element(&mut phone, "received carbon", |element| {
        element.is("message", CLIENT_NS) && element.has_child("received", CARBONS_NS)
    })
    .await;
    assert_eq!(received.attr("from"), Some("alice@localhost"));

    send_element(&mut laptop, chat("bob@localhost/desk", "carbon-2", "reply")).await;
    recv_message(&mut bob, "carbon-2").await;
    recv_element(&mut phone, "sent carbon", |element| {
        element.is("message", CLIENT_NS) && element.has_child("sent", CARBONS_NS)
    })
    .await;
}

#[tokio::test]
async fn a_closed_resource_is_freed_and_its_next_binding_receives_the_offline_message() {
    let fixture = fixture().await;
    let mut alice = bound(&fixture.server, 0, "laptop").await;
    go_available(&mut alice, &fixture.pool, "alice@localhost/laptop", 0).await;
    alice.close().await.expect("close the stream");
    wait_for_binding(&fixture.pool, "alice@localhost/laptop", is_free).await;

    let mut bob = bound(&fixture.server, 1, "desk").await;
    send_element(&mut bob, chat("alice@localhost", "offline-1", "later")).await;
    // The sender's next round trip orders the test after the message.
    send_element(
        &mut bob,
        iq(
            "get",
            "ping-1",
            Element::builder("ping", "urn:xmpp:ping").build(),
        ),
    )
    .await;
    recv_element(&mut bob, "ping result", |element| {
        element.is("iq", CLIENT_NS) && element.attr("id") == Some("ping-1")
    })
    .await;

    let mut again = bound(&fixture.server, 0, "laptop").await;
    go_available(&mut again, &fixture.pool, "alice@localhost/laptop", 0).await;
    recv_message(&mut again, "offline-1").await;
    let row = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.offline_flushed == 1)
    })
    .await
    .expect("row");
    assert_eq!(row.state, STATE_ACTIVE);
}

#[tokio::test]
async fn a_resumed_session_is_routable_on_the_snapshot_it_inherited() {
    let fixture = fixture().await;
    let mut alice = bound(&fixture.server, 0, "laptop").await;
    let previd = enable_stream_management(&mut alice).await;
    go_available(&mut alice, &fixture.pool, "alice@localhost/laptop", 2).await;
    wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.sm_session_id.is_some())
    })
    .await;
    drop(alice);
    wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        holds_binding(row, STATE_DETACHED)
    })
    .await;

    let mut bob = bound(&fixture.server, 1, "desk").await;
    let mut resumed = authenticate(&fixture.server, 0).await;
    send_element(
        &mut resumed,
        Element::builder("resume", SM_NS)
            .attr(nc!("previd"), previd)
            .attr(nc!("h"), "0")
            .build(),
    )
    .await;
    recv_element(&mut resumed, "resumed", |element| {
        element.is("resumed", SM_NS)
    })
    .await;
    let row = wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        holds_binding(row, STATE_ACTIVE)
    })
    .await
    .expect("resumed row");
    assert_eq!(row.presence_available, 1, "the resume keeps the presence");
    assert_eq!(row.presence_priority, 2);

    // No presence follows the resume: the coordinator routes on the
    // snapshot the resumed binding inherited.
    send_element(
        &mut bob,
        chat("alice@localhost", "resumed-1", "welcome back"),
    )
    .await;
    recv_message(&mut resumed, "resumed-1").await;
}

#[tokio::test]
async fn a_roster_push_reaches_the_interested_resource() {
    let fixture = fixture().await;
    let mut laptop = bound(&fixture.server, 0, "laptop").await;
    let mut phone = bound(&fixture.server, 0, "phone").await;
    send_element(
        &mut laptop,
        iq(
            "get",
            "roster-1",
            Element::builder("query", ROSTER_NS).build(),
        ),
    )
    .await;
    recv_element(&mut laptop, "roster result", |element| {
        element.is("iq", CLIENT_NS) && element.attr("id") == Some("roster-1")
    })
    .await;
    wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.roster_interested == 1)
    })
    .await;

    send_element(
        &mut phone,
        iq(
            "set",
            "roster-2",
            Element::builder("query", ROSTER_NS)
                .append(
                    Element::builder("item", ROSTER_NS)
                        .attr(nc!("jid"), "bob@localhost")
                        .attr(nc!("name"), "Bob")
                        .build(),
                )
                .build(),
        ),
    )
    .await;
    let push = recv_element(&mut laptop, "roster push", |element| {
        element.is("iq", CLIENT_NS)
            && element.attr("type") == Some("set")
            && element.has_child("query", ROSTER_NS)
    })
    .await;
    let item = push
        .get_child("query", ROSTER_NS)
        .and_then(|query| query.get_child("item", ROSTER_NS))
        .expect("pushed item");
    assert_eq!(item.attr("jid"), Some("bob@localhost"));
    let phone_row = binding(&fixture.pool, "alice@localhost/phone")
        .await
        .expect("phone row");
    assert_eq!(phone_row.roster_interested, 0);
}

#[tokio::test]
async fn a_binding_of_a_previous_process_is_retired_when_its_resource_binds_again() {
    let dir = tempfile::tempdir().expect("test directory");
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("database.sqlite").display()
    );
    let first_server = TestServer::start_persistent_with_extra_envs(&db_url, ACCOUNTS, SELECTORS);
    let pool = sqlx::SqlitePool::connect(&db_url)
        .await
        .expect("observe SQLite");
    let mut laptop = bound(&first_server, 0, "laptop").await;
    let mut phone = bound(&first_server, 0, "phone").await;
    go_available(&mut laptop, &pool, "alice@localhost/laptop", 1).await;
    go_available(&mut phone, &pool, "alice@localhost/phone", 9).await;
    let before = binding(&pool, "alice@localhost/laptop")
        .await
        .expect("laptop row");
    // The process ends without running any connection's teardown, so both
    // rows still hold an active binding.
    drop(first_server);
    assert!(holds_binding(
        binding(&pool, "alice@localhost/phone").await.as_ref(),
        STATE_ACTIVE
    ));

    let server = TestServer::start_persistent_with_extra_envs(&db_url, ACCOUNTS, SELECTORS);
    let mut again = bound(&server, 0, "laptop").await;
    let after = wait_for_binding(&pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.state == STATE_ACTIVE && row.binding_ref != before.binding_ref)
    })
    .await
    .expect("laptop row");
    assert_eq!(
        after.presence_available, 0,
        "the new binding starts without the previous binding's presence"
    );
    go_available(&mut again, &pool, "alice@localhost/laptop", 0).await;

    // The phone's binding belongs to the previous process: it has no
    // connection and is not a destination, whatever priority it recorded.
    let mut bob = bound(&server, 1, "desk").await;
    send_element(
        &mut bob,
        chat("alice@localhost", "restart-1", "hello again"),
    )
    .await;
    recv_message(&mut again, "restart-1").await;
}

fn presence_to(kind: &str, to: &str) -> Element {
    Element::builder("presence", CLIENT_NS)
        .attr(nc!("type"), kind)
        .attr(nc!("to"), to)
        .build()
}

fn is_presence_from(element: &Element, from: &str, kind: Option<&str>) -> bool {
    element.is("presence", CLIENT_NS)
        && element.attr("from") == Some(from)
        && element.attr("type") == kind
}

#[tokio::test]
async fn a_subscription_request_is_presented_once_and_presence_reaches_the_subscriber() {
    let fixture = fixture().await;
    let mut alice = bound(&fixture.server, 0, "laptop").await;
    go_available(&mut alice, &fixture.pool, "alice@localhost/laptop", 0).await;
    send_element(
        &mut alice,
        iq(
            "get",
            "roster-3",
            Element::builder("query", ROSTER_NS).build(),
        ),
    )
    .await;
    wait_for_binding(&fixture.pool, "alice@localhost/laptop", |row| {
        row.is_some_and(|row| row.roster_interested == 1)
    })
    .await;
    send_element(&mut alice, presence_to("subscribe", "bob@localhost")).await;
    // The requester's next round trip orders the test after the request.
    send_element(
        &mut alice,
        iq(
            "get",
            "ping-3",
            Element::builder("ping", "urn:xmpp:ping").build(),
        ),
    )
    .await;
    recv_element(&mut alice, "ping result", |element| {
        element.is("iq", CLIENT_NS) && element.attr("id") == Some("ping-3")
    })
    .await;

    let mut bob = bound(&fixture.server, 1, "desk").await;
    go_available(&mut bob, &fixture.pool, "bob@localhost/desk", 4).await;
    recv_element(&mut bob, "queued subscription request", |element| {
        is_presence_from(element, "alice@localhost", Some("subscribe"))
    })
    .await;
    let row = wait_for_binding(&fixture.pool, "bob@localhost/desk", |row| {
        row.is_some_and(|row| row.subscriptions_flushed == 1)
    })
    .await
    .expect("bob row");
    assert_eq!(row.state, STATE_ACTIVE);

    send_element(&mut bob, presence_to("subscribed", "alice@localhost")).await;
    // The approval goes to the requester's roster-interested resources and
    // the contact's current presence to its available ones; this resource
    // is both, and the two arrive in either order.
    let mut approved = false;
    let mut current = None;
    while !approved || current.is_none() {
        let frame = recv_element(&mut alice, "approval and current presence", |element| {
            is_presence_from(element, "bob@localhost", Some("subscribed"))
                || is_presence_from(element, "bob@localhost/desk", None)
        })
        .await;
        if frame.attr("type") == Some("subscribed") {
            approved = true;
        } else {
            current = Some(frame);
        }
    }
    assert_eq!(
        current
            .expect("current presence")
            .get_child("priority", CLIENT_NS)
            .map(minidom::Element::text),
        Some("4".to_owned())
    );

    send_element(&mut bob, available(6, "in a meeting")).await;
    let update = recv_element(&mut alice, "presence update", |element| {
        is_presence_from(element, "bob@localhost/desk", None)
            && element
                .get_child("status", CLIENT_NS)
                .is_some_and(|status| status.text() == "in a meeting")
    })
    .await;
    assert_eq!(update.attr("to"), Some("alice@localhost"));
    wait_for_binding(&fixture.pool, "bob@localhost/desk", |row| {
        row.is_some_and(|row| row.presence_priority == 6)
    })
    .await;

    send_element(&mut alice, presence_to("probe", "bob@localhost")).await;
    let answer = recv_element(&mut alice, "probe answer", |element| {
        is_presence_from(element, "bob@localhost/desk", None)
    })
    .await;
    assert_eq!(
        answer
            .get_child("status", CLIENT_NS)
            .map(minidom::Element::text),
        Some("in a meeting".to_owned())
    );

    bob.close().await.expect("close the stream");
    recv_element(&mut alice, "unavailable presence", |element| {
        is_presence_from(element, "bob@localhost/desk", Some("unavailable"))
    })
    .await;
}

#[tokio::test]
async fn a_lost_transport_without_stream_management_frees_the_resource() {
    let fixture = fixture().await;
    let mut alice = bound(&fixture.server, 0, "laptop").await;
    go_available(&mut alice, &fixture.pool, "alice@localhost/laptop", 0).await;
    drop(alice);
    wait_for_binding(&fixture.pool, "alice@localhost/laptop", is_free).await;
}
