//! Retiring an account ends its open sessions (ADR-0020 D8;
//! `docs/operations/account-retirement.md`).
//!
//! After the retirement committed, the operator command records a request;
//! every serving replica takes it up, closes the account's live streams with
//! `<not-authorized/>` (RFC 6120 §4.9.3.12), ends its detached
//! stream-management sessions, refuses its bindings and acknowledges; the
//! command reports each replica whose lease counts.
//!
//! The replica cases drive the production `/ws` router over a loopback
//! socket against the in-memory test state, and run the replica's lease and
//! its work one tick at a time instead of on their timers. The command's
//! wait and report run against an SQLite database of their own, and the
//! operator subcommand runs as the server binary against an SQLite file. No
//! PostgreSQL, SpiceDB or second server process is involved.
#![cfg(feature = "test-support")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use uuid::Uuid;
use waddle_server::auth::retirement::retire_account;
use waddle_server::auth::session_endings::{
    await_session_endings, prune_session_endings, record_completed, register_replica,
    release_replica, renew_replica, request_session_ending, requests_to_complete, EndedSessions,
    ReplicaEnding, SessionEndingRequest,
};
use waddle_server::auth::{NativeUserStore, RegisterRequest};
use waddle_server::db::actor::{DbActor, DbExecute, DbQueryOne};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner, Value};
use waddle_server::server::account_session_endings::{ReplicaLease, RequestedEndings};
use waddle_server::test_support::{
    make_deployment_owner, router, seed_local_account, websocket_state, WebSocketState,
};
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::stream_management::SM_NS;
use xmpp_parsers::bind::BindQuery;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::ping::Ping;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

/// The test state's account domain and MUC service.
const DOMAIN: &str = "example.com";
const MUC: &str = "muc.example.com";
const PASSWORD: &str = "retirement-sessions-password";
/// RFC 7395 framing.
const FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
/// How long a case waits for something that arrives.
const WAIT: Duration = Duration::from_secs(10);

/// The in-memory test state served on loopback.
struct Server {
    state: Arc<WebSocketState>,
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn start() -> Self {
        let state = websocket_state().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        let app = router(Arc::clone(&state));
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            state,
            url: ["ws://", &address.to_string(), "/ws"].concat(),
            task,
        }
    }

    fn db(&self) -> &Database {
        self.state.deps.app_state.db_pool.global()
    }

    /// Create the native account `localpart@example.com`.
    async fn account(&self, localpart: &str) -> BareJid {
        seed_local_account(&self.state, localpart).await;
        NativeUserStore::new(self.state.deps.app_state.db_pool.global_actor().clone())
            .register(RegisterRequest {
                username: localpart.to_owned(),
                domain: DOMAIN.to_owned(),
                password: PASSWORD.to_owned(),
                email: None,
            })
            .await
            .expect("register native account");
        account(localpart)
    }

    /// A client of `localpart` that has authenticated and bound nothing.
    async fn authenticated(&self, localpart: &str) -> WsXmppClient {
        let mut client = WsXmppClient::connect(&self.url)
            .await
            .expect("connect a client");
        client
            .authenticate(DOMAIN, localpart, PASSWORD)
            .await
            .expect("authenticate");
        client
    }

    /// A client of `localpart` with `resource` bound.
    async fn client(&self, localpart: &str, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(&self.url, DOMAIN, localpart, PASSWORD, resource)
            .await
            .expect("authenticated client")
    }

    /// A new room on the test state's MUC service. `owner` is made the
    /// deployment owner, whose first entry creates the room as an instant
    /// room (XEP-0045 §10.1.3) that other accounts may then enter.
    async fn room(&self, owner: &str) -> BareJid {
        make_deployment_owner(&self.state, owner).await;
        ["lobby-", &Uuid::new_v4().simple().to_string(), "@", MUC]
            .concat()
            .parse()
            .expect("room JID")
    }

    /// Retire `account` and record the request to end its sessions, as the
    /// operator command does.
    async fn retire(&self, account: &BareJid) -> SessionEndingRequest {
        retire_account(self.db(), account)
            .await
            .expect("the account is retired");
        request_session_ending(self.db(), account)
            .await
            .expect("the request is recorded")
    }

    /// A replica of this server with its lease registered.
    async fn replica(&self) -> (ReplicaLease, RequestedEndings) {
        let mut lease = ReplicaLease::new(&self.state);
        lease.keep(self.db()).await.expect("the lease registers");
        let endings = RequestedEndings::new(lease.replica());
        (lease, endings)
    }

    /// The full JIDs of `account` the session registry holds as detached.
    async fn detached(&self, account: &BareJid) -> Vec<FullJid> {
        self.state
            .deps
            .protocol
            .sm_session_registry
            .detached_resources_for_user(account)
            .await
            .expect("the session registry is readable")
    }
}

