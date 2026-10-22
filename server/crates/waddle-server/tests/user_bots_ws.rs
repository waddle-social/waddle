//! User bot accounts on `bots.<domain>` through the production WebSocket path.
//!
//! Bots are hosted accounts that never bind a resource: a DM to an enabled
//! bot is archived in the bot's MAM and inbox and is never stored offline
//! (XEP-0160 exemption, D13). Missing or disabled bots, the bare bots domain
//! and human localparts on the bots domain bounce like a nonexistent account
//! (RFC 6121 §8.5.1). No client stream can be opened on the bots domain, and
//! a bot API token is not an XMPP credential. Deleting a bot through
//! `/api/bots` takes it out of every room it was a member of, unless it is
//! a room's only owner: then the delete is refused and nothing changes.
#![cfg(feature = "test-support")]

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use kameo::actor::Spawn;
use minidom::Element;
use sha2::Sha256;
use sqlx::SqlitePool;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::permissions::{
    CheckPermission, DeleteTuple, ListSubjectTuples, Object, ObjectType, Permission,
    PermissionActor, PermissionError, Relation, Subject, Tuple, WriteTuple,
};
use waddle_ws_test_support::{TestServer, WsXmppClient};
use xmpp_parsers::message::{Lang, Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const DOMAIN: &str = "localhost";
const BOTS_DOMAIN: &str = "bots.localhost";
const PASSWORD: &str = "user-bots-password";
const ACCOUNTS: &[(&str, &str)] = &[("alice", PASSWORD), ("bob", PASSWORD)];
/// `WADDLE_SESSION_KEY` the harness gives every server; bot token hashes are
/// keyed with it.
const SESSION_KEY: &[u8] = b"integration-test-session-key-32-bytes-long";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_XDATA: &str = "jabber:x:data";
const NS_PING: &str = "urn:xmpp:ping";
const NS_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
const NS_SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";

struct Fixture {
    server: TestServer,
    pool: SqlitePool,
    url: String,
    // Keep the database directory alive until the server and pool drop.
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let directory = tempfile::tempdir().expect("database directory");
        let url = format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("user-bots.sqlite3").display()
        );
        let server = TestServer::start_persistent_with_extra_accounts(&url, ACCOUNTS);
        let pool = SqlitePool::connect(&url).await.expect("seed database");
        Self {
            server,
            pool,
            url,
            _directory: directory,
        }
    }

    async fn connect(&self, username: &str, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(&self.server.ws_url(), DOMAIN, username, PASSWORD, resource)
            .await
            .expect("authenticated client")
    }

    async fn connect_admin(&self, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(
            &self.server.ws_url(),
            DOMAIN,
            "admin",
            self.server.fixed_account_password(),
            resource,
        )
        .await
        .expect("authenticated admin")
    }

    /// Insert a bot row the way `CreateBotAccount` does.
    async fn seed_bot(&self, owner: &str, name: &str, disabled: bool) -> String {
        let localpart = format!("{owner}+{name}");
        let jid = format!("{localpart}@{BOTS_DOMAIN}");
        let (disabled_at, disabled_reason) = if disabled {
            (
                Some("2026-01-02T00:00:00.000000Z"),
                Some("owner_not_member"),
            )
        } else {
            (None, None)
        };
        sqlx::query(
            "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, display_name, \
             kind, owner_jid, disabled_at, disabled_reason, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 'bot', ?, ?, ?, ?, ?)",
        )
        .bind(&jid)
        .bind(&localpart)
        .bind(&localpart)
        .bind(&localpart)
        .bind("Standup Bot")
        .bind(format!("{owner}@{DOMAIN}"))
        .bind(disabled_at)
        .bind(disabled_reason)
        .bind("2026-01-01T00:00:00.000000Z")
        .bind("2026-01-01T00:00:00.000000Z")
        .execute(&self.pool)
        .await
        .expect("seed bot account");
        jid
    }

    /// Store an active API token for `bot` and return its secret.
    async fn seed_token(&self, bot: &str) -> String {
        let secret = format!(
            "wbt_{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
        );
        assert_eq!(secret.len(), 47);
        sqlx::query(
            "INSERT INTO bot_tokens (id, bot_jid, token_hash, label, created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(bot)
        .bind(bot_token_hash(&secret))
        .bind("ci")
        .bind("2026-01-01T00:00:00.000000Z")
        .bind("2999-01-01T00:00:00.000000Z")
        .execute(&self.pool)
        .await
        .expect("seed bot token");
        secret
    }

    async fn count(&self, sql: &str, bind: &str) -> i64 {
        sqlx::query_scalar(sql)
            .bind(bind)
            .fetch_one(&self.pool)
            .await
            .expect("count rows")
    }

    async fn wait_count(&self, sql: &str, bind: &str, expected: i64) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.count(sql, bind).await != expected {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{sql} [{bind}] never reached {expected}"));
    }
}

