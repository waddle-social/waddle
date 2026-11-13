//! XEP-0198 over the loopback WebSocket with the durable session authority
//! selected (`WADDLE_XMPP_DURABLE_SM=durable`) on single-node SQLite.
//!
//! The durable authority commits a session before `<enabled/>` is built,
//! hands the client a resume token that is not the storage key, takes
//! custody of every counted outbound stanza before it is written, and owns
//! resume, detach, close and replacement. These tests observe the wire and
//! the authority's rows.

#![cfg(feature = "test-support")]

use std::time::Duration;

use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::SM_NS;

const ACCOUNTS: &[(&str, &str)] = &[("alice", "durable-sm-alice"), ("bob", "durable-sm-bob")];
const DURABLE: (&str, &str) = ("WADDLE_XMPP_DURABLE_SM", "durable");

/// Authority row states, as `sm_durable_sessions.state` stores them.
const STATE_LIVE: i64 = 1;
const STATE_DETACHED: i64 = 2;
const STATE_TERMINAL: i64 = 4;
/// `sm_durable_sessions.terminal_reason` values.
const REASON_REPLACED: i64 = 3;
const REASON_CLOSED: i64 = 4;

struct Fixture {
    server: TestServer,
    pool: sqlx::SqlitePool,
    _dir: tempfile::TempDir,
}

async fn fixture(extra_envs: &[(&str, &str)]) -> Fixture {
    let dir = tempfile::tempdir().expect("test directory");
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("database.sqlite").display()
    );
    let mut envs = vec![DURABLE];
    envs.extend_from_slice(extra_envs);
    let server = TestServer::start_persistent_with_extra_envs(&db_url, ACCOUNTS, &envs);
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

/// An owned attribute name for the element builder.
macro_rules! nc {
    ($name:literal) => {
        minidom::rxml::xml_ncname!($name).to_owned()
    };
}

