//! Delivery to a detached durable XEP-0198 session on a single node with
//! the durable session authority selected (`WADDLE_XMPP_DURABLE_SM=durable`).
//!
//! A session whose transport is gone is a recipient for the rest of its
//! resume window: a stanza sent to it is retained in the session's custody
//! and replayed once, in order, when the client resumes, or transferred to
//! offline delivery once when the session expires. Resource selection for a
//! bare JID follows the rule a detached legacy session follows.

#![cfg(feature = "test-support")]

use std::time::Duration;

use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::SM_NS;

const ACCOUNTS: &[(&str, &str)] = &[
    ("alice", "durable-detached-alice"),
    ("bob", "durable-detached-bob"),
];
const DURABLE: (&str, &str) = ("WADDLE_XMPP_DURABLE_SM", "durable");
const STATE_DETACHED: i64 = 2;
const STATE_TERMINAL: i64 = 4;
const PHONE: &str = "alice@localhost/phone";

/// An owned attribute name for the element builder.
macro_rules! nc {
    ($name:literal) => {
        minidom::rxml::xml_ncname!($name).to_owned()
    };
}

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
    let server = TestServer::start_persistent_with_extra_envs(&db_url, ACCOUNTS, &[DURABLE]);
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

async fn send_element(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize");
    client
        .send(&String::from_utf8(bytes).expect("UTF-8"))
        .await
        .expect("send");
}

fn sm(name: &str) -> minidom::ElementBuilder {
    Element::builder(name, SM_NS)
}

fn resume(previd: &str, h: u32) -> Element {
    sm("resume")
        .attr(nc!("previd"), previd)
        .attr(nc!("h"), h.to_string())
        .build()
}

fn presence() -> Element {
    Element::builder("presence", "jabber:client").build()
}

fn chat(to: &str, id: &str) -> Element {
    Element::builder("message", "jabber:client")
        .attr(nc!("to"), to)
        .attr(nc!("type"), "chat")
        .attr(nc!("id"), id)
        .append(
            Element::builder("body", "jabber:client")
                .append("while detached")
                .build(),
        )
        .build()
}

async fn recv_sm(client: &mut WsXmppClient, name: &str) -> Element {
    let frame = client
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.is(name, SM_NS))
        })
        .await
        .unwrap_or_else(|error| panic!("no <{name}/>: {error}"));
    frame.parse().expect("server XML")
}