/// Mirrors the server's bot token hash (D10), so the seeded secret is a
/// working bot credential.
fn bot_token_hash(secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SESSION_KEY).expect("HMAC key");
    mac.update(b"waddle.bot-token.v1");
    mac.update(&[0]);
    mac.update(secret.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

async fn send(client: &mut WsXmppClient, element: Element) {
    client
        .send(&String::from(&element))
        .await
        .expect("send stanza");
}

fn stream_open(to: &str) -> Element {
    Element::builder("open", NS_FRAMING)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("version").to_owned(), "1.0")
        .build()
}

async fn send_chat(client: &mut WsXmppClient, to: &str, id: &str) {
    let mut message = Message::new(Some(to.parse().expect("recipient JID")));
    message.type_ = MessageType::Chat;
    message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
    message.bodies.insert(Lang::new(), format!("body {id}"));
    send(client, message.into()).await;
}

/// The stanza error carried by the bounce of message `id`.
async fn bounce(client: &mut WsXmppClient, id: &str) -> StanzaError {
    let frame = client
        .recv_matching(|frame| frame.contains("<message") && frame.contains(id))
        .await
        .expect("bounce");
    let message =
        Message::try_from(frame.parse::<Element>().expect("bounce XML")).expect("typed bounce");
    assert_eq!(
        message.type_,
        MessageType::Error,
        "expected a bounce: {frame}"
    );
    message
        .payloads
        .iter()
        .find_map(|payload| StanzaError::try_from(payload.clone()).ok())
        .expect("stanza error")
}

fn assert_service_unavailable(error: &StanzaError) {
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );
}

/// Round-trip a ping; every frame before its result is returned, so a bounce
/// that precedes the barrier is visible to the caller.
async fn barrier(client: &mut WsXmppClient, id: &str) -> Vec<String> {
    send(
        client,
        Element::builder("iq", xmpp_parsers::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), DOMAIN)
            .append(Element::builder("ping", NS_PING).build())
            .build(),
    )
    .await;
    client
        .recv_until(|frame| frame.contains("<iq") && frame.contains(id))
        .await
        .expect("ping barrier")
}

#[tokio::test]
async fn dm_to_offline_bot_lands_in_bot_mam_and_inbox() {
    let fixture = Fixture::start().await;
    let bot = fixture.seed_bot("alice", "standup", false).await;
    let mut alice = fixture.connect("alice", "dm-bot").await;

    send_chat(&mut alice, &bot, "dm-bot-1").await;

    fixture
        .wait_count(
            "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ? AND body = 'body dm-bot-1'",
            &bot,
            1,
        )
        .await;
    fixture
        .wait_count(
            "SELECT COUNT(*) FROM inbox_entries WHERE user_jid = ? AND partner_jid = 'alice@localhost'",
            &bot,
            1,
        )
        .await;
    assert_eq!(
        fixture
            .count(
                "SELECT COUNT(*) FROM pending_delivery WHERE recipient_jid = ?",
                &bot
            )
            .await,
        0,
        "bots never bind a resource, so their DMs are not stored offline"
    );
    let frames = barrier(&mut alice, "dm-bot-barrier").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let late = alice.recv_timeout(Duration::from_millis(200)).await.ok();
    assert!(
        frames
            .iter()
            .chain(late.iter())
            .all(|frame| !frame.contains("type='error'") && !frame.contains("type=\"error\"")),
        "the sender must not be bounced: {frames:?} {late:?}"
    );
    alice.close().await.expect("close");
}