fn sm(name: &str) -> minidom::ElementBuilder {
    Element::builder(name, SM_NS)
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

/// Bind `resource` and enable resumable stream management; returns the
/// `<enabled/>` reply.
async fn bind_and_enable(client: &mut WsXmppClient, resource: &str) -> Element {
    client.bind(resource).await.expect("bind");
    send_element(client, sm("enable").attr(nc!("resume"), "true").build()).await;
    recv_sm(client, "enabled").await
}

#[derive(Debug, sqlx::FromRow)]
struct SessionRow {
    session_id: String,
    state: i64,
    terminal_reason: Option<i64>,
    generation: i64,
    inbound_handled: i64,
    outbound_acked: i64,
    outbound_exposure_ceiling: i64,
    outbound_appended: i64,
}

async fn sessions(pool: &sqlx::SqlitePool, full_jid: &str) -> Vec<SessionRow> {
    sqlx::query_as(
        "SELECT session_id, state, terminal_reason, generation, inbound_handled, outbound_acked, \
         outbound_exposure_ceiling, outbound_appended FROM sm_durable_sessions \
         WHERE full_jid = ? ORDER BY updated_at_ms",
    )
    .bind(full_jid)
    .fetch_all(pool)
    .await
    .expect("read durable sessions")
}

async fn wait_for_session<F: Fn(&SessionRow) -> bool>(
    pool: &sqlx::SqlitePool,
    full_jid: &str,
    accept: F,
) -> SessionRow {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(row) = sessions(pool, full_jid).await.into_iter().find(&accept) {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
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

fn chat(to: &str, id: &str, body: &str) -> Element {
    Element::builder("message", "jabber:client")
        .attr(nc!("to"), to)
        .attr(nc!("type"), "chat")
        .attr(nc!("id"), id)
        .append(
            Element::builder("body", "jabber:client")
                .append(body)
                .build(),
        )
        .build()
}

fn is_counted_stanza(element: &Element) -> bool {
    matches!(element.name(), "message" | "presence" | "iq") && element.ns() == "jabber:client"
}

/// Read frames until every chat message in `ids` has arrived. Returns how
/// many counted stanzas (XEP-0198 `h` units) were read, in arrival order.
async fn recv_chats(client: &mut WsXmppClient, ids: &[&str]) -> u32 {
    let mut counted = 0;
    let mut missing: Vec<&str> = ids.to_vec();
    while !missing.is_empty() {
        let frame = client
            .recv_timeout(Duration::from_secs(10))
            .await
            .expect("frame before the expected chat messages");
        let Ok(element) = frame.parse::<Element>() else {
            continue;
        };
        if is_counted_stanza(&element) {
            counted += 1;
        }
        if element.name() == "message" {
            missing.retain(|id| element.attr("id") != Some(*id));
        }
    }
    counted
}

/// Bob, bound and ready to message Alice.
async fn bob(server: &TestServer) -> WsXmppClient {
    let mut bob = authenticate(server, 1).await;
    bob.bind("laptop").await.expect("bob bind");
    bob
}

#[tokio::test]
async fn durable_enable_commits_the_session_before_enabled_and_hides_its_key() {
    let fixture = fixture(&[]).await;
    let mut alice = authenticate(&fixture.server, 0).await;
    let enabled = bind_and_enable(&mut alice, "phone").await;

    assert_eq!(enabled.attr("resume"), Some("true"));
    let token = enabled.attr("id").expect("resume token").to_owned();
    let rows = sessions(&fixture.pool, "alice@localhost/phone").await;
    assert_eq!(rows.len(), 1, "one session row committed for the enable");
    assert_eq!(rows[0].state, STATE_LIVE);
    assert_eq!(rows[0].generation, 1);
    assert_ne!(
        rows[0].session_id, token,
        "the resume token on the wire is not the storage key"
    );
    assert!(
        uuid::Uuid::parse_str(&token).is_err(),
        "the client holds an opaque token, not a session key"
    );
    let stored_plaintext: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sm_durable_sessions WHERE resume_token_sha256 = ? OR session_id = ?",
    )
    .bind(&token)
    .bind(&token)
    .fetch_one(&fixture.pool)
    .await
    .expect("search for the plaintext token");
    assert_eq!(stored_plaintext, 0, "only the token digest is stored");

    send_element(&mut alice, sm("enable").attr(nc!("resume"), "true").build()).await;
    let failed = recv_sm(&mut alice, "failed").await;
    assert!(failed
        .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert_eq!(
        sessions(&fixture.pool, "alice@localhost/phone").await.len(),
        1,
        "a refused enable commits no second session"
    );
}

#[tokio::test]
async fn non_resumable_enable_creates_no_durable_session() {
    let fixture = fixture(&[]).await;
    let mut alice = authenticate(&fixture.server, 0).await;
    alice.bind("tablet").await.expect("bind");
    send_element(&mut alice, sm("enable").build()).await;
    let enabled = recv_sm(&mut alice, "enabled").await;

    assert_eq!(enabled.attr("resume"), None);
    assert!(sessions(&fixture.pool, "alice@localhost/tablet")
        .await
        .is_empty());
}

#[tokio::test]
async fn counted_stanzas_are_in_durable_custody_and_an_ack_releases_its_prefix() {
    let fixture = fixture(&[]).await;
    let mut alice = authenticate(&fixture.server, 0).await;
    bind_and_enable(&mut alice, "phone").await;
    let mut bob = bob(&fixture.server).await;

    send_element(&mut bob, chat("alice@localhost/phone", "m1", "one")).await;
    send_element(&mut bob, chat("alice@localhost/phone", "m2", "two")).await;
    let written = recv_chats(&mut alice, &["m1", "m2"]).await;
    assert!(written >= 2);

    let row = wait_for_session(&fixture.pool, "alice@localhost/phone", |row| {
        row.outbound_appended >= i64::from(written)
    })
    .await;
    assert_eq!(
        row.outbound_exposure_ceiling, row.outbound_appended,
        "a live owner's frame is exposed by the commit that stores it"
    );
    assert_eq!(row.outbound_acked, 0);
    assert_eq!(
        retained_frames(&fixture.pool, &row.session_id).await,
        row.outbound_appended,
        "every counted stanza on the wire has a retained frame"
    );

    send_element(&mut alice, sm("a").attr(nc!("h"), "1").build()).await;
    let acked = wait_for_session(&fixture.pool, "alice@localhost/phone", |row| {
        row.outbound_acked == 1
    })
    .await;
    assert_eq!(
        retained_frames(&fixture.pool, &acked.session_id).await,
        acked.outbound_appended - 1,
        "the acknowledgement releases exactly the acknowledged frame"
    );
}

#[tokio::test]
async fn acknowledging_a_stanza_that_was_never_exposed_is_a_stream_error() {
    let fixture = fixture(&[]).await;
    let mut alice = authenticate(&fixture.server, 0).await;
    bind_and_enable(&mut alice, "phone").await;
    let mut bob = bob(&fixture.server).await;
    send_element(&mut bob, chat("alice@localhost/phone", "m1", "one")).await;
    let written = recv_chats(&mut alice, &["m1"]).await;

    send_element(&mut alice, sm("a").attr(nc!("h"), "5000").build()).await;
    let error = alice
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame.contains("handled-count-too-high")
        })
        .await
        .expect("handled-count-too-high stream error");
    assert!(error.contains(SM_NS));

    let row = wait_for_session(&fixture.pool, "alice@localhost/phone", |row| {
        row.outbound_appended >= i64::from(written)
    })
    .await;
    assert_eq!(row.outbound_acked, 0, "an invalid count releases nothing");
    assert_eq!(
        retained_frames(&fixture.pool, &row.session_id).await,
        row.outbound_appended
    );
}

#[tokio::test]
async fn answering_a_request_reports_the_handled_count_to_the_authority() {
    let fixture = fixture(&[]).await;
    let mut alice = authenticate(&fixture.server, 0).await;
    bind_and_enable(&mut alice, "phone").await;

    for id in ["p1", "p2"] {
        send_element(
            &mut alice,
            Element::builder("iq", "jabber:client")
                .attr(nc!("type"), "get")
                .attr(nc!("id"), id)
                .append(Element::builder("ping", "urn:xmpp:ping").build())
                .build(),
        )
        .await;
    }
    send_element(&mut alice, sm("r").build()).await;
    let answer = recv_sm(&mut alice, "a").await;
    assert_eq!(answer.attr("h"), Some("2"));

    wait_for_session(&fixture.pool, "alice@localhost/phone", |row| {
        row.inbound_handled == 2
    })
    .await;
}

fn resume(previd: &str, h: u32) -> Element {
    sm("resume")
        .attr(nc!("previd"), previd)
        .attr(nc!("h"), h.to_string())
        .build()
}

#[tokio::test]
async fn a_resume_token_that_names_no_session_fails_and_leaves_the_client_free_to_bind() {
    let fixture = fixture(&[]).await;
    let mut alice = authenticate(&fixture.server, 0).await;

    // Neither a value that is not a token nor a well-formed token nobody was
    // issued names a session.
    for previd in [
        "8a1c5d6e-0000-4000-8000-000000000000",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        send_element(&mut alice, resume(previd, 0)).await;
        let failed = recv_sm(&mut alice, "failed").await;
        assert!(failed
            .get_child("item-not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some());
    }

    assert_eq!(
        alice
            .bind("phone")
            .await
            .expect("fresh bind after rejection"),
        "alice@localhost/phone"
    );
    assert!(sessions(&fixture.pool, "alice@localhost/phone")
        .await
        .is_empty());
}

/// Alice with a resumable session on `phone` that holds the chat messages
/// `ids` unacknowledged, and whose transport was then lost. Returns the
/// resume token and how many counted stanzas the lost transport carried.
async fn detached_session_holding(fixture: &Fixture, ids: &[&str]) -> (String, u32) {
    let mut alice = authenticate(&fixture.server, 0).await;
    let enabled = bind_and_enable(&mut alice, "phone").await;
    let token = enabled.attr("id").expect("resume token").to_owned();
    let mut bob = bob(&fixture.server).await;
    for id in ids {
        send_element(&mut bob, chat("alice@localhost/phone", id, "held")).await;
    }
    let written = recv_chats(&mut alice, ids).await;
    drop(alice);
    wait_for_session(&fixture.pool, "alice@localhost/phone", |row| {
        row.state == STATE_DETACHED
    })
    .await;
    (token, written)
}

#[tokio::test]
async fn a_lost_transport_detaches_and_a_resume_replays_before_live_output() {
    let fixture = fixture(&[]).await;
    let (token, written) = detached_session_holding(&fixture, &["m1", "m2"]).await;
    let detached = wait_for_session(&fixture.pool, "alice@localhost/phone", |row| {
        row.state == STATE_DETACHED
    })
    .await;
    assert_eq!(detached.generation, 1);
    assert_eq!(
        retained_frames(&fixture.pool, &detached.session_id).await,
        i64::from(written),
        "the detached session retains every unacknowledged frame"
    );

    // The client handled all but the last stanza.
    let mut alice = authenticate(&fixture.server, 0).await;
    send_element(&mut alice, resume(&token, written - 1)).await;
    let resumed = recv_sm(&mut alice, "resumed").await;
    assert_eq!(resumed.attr("previd"), Some(token.as_str()));
    assert_eq!(
        recv_chats(&mut alice, &["m2"]).await,
        1,
        "exactly the unhandled stanza is replayed"
    );

    let live = wait_for_session(&fixture.pool, "alice@localhost/phone", |row| {
        row.state == STATE_LIVE
    })
    .await;
    assert_eq!(
        live.session_id, detached.session_id,
        "the same session resumed"
    );
    assert_eq!(
        live.generation, 2,
        "the resume committed the next generation"
    );
    assert_eq!(live.outbound_acked, i64::from(written - 1));

    let mut bob = bob(&fixture.server).await;
    send_element(&mut bob, chat("alice@localhost/phone", "m3", "live")).await;
    recv_chats(&mut alice, &["m3"]).await;
    let after = wait_for_session(&fixture.pool, "alice@localhost/phone", |row| {
        row.outbound_appended > i64::from(written)
    })
    .await;
    assert_eq!(
        after.outbound_exposure_ceiling, after.outbound_appended,
        "live output continues the committed counter"
    );
}

#[tokio::test]
async fn a_resume_by_another_account_is_refused_and_leaves_the_session_detached() {
    let fixture = fixture(&[]).await;
    let (token, _) = detached_session_holding(&fixture, &["m1"]).await;

    let mut intruder = authenticate(&fixture.server, 1).await;
    send_element(&mut intruder, resume(&token, 0)).await;
    let failed = recv_sm(&mut intruder, "failed").await;
    assert!(failed
        .get_child("not-authorized", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());

    let rows = sessions(&fixture.pool, "alice@localhost/phone").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, STATE_DETACHED);
    assert_eq!(rows[0].generation, 1);
}

#[tokio::test]
async fn a_resume_count_above_the_exposure_ceiling_is_a_stream_error() {
    let fixture = fixture(&[]).await;
    let (token, _) = detached_session_holding(&fixture, &["m1"]).await;

    let mut alice = authenticate(&fixture.server, 0).await;
    send_element(&mut alice, resume(&token, 5000)).await;
    alice
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame.contains("handled-count-too-high")
        })
        .await
        .expect("handled-count-too-high stream error");

    let rows = sessions(&fixture.pool, "alice@localhost/phone").await;
    assert_eq!(
        rows[0].state, STATE_DETACHED,
        "a refused resume changes nothing"
    );
    assert_eq!(rows[0].outbound_acked, 0);
}

#[tokio::test]
async fn closing_the_stream_ends_the_session_and_releases_its_custody() {
    let fixture = fixture(&[]).await;
    let mut alice = authenticate(&fixture.server, 0).await;
    bind_and_enable(&mut alice, "phone").await;
    let mut bob = bob(&fixture.server).await;
    send_element(&mut bob, chat("alice@localhost/phone", "m1", "unacked")).await;
    recv_chats(&mut alice, &["m1"]).await;

    send_element(
        &mut alice,
        Element::builder("close", "urn:ietf:params:xml:ns:xmpp-framing").build(),
    )
    .await;

    let closed = wait_for_session(&fixture.pool, "alice@localhost/phone", |row| {
        row.state == STATE_TERMINAL && row.outbound_acked == row.outbound_appended
    })
    .await;
    assert_eq!(closed.terminal_reason, Some(REASON_CLOSED));
    assert_eq!(
        retained_frames(&fixture.pool, &closed.session_id).await,
        0,
        "the closed session's frames were transferred and released"
    );
}

#[tokio::test]
async fn binding_the_resource_again_replaces_its_detached_session() {
    let fixture = fixture(&[]).await;
    let (token, _) = detached_session_holding(&fixture, &["m1"]).await;

    let mut alice = authenticate(&fixture.server, 0).await;
    alice.bind("phone").await.expect("fresh bind");

    let replaced = wait_for_session(&fixture.pool, "alice@localhost/phone", |row| {
        row.state == STATE_TERMINAL && row.outbound_acked == row.outbound_appended
    })
    .await;
    assert_eq!(replaced.terminal_reason, Some(REASON_REPLACED));
    assert_eq!(
        retained_frames(&fixture.pool, &replaced.session_id).await,
        0
    );

    // The replaced session cannot be resumed by a later connection.
    let mut late = authenticate(&fixture.server, 0).await;
    send_element(&mut late, resume(&token, 0)).await;
    let failed = recv_sm(&mut late, "failed").await;
    assert!(
        failed
            .get_child("item-not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some(),
        "resume of a replaced session: {failed:?}"
    );
}

/// The server process is killed while Alice's session is live, so its owner
/// never detached it and no resource snapshot exists. After a restart on the
/// same database the session is resumable: the resume succeeds, the stanza
/// the client had not handled is replayed once, ahead of live output.
#[tokio::test]
async fn a_session_live_when_the_process_died_resumes_after_a_restart() {
    let dir = tempfile::tempdir().expect("test directory");
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("database.sqlite").display()
    );
    let server = TestServer::start_persistent_with_extra_envs(&db_url, ACCOUNTS, &[DURABLE]);
    let pool = sqlx::SqlitePool::connect(&db_url)
        .await
        .expect("observe SQLite");

    let mut alice = authenticate(&server, 0).await;
    let enabled = bind_and_enable(&mut alice, "phone").await;
    let token = enabled.attr("id").expect("resume token").to_owned();
    let mut bob_on_first = bob(&server).await;
    for id in ["m1", "m2"] {
        send_element(&mut bob_on_first, chat("alice@localhost/phone", id, "held")).await;
    }
    let written = recv_chats(&mut alice, &["m1", "m2"]).await;
    let live = wait_for_session(&pool, "alice@localhost/phone", |row| {
        row.state == STATE_LIVE && row.outbound_appended >= i64::from(written)
    })
    .await;

    // The process dies with the session live; the transport goes with it.
    drop(server);
    drop(alice);
    drop(bob_on_first);
    let server = TestServer::start_persistent_with_extra_envs(&db_url, ACCOUNTS, &[DURABLE]);

    let mut alice = authenticate(&server, 0).await;
    send_element(&mut alice, resume(&token, written - 1)).await;
    let resumed = recv_sm(&mut alice, "resumed").await;
    assert_eq!(resumed.attr("previd"), Some(token.as_str()));

    let mut bob = bob(&server).await;
    send_element(&mut bob, chat("alice@localhost/phone", "m3", "live")).await;
    let mut seen = Vec::new();
    while !seen.iter().any(|id| id == "m3") {
        let frame = alice
            .recv_timeout(Duration::from_secs(10))
            .await
            .unwrap_or_else(|error| panic!("no frame after {seen:?}: {error}"));
        let Ok(element) = frame.parse::<Element>() else {
            continue;
        };
        if element.is("message", "jabber:client") && element.attr("type") == Some("chat") {
            seen.push(element.attr("id").expect("chat id").to_owned());
        }
    }
    assert_eq!(
        seen,
        ["m2", "m3"],
        "the unhandled stanza is replayed once, ahead of live output"
    );
    let after = wait_for_session(&pool, "alice@localhost/phone", |row| {
        row.state == STATE_LIVE && row.generation == live.generation + 1
    })
    .await;
    assert_eq!(
        after.session_id, live.session_id,
        "the same session resumed"
    );
}
