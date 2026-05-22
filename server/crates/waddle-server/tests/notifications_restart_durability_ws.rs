//! Restart-durability integration test for the XEP-0357 push pipeline
//! (#531 partial slice).
//!
//! Proves that the **push-pipeline** durable rows —
//! XEP-0357 registrations (`push_registrations`, `push_nodes`,
//! `push_devices`), T0 candidates (`notification_candidates`), and T1
//! outbox jobs (`notification_outbox`) — survive a server restart
//! pointed at the same SQLite file, AND that the second server does
//! not insert duplicate rows when resuming the pipeline.
//!
//! ## Scope
//!
//! `ws_common::TestServer::start_persistent_with_extra_accounts`
//! only points `WADDLE_DATABASE_URL` (the user-server / push-pipeline
//! database) at the on-disk file. The harness leaves MAM
//! (`WADDLE_XMPP_MAM_DATABASE_URL`), inbox, stream-management,
//! pending-delivery, and pubsub-event databases at `sqlite::memory:`
//! per `ws_common/mod.rs`. Those XMPP surfaces therefore reset on
//! restart in this test — separate restart-durability tests would
//! be required to assert their persistence. This test scope is
//! deliberately narrow to the **push-pipeline** durable rows
//! enumerated above; the test passes iff those rows survive AND no
//! duplicate row appears post-restart.
//!
//! ## Harness pattern
//!
//! 1. Spawn a `waddle-server` against a persistent SQLite file.
//! 2. Bob registers a device + enables XEP-0357 push.
//! 3. Admin sends an offline DM. The push-pipeline T0 emission path
//!    writes the durable `notification_candidates` row regardless
//!    of whether the in-memory MAM/inbox/offline-storage surfaces
//!    also persisted (those are out of scope for THIS test).
//! 4. Poll the SQLite file directly until a `notification_candidates`
//!    row exists for Bob, confirming T0 wrote the durable row.
//! 5. Kill the first server (via `Drop`).
//! 6. Spawn a second `waddle-server` against the SAME SQLite file.
//! 7. Assert via SQL: registration row, candidate-or-outbox row, and
//!    push_devices row all survive AND no duplicate candidate row was
//!    inserted by the second server (the PRIMARY KEY contract on
//!    `notification_candidates` forces an idempotent re-emission to
//!    collapse via the `Duplicate` arm of `insert_candidate`).
//!
//! The publish-completing-after-restart path is exercised by
//! `xep0357_offline_dm_emits_durable_summary_pubsub_publish_job` on a
//! single server; this test deliberately stays focused on durability
//! so the assertion is insensitive to second-server warmup timing.

mod ws_common;

use std::{
    str::FromStr,
    time::{Duration, Instant},
};

use sqlx::Row;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const CLIENT_NS: &str = "jabber:client";
const NS_PUSH: &str = "urn:xmpp:push:0";
const NS_WADDLE_PUSH_SERVICE: &str = "urn:waddle:push-service:0";
const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const PUSH_SERVICE_JID: &str = "push.localhost";

fn element_to_xml(element: Element) -> String {
    let mut bytes = Vec::new();
    element
        .write_to(&mut bytes)
        .expect("serialize element to wire xml");
    String::from_utf8(bytes).expect("xmpp_parsers serializes valid UTF-8")
}

fn iq_frame(iq_type: &str, id: &str, to: &str, payload: Element) -> String {
    element_to_xml(
        Element::builder("iq", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), iq_type)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
            .append(payload)
            .build(),
    )
}

async fn send_iq(client: &mut WsXmppClient, frame: String, id: &str) -> String {
    client.send(&frame).await.expect("send iq");
    client
        .recv_matching(|xml| {
            xml.contains(&format!("id='{id}'")) || xml.contains(&format!("id=\"{id}\""))
        })
        .await
        .expect("await iq response")
}

fn child_attr(xml: &str, child_name: &str, attr: &str) -> Option<String> {
    let parsed = Element::from_str(xml).ok()?;
    parsed
        .children()
        .find(|c| c.name() == child_name)
        .and_then(|c| c.attr(attr).map(str::to_string))
}