#[tokio::test]
async fn dm_to_unknown_bot_bounces() {
    let fixture = Fixture::start().await;
    let mut alice = fixture.connect("alice", "unknown-bot").await;

    send_chat(&mut alice, "alice+ghost@bots.localhost", "unknown-bot").await;
    let bot_error = bounce(&mut alice, "unknown-bot").await;
    assert_service_unavailable(&bot_error);

    // Indistinguishable from a nonexistent human account.
    send_chat(&mut alice, "ghost@localhost", "unknown-human").await;
    let human_error = bounce(&mut alice, "unknown-human").await;
    assert_eq!(bot_error.type_, human_error.type_);
    assert_eq!(bot_error.defined_condition, human_error.defined_condition);
    assert_eq!(
        fixture
            .count(
                "SELECT COUNT(*) FROM pending_delivery WHERE recipient_jid = ?",
                "alice+ghost@bots.localhost"
            )
            .await,
        0
    );
    alice.close().await.expect("close");
}

#[tokio::test]
async fn dm_to_disabled_bot_bounces() {
    let fixture = Fixture::start().await;
    let bot = fixture.seed_bot("alice", "standup", true).await;
    let mut alice = fixture.connect("alice", "disabled-bot").await;

    send_chat(&mut alice, &bot, "disabled-bot").await;
    assert_service_unavailable(&bounce(&mut alice, "disabled-bot").await);
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM mam_messages WHERE room_jid = ?", &bot)
            .await,
        0
    );
    alice.close().await.expect("close");
}

#[tokio::test]
async fn message_to_bare_bots_domain_is_service_unavailable() {
    let fixture = Fixture::start().await;
    let mut alice = fixture.connect("alice", "bare-bots").await;

    send_chat(&mut alice, BOTS_DOMAIN, "bare-bots").await;
    assert_service_unavailable(&bounce(&mut alice, "bare-bots").await);
    alice.close().await.expect("close");
}

#[tokio::test]
async fn plus_localpart_on_primary_domain_is_not_an_account() {
    let fixture = Fixture::start().await;
    fixture.seed_bot("alice", "standup", false).await;
    let mut bob = fixture.connect("bob", "plus-primary").await;

    send_chat(&mut bob, "alice+standup@localhost", "plus-primary").await;
    assert_service_unavailable(&bounce(&mut bob, "plus-primary").await);
    assert_eq!(
        fixture
            .count(
                "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ?",
                "alice+standup@localhost"
            )
            .await,
        0
    );
    bob.close().await.expect("close");
}

#[tokio::test]
async fn human_localpart_on_bots_domain_is_not_an_account() {
    let fixture = Fixture::start().await;
    let mut bob = fixture.connect("bob", "human-bots").await;

    send_chat(&mut bob, "alice@bots.localhost", "human-bots").await;
    assert_service_unavailable(&bounce(&mut bob, "human-bots").await);
    bob.close().await.expect("close");
}

