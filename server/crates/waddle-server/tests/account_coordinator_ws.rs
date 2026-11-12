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
}

async fn binding(pool: &sqlx::SqlitePool, full_jid: &str) -> Option<BindingRow> {
    sqlx::query_as(
        "SELECT full_jid, state, binding_ref, presence_available, presence_priority, \
         sm_session_id, offline_flushed, subscriptions_flushed \
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