async fn wait_for_candidate_row(database_url: &str, recipient: &str) {
    let pool = sqlx::SqlitePool::connect(database_url)
        .await
        .expect("open sqlite db");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count \
             FROM notification_candidates \
             WHERE recipient_bare_jid = ?",
        )
        .bind(recipient)
        .fetch_one(&pool)
        .await
        .expect("query notification_candidates");
        let count: i64 = row.get("count");
        if count > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for notification_candidates row for {recipient}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn count(database_url: &str, sql: &str, bind: &str) -> i64 {
    let pool = sqlx::SqlitePool::connect(database_url)
        .await
        .expect("open sqlite db");
    let row = sqlx::query(sql)
        .bind(bind)
        .fetch_one(&pool)
        .await
        .expect("count query");
    row.get(0)
}

/// #531 acceptance criterion: "Restart tests prove durable
/// registrations, candidates, and outbox jobs survive process
/// restart."
///
/// Spawn a server with a persistent SQLite file, register Bob's
/// push, queue an offline DM that produces a `notification_candidates`
/// row, kill the server, then spawn a NEW server pointed at the same
/// SQLite file. Assert that the durable rows survive AND the new
/// server doesn't insert duplicate candidate rows.
///
/// The post-restart drain → publish pipeline is exercised by the
/// single-server `xep0357_offline_dm_emits_durable_summary_pubsub_publish_job`
/// test; this test focuses ONLY on durability of the surviving rows
/// so the assertion stays insensitive to second-server warmup
/// timing.
#[tokio::test]
async fn push_pipeline_durable_rows_survive_server_restart() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("restart-durability.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());

    // ── Phase 1: spin up first server, write durable rows ─────────
    let first = TestServer::start_persistent_with_extra_accounts(
        &database_url,
        &[("bob", "bob-restart-password")],
    );
    let ws_url = first.ws_url();
    let admin_password = first.fixed_account_password().to_string();
    let mut bob = WsXmppClient::connect_and_auth(
        &ws_url,
        DOMAIN,
        "bob",
        "bob-restart-password",
        &format!("restart-bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection (phase 1)");

    let node_response = send_iq(
        &mut bob,
        iq_frame(
            "set",
            "restart-bob-ensure-node",
            PUSH_SERVICE_JID,
            Element::builder("ensure-node", NS_WADDLE_PUSH_SERVICE)
                .attr(minidom::rxml::xml_ncname!("app-id").to_owned(), "web")
                .build(),
        ),
        "restart-bob-ensure-node",
    )
    .await;
    let node = child_attr(&node_response, "node", "id").expect("node id");
    let register_device = Element::builder("register-device", NS_WADDLE_PUSH_SERVICE)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
        .attr(
            minidom::rxml::xml_ncname!("device-id").to_owned(),
            "bob-web-restart",
        )
        .attr(minidom::rxml::xml_ncname!("platform").to_owned(), "web")
        .attr(minidom::rxml::xml_ncname!("environment").to_owned(), "test")
        .append(
            Element::builder("provider-token", NS_WADDLE_PUSH_SERVICE)
                .append("bob-restart-provider-secret")
                .build(),
        )
        .build();
    let _ = send_iq(
        &mut bob,
        iq_frame(
            "set",
            "restart-bob-register-device",
            PUSH_SERVICE_JID,
            register_device,
        ),
        "restart-bob-register-device",
    )
    .await;
    let enable = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            PUSH_SERVICE_JID,
        )
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
        .build();
    let _ = send_iq(
        &mut bob,
        iq_frame("set", "restart-bob-enable", DOMAIN, enable),
        "restart-bob-enable",
    )
    .await;
    let _ = bob.close().await;

    let mut admin = WsXmppClient::connect_and_auth(
        &ws_url,
        DOMAIN,
        USERNAME,
        &admin_password,
        &format!("restart-admin-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("admin connection (phase 1)");
    let offline_message = element_to_xml(
        Element::builder("message", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), "bob@localhost")
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "restart-offline-dm",
            )
            .append(
                Element::builder("body", CLIENT_NS)
                    .append("DM that must survive a server restart")
                    .build(),
            )
            .build(),
    );
    admin
        .send(&offline_message)
        .await
        .expect("send offline DM (phase 1)");

    // Wait for the durable T0 candidate row to land. This is the
    // load-bearing wait: after this returns, the restart MUST
    // preserve the row.
    wait_for_candidate_row(&database_url, "bob@localhost").await;

    let candidates_before_restart = count(
        &database_url,
        "SELECT COUNT(*) FROM notification_candidates WHERE recipient_bare_jid = ?",
        "bob@localhost",
    )
    .await;
    let registrations_before_restart = count(
        &database_url,
        "SELECT COUNT(*) FROM push_registrations WHERE owner_bare_jid = ?",
        "bob@localhost",
    )
    .await;
    let devices_before_restart = count(
        &database_url,
        "SELECT COUNT(*) FROM push_devices WHERE node = ?",
        node.as_str(),
    )
    .await;
    assert!(
        candidates_before_restart >= 1,
        "expected at least one notification_candidates row before restart"
    );
    assert!(
        registrations_before_restart >= 1,
        "expected at least one push_registrations row before restart"
    );
    assert!(
        devices_before_restart >= 1,
        "expected at least one push_devices row before restart"
    );

    let _ = admin.close().await;
    // Drop the first server — its `Drop` kills the child process
    // and waits for it. The SQLite file remains on disk.
    drop(first);

    // ── Phase 2: restart against the SAME SQLite file ─────────────
    let second = TestServer::start_persistent_with_extra_accounts(
        &database_url,
        &[("bob", "bob-restart-password")],
    );

    // Durable rows survive. The second server's outbox janitor may
    // have already drained the candidate row by the time this SELECT
    // runs (the janitor ticks on startup), so the upper bound is
    // `candidates_before_restart` — but the row MUST have existed
    // at least long enough for the drain to consume it. A regression
    // where the rows DISAPPEAR from disk on shutdown/startup would
    // produce a count of 0 AND skip the candidate-coalesce path,
    // which is what this assertion guards against (the durable row
    // visible OR the drain that consumed it both prove durability —
    // a zero count would mean the row never existed post-restart).
    let candidates_after_restart = count(
        &database_url,
        "SELECT COUNT(*) FROM notification_candidates WHERE recipient_bare_jid = ?",
        "bob@localhost",
    )
    .await;
    let outboxed_after_restart = count(
        &database_url,
        "SELECT COUNT(*) FROM notification_outbox WHERE recipient_bare_jid = ?",
        "bob@localhost",
    )
    .await;
    assert!(
        candidates_after_restart + outboxed_after_restart >= 1,
        "post-restart durability violation: both notification_candidates \
         AND notification_outbox are empty for bob@localhost; the \
         pre-restart candidate row did not survive. \
         candidates_before_restart={candidates_before_restart} \
         candidates_after_restart={candidates_after_restart} \
         outboxed_after_restart={outboxed_after_restart}"
    );
    assert!(
        candidates_after_restart <= candidates_before_restart,
        "candidates count MUST NOT grow across restart — duplicate \
         insertion would violate PRIMARY KEY idempotency. \
         before={candidates_before_restart} \
         after={candidates_after_restart}"
    );
    let registrations_after_restart = count(
        &database_url,
        "SELECT COUNT(*) FROM push_registrations WHERE owner_bare_jid = ?",
        "bob@localhost",
    )
    .await;
    assert_eq!(
        registrations_after_restart, registrations_before_restart,
        "push_registrations rows MUST NOT disappear on restart"
    );
    let devices_after_restart = count(
        &database_url,
        "SELECT COUNT(*) FROM push_devices WHERE node = ?",
        node.as_str(),
    )
    .await;
    assert_eq!(
        devices_after_restart, devices_before_restart,
        "push_devices rows MUST NOT disappear on restart"
    );

    // Connect a client to the SECOND server so we know it's serving
    // requests post-restart, then sample candidate count again. The
    // second server's drain workers might or might not have ticked
    // yet (depends on janitor interval) — but the row count MUST NOT
    // grow due to a duplicate-insert bug, regardless of drain
    // progress.
    let bob = WsXmppClient::connect_and_auth(
        &second.ws_url(),
        DOMAIN,
        "bob",
        "bob-restart-password",
        &format!("restart-bob-phase2-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection (phase 2)");
    let _ = bob.close().await;

    let candidates_after_phase2_warmup = count(
        &database_url,
        "SELECT COUNT(*) FROM notification_candidates WHERE recipient_bare_jid = ?",
        "bob@localhost",
    )
    .await;
    assert!(
        candidates_after_phase2_warmup <= candidates_after_restart,
        "candidates count MUST NOT grow on the second server — the \
         drain MAY remove rows (post-publish prune) but MUST NOT \
         duplicate; before_restart={candidates_before_restart} \
         after_restart={candidates_after_restart} \
         after_phase2_warmup={candidates_after_phase2_warmup}"
    );

    drop(second);
}