#[tokio::test]
async fn oauthbearer_with_bot_token_is_not_authorized() {
    let fixture = Fixture::start().await;
    let bot = fixture.seed_bot("alice", "standup", false).await;
    let secret = fixture.seed_token(&bot).await;

    let mut client = WsXmppClient::connect(&fixture.server.ws_url())
        .await
        .expect("connect");
    send(&mut client, stream_open(DOMAIN)).await;
    let _open = client.recv().await.expect("open response");
    let features = client.recv().await.expect("stream features");
    assert!(features.contains("OAUTHBEARER"), "{features}");
    let initial = base64::engine::general_purpose::STANDARD
        .encode(format!("n,,\u{1}auth=Bearer {secret}\u{1}\u{1}"));
    send(
        &mut client,
        Element::builder("auth", NS_SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(initial)
            .build(),
    )
    .await;
    let result = client.recv().await.expect("SASL result");
    assert!(
        result.contains("<failure") && result.contains("not-authorized"),
        "a bot API token is not an XMPP credential: {result}"
    );
}

#[tokio::test]
async fn stream_to_bots_domain_cannot_authenticate() {
    let fixture = Fixture::start().await;

    let mut client = WsXmppClient::connect(&fixture.server.ws_url())
        .await
        .expect("connect");
    send(&mut client, stream_open(BOTS_DOMAIN)).await;
    let open = client.recv().await.expect("open response");
    assert!(open.contains("<open"), "{open}");
    let error = client.recv().await.expect("stream error");
    assert!(
        error.contains("stream:error") || error.contains("<error"),
        "expected a stream error: {error}"
    );
    assert!(error.contains("host-unknown"), "{error}");
    let close = client.recv().await.expect("close");
    assert!(close.contains("<close"), "{close}");

    // A full SCRAM attempt against the bots domain with valid human
    // credentials still fails.
    assert!(WsXmppClient::connect_and_auth(
        &fixture.server.ws_url(),
        BOTS_DOMAIN,
        "alice",
        PASSWORD,
        "bots-domain",
    )
    .await
    .is_err());
}

fn owner_config_iq(room: &str, id: &str) -> Element {
    let field = |var: &str, value: &str, kind: Option<&str>| {
        let mut field = Element::builder("field", NS_XDATA)
            .attr(minidom::rxml::xml_ncname!("var").to_owned(), var);
        if let Some(kind) = kind {
            field = field.attr(minidom::rxml::xml_ncname!("type").to_owned(), kind);
        }
        field
            .append(Element::builder("value", NS_XDATA).append(value).build())
            .build()
    };
    Element::builder("iq", xmpp_parsers::ns::JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .append(
            Element::builder("query", NS_MUC_OWNER)
                .append(
                    Element::builder("x", NS_XDATA)
                        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                        .append(field(
                            "FORM_TYPE",
                            "http://jabber.org/protocol/muc#roomconfig",
                            Some("hidden"),
                        ))
                        .append(field("muc#roomconfig_membersonly", "1", None))
                        .build(),
                )
                .build(),
        )
        .build()
}

fn member_affiliation_iq(room: &str, id: &str, jid: &str) -> Element {
    affiliation_iq(room, id, jid, "member")
}

fn affiliation_iq(room: &str, id: &str, jid: &str, affiliation: &str) -> Element {
    Element::builder("iq", xmpp_parsers::ns::JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .append(
            Element::builder("query", NS_MUC_ADMIN)
                .append(
                    Element::builder("item", NS_MUC_ADMIN)
                        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid)
                        .attr(
                            minidom::rxml::xml_ncname!("affiliation").to_owned(),
                            affiliation,
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

async fn expect_iq_result(client: &mut WsXmppClient, id: &str) {
    let frame = client
        .recv_matching(|frame| frame.contains("<iq") && frame.contains(id))
        .await
        .expect("IQ response");
    assert!(
        frame.contains("type='result'") || frame.contains("type=\"result\""),
        "expected a result for {id}: {frame}"
    );
}

#[tokio::test]
async fn manage_members_can_grant_bot_room_membership() {
    let fixture = Fixture::start().await;
    let bot = fixture.seed_bot("alice", "standup", false).await;
    let channel = format!("bots-{}", uuid::Uuid::new_v4().simple());
    let room = format!("{channel}@muc.{DOMAIN}");
    let mut admin = fixture.connect_admin("grant-bot").await;

    send(
        &mut admin,
        Element::builder("presence", xmpp_parsers::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                format!("{room}/admin"),
            )
            .append(Element::builder("x", NS_MUC).build())
            .build(),
    )
    .await;
    admin
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("room join");
    send(&mut admin, owner_config_iq(&room, "members-only")).await;
    expect_iq_result(&mut admin, "members-only").await;

    let database = Database::from_config(
        "user-bots-inspect",
        &DatabaseConfig::new(DatabaseDriver::Sqlite, fixture.url.clone()),
    )
    .await
    .expect("inspection database");
    // The local checker caches decisions, so each check gets a fresh actor
    // over the server's tuple store.
    let can_read = || async {
        PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(database.clone())))
            .ask(CheckPermission {
                subject: Subject::user(&bot),
                permission: Permission::Read,
                object: Object::new(ObjectType::Channel, &channel),
            })
            .await
            .expect("permission check")
            .allowed
    };
    assert!(!can_read().await, "no grant yet");

    send(&mut admin, member_affiliation_iq(&room, "grant-bot", &bot)).await;
    expect_iq_result(&mut admin, "grant-bot").await;
    assert!(
        can_read().await,
        "the member affiliation grants the bot read"
    );
    admin.close().await.expect("close");
}

/// Sign `owner` in for the bot management API: a human `users` row, a
/// browser session keyed like the server's (HMAC of the id under the session
/// key) and the `server#member` grant that allows `create_bot`.
async fn sign_in_for_bots_api(fixture: &Fixture, database: &Database, owner: &str) -> String {
    let jid = format!("{owner}@{DOMAIN}");
    let now = "2026-01-01T00:00:00+00:00";
    sqlx::query(
        "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (jid) DO NOTHING",
    )
    .bind(&jid)
    .bind(owner)
    .bind(owner)
    .bind(owner)
    .bind(now)
    .bind(now)
    .execute(&fixture.pool)
    .await
    .expect("seed owner row");
    let session_id = uuid::Uuid::new_v4().to_string();
    let mut mac = Hmac::<Sha256>::new_from_slice(SESSION_KEY).expect("HMAC key");
    mac.update(session_id.as_bytes());
    sqlx::query(
        "INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, \
         principal_auth_epoch, created_at, last_used_at) VALUES (?, ?, ?, ?, 0, 0, ?, ?)",
    )
    .bind(&session_id)
    .bind(&jid)
    .bind(hex::encode(mac.finalize().into_bytes()))
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(now)
    .bind(now)
    .execute(&fixture.pool)
    .await
    .expect("seed owner session");
    let granted =
        PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(database.clone())))
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Server, "deployment"),
                    Relation::new("member"),
                    Subject::user(&jid),
                ),
            })
            .await;
    match granted {
        Ok(())
        | Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => {}
        Err(error) => panic!("grant server membership: {error}"),
    }
    session_id
}