fn account(localpart: &str) -> BareJid {
    [localpart, "@", DOMAIN]
        .concat()
        .parse()
        .expect("account JID")
}

async fn send(client: &mut WsXmppClient, stanza: impl Into<Element>) {
    let element: Element = stanza.into();
    client
        .send(&String::from(&element))
        .await
        .expect("send a stanza");
}

/// XEP-0045 §7.2: enter `room` as `nick`.
fn join(room: &BareJid, nick: &str) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.to = Some(Jid::from(
        room.with_resource_str(nick).expect("occupant JID"),
    ));
    presence
        .payloads
        .push(Element::builder("x", xmpp_parsers::ns::MUC).build());
    presence
}

/// The first frame that reaches `client` and `matches`.
async fn receive(client: &mut WsXmppClient, matches: impl Fn(&Element) -> bool) -> Element {
    let frame = client
        .recv_matching_within(WAIT, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| matches(&element))
        })
        .await
        .unwrap_or_else(|error| panic!("a matching frame reaches the client: {error}"));
    frame.parse().expect("frame XML")
}

/// Enter `room` as `nick` and wait for the room's presence of that occupant.
async fn enter(client: &mut WsXmppClient, room: &BareJid, nick: &str) {
    send(client, join(room, nick)).await;
    let occupant = [room.as_str(), "/", nick].concat();
    let presence = receive(client, |frame| {
        frame.name() == "presence" && frame.attr("from") == Some(occupant.as_str())
    })
    .await;
    assert_ne!(presence.attr("type"), Some("error"), "{presence:?}");
}

/// Every frame `client` receives until its stream ends, parsed.
async fn frames_until_closed(client: &mut WsXmppClient) -> Vec<Element> {
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut frames = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the stream did not end; received {frames:?}"
        );
        match client.recv_timeout(remaining).await {
            Ok(frame) => {
                if let Ok(element) = frame.parse::<Element>() {
                    frames.push(element);
                }
            }
            // A WebSocket close frame, or the end of the stream.
            Err(error) if !error.starts_with("Timeout") => return frames,
            Err(error) => panic!("the stream did not end: {error}; received {frames:?}"),
        }
    }
}

/// RFC 6120 §4.9.3.12.
fn is_not_authorized(frame: &Element) -> bool {
    frame.is("error", waddle_xmpp::ns::STREAM)
        && frame.has_child("not-authorized", waddle_xmpp::ns::STREAMS)
}

/// The stream errors and framing close among `frames` mean the stream
/// ended with `<not-authorized/>`, and nothing else ended it.
fn ended_not_authorized(frames: &[Element]) {
    let errors: Vec<&Element> = frames
        .iter()
        .filter(|frame| frame.is("error", waddle_xmpp::ns::STREAM))
        .collect();
    assert_eq!(errors.len(), 1, "one stream error: {frames:?}");
    assert!(is_not_authorized(errors[0]), "{frames:?}");
    assert!(
        frames.iter().any(|frame| frame.is("close", FRAMING)),
        "the stream is closed: {frames:?}"
    );
}

/// Whether `frame` is the room's presence of the occupant `nick`, of
/// `type_` (`None` for available).
fn occupant_presence(frame: &Element, room: &BareJid, nick: &str, type_: Option<&str>) -> bool {
    let occupant = [room.as_str(), "/", nick].concat();
    frame.name() == "presence"
        && frame.attr("from") == Some(occupant.as_str())
        && frame.attr("type") == type_
}

/// Bob, the deployment owner, creates a room and Alice enters it. Returns
/// Alice's account, the room, Bob's client (which has seen Alice enter) and
/// Alice's.
async fn occupied_room(server: &Server) -> (BareJid, BareJid, WsXmppClient, WsXmppClient) {
    let alice = server.account("alice").await;
    server.account("bob").await;
    let room = server.room("bob").await;
    let mut bobs = server.client("bob", "laptop").await;
    enter(&mut bobs, &room, "bob").await;
    let mut alices = server.client("alice", "phone").await;
    enter(&mut alices, &room, "alice").await;
    receive(&mut bobs, |frame| {
        occupant_presence(frame, &room, "alice", None)
    })
    .await;
    (alice, room, bobs, alices)
}

