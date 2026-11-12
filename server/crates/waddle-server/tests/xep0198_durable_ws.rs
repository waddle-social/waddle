//! XEP-0198 over the loopback WebSocket with the durable session authority
//! selected (`WADDLE_XMPP_DURABLE_SM=durable`) on single-node SQLite.
//!
//! The durable authority commits a session before `<enabled/>` is built and
//! hands the client a resume token that is not the storage key. These tests
//! observe the wire and the authority's rows.

#![cfg(feature = "test-support")]

use std::time::Duration;

use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::SM_NS;

const ACCOUNTS: &[(&str, &str)] = &[("alice", "durable-sm-alice"), ("bob", "durable-sm-bob")];
const DURABLE: (&str, &str) = ("WADDLE_XMPP_DURABLE_SM", "durable");

/// Authority row states, as `sm_durable_sessions.state` stores them.
const STATE_LIVE: i64 = 1;

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
    generation: i64,
}

async fn sessions(pool: &sqlx::SqlitePool, full_jid: &str) -> Vec<SessionRow> {
    sqlx::query_as(
        "SELECT session_id, state, generation FROM sm_durable_sessions WHERE full_jid = ? \
         ORDER BY updated_at_ms",
    )
    .bind(full_jid)
    .fetch_all(pool)
    .await
    .expect("read durable sessions")
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