async fn bots_api(
    fixture: &Fixture,
    session: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> reqwest::StatusCode {
    bots_api_response(fixture, session, method, path, body)
        .await
        .status()
}

async fn bots_api_response(
    fixture: &Fixture,
    session: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .request(method, format!("{}{path}", fixture.server.http_base_url()))
        .header("x-waddle-session-id", session);
    if let Some(body) = body {
        request = request.json(&body);
    }
    request.send().await.expect("bots API request")
}

/// Join `room` as the admin, creating it, and make it members-only.
async fn create_members_only_room(admin: &mut WsXmppClient, room: &str, id: &str) {
    send(
        admin,
        Element::builder("presence", xmpp_parsers::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                format!("{room}/admin"),
            )
            .append(Element::builder("x", NS_MUC).build())
            .build(),
    )
    .await;
    admin
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("room join");
    send(admin, owner_config_iq(room, id)).await;
    expect_iq_result(admin, id).await;
}

/// The room's member list, as its owner sees it through XEP-0045 §9.5.
async fn room_members(client: &mut WsXmppClient, room: &str, id: &str) -> String {
    send(
        client,
        Element::builder("iq", xmpp_parsers::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
            .append(
                Element::builder("query", NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "member",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
    .await;
    let frame = client
        .recv_matching(|frame| frame.contains("<iq") && frame.contains(id))
        .await
        .expect("member list");
    assert!(
        frame.contains("type='result'") || frame.contains("type=\"result\""),
        "expected a member list: {frame}"
    );
    frame
}

/// Durable mirror rows for `member` in `room`. The mirror only exists when
/// the server runs clustered on Postgres; without it there is nothing left
/// behind.
async fn mirror_rows(fixture: &Fixture, room: &str, member: &str) -> i64 {
    let table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'table' AND name = 'clustering_muc_room_affiliations'",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("schema lookup");
    if table == 0 {
        return 0;
    }
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM clustering_muc_room_affiliations \
         WHERE room_jid = ? AND member_jid = ?",
    )
    .bind(room)
    .bind(member)
    .fetch_one(&fixture.pool)
    .await
    .expect("mirror rows")
}

#[tokio::test]
async fn delete_clears_room_affiliation_and_durable_recipients() {
    let fixture = Fixture::start().await;
    let database = Database::from_config(
        "user-bots-delete",
        &DatabaseConfig::new(DatabaseDriver::Sqlite, fixture.url.clone()),
    )
    .await
    .expect("inspection database");
    let session = sign_in_for_bots_api(&fixture, &database, "alice").await;
    let bot = format!("alice+standup@{BOTS_DOMAIN}");
    let bot_path = format!("/api/bots/{}", urlencoding::encode(&bot));
    let create = serde_json::json!({ "name": "standup" });

    assert_eq!(
        bots_api(
            &fixture,
            &session,
            reqwest::Method::POST,
            "/api/bots",
            Some(create.clone())
        )
        .await,
        reqwest::StatusCode::CREATED
    );

    let channel = format!("bots-{}", uuid::Uuid::new_v4().simple());
    let room = format!("{channel}@muc.{DOMAIN}");
    let mut admin = fixture.connect_admin("delete-bot").await;
    send(
        &mut admin,
        Element::builder("presence", xmpp_parsers::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                format!("{room}/admin"),
            )
            .append(Element::builder("x", NS_MUC).build())
            .build(),
    )
    .await;
    admin
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("room join");
    send(&mut admin, owner_config_iq(&room, "delete-members-only")).await;
    expect_iq_result(&mut admin, "delete-members-only").await;
    send(
        &mut admin,
        member_affiliation_iq(&room, "delete-grant", &bot),
    )
    .await;
    expect_iq_result(&mut admin, "delete-grant").await;
    assert!(
        room_members(&mut admin, &room, "members-before")
            .await
            .contains(&bot),
        "the bot is a member before the delete"
    );

    // The local checker caches decisions, so each check gets a fresh actor.
    let can_read = || async {
        PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(database.clone())))
            .ask(CheckPermission {
                subject: Subject::user(&bot),
                permission: Permission::Read,
                object: Object::new(ObjectType::Channel, &channel),
            })
            .await
            .expect("permission check")
            .allowed
    };
    assert!(can_read().await);

    assert_eq!(
        bots_api(&fixture, &session, reqwest::Method::DELETE, &bot_path, None).await,
        reqwest::StatusCode::NO_CONTENT
    );
    let members = room_members(&mut admin, &room, "members-after-delete").await;
    assert!(
        !members.contains(&bot),
        "the room's affiliation list still has the bot: {members}"
    );
    assert_eq!(mirror_rows(&fixture, &room, &bot).await, 0);
    assert!(!can_read().await);
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM users WHERE jid = ?", &bot)
            .await,
        0
    );

    // Recreating the name gives the same JID and none of the old membership.
    assert_eq!(
        bots_api(
            &fixture,
            &session,
            reqwest::Method::POST,
            "/api/bots",
            Some(create)
        )
        .await,
        reqwest::StatusCode::CREATED
    );
    let members = room_members(&mut admin, &room, "members-after-recreate").await;
    assert!(
        !members.contains(&bot),
        "a recreated bot inherited room membership: {members}"
    );
    assert!(!can_read().await);
    admin.close().await.expect("close");
}

/// A room can list the bot without any channel tuple naming it, so the
/// delete must find rooms by affiliation, not only through tuples.
#[tokio::test]
async fn delete_clears_room_affiliation_without_a_channel_tuple() {
    let fixture = Fixture::start().await;
    let database = Database::from_config(
        "user-bots-delete-untupled",
        &DatabaseConfig::new(DatabaseDriver::Sqlite, fixture.url.clone()),
    )
    .await
    .expect("inspection database");
    let session = sign_in_for_bots_api(&fixture, &database, "alice").await;
    let bot = format!("alice+standup@{BOTS_DOMAIN}");
    let bot_path = format!("/api/bots/{}", urlencoding::encode(&bot));
    let create = serde_json::json!({ "name": "standup" });
    assert_eq!(
        bots_api(
            &fixture,
            &session,
            reqwest::Method::POST,
            "/api/bots",
            Some(create.clone())
        )
        .await,
        reqwest::StatusCode::CREATED
    );

    let room = format!("bots-{}@muc.{DOMAIN}", uuid::Uuid::new_v4().simple());
    let mut admin = fixture.connect_admin("delete-untupled").await;
    create_members_only_room(&mut admin, &room, "untupled-members-only").await;
    send(
        &mut admin,
        member_affiliation_iq(&room, "untupled-grant", &bot),
    )
    .await;
    expect_iq_result(&mut admin, "untupled-grant").await;

    // Leave the grant only in the room: drop every channel tuple naming the
    // bot.
    let permissions =
        PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(database.clone())));
    let tuples = permissions
        .ask(ListSubjectTuples {
            subject: Subject::user(&bot),
        })
        .await
        .expect("list bot tuples");
    let channel_tuples: Vec<Tuple> = tuples
        .into_iter()
        .filter(|tuple| tuple.object.object_type == ObjectType::Channel)
        .collect();
    assert!(
        !channel_tuples.is_empty(),
        "the grant wrote a channel tuple"
    );
    for tuple in channel_tuples {
        permissions
            .ask(DeleteTuple { tuple })
            .await
            .expect("drop channel tuple");
    }
    assert!(
        room_members(&mut admin, &room, "untupled-members-before")
            .await
            .contains(&bot),
        "the room still lists the bot"
    );

    assert_eq!(
        bots_api(&fixture, &session, reqwest::Method::DELETE, &bot_path, None).await,
        reqwest::StatusCode::NO_CONTENT
    );
    let members = room_members(&mut admin, &room, "untupled-members-after-delete").await;
    assert!(
        !members.contains(&bot),
        "the room's affiliation list still has the bot: {members}"
    );

    assert_eq!(
        bots_api(
            &fixture,
            &session,
            reqwest::Method::POST,
            "/api/bots",
            Some(create)
        )
        .await,
        reqwest::StatusCode::CREATED
    );
    let members = room_members(&mut admin, &room, "untupled-members-after-recreate").await;
    assert!(
        !members.contains(&bot),
        "a recreated bot inherited room membership: {members}"
    );
    admin.close().await.expect("close");
}