/// Every counted stanza the server writes within `wait`: how many there
/// were, and the ids of the chat messages among them in arrival order.
async fn drain_counted(client: &mut WsXmppClient, wait: Duration) -> (u32, Vec<String>) {
    let deadline = tokio::time::Instant::now() + wait;
    let mut counted = 0;
    let mut chats = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return (counted, chats);
        }
        let Ok(frame) = client.recv_timeout(remaining).await else {
            return (counted, chats);
        };
        let Ok(element) = frame.parse::<Element>() else {
            continue;
        };
        if matches!(element.name(), "message" | "presence" | "iq")
            && element.ns() == "jabber:client"
        {
            counted += 1;
        }
        if element.is("message", "jabber:client") && element.attr("type") == Some("chat") {
            if let Some(id) = element.attr("id") {
                chats.push(id.to_owned());
            }
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct SessionRow {
    session_id: String,
    state: i64,
    outbound_acked: i64,
    outbound_appended: i64,
}

async fn wait_for_session<F: Fn(&SessionRow) -> bool>(
    pool: &sqlx::SqlitePool,
    full_jid: &str,
    wait: Duration,
    accept: F,
) -> SessionRow {
    tokio::time::timeout(wait, async {
        loop {
            let rows: Vec<SessionRow> = sqlx::query_as(
                "SELECT session_id, state, outbound_acked, outbound_appended \
                 FROM sm_durable_sessions WHERE full_jid = ? ORDER BY updated_at_ms",
            )
            .bind(full_jid)
            .fetch_all(pool)
            .await
            .expect("read durable sessions");
            if let Some(row) = rows.into_iter().find(&accept) {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("durable session reached the expected state")
}

async fn retained_frames(pool: &sqlx::SqlitePool, session_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM sm_unacked WHERE stream_id = ?")
        .bind(session_id)
        .fetch_one(pool)
        .await
        .expect("read retained frames")
}

/// Alice's phone with resumable stream management, then its transport lost
/// with nothing unacknowledged. Returns the resume token, the handled count
/// the client reports on resume and the detached session's id.
async fn detached_phone(fixture: &Fixture) -> (String, u32, String) {
    let mut alice = authenticate(&fixture.server, 0).await;
    alice.bind("phone").await.expect("bind phone");
    send_element(&mut alice, sm("enable").attr(nc!("resume"), "true").build()).await;
    let token = recv_sm(&mut alice, "enabled")
        .await
        .attr("id")
        .expect("resume token")
        .to_owned();
    send_element(&mut alice, presence()).await;
    let (handled, _) = drain_counted(&mut alice, Duration::from_secs(2)).await;
    drop(alice);
    let detached = wait_for_session(&fixture.pool, PHONE, Duration::from_secs(15), |row| {
        row.state == STATE_DETACHED
    })
    .await;
    (token, handled, detached.session_id)
}

async fn bob(server: &TestServer) -> WsXmppClient {
    let mut bob = authenticate(server, 1).await;
    bob.bind("laptop").await.expect("bob bind");
    bob
}

async fn wait_for_retained(pool: &sqlx::SqlitePool, session_id: &str, count: i64) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while retained_frames(pool, session_id).await < count {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the detached session retained the routed stanzas");
}

/// Messages sent to the detached phone are retained in its custody, not
/// stored offline, and a resume replays each once in the order sent, ahead
/// of live output.
#[tokio::test]
async fn messages_to_a_detached_durable_session_replay_once_in_order_on_resume() {
    let fixture = fixture().await;
    let (token, handled, session_id) = detached_phone(&fixture).await;

    let mut bob = bob(&fixture.server).await;
    for id in ["d1", "d2", "d3"] {
        send_element(&mut bob, chat(PHONE, id)).await;
    }
    wait_for_retained(&fixture.pool, &session_id, 3).await;

    let mut alice = authenticate(&fixture.server, 0).await;
    send_element(&mut alice, resume(&token, handled)).await;
    recv_sm(&mut alice, "resumed").await;
    send_element(&mut bob, chat(PHONE, "live")).await;
    let (_, chats) = drain_counted(&mut alice, Duration::from_secs(5)).await;
    assert_eq!(
        chats,
        ["d1", "d2", "d3", "live"],
        "the retained messages are replayed once, in order, ahead of live output"
    );
}

/// A message retained by a detached session whose window elapses reaches
/// offline delivery exactly once, and the session releases its custody.
#[tokio::test]
async fn a_message_retained_by_an_expired_detached_session_is_delivered_offline_once() {
    let fixture = fixture().await;
    let (_token, _handled, session_id) = detached_phone(&fixture).await;

    let mut bob = bob(&fixture.server).await;
    send_element(&mut bob, chat(PHONE, "x1")).await;
    wait_for_retained(&fixture.pool, &session_id, 1).await;

    // The window is moved into the past; the janitor's next sweep expires
    // the session and transfers what it retained.
    sqlx::query(
        "UPDATE sm_durable_sessions \
         SET detached_at_ms = detached_at_ms - (max_resume_secs + 1) * 1000 \
         WHERE session_id = ?",
    )
    .bind(&session_id)
    .execute(&fixture.pool)
    .await
    .expect("elapse the resume window");
    let expired = wait_for_session(&fixture.pool, PHONE, Duration::from_secs(150), |row| {
        row.state == STATE_TERMINAL && row.outbound_acked == row.outbound_appended
    })
    .await;
    assert_eq!(retained_frames(&fixture.pool, &expired.session_id).await, 0);

    let mut alice = authenticate(&fixture.server, 0).await;
    alice.bind("desk").await.expect("bind desk");
    send_element(&mut alice, presence()).await;
    let (_, chats) = drain_counted(&mut alice, Duration::from_secs(5)).await;
    assert_eq!(
        chats,
        ["x1"],
        "the retained message is delivered offline once"
    );
}

/// A bare-JID message with one live available resource and one detached
/// durable session follows the rule a detached legacy session follows: the
/// live resource of the highest non-negative priority receives it, and the
/// detached session retains it for its resume.
#[tokio::test]
async fn a_bare_jid_message_reaches_the_live_resource_and_the_detached_session() {
    let fixture = fixture().await;
    let (token, handled, session_id) = detached_phone(&fixture).await;

    let mut desk = authenticate(&fixture.server, 0).await;
    desk.bind("desk").await.expect("bind desk");
    send_element(&mut desk, presence()).await;
    drain_counted(&mut desk, Duration::from_secs(2)).await;

    let mut bob = bob(&fixture.server).await;
    send_element(&mut bob, chat("alice@localhost", "b1")).await;
    let (_, at_desk) = drain_counted(&mut desk, Duration::from_secs(5)).await;
    assert_eq!(
        at_desk,
        ["b1"],
        "the live resource receives the message once"
    );
    wait_for_retained(&fixture.pool, &session_id, 1).await;

    let mut phone = authenticate(&fixture.server, 0).await;
    send_element(&mut phone, resume(&token, handled)).await;
    recv_sm(&mut phone, "resumed").await;
    let (_, at_phone) = drain_counted(&mut phone, Duration::from_secs(5)).await;
    assert_eq!(
        at_phone,
        ["b1"],
        "the detached session replays the message once"
    );
}
