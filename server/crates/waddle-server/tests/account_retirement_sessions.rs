//! Retiring an account ends its open sessions (ADR-0020 D8;
//! `docs/operations/account-retirement.md`).
//!
//! After the retirement committed, the operator command records a request;
//! every serving replica takes it up, closes the account's live streams with
//! `<not-authorized/>` (RFC 6120 §4.9.3.12), ends its detached
//! stream-management sessions, refuses its bindings and acknowledges; the
//! command reports each replica whose lease counts. What happens to the
//! account's XEP-0198 sessions is `xep0198_account_retirement_ws.rs`.
//!
//! The replica cases drive the production `/ws` router over a loopback
//! socket against the in-memory test state, and run the replica's lease and
//! its work one tick at a time instead of on their timers. The command's
//! wait and report run against an SQLite database of their own, and the
//! operator subcommand runs as the server binary against an SQLite file. No
//! PostgreSQL, SpiceDB or second server process is involved.
#![cfg(feature = "test-support")]

// The fixture serves two suites; each uses a part of it.
#[allow(dead_code, unused_imports)]
#[path = "account_retirement_support/mod.rs"]
mod support;

use std::time::{Duration, Instant};

use jid::FullJid;
use support::{
    account, answers_ping, ended_not_authorized, frames_until_closed, occupant_presence,
    occupied_room, receive, send, Server, WAIT,
};
use uuid::Uuid;
use waddle_server::auth::retirement::retire_account;
use waddle_server::auth::session_endings::{
    await_session_endings, prune_session_endings, record_completed, register_replica,
    release_replica, renew_replica, request_session_ending, requests_to_complete, EndedSessions,
    ReplicaEnding, SessionEndingRequest,
};
use waddle_server::db::actor::{DbActor, DbExecute, DbQueryOne};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner, Value};
use xmpp_parsers::bind::BindQuery;
use xmpp_parsers::iq::Iq;

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