#[tokio::test]
async fn delete_bot_that_is_a_rooms_only_owner_is_409_and_changes_nothing() {
    let fixture = Fixture::start().await;
    let database = Database::from_config(
        "user-bots-sole-owner",
        &DatabaseConfig::new(DatabaseDriver::Sqlite, fixture.url.clone()),
    )
    .await
    .expect("inspection database");
    let session = sign_in_for_bots_api(&fixture, &database, "alice").await;
    let bot = format!("alice+standup@{BOTS_DOMAIN}");
    let bot_path = format!("/api/bots/{}", urlencoding::encode(&bot));
    assert_eq!(
        bots_api(
            &fixture,
            &session,
            reqwest::Method::POST,
            "/api/bots",
            Some(serde_json::json!({ "name": "standup" })),
        )
        .await,
        reqwest::StatusCode::CREATED
    );

    let mut admin = fixture.connect_admin("sole-owner").await;
    let member_channel = format!("bots-{}", uuid::Uuid::new_v4().simple());
    let member_room = format!("{member_channel}@muc.{DOMAIN}");
    create_members_only_room(&mut admin, &member_room, "sole-member-room").await;
    send(
        &mut admin,
        member_affiliation_iq(&member_room, "sole-member-grant", &bot),
    )
    .await;
    expect_iq_result(&mut admin, "sole-member-grant").await;

    // The bot becomes an owner, then the human owner steps down, leaving the
    // bot as the room's only owner.
    let owned_room = format!("bots-{}@muc.{DOMAIN}", uuid::Uuid::new_v4().simple());
    create_members_only_room(&mut admin, &owned_room, "sole-owned-room").await;
    send(
        &mut admin,
        affiliation_iq(&owned_room, "sole-owner-grant", &bot, "owner"),
    )
    .await;
    expect_iq_result(&mut admin, "sole-owner-grant").await;
    send(
        &mut admin,
        affiliation_iq(
            &owned_room,
            "sole-owner-step-down",
            &format!("admin@{DOMAIN}"),
            "member",
        ),
    )
    .await;
    expect_iq_result(&mut admin, "sole-owner-step-down").await;

    for attempt in 0..2 {
        let response =
            bots_api_response(&fixture, &session, reqwest::Method::DELETE, &bot_path, None).await;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::CONFLICT,
            "attempt {attempt}"
        );
        let body: serde_json::Value = response.json().await.expect("error body");
        assert_eq!(body["error"], "bot_sole_room_owner", "attempt {attempt}");
    }

    // Nothing was cleared: the bot keeps its account and its other room.
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM users WHERE jid = ?", &bot)
            .await,
        1
    );
    assert!(
        room_members(&mut admin, &member_room, "sole-members-after")
            .await
            .contains(&bot),
        "a refused delete still removed the bot from another room"
    );
    let can_read = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(database)))
        .ask(CheckPermission {
            subject: Subject::user(&bot),
            permission: Permission::Read,
            object: Object::new(ObjectType::Channel, &member_channel),
        })
        .await
        .expect("permission check")
        .allowed;
    assert!(can_read, "a refused delete still purged the bot's grants");
    admin.close().await.expect("close");
}