/// XEP-0198 §4 with resumption: the stream's session can be resumed.
async fn enable_resumption(client: &mut WsXmppClient) {
    send(
        client,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    receive(client, |frame| frame.is("enabled", SM_NS)).await;
}

/// Whether `client` still has a working stream: its server answers a ping.
async fn answers_ping(client: &mut WsXmppClient, id: &str) -> bool {
    matches!(
        round_trip(client, Iq::from_get(id, Ping)).await,
        Iq::Result { .. }
    )
}

#[tokio::test]
async fn a_live_stream_of_a_retired_account_is_closed_and_leaves_its_room() {
    let server = Server::start().await;
    let (alice, room, mut bobs, mut alices) = occupied_room(&server).await;

    let request = server.retire(&alice).await;
    let (_lease, mut endings) = server.replica().await;
    assert_eq!(endings.run(&server.state).await.expect("a pass"), 1);

    // The live stream is told why it ends, and ends.
    ended_not_authorized(&frames_until_closed(&mut alices).await);
    // The room fans the departure out to the occupants who stay.
    receive(&mut bobs, |frame| {
        occupant_presence(frame, &room, "alice", Some("unavailable"))
    })
    .await;
    assert!(answers_ping(&mut bobs, "bob-still-here").await);

    // The replica's acknowledgement is what the command reports.
    let report = await_session_endings(server.db(), &request, WAIT)
        .await
        .expect("the acknowledgements are readable");
    assert!(report.confirmed(), "{report:?}");
    assert_eq!(report.replicas.len(), 1, "{report:?}");
    assert_eq!(
        report.replicas[0].ending,
        ReplicaEnding::Completed(EndedSessions {
            streams_closed: 1,
            detached_sessions_ended: 0,
        })
    );
    // A completed request is not taken up again by the same replica.
    assert_eq!(endings.run(&server.state).await.expect("a pass"), 0);
}

#[tokio::test]
async fn a_resumable_live_stream_of_a_retired_account_is_closed_and_not_kept_for_resumption() {
    let server = Server::start().await;
    let (alice, room, mut bobs, mut alices) = occupied_room(&server).await;
    enable_resumption(&mut alices).await;

    let request = server.retire(&alice).await;
    let (_lease, mut endings) = server.replica().await;
    assert_eq!(endings.run(&server.state).await.expect("a pass"), 1);

    // The stream is closed as a replacing bind closes one: its session is
    // ended, not detached for a resumption.
    ended_not_authorized(&frames_until_closed(&mut alices).await);
    assert!(server.detached(&alice).await.is_empty());
    receive(&mut bobs, |frame| {
        occupant_presence(frame, &room, "alice", Some("unavailable"))
    })
    .await;
    let report = await_session_endings(server.db(), &request, WAIT)
        .await
        .expect("the acknowledgements are readable");
    assert_eq!(
        report.replicas[0].ending,
        ReplicaEnding::Completed(EndedSessions {
            streams_closed: 1,
            detached_sessions_ended: 0,
        })
    );
}

#[tokio::test]
async fn a_detached_session_of_a_retired_account_is_ended_and_leaves_its_room() {
    let server = Server::start().await;
    let (alice, room, mut bobs, mut alices) = occupied_room(&server).await;
    let resource: FullJid = alice.with_resource_str("phone").expect("full JID");

    // XEP-0198 §5: a resumable session whose connection is lost waits for
    // a resumption, and keeps its room.
    enable_resumption(&mut alices).await;
    drop(alices);
    let deadline = Instant::now() + WAIT;
    while !server.detached(&alice).await.contains(&resource) {
        assert!(Instant::now() < deadline, "the session detaches");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let request = server.retire(&alice).await;
    let (_lease, mut endings) = server.replica().await;
    assert_eq!(endings.run(&server.state).await.expect("a pass"), 1);

    assert!(server.detached(&alice).await.is_empty());
    receive(&mut bobs, |frame| {
        occupant_presence(frame, &room, "alice", Some("unavailable"))
    })
    .await;
    let report = await_session_endings(server.db(), &request, WAIT)
        .await
        .expect("the acknowledgements are readable");
    assert_eq!(
        report.replicas[0].ending,
        ReplicaEnding::Completed(EndedSessions {
            streams_closed: 0,
            detached_sessions_ended: 1,
        })
    );
}

#[tokio::test]
async fn a_binding_after_the_request_is_refused_with_not_authorized() {
    let server = Server::start().await;
    let alice = server.account("alice").await;
    // Authenticated before the retirement committed, bound after the
    // replica took the request up.
    let mut alices = server.authenticated("alice").await;
    server.retire(&alice).await;
    let (_lease, mut endings) = server.replica().await;
    assert_eq!(endings.run(&server.state).await.expect("a pass"), 1);

    send(
        &mut alices,
        Iq::from_set(
            "bind-after-retirement",
            BindQuery::new(Some("phone".to_owned())),
        ),
    )
    .await;
    let frames = frames_until_closed(&mut alices).await;
    ended_not_authorized(&frames);
    // RFC 6120 §7.6.2.2: no resource was bound.
    assert!(
        !frames
            .iter()
            .any(|frame| frame.name() == "iq" && frame.attr("type") == Some("result")),
        "{frames:?}"
    );
    let bound: FullJid = alice.with_resource_str("phone").expect("full JID");
    assert!(!server
        .state
        .deps
        .protocol
        .connection_registry
        .is_connected(&bound));
}

#[tokio::test]
async fn a_replica_ends_nothing_without_a_request_of_a_retired_account() {
    let server = Server::start().await;
    let alice = server.account("alice").await;
    let mut alices = server.client("alice", "phone").await;
    let (_lease, mut endings) = server.replica().await;

    assert_eq!(endings.run(&server.state).await.expect("a pass"), 0);
    assert!(answers_ping(&mut alices, "no-request").await);

    // A request names an account the ledger of retired JIDs does not hold:
    // only the owner's command records one, after the retirement.
    request_session_ending(server.db(), &alice)
        .await
        .expect("the request is recorded");
    assert_eq!(endings.run(&server.state).await.expect("a pass"), 0);
    assert!(answers_ping(&mut alices, "not-retired").await);
}

#[tokio::test]
async fn a_replica_that_starts_later_completes_the_request_too() {
    let server = Server::start().await;
    let alice = server.account("alice").await;
    let request = server.retire(&alice).await;
    let (_first_lease, mut first) = server.replica().await;
    assert_eq!(first.run(&server.state).await.expect("a pass"), 1);

    // A replica registered after the request, as a process that started
    // later is, takes it up within the window and acknowledges it.
    let (_second_lease, mut second) = server.replica().await;
    assert_eq!(second.run(&server.state).await.expect("a pass"), 1);
    let report = await_session_endings(server.db(), &request, WAIT)
        .await
        .expect("the acknowledgements are readable");
    assert_eq!(report.replicas.len(), 2, "{report:?}");
    assert!(report
        .replicas
        .iter()
        .all(|replica| matches!(replica.ending, ReplicaEnding::Completed(_))));
}

/// A migrated SQLite database of its own.
async fn store(name: &str) -> Database {
    let db = Database::in_memory(name).await.expect("in-memory database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");
    db
}

/// Retire `localpart@example.com` on `db` and request its sessions' end.
async fn retired_request(db: &Database, localpart: &str) -> SessionEndingRequest {
    let account = account(localpart);
    retire_account(db, &account)
        .await
        .expect("the JID is retired");
    request_session_ending(db, &account)
        .await
        .expect("the request is recorded")
}

/// Renew `replica`'s lease every 200 ms until the task is aborted.
fn keep_renewing(db: &Database, replica: Uuid) -> tokio::task::JoinHandle<()> {
    let db = db.clone();
    tokio::spawn(async move {
        loop {
            renew_replica(&db, replica).await.expect("the lease renews");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
}

#[tokio::test]
async fn a_replica_that_stops_renewing_its_lease_is_reported_lapsed() {
    let db = store("session_endings_lapsed").await;
    let serving = Uuid::new_v4();
    let stopped = Uuid::new_v4();
    register_replica(&db, serving, "node-a")
        .await
        .expect("a lease");
    register_replica(&db, stopped, "node-b")
        .await
        .expect("a lease");
    let request = retired_request(&db, "alice").await;
    let ended = EndedSessions {
        streams_closed: 2,
        detached_sessions_ended: 1,
    };
    record_completed(&db, request.id, serving, ended)
        .await
        .expect("an acknowledgement");

    // The stopped replica's lease is not renewed: after the lease and the
    // grace the command stops waiting for it, and says so.
    let started = Instant::now();
    let report = await_session_endings(&db, &request, Duration::from_secs(30))
        .await
        .expect("the acknowledgements are readable");
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(report.confirmed(), "{report:?}");
    let ending_of = |replica: Uuid| {
        report
            .replicas
            .iter()
            .find(|line| line.replica == replica)
            .map(|line| (line.node.clone(), line.ending))
    };
    assert_eq!(
        ending_of(serving),
        Some(("node-a".to_owned(), ReplicaEnding::Completed(ended)))
    );
    assert_eq!(
        ending_of(stopped),
        Some(("node-b".to_owned(), ReplicaEnding::Lapsed))
    );
}

#[tokio::test]
async fn a_serving_replica_that_has_not_completed_keeps_the_request_pending() {
    let db = store("session_endings_pending").await;
    let replica = Uuid::new_v4();
    register_replica(&db, replica, "node-c")
        .await
        .expect("a lease");
    let renewing = keep_renewing(&db, replica);
    let request = retired_request(&db, "alice").await;

    let report = await_session_endings(&db, &request, Duration::from_secs(2))
        .await
        .expect("the acknowledgements are readable");
    assert!(!report.confirmed(), "{report:?}");
    assert_eq!(report.replicas[0].ending, ReplicaEnding::Pending);

    // The replica's own reading of the requests names this one; once it
    // records the request as completed the command is confirmed.
    let to_complete = requests_to_complete(&db, replica)
        .await
        .expect("the requests are readable");
    assert_eq!(
        to_complete
            .iter()
            .map(|request| request.id)
            .collect::<Vec<_>>(),
        vec![request.id]
    );
    record_completed(&db, request.id, replica, EndedSessions::default())
        .await
        .expect("an acknowledgement");
    let report = await_session_endings(&db, &request, Duration::from_secs(5))
        .await
        .expect("the acknowledgements are readable");
    assert!(report.confirmed(), "{report:?}");
    assert!(requests_to_complete(&db, replica)
        .await
        .expect("the requests are readable")
        .is_empty());
    renewing.abort();
}

#[tokio::test]
async fn a_released_lease_does_not_count() {
    let db = store("session_endings_released").await;
    let replica = Uuid::new_v4();
    register_replica(&db, replica, "node-d")
        .await
        .expect("a lease");
    release_replica(&db, replica).await.expect("released");
    let request = retired_request(&db, "alice").await;

    let started = Instant::now();
    let report = await_session_endings(&db, &request, Duration::from_secs(10))
        .await
        .expect("the acknowledgements are readable");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(report.confirmed());
    assert!(report.replicas.is_empty(), "{report:?}");
}

async fn count(actor: &kameo::actor::ActorRef<DbActor>, sql: &str) -> i64 {
    let row = actor
        .ask(DbQueryOne {
            sql: sql.to_owned(),
            params: Vec::new(),
        })
        .await
        .expect("count query")
        .expect("count row");
    match row.first() {
        Some(Value::Integer(count)) => *count,
        other => panic!("unexpected count {other:?}"),
    }
}

#[tokio::test]
async fn the_owner_removes_old_leases_requests_and_acknowledgements() {
    use kameo::actor::Spawn;

    let db = store("session_endings_prune").await;
    let actor = DbActor::spawn(DbActor::new(db.clone()));
    let three_days_ago = chrono::Utc::now().timestamp_millis() - 3 * 24 * 60 * 60 * 1000;
    let old_replica = Uuid::new_v4().simple().to_string();
    let old_request = Uuid::new_v4().simple().to_string();
    for (sql, params) in [
        (
            "INSERT INTO account_session_ending_replicas \
             (replica_id, node, registered_at_ms, lease_expires_at_ms) VALUES (?, 'old', ?, ?)",
            vec![
                Value::from(old_replica.clone()),
                Value::from(three_days_ago),
                Value::from(three_days_ago),
            ],
        ),
        (
            "INSERT INTO account_session_ending_requests \
             (request_id, account_jid, requested_at_ms) VALUES (?, 'old@example.com', ?)",
            vec![
                Value::from(old_request.clone()),
                Value::from(three_days_ago),
            ],
        ),
        (
            "INSERT INTO account_session_ending_acks \
             (request_id, replica_id, observed_at_ms) VALUES (?, ?, ?)",
            vec![
                Value::from(old_request.clone()),
                Value::from(old_replica.clone()),
                Value::from(three_days_ago),
            ],
        ),
    ] {
        actor
            .ask(DbExecute {
                sql: sql.to_owned(),
                params,
            })
            .await
            .expect("an old row");
    }
    let current = Uuid::new_v4();
    register_replica(&db, current, "node-e")
        .await
        .expect("a lease");
    retired_request(&db, "alice").await;

    let pruned = prune_session_endings(&db).await.expect("pruned");
    assert_eq!(
        (pruned.leases, pruned.requests, pruned.acknowledgements),
        (1, 1, 1)
    );
    for table in [
        "account_session_ending_replicas",
        "account_session_ending_requests",
    ] {
        assert_eq!(
            count(&actor, &["SELECT count(*) FROM ", table].concat()).await,
            1,
            "{table} keeps the current row"
        );
    }
    assert_eq!(
        count(&actor, "SELECT count(*) FROM account_session_ending_acks").await,
        0
    );
}

/// Run `waddle-server account retire` with `args`; its exit code and the
/// object it printed.
async fn account_retire(args: &[&str]) -> (i32, serde_json::Value, String) {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_waddle-server"));
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        if key.starts_with("WADDLE_") || key.starts_with("OTEL_") || key.starts_with("PG") {
            command.env_remove(key);
        }
    }
    command
        .args(["account", "retire"])
        .args(args)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(120), command.output())
        .await
        .expect("the subcommand exits")
        .expect("the subcommand runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (
        output.status.code().expect("exit code"),
        serde_json::from_str(stdout.trim()).unwrap_or(serde_json::Value::Null),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A replica on `db` that renews its lease and, when `completes`, records
/// every request it reads as completed with nothing ended.
fn simulated_replica(db: &Database, replica: Uuid, completes: bool) -> tokio::task::JoinHandle<()> {
    let db = db.clone();
    tokio::spawn(async move {
        loop {
            let _ = renew_replica(&db, replica).await;
            if completes {
                if let Ok(requests) = requests_to_complete(&db, replica).await {
                    for request in requests {
                        let _ =
                            record_completed(&db, request.id, replica, EndedSessions::default())
                                .await;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
}

#[tokio::test]
async fn the_operator_subcommand_reports_each_serving_replica() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("waddle.db");
    let file = path.to_string_lossy().into_owned();
    let db = Database::from_config(
        "retire-sessions-cli",
        &DatabaseConfig::new(
            DatabaseDriver::Sqlite,
            ["sqlite://", &file, "?mode=rwc"].concat(),
        ),
    )
    .await
    .expect("SQLite database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");

    // No server holds a lease: the command reports that and succeeds.
    let (code, printed, stderr) =
        account_retire(&["--jid", "heidi@example.com", "--sqlite-database", &file]).await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(printed["sessions"]["confirmed"], true);
    assert_eq!(printed["sessions"]["replicas"], serde_json::json!([]));

    // A serving replica that acknowledges is reported completed.
    let acknowledging = Uuid::new_v4();
    register_replica(&db, acknowledging, "node-f")
        .await
        .expect("a lease");
    let replica = simulated_replica(&db, acknowledging, true);
    let (code, printed, stderr) =
        account_retire(&["--jid", "ivan@example.com", "--sqlite-database", &file]).await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(printed["newly_retired"], true);
    assert_eq!(printed["sessions"]["confirmed"], true);
    let replicas = printed["sessions"]["replicas"]
        .as_array()
        .expect("replica lines");
    assert_eq!(replicas.len(), 1, "{printed}");
    assert_eq!(replicas[0]["replica"], acknowledging.simple().to_string());
    assert_eq!(replicas[0]["node"], "node-f");
    assert_eq!(replicas[0]["state"], "completed");
    assert_eq!(replicas[0]["streams_closed"], 0);
    replica.abort();
    release_replica(&db, acknowledging).await.expect("released");

    // A serving replica that does not acknowledge within the wait: the
    // account is retired, and the command exits as pending (3).
    let silent = Uuid::new_v4();
    register_replica(&db, silent, "node-g")
        .await
        .expect("a lease");
    let replica = simulated_replica(&db, silent, false);
    let (code, printed, stderr) = account_retire(&[
        "--jid",
        "judy@example.com",
        "--sqlite-database",
        &file,
        "--session-wait",
        "2",
    ])
    .await;
    assert_eq!(code, 3, "{stderr}");
    assert_eq!(printed["newly_retired"], true);
    assert_eq!(printed["sessions"]["confirmed"], false);
    assert_eq!(printed["sessions"]["replicas"][0]["state"], "pending");
    replica.abort();
    drop(db);
}
