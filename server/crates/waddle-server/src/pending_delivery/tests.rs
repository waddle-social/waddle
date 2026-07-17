use super::*;
use chrono::Utc;
use kameo::actor::{ActorRef, Spawn};
use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow};
use waddle_xmpp::registry::UserRegistryActor;
use xmpp_parsers::message::{Message, MessageType};

fn bare(s: &str) -> BareJid {
    s.parse().expect("bare jid")
}

/// A fresh, empty actor-authoritative registry for
/// `sm_promotion::promote_session_unacked` (ADR-0017 Phase 3 Slice 9).
fn test_user_registry() -> ActorRef<UserRegistryActor> {
    UserRegistryActor::spawn(UserRegistryActor::new())
}

fn full(s: &str) -> FullJid {
    s.parse().expect("full jid")
}

fn transient_row(recipient: &str, body: &str) -> PendingRow {
    let mut m = Message::new(Some(recipient.parse::<jid::Jid>().expect("jid")));
    m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().expect("jid"));
    m.type_ = MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang(String::new()), body.to_string());
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: bare(recipient),
        original_receipt_at: Utc::now(),
        payload: PendingPayload::Transient(Box::new(m)),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

fn archived_row(recipient: &str, archive_id: &str) -> PendingRow {
    let recipient_bare = bare(recipient);
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient_bare.clone(),
        original_receipt_at: Utc::now(),
        payload: PendingPayload::Archived(StanzaId::new(
            archive_id,
            jid::Jid::from(recipient_bare),
        )),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

fn message_xml(message: Message) -> String {
    let element: xmpp_parsers::minidom::Element = message.into();
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize message");
    String::from_utf8(buf).expect("utf-8 xml")
}

fn transient_message_xml(recipient: &str, body: &str) -> String {
    let mut m = Message::new(Some(recipient.parse::<jid::Jid>().expect("jid")));
    m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().expect("jid"));
    m.type_ = MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang(String::new()), body.to_string());
    message_xml(m)
}

fn mam_query_frame_xml(recipient: &str, child_name: &str) -> String {
    let mut m = Message::new(Some(recipient.parse::<jid::Jid>().expect("jid")));
    m.from = Some("alice@example.com".parse::<jid::Jid>().expect("jid"));
    m.type_ = MessageType::Normal;
    let payload = match child_name {
        "result" => {
            xmpp_parsers::minidom::Element::builder("result", waddle_xmpp_core::mam::MAM_NS)
                .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q1")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "archive-id-1")
                .append(
                    xmpp_parsers::minidom::Element::builder(
                        "forwarded",
                        waddle_xmpp_core::mam::FORWARD_NS,
                    )
                    .build(),
                )
                .build()
        }
        "fin" => xmpp_parsers::minidom::Element::builder("fin", waddle_xmpp_core::mam::MAM_NS)
            .append(
                xmpp_parsers::minidom::Element::builder("set", waddle_xmpp_core::mam::RSM_NS)
                    .build(),
            )
            .build(),
        other => {
            xmpp_parsers::minidom::Element::builder(other, waddle_xmpp_core::mam::MAM_NS).build()
        }
    };
    m.payloads.push(payload);
    message_xml(m)
}

fn transient_message_with_mam_payload_xml(recipient: &str, body: &str) -> String {
    let mut m = Message::new(Some(recipient.parse::<jid::Jid>().expect("jid")));
    m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().expect("jid"));
    m.type_ = MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang(String::new()), body.to_string());
    m.payloads.push(
        xmpp_parsers::minidom::Element::builder("result", waddle_xmpp_core::mam::MAM_NS)
            .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q1")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "archive-id-1")
            .append(
                xmpp_parsers::minidom::Element::builder(
                    "forwarded",
                    waddle_xmpp_core::mam::FORWARD_NS,
                )
                .build(),
            )
            .build(),
    );
    message_xml(m)
}

#[tokio::test]
async fn flush_with_no_rows_is_noop() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &full("alice@example.com/web"),
        FlushContext {
            server_domain: "example.com",
            sm_session: None,
            blocking_storage: None,
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(outcome, FlushOutcome::default());
}

#[tokio::test]
async fn db_storage_startup_deletes_legacy_mam_query_frames() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir
        .path()
        .join("pending_delivery-cleanup.sqlite")
        .to_str()
        .expect("utf-8 path")
        .to_string();
    let url = format!("sqlite://{path}");
    let receipt_ms = Utc::now().timestamp_millis();

    {
        let db = crate::db::Database::from_config(
            "legacy_pending_delivery",
            &crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Sqlite, url.clone()),
        )
        .await
        .expect("open legacy db");
        let conn = db.guard().await.expect("db guard");
        conn.execute(
            "CREATE TABLE pending_delivery (
                row_id TEXT PRIMARY KEY,
                recipient_jid TEXT NOT NULL,
                original_receipt_at INTEGER NOT NULL,
                payload_kind TEXT NOT NULL,
                archive_stanza_by TEXT,
                archive_stanza_id TEXT,
                transient_xml TEXT,
                flushed_in_session TEXT
            )",
            (),
        )
        .await
        .expect("create legacy table");
        let mut legacy_rows = vec![
            (
                "mam-result".to_string(),
                mam_query_frame_xml("alice@example.com/web", "result"),
            ),
            (
                "mam-fin".to_string(),
                mam_query_frame_xml("alice@example.com/web", "fin"),
            ),
            (
                "normal-message".to_string(),
                transient_message_xml("alice@example.com", "keep"),
            ),
            (
                "body-with-mam-payload".to_string(),
                transient_message_with_mam_payload_xml("alice@example.com", "keep-with-payload"),
            ),
        ];
        for i in 0..140 {
            legacy_rows.push((
                format!("mam-result-{i:03}"),
                mam_query_frame_xml("alice@example.com/web", "result"),
            ));
        }

        for (row_id, xml) in legacy_rows {
            conn.execute(
                "INSERT INTO pending_delivery (
                    row_id, recipient_jid, original_receipt_at, payload_kind,
                    archive_stanza_by, archive_stanza_id, transient_xml, flushed_in_session
                 ) VALUES (?, ?, ?, 'transient', NULL, NULL, ?, NULL)",
                crate::db_params![row_id, "alice@example.com", receipt_ms, xml],
            )
            .await
            .expect("insert legacy row");
        }
    }

    let storage = DatabasePendingDeliveryStorage::open(Some(&url), QuotaPolicy::Unlimited)
        .await
        .expect("open storage with startup cleanup");
    let rows = storage
        .list(&bare("alice@example.com"))
        .await
        .expect("list cleaned rows");

    let mut bodies = rows
        .iter()
        .filter_map(|row| match &row.payload {
            PendingPayload::Transient(message) => message.bodies.get("").map(|body| body.as_str()),
            PendingPayload::Archived(_) => None,
        })
        .collect::<Vec<_>>();
    bodies.sort_unstable();
    assert_eq!(bodies, ["keep", "keep-with-payload"]);

    let db = crate::db::Database::from_config(
        "legacy_pending_delivery_marker",
        &crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Sqlite, url.clone()),
    )
    .await
    .expect("open cleaned db");
    let conn = db.guard().await.expect("db guard");
    let mut marker_rows = conn
        .query(
            "SELECT completed_at FROM pending_delivery_startup_migrations \
             WHERE name = 'legacy_mam_query_frames_v1'",
            (),
        )
        .await
        .expect("query cleanup marker");
    let marker_row = marker_rows
        .next()
        .await
        .expect("read cleanup marker")
        .expect("cleanup marker row");
    let completed_at: i64 = marker_row.get(0).expect("cleanup marker timestamp");
    assert!(completed_at > i64::from(i32::MAX));
}

#[tokio::test]
async fn flush_pushes_transient_rows_and_keeps_them_for_sm_ack() {
    // SM-enabled flush: rows are pushed but stay in storage
    // claimed by the SM session until `delete_acked_in_window` is
    // called by the SM ack handler.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    for body in ["one", "two"] {
        storage
            .insert(transient_row("alice@example.com", body))
            .await
            .unwrap();
    }

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);

    let sm_session = SmSessionId::new("sm-stream-uuid-1");
    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(outcome.claimed, 2);
    assert_eq!(outcome.pushed, 2);
    assert_eq!(outcome.unresolved, 0);

    let mut received = Vec::new();
    while let Ok(stanza) = rx.try_recv() {
        received.push(stanza);
    }
    assert_eq!(received.len(), 2);

    // Locked Q7b SM-ack lifecycle: rows stay in storage tagged
    // `flushed_in_session` after push; deletion happens on SM
    // `<a h>` ack via `delete_acked_in_window`, NOT on send.
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 2);
    let listed = storage.list(&bare("alice@example.com")).await.unwrap();
    for row in &listed {
        assert_eq!(
            row.flushed_in_session.as_ref(),
            Some(&sm_session),
            "row claimed by the recovering SM session until SM-ack"
        );
    }
    let row_ids: std::collections::HashSet<_> = received
        .iter()
        .filter_map(|o| o.pending_row_id.clone())
        .collect();
    assert_eq!(
        row_ids.len(),
        2,
        "every flush stanza carries its pending_row_id"
    );
}

#[tokio::test]
async fn flush_non_sm_session_deletes_on_push() {
    // Codex review on PR #358: when the recovering connection has
    // NOT enabled XEP-0198, the SM ack handler will never fire to
    // delete claimed rows. The flush function must fall back to
    // delete-on-push so the queue doesn't leak forever for non-SM
    // clients.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    for body in ["one", "two"] {
        storage
            .insert(transient_row("alice@example.com", body))
            .await
            .unwrap();
    }

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);

    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: None, // ← no SM session: delete-on-push fallback
            blocking_storage: None,
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(outcome.claimed, 2);
    assert_eq!(outcome.pushed, 2);

    // Both messages were sent on the wire.
    let mut received = Vec::new();
    while let Ok(stanza) = rx.try_recv() {
        received.push(stanza);
    }
    assert_eq!(received.len(), 2);

    // Non-SM fallback: rows are deleted on Sent (no ack will ever
    // fire). Storage is empty.
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
}

#[tokio::test]
async fn flush_releases_rows_when_no_push_succeeds() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(transient_row("alice@example.com", "hi"))
        .await
        .unwrap();

    // No connection registered → send_to returns NotConnected.
    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");

    let sm_session = SmSessionId::new("sm-stream-uuid-1");
    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.pushed, 0);
    // Row stays in storage but with flushed_in_session cleared so
    // a later flush can retry.
    let rows = storage.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].flushed_in_session.is_none());
}

#[tokio::test]
async fn flush_drains_large_backlog_in_bounded_batches_with_concurrent_consumer() {
    // Issue #1220 regression: a backlog far larger than the recipient's
    // outbound mpsc capacity must flush completely without wedging. The flush
    // now claims and pushes in `FLUSH_BATCH_SIZE` chunks and backpressures on
    // the channel while a concurrent consumer (standing in for the recipient's
    // connection task) drains it. Before the fix the unbounded claim + tight
    // push loop ran inline ON that same connection task, so a backlog past the
    // 256-slot channel self-deadlocked. Here the consumer runs on its own task,
    // and the assertion that matters is that all rows arrive AND more than one
    // batch was drained.
    const ROWS: usize = 300;
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    for n in 0..ROWS {
        storage
            .insert(transient_row("alice@example.com", &format!("msg-{n}")))
            .await
            .unwrap();
    }

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    // Channel smaller than the backlog so an undrained flush would block.
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    registry.register(resource.clone(), tx);

    // Consumer drains concurrently, mirroring the recipient's connection task.
    let consumer = tokio::spawn(async move {
        let mut received = 0usize;
        while (rx.recv().await).is_some() {
            received += 1;
            if received == ROWS {
                break;
            }
        }
        received
    });

    let sm_session = SmSessionId::new("sm-stream-uuid-large");
    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;

    let received = consumer.await.unwrap();
    assert_eq!(outcome.claimed, ROWS as u32);
    assert_eq!(outcome.pushed, ROWS as u32);
    assert_eq!(received, ROWS);
    assert_eq!(
        outcome.batches,
        (ROWS as u32).div_ceil(FLUSH_BATCH_SIZE as u32),
        "large backlog drained in bounded FIFO batches"
    );
    assert_eq!(
        storage.count(&bare("alice@example.com")).await.unwrap(),
        ROWS as u32,
        "SM rows stay claimed until the recovering session acks"
    );
}

#[tokio::test]
async fn flush_owner_gated_sm_push_skips_mismatched_owner_and_releases_row() {
    // Issue #1220 review: the SM flush push is owner-gated so a same-full-JID
    // replacement racing in mid-flush cannot receive rows claimed under the
    // original session's stream id. A flush carrying a NON-matching owner
    // token must deliver nothing and release the row for the replacement's own
    // flush; the SAME flush with the live owner token delivers.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(transient_row("alice@example.com", "hi"))
        .await
        .unwrap();
    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let live_owner = registry.register(resource.clone(), tx); // the entry's real owner token
    let stale_owner = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)); // a DIFFERENT token
    let sm_session = SmSessionId::new("sm-owner");
    let recipient = bare("alice@example.com");

    let gated = flush_for_resource(
        &storage,
        &registry,
        &recipient,
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: Some(&stale_owner),
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(gated.claimed, 1);
    assert_eq!(gated.pushed, 0, "mismatched-owner SM push is gated out");
    assert!(
        rx.try_recv().is_err(),
        "nothing delivered when the owner token does not match"
    );
    let rows = storage.list(&recipient).await.unwrap();
    assert!(
        rows[0].flushed_in_session.is_none(),
        "gated-out row is released for the replacement's own flush"
    );

    // Same flush, live owner token: delivers.
    let delivered = flush_for_resource(
        &storage,
        &registry,
        &recipient,
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: Some(&live_owner),
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(delivered.pushed, 1, "matching owner token delivers");
    assert!(rx.try_recv().is_ok());
}

// ── DatabasePendingDeliveryStorage integration tests ────────────────

#[tokio::test]
async fn db_storage_claim_batch_first_caller_wins_across_sessions() {
    // Issue #1220 review: the SQL claim's outer `flushed_in_session IS NULL`
    // guard makes it first-caller-wins even under concurrent claims of the
    // same recipient (re-checked at commit time, so a READ COMMITTED loser's
    // UPDATE becomes a no-op). This sequential test pins the contract; the
    // serialized in-memory libSQL harness cannot reproduce the concurrent
    // interleave, so the concurrency rationale lives in the SQL comment.
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("open in-memory storage");
    let recipient = bare("alice@example.com");
    for n in 0..3 {
        storage
            .insert(transient_row("alice@example.com", &format!("m{n}")))
            .await
            .unwrap();
    }
    let s1 = SmSessionId::new("s1");
    let s2 = SmSessionId::new("s2");
    let b1 = storage
        .claim_batch_for_session(&recipient, &s1, None, 8)
        .await
        .unwrap();
    assert_eq!(b1.len(), 3);
    let b2 = storage
        .claim_batch_for_session(&recipient, &s2, None, 8)
        .await
        .unwrap();
    assert!(
        b2.is_empty(),
        "first caller wins: the second session claims nothing"
    );
}

#[tokio::test]
async fn db_storage_claim_batch_does_not_redeliver_unstamped_prior_pass_rows() {
    // Issue #1220 review regression (SQL backend). A flush pass claims rows and
    // pushes them, but the recipient's connection task stamps `outbound_sequence`
    // asynchronously — so between passes the rows linger as
    // flushed_in_session=session, outbound_sequence=NULL. A re-flush pass
    // (cursor=None, same session, triggered by reset_offline_flush after a
    // transient MAM error) must NOT re-return those already-claimed unstamped
    // rows, or they double-deliver. The SQL backend upholds this via
    // UPDATE ... RETURNING (returns only rows it transitioned), matching the
    // in-memory backend's flushed_in_session.is_none() filter.
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("open in-memory storage");
    let recipient = bare("alice@example.com");
    for n in 0..3 {
        storage
            .insert(transient_row("alice@example.com", &format!("m{n}")))
            .await
            .unwrap();
    }
    let session = SmSessionId::new("sm-reflush");

    // Pass 1 claims all three; they remain flushed=session, outbound_sequence
    // NULL (pushed but not yet stamped).
    let pass1 = storage
        .claim_batch_for_session(&recipient, &session, None, 8)
        .await
        .unwrap();
    assert_eq!(pass1.len(), 3);

    // Pass 2 (reset retry): cursor=None, same session, nothing newly unclaimed.
    let pass2 = storage
        .claim_batch_for_session(&recipient, &session, None, 8)
        .await
        .unwrap();
    assert!(
        pass2.is_empty(),
        "already-claimed unstamped rows must not be re-returned on a re-flush pass"
    );

    // A genuinely new row IS picked up by the retry (the retry still works).
    storage
        .insert(transient_row("alice@example.com", "m3"))
        .await
        .unwrap();
    let pass3 = storage
        .claim_batch_for_session(&recipient, &session, None, 8)
        .await
        .unwrap();
    assert_eq!(
        pass3.len(),
        1,
        "a freshly inserted unclaimed row is still claimed"
    );
}

#[tokio::test]
async fn db_storage_claim_batch_returns_fifo_prefix_and_continues_by_cursor() {
    // Cross-check the SQL batch path against the storage-level in-memory tests:
    // bounded FIFO prefix + cursor continuation.
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("open in-memory storage");
    let recipient = bare("alice@example.com");
    for n in 0..5 {
        storage
            .insert(transient_row("alice@example.com", &format!("m{n}")))
            .await
            .unwrap();
    }
    let session = SmSessionId::new("sm-fifo");

    let b1 = storage
        .claim_batch_for_session(&recipient, &session, None, 2)
        .await
        .unwrap();
    assert_eq!(b1.len(), 2);
    let cursor = b1.last().unwrap().id.clone();
    let b2 = storage
        .claim_batch_for_session(&recipient, &session, Some(&cursor), 2)
        .await
        .unwrap();
    assert_eq!(b2.len(), 2);
    // Batches are disjoint and strictly increasing by row_id (FIFO).
    assert!(b1.last().unwrap().id.as_str() < b2.first().unwrap().id.as_str());
    let cursor = b2.last().unwrap().id.clone();
    let b3 = storage
        .claim_batch_for_session(&recipient, &session, Some(&cursor), 2)
        .await
        .unwrap();
    assert_eq!(b3.len(), 1, "final short batch drains the backlog");
}

#[tokio::test]
async fn db_storage_round_trips_archived_and_transient_rows() {
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("open in-memory storage");
    // Insert one Archived + one Transient
    let recipient = bare("alice@example.com");
    let archived = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
            1_700_000_000_000,
        )
        .unwrap(),
        payload: PendingPayload::Archived(StanzaId::new(
            "mam-id",
            jid::Jid::from(recipient.clone()),
        )),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let trans = transient_row("alice@example.com", "transient body");
    assert_eq!(
        storage.insert(archived).await.unwrap(),
        InsertOutcome::Inserted
    );
    assert_eq!(
        storage.insert(trans).await.unwrap(),
        InsertOutcome::Inserted
    );

    let rows = storage.list(&recipient).await.unwrap();
    assert_eq!(rows.len(), 2);
    // FIFO: archived inserted first.
    assert!(rows[0].payload.is_archived());
    assert!(rows[1].payload.is_transient());
}

#[tokio::test]
async fn db_storage_lists_only_unoutboxed_unclaimed_archived_rows() {
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("open in-memory storage");
    let keep = archived_row("alice@example.com", "archive-keep");
    let keep_id = keep.id.clone();
    let marked = archived_row("alice@example.com", "archive-marked");
    let marked_id = marked.id.clone();
    let claimed = archived_row("bob@example.com", "archive-claimed");

    assert_eq!(storage.insert(keep).await.unwrap(), InsertOutcome::Inserted);
    assert_eq!(
        storage.insert(marked).await.unwrap(),
        InsertOutcome::Inserted
    );
    assert_eq!(
        storage.insert(claimed).await.unwrap(),
        InsertOutcome::Inserted
    );
    assert_eq!(
        storage
            .insert(transient_row("alice@example.com", "transient"))
            .await
            .unwrap(),
        InsertOutcome::Inserted
    );
    assert_eq!(
        storage
            .mark_notification_outboxed(&marked_id)
            .await
            .unwrap(),
        1
    );
    let claimed_rows = storage
        .claim_for_session(
            &bare("bob@example.com"),
            &SmSessionId::new("claimed-session"),
        )
        .await
        .expect("claim bob rows");
    assert_eq!(claimed_rows.len(), 1);

    let unoutboxed = storage
        .list_unoutboxed_archived(16)
        .await
        .expect("list unoutboxed archived");

    assert_eq!(unoutboxed.len(), 1);
    assert_eq!(unoutboxed[0].id, keep_id);
    assert_eq!(
        storage.mark_notification_outboxed(&keep_id).await.unwrap(),
        1
    );
    assert!(storage
        .list_unoutboxed_archived(16)
        .await
        .expect("list after mark")
        .is_empty());
}

#[tokio::test]
async fn db_storage_reclaim_excludes_already_pushed_rows_for_same_session() {
    // Regression (issue #1122 review, P1): after a transient MAM error
    // defers rows mid-batch and `reset_offline_flush` re-opens the CAS, the
    // NEXT flush re-claims the same SM session. `claim_for_session` must
    // return only the freshly-claimed (deferred) rows, never a row already
    // pushed on this session and still awaiting its SM ack — re-pushing it
    // would duplicate delivery and overwrite its outbound_sequence. Exercises
    // the real SQL backend (the in-memory backend already had this property,
    // which is why the earlier flush tests missed it).
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("open in-memory storage");
    let session = SmSessionId::new("resumed-session");

    let pushed = transient_row("carol@example.com", "pushed-first");
    let pushed_id = pushed.id.clone();
    let deferred = transient_row("carol@example.com", "deferred-second");
    let deferred_id = deferred.id.clone();
    assert_eq!(
        storage.insert(pushed).await.unwrap(),
        InsertOutcome::Inserted
    );
    assert_eq!(
        storage.insert(deferred).await.unwrap(),
        InsertOutcome::Inserted
    );

    // First flush claims both rows FIFO.
    let first = storage
        .claim_for_session(&bare("carol@example.com"), &session)
        .await
        .expect("first claim");
    assert_eq!(first.len(), 2, "both rows claimed on first flush");

    // Row one is pushed (SM sequence stamped); row two is released back for
    // retry, exactly as the transient-error batch abort does.
    assert_eq!(storage.record_pushed_at(&pushed_id, 1).await.unwrap(), 1);
    assert_eq!(storage.release_row(&deferred_id).await.unwrap(), 1);

    // Re-flush of the SAME session must re-claim ONLY the deferred row.
    let second = storage
        .claim_for_session(&bare("carol@example.com"), &session)
        .await
        .expect("second claim");
    assert_eq!(second.len(), 1, "only the deferred row is re-claimed");
    assert_eq!(second[0].id, deferred_id);

    // The already-pushed row keeps its outbound_sequence (not re-claimed,
    // not cleared), so the pending SM ack still deletes exactly it.
    let rows = storage
        .list(&bare("carol@example.com"))
        .await
        .expect("list rows");
    let pushed_row = rows
        .iter()
        .find(|r| r.id == pushed_id)
        .expect("pushed row still present");
    assert_eq!(pushed_row.outbound_sequence, Some(1));
    assert_eq!(pushed_row.flushed_in_session.as_ref(), Some(&session));
}

#[tokio::test]
async fn db_storage_archived_full_jid_by_round_trips_as_bare() {
    // Regression: `StanzaId.by` is a `jid::Jid`, so a future call site
    // could legitimately construct one with a resource. The
    // `archive_stanza_by` column is decoded back as a `BareJid`, so the
    // insert path must narrow with `.to_bare()`. Without that fix,
    // round-tripping a Full-JID `StanzaId` through SQL would fail in
    // `decode_row` and poison the recipient's pending queue.
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("open in-memory storage");
    let recipient = bare("alice@example.com");
    let full_by: jid::Jid = "alice@example.com/resource"
        .parse()
        .expect("valid full jid");
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
            1_700_000_000_000,
        )
        .unwrap(),
        payload: PendingPayload::Archived(StanzaId::new("mam-id", full_by)),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    assert_eq!(storage.insert(row).await.unwrap(), InsertOutcome::Inserted);

    let rows = storage.list(&recipient).await.unwrap();
    assert_eq!(rows.len(), 1);
    match &rows[0].payload {
        PendingPayload::Archived(stanza_id) => {
            assert_eq!(stanza_id.id, "mam-id");
            // Decoded `by` must be the bare form even though we
            // inserted a Full JID, so the column round-trips cleanly.
            assert_eq!(stanza_id.by, jid::Jid::from(recipient));
        }
        other => panic!("expected Archived payload, got {other:?}"),
    }
}

#[tokio::test]
async fn db_storage_quota_returns_quota_exceeded_outcome() {
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::CountCap { max_rows: 2 })
        .await
        .unwrap();
    let recipient = bare("alice@example.com");
    for n in 0..2 {
        assert_eq!(
            storage
                .insert(transient_row("alice@example.com", &format!("body-{n}"),))
                .await
                .unwrap(),
            InsertOutcome::Inserted
        );
    }
    assert_eq!(
        storage
            .insert(transient_row("alice@example.com", "overflow"))
            .await
            .unwrap(),
        InsertOutcome::QuotaExceeded
    );
    assert_eq!(storage.count(&recipient).await.unwrap(), 2);
}

#[tokio::test]
async fn db_storage_claim_release_delete_lifecycle() {
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .unwrap();
    let recipient = bare("alice@example.com");
    for n in 0..3 {
        storage
            .insert(transient_row("alice@example.com", &format!("body-{n}")))
            .await
            .unwrap();
    }

    let session1 = SmSessionId::new("session-1");
    let claimed1 = storage
        .claim_for_session(&recipient, &session1)
        .await
        .unwrap();
    assert_eq!(claimed1.len(), 3);
    // Concurrent claim by another session sees no unclaimed rows.
    let session2 = SmSessionId::new("session-2");
    let claimed2 = storage
        .claim_for_session(&recipient, &session2)
        .await
        .unwrap();
    assert_eq!(claimed2.len(), 0);

    // Release session1's claim → rows become available for session2.
    let released = storage.release_claim(&session1).await.unwrap();
    assert_eq!(released, 3);
    let claimed2 = storage
        .claim_for_session(&recipient, &session2)
        .await
        .unwrap();
    assert_eq!(claimed2.len(), 3);

    // Delete on SM-ack of session2's flush stanzas.
    let removed = storage.delete_claimed(&session2).await.unwrap();
    assert_eq!(removed, 3);
    assert_eq!(storage.count(&recipient).await.unwrap(), 0);
}

#[tokio::test]
async fn pending_row_deleted_only_after_sm_ack() {
    // Q7b end-to-end (issue #209 PR #347):
    // 1. Insert a transient row.
    // 2. Flush to a registered resource — row is claimed + pushed
    //    (OutboundStanza in the channel) but stays in storage.
    // 3. Simulate the recipient main loop's `record_pushed_at`
    //    after `record_outbound`.
    // 4. Simulate an SM `<a h>` ack via `delete_acked_in_window`.
    // 5. Verify the row is now gone.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(transient_row("alice@example.com", "hi"))
        .await
        .unwrap();

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);

    let session_id = waddle_xmpp::pending_delivery::SmSessionId::new("sm-stream-uuid-7");
    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&session_id),
            blocking_storage: None,
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(outcome.pushed, 1);
    // Row stays in storage post-flush, claimed by this session.
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);
    let pushed = rx.try_recv().expect("flush stanza pushed to channel");
    let row_id = pushed
        .pending_row_id
        .clone()
        .expect("flush stanza carries source row id");

    // Recipient main loop simulation: after `record_outbound`
    // assigns SM outbound counter (say h=7), bind it to the row.
    storage.record_pushed_at(&row_id, 7).await.unwrap();

    // Pre-ack: row is still there.
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);

    // Pre-ack with h=6 (covers earlier stanzas, not this one).
    let removed = storage
        .delete_acked_in_window(&session_id, 0, 6)
        .await
        .unwrap();
    assert_eq!(removed, 0, "ack(h=6) does not cover h=7 row");
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);

    // SM ack arrives covering h=7.
    let removed = storage
        .delete_acked_in_window(&session_id, 6, 7)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(
        storage.count(&bare("alice@example.com")).await.unwrap(),
        0,
        "row deleted only after SM-ack (locked Q7b)"
    );
}

#[tokio::test]
async fn pending_row_released_on_pre_ack_session_death() {
    // Q7c end-to-end (issue #209 PR #347):
    // 1. Insert a row + flush via session-A.
    // 2. Stamp it with outbound_sequence (push happened).
    // 3. Session-A dies BEFORE the recipient's SM `<a h>` ack
    //    arrives (e.g. socket dropped). `release_claim(session_A)`
    //    is called by the SM janitor / shutdown drain.
    // 4. A second resource (session-B) recovers and re-claims —
    //    the released row must be eligible.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(transient_row("alice@example.com", "hi"))
        .await
        .unwrap();

    let registry = ConnectionRegistry::new();
    let resource_a = full("alice@example.com/laptop");
    let (tx_a, mut rx_a) = tokio::sync::mpsc::channel(8);
    registry.register(resource_a.clone(), tx_a);

    let session_a = waddle_xmpp::pending_delivery::SmSessionId::new("sm-stream-laptop-uuid");
    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource_a,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&session_a),
            blocking_storage: None,
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(outcome.pushed, 1);
    let pushed = rx_a.try_recv().expect("flush stanza pushed");
    let row_id = pushed.pending_row_id.clone().unwrap();
    // Recipient stamped sequence, but no SM-ack arrives.
    storage.record_pushed_at(&row_id, 3).await.unwrap();

    // Session-A dies pre-ack — the SM janitor's release_claim
    // restores the row to the unclaimed pool.
    let released = storage.release_claim(&session_a).await.unwrap();
    assert_eq!(released, 1);

    // Verify release_claim cleared outbound_sequence too — a
    // stale value would let session-B's first ack delete the row
    // before it even pushes (Qodo review on PR #358).
    let after_release = storage.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(after_release.len(), 1);
    assert!(after_release[0].outbound_sequence.is_none());
    assert!(after_release[0].flushed_in_session.is_none());

    // Second resource comes online and claims for itself with a
    // distinct SM session id (different XEP-0198 stream).
    let resource_b = full("alice@example.com/web");
    let (tx_b, mut rx_b) = tokio::sync::mpsc::channel(8);
    registry.register(resource_b.clone(), tx_b);
    let session_b = waddle_xmpp::pending_delivery::SmSessionId::new("sm-stream-web-uuid");
    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource_b,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&session_b),
            blocking_storage: None,
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(outcome.pushed, 1, "row re-flushed to recovering resource-B");
    let pushed_b = rx_b.try_recv().expect("flush stanza pushed to resource-B");
    assert_eq!(pushed_b.pending_row_id.unwrap(), row_id, "same row");
}

#[tokio::test]
async fn ack_before_record_pushed_at_skips_unsequenced_row() {
    // Greptile review on PR #358: documents the storage-layer
    // contract that motivates the `record_pushed_at` /
    // `delete_acked_in_window` ordering rule in the websocket main
    // loop. If `delete_acked_in_window` runs while a freshly-claimed
    // row's `outbound_sequence` is still NULL, the row is skipped
    // (correct: NULL means "not yet pushed, no h-coverage
    // possible"). The websocket main loop guarantees the
    // record_pushed_at completes before the next inbound frame
    // (including the SM ack) is processed by awaiting it inline
    // — this test pins down the storage semantics so a future
    // refactor that re-introduces async stamping breaks visibly.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(transient_row("alice@example.com", "hi"))
        .await
        .unwrap();
    let session = waddle_xmpp::pending_delivery::SmSessionId::new("sm-stream");
    let claimed = storage
        .claim_for_session(&bare("alice@example.com"), &session)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    let row_id = claimed[0].id.clone();
    // Ack runs before record_pushed_at — outbound_sequence is
    // NULL so the row is skipped. This is the failure mode
    // Greptile flagged when both calls were spawned: the row
    // would persist claimed-but-never-acked until session death.
    let removed = storage
        .delete_acked_in_window(&session, 0, 100)
        .await
        .unwrap();
    assert_eq!(
        removed, 0,
        "NULL outbound_sequence is skipped by delete_acked_in_window"
    );
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);

    // Now record_pushed_at fires (inline ordering would have done
    // this BEFORE the ack). A subsequent ack covering the same
    // h DOES delete the row. This proves recovery — the next ack
    // after the stamp completes the cleanup.
    storage.record_pushed_at(&row_id, 50).await.unwrap();
    let removed = storage
        .delete_acked_in_window(&session, 0, 50)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
}

#[tokio::test]
async fn list_orphaned_claims_returns_only_dead_session_rows() {
    // Issue #209 PR #360 storage-layer contract test for the
    // claim-expiry janitor. Three rows: row-A claimed by
    // session-live, row-B claimed by session-dead, row-C
    // unclaimed. With live=[session-live], the janitor should see
    // only row-B in the orphan list. row-A is recoverable, row-C
    // doesn't need recovery.
    let storage = InMemoryPendingDeliveryStorage::unlimited();
    let alice = bare("alice@example.com");
    for body in ["a", "b", "c"] {
        storage
            .insert(transient_row("alice@example.com", body))
            .await
            .unwrap();
    }
    let session_live = SmSessionId::new("sm-stream-live");
    let session_dead = SmSessionId::new("sm-stream-dead");
    // Claim two rows under each session in turn (claim_for_session
    // takes whatever's currently unclaimed, so call sequentially).
    let claimed_live = storage
        .claim_for_session(&alice, &session_live)
        .await
        .unwrap();
    assert_eq!(claimed_live.len(), 3);
    // Release one row back to unclaimed (simulating partial-success);
    // then "transfer" one to session_dead by releasing all and
    // re-claiming individually.
    for row in &claimed_live {
        storage.release_row(&row.id).await.unwrap();
    }
    // Now manually claim row[0] under session_live, row[1] under
    // session_dead, leave row[2] unclaimed.
    // claim_for_session is all-or-nothing, so build the state via
    // direct inserts on a fresh storage.
    let storage = InMemoryPendingDeliveryStorage::unlimited();
    for (body, session_opt) in [
        ("a", Some(&session_live)),
        ("b", Some(&session_dead)),
        ("c", None),
    ] {
        let mut row = transient_row("alice@example.com", body);
        row.flushed_in_session = session_opt.cloned();
        storage.insert(row).await.unwrap();
    }
    // Janitor sequence: adopt unstamped claims (these rows were
    // inserted with the claim pre-set, so no stamp exists), then list.
    storage
        .stamp_unstamped_claims(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let orphans = storage
        .list_orphaned_claims(std::slice::from_ref(&session_live), i64::MAX)
        .await
        .unwrap();
    assert_eq!(orphans.len(), 1, "only the dead-session row is orphaned");
    assert_eq!(
        orphans[0].1, session_dead,
        "orphan tagged with dead session"
    );
    // Releasing the orphan via `release_row` clears the claim.
    storage.release_row(&orphans[0].0).await.unwrap();
    let after = storage
        .list_orphaned_claims(std::slice::from_ref(&session_live), i64::MAX)
        .await
        .unwrap();
    assert!(after.is_empty(), "no orphans after release");
    // The live-session and unclaimed rows remain in storage.
    assert_eq!(storage.count(&alice).await.unwrap(), 3);
}

#[tokio::test]
async fn list_orphaned_claims_with_empty_live_set_returns_all_claims() {
    // Startup recovery scenario (issue #209 PR #360): SM registry
    // is empty after a restart, every claim is orphaned. The
    // janitor releases them all so the recovering resources can
    // re-flush.
    let storage = InMemoryPendingDeliveryStorage::unlimited();
    for body in ["a", "b"] {
        let mut row = transient_row("alice@example.com", body);
        row.flushed_in_session = Some(SmSessionId::new("sm-stream-pre-restart"));
        storage.insert(row).await.unwrap();
    }
    storage
        .stamp_unstamped_claims(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let orphans = storage.list_orphaned_claims(&[], i64::MAX).await.unwrap();
    assert_eq!(orphans.len(), 2);
}

// #1124 mixed-version guard (Greptile review): a claim with NO
// recency stamp — written by a pre-#1124 binary during a rolling
// deploy — means "recency unknown" and must NOT be release-eligible
// until the janitor adopts it, even with an empty live-set and a
// wide-open floor. Otherwise the mid-flight release re-opens exactly
// during the deploy window.
#[tokio::test]
async fn unstamped_claim_is_skipped_until_adopted() {
    let storage = InMemoryPendingDeliveryStorage::unlimited();
    let mut row = transient_row("alice@example.com", "legacy-claimed");
    row.flushed_in_session = Some(SmSessionId::new("transient:web:legacy"));
    storage.insert(row).await.unwrap();

    let orphans = storage.list_orphaned_claims(&[], i64::MAX).await.unwrap();
    assert!(
        orphans.is_empty(),
        "an unstamped claim must be invisible until adopted"
    );

    let adopted = storage
        .stamp_unstamped_claims(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    assert_eq!(adopted, 1);
    // Adoption starts the floor clock: still fresh under the
    // production floor, eligible once aged past it.
    let floor_in_past = chrono::Utc::now().timestamp_millis() - 180_000;
    assert!(storage
        .list_orphaned_claims(&[], floor_in_past)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        storage
            .list_orphaned_claims(&[], i64::MAX)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn flush_drops_pending_row_when_sender_blocked_after_intake() {
    // Locked XEP-0191 §2 step 4 (issue #209 PR #360): if the
    // recipient blocks the sender AFTER the row was queued, the
    // flush MUST drop the row instead of replaying it. The block
    // is final until lifted, so the row is `delete_row`'d (not
    // released) — no retry needed.
    use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(transient_row("alice@example.com", "blocked-after-intake"))
        .await
        .unwrap();
    // Recipient blocks the sender BEFORE flush.
    let blocking = InMemoryBlockingStorage::new();
    blocking.set_blocklist(bare("alice@example.com"), vec![bare("bob@elsewhere")]);
    let blocking_arc: Arc<dyn BlockingStorage> = Arc::new(blocking);
    // Wire a recovering session.
    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let sm_session = SmSessionId::new("sm-stream-block-test");
    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: Some(&blocking_arc),
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.pushed, 0, "blocked sender's row not pushed");
    assert_eq!(outcome.dropped_blocked, 1);
    // Row is deleted from storage (block is final until lifted).
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
    // Nothing was sent on the wire.
    assert!(
        rx.try_recv().is_err(),
        "no flush stanza pushed for blocked sender"
    );
}

#[tokio::test]
async fn flush_aborts_on_blocking_storage_failure_fail_closed() {
    // Fail-closed semantic (mirrors interpret.rs intake-pass policy):
    // if blocking-storage errors at flush time, the flush MUST abort
    // rather than degrade to an empty blocklist (which would silently
    // let blocked senders through to MAM/inbox via re-delivery).
    use async_trait::async_trait;
    use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};
    #[derive(Debug, thiserror::Error)]
    #[error("simulated backend down")]
    struct SimulatedFailure;
    struct FailingBlocking;
    #[async_trait]
    impl BlockingStorage for FailingBlocking {
        async fn list_blocked_jids(
            &self,
            _user: &BareJid,
        ) -> Result<Vec<BareJid>, BlockingStorageError> {
            Err(BlockingStorageError::new(SimulatedFailure))
        }
    }
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(transient_row("alice@example.com", "must-not-leak"))
        .await
        .unwrap();
    let blocking_arc: Arc<dyn BlockingStorage> = Arc::new(FailingBlocking);
    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let sm_session = SmSessionId::new("sm-stream-fail-closed");
    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: Some(&blocking_arc),
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    // Fail-closed: nothing claimed, nothing pushed, row stays for retry.
    assert_eq!(outcome.claimed, 0);
    assert_eq!(outcome.pushed, 0);
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);
}

#[tokio::test]
async fn db_storage_release_row_if_session_skips_when_session_changed() {
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .unwrap();
    let recipient = bare("alice@example.com");
    storage
        .insert(transient_row("alice@example.com", "wedge-target"))
        .await
        .unwrap();

    let dead_session = SmSessionId::new("sm-stream-dead");
    let live_session = SmSessionId::new("sm-stream-live");
    let claimed = storage
        .claim_for_session(&recipient, &dead_session)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    let row_id = claimed[0].id.clone();
    storage.record_pushed_at(&row_id, 7).await.unwrap();

    storage.release_claim(&dead_session).await.unwrap();
    let reclaimed = storage
        .claim_for_session(&recipient, &live_session)
        .await
        .unwrap();
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].id, row_id);
    storage.record_pushed_at(&row_id, 11).await.unwrap();

    let result = storage
        .release_row_if_session(&row_id, &dead_session)
        .await
        .unwrap();
    assert_eq!(result, 0);

    let after = storage.list(&recipient).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].flushed_in_session.as_ref(), Some(&live_session));
    assert_eq!(after[0].outbound_sequence, Some(11));

    let cleared = storage
        .release_row_if_session(&row_id, &live_session)
        .await
        .unwrap();
    assert_eq!(cleared, 1);
    let after = storage.list(&recipient).await.unwrap();
    assert!(after[0].flushed_in_session.is_none());
    assert!(after[0].outbound_sequence.is_none());
}

#[tokio::test]
async fn release_row_if_session_skips_when_a_fresh_claim_replaced_the_dead_session() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let alice = bare("alice@example.com");
    storage
        .insert(transient_row("alice@example.com", "wedge-target"))
        .await
        .unwrap();

    let dead_session = SmSessionId::new("sm-stream-dead");
    let live_session = SmSessionId::new("sm-stream-fresh-bind");
    let claimed = storage
        .claim_for_session(&alice, &dead_session)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    let row_id = claimed[0].id.clone();
    storage.record_pushed_at(&row_id, 7).await.unwrap();

    storage.release_claim(&dead_session).await.unwrap();
    let reclaimed = storage
        .claim_for_session(&alice, &live_session)
        .await
        .unwrap();
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].id, row_id);
    storage.record_pushed_at(&row_id, 11).await.unwrap();

    let result = storage
        .release_row_if_session(&row_id, &dead_session)
        .await
        .unwrap();
    assert_eq!(result, 0);

    let after = storage.list(&alice).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].flushed_in_session.as_ref(), Some(&live_session));
    assert_eq!(after[0].outbound_sequence, Some(11));
}

#[tokio::test]
async fn release_row_if_session_releases_when_claim_unchanged() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let alice = bare("alice@example.com");
    storage
        .insert(transient_row("alice@example.com", "still-dead"))
        .await
        .unwrap();
    let dead_session = SmSessionId::new("sm-stream-dead");
    let claimed = storage
        .claim_for_session(&alice, &dead_session)
        .await
        .unwrap();
    let row_id = claimed[0].id.clone();
    storage.record_pushed_at(&row_id, 3).await.unwrap();

    let result = storage
        .release_row_if_session(&row_id, &dead_session)
        .await
        .unwrap();
    assert_eq!(result, 1);

    let after = storage.list(&alice).await.unwrap();
    assert!(after[0].flushed_in_session.is_none());
    assert!(after[0].outbound_sequence.is_none());
}

#[tokio::test]
async fn list_orphaned_claims_excludes_active_session_rows() {
    // Codex/Qodo P1 review on PR #360: the claim-expiry janitor's
    // "live" set MUST include both detached/resumable SM sessions
    // (`sm_session_registry.live_session_ids()`) AND currently-
    // connected active SM sessions (`ConnectionEntry.sm_stream_id`).
    // Without the active half, a row claimed by a connected
    // resource awaiting `<a h>` would be misclassified as orphaned
    // and `release_row`'d, breaking the SM-ack lifecycle and
    // producing a duplicate flush on the next presence transition.
    //
    // This test pins the storage-layer contract: passing the
    // active session id into `list_orphaned_claims`'s `live`
    // argument MUST exclude its rows from the orphan list. The
    // janitor wiring in `start_with_config` builds the union and
    // is exercised by integration coverage above.
    let storage = InMemoryPendingDeliveryStorage::unlimited();
    let mut row = transient_row("alice@example.com", "active");
    let active_session = SmSessionId::new("sm-stream-active");
    row.flushed_in_session = Some(active_session.clone());
    storage.insert(row).await.unwrap();

    // Sweep with active_session in the live set: NOT an orphan.
    storage
        .stamp_unstamped_claims(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let orphans = storage
        .list_orphaned_claims(std::slice::from_ref(&active_session), i64::MAX)
        .await
        .unwrap();
    assert!(
        orphans.is_empty(),
        "row claimed by an active SM session must not be flagged as orphaned"
    );

    // Sweep with active session MISSING from the live set: now an
    // orphan. This is the failure mode that prompted the fix —
    // the previous janitor only consulted the detached registry,
    // which would have produced this incorrect result.
    let dead_session = SmSessionId::new("sm-stream-something-else");
    let orphans = storage
        .list_orphaned_claims(std::slice::from_ref(&dead_session), i64::MAX)
        .await
        .unwrap();
    assert_eq!(
        orphans.len(),
        1,
        "row IS an orphan when its session is missing from the live set"
    );
    assert_eq!(orphans[0].1, active_session);
}

#[tokio::test]
async fn janitor_releases_rows_with_dead_sessions() {
    // End-to-end exercise of the claim-expiry janitor's data flow
    // (issue #209 PR #360): given a mixture of rows tagged with
    // a live session, a dead session, and an unclaimed row, the
    // janitor's expected sequence (`list_orphaned_claims(live)`
    // → `release_row(orphan)`) MUST release exactly the dead-
    // session rows and leave the live + unclaimed rows alone.
    //
    // The janitor task itself runs in the websocket runtime and
    // is not directly addressable from a unit test; this test
    // pins the storage-layer flow that the janitor relies on.
    // The websocket wiring (live-set union of detached +
    // active SM streams) is verified separately by
    // `list_orphaned_claims_excludes_active_session_rows`.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let alice = bare("alice@example.com");

    // Build the state directly (claim_for_session is all-or-nothing,
    // which doesn't fit the test setup of mixed claim states).
    let live_session = SmSessionId::new("sm-stream-live");
    let dead_session_a = SmSessionId::new("sm-stream-dead-a");
    let dead_session_b = SmSessionId::new("sm-stream-dead-b");
    for (body, session_opt, sequence_opt) in [
        ("live-claimed", Some(live_session.clone()), Some(7u32)),
        ("dead-claimed-a", Some(dead_session_a.clone()), Some(3u32)),
        ("dead-claimed-b-no-seq", Some(dead_session_b.clone()), None),
        ("unclaimed", None, None),
    ] {
        let mut row = transient_row("alice@example.com", body);
        row.flushed_in_session = session_opt;
        row.outbound_sequence = sequence_opt;
        storage.insert(row).await.unwrap();
    }
    assert_eq!(storage.count(&alice).await.unwrap(), 4);

    // Janitor sweep step 1: adopt unstamped claims, then ask for
    // orphans given the live set.
    storage
        .stamp_unstamped_claims(chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let orphans = storage
        .list_orphaned_claims(std::slice::from_ref(&live_session), i64::MAX)
        .await
        .unwrap();
    assert_eq!(orphans.len(), 2, "two dead-session rows are orphaned");
    let orphan_sessions: std::collections::HashSet<_> =
        orphans.iter().map(|(_, s)| s.clone()).collect();
    assert!(orphan_sessions.contains(&dead_session_a));
    assert!(orphan_sessions.contains(&dead_session_b));

    // Janitor sweep step 2: release each orphan row.
    for (row_id, _) in &orphans {
        storage.release_row(row_id).await.unwrap();
    }

    // Post-sweep assertions:
    // - Live row stays tagged + sequenced (will be deleted by SM ack).
    // - Both dead-session rows are now unclaimed (re-flush eligible).
    // - The originally-unclaimed row is untouched.
    // - No rows were deleted — the janitor only releases.
    assert_eq!(storage.count(&alice).await.unwrap(), 4, "no rows deleted");
    let after = storage.list(&alice).await.unwrap();
    let by_body: std::collections::HashMap<&str, &PendingRow> = after
        .iter()
        .map(|row| {
            let body_marker = match &row.payload {
                PendingPayload::Transient(m) => m.bodies.get("").map(|b| b.as_str()).unwrap_or(""),
                _ => "",
            };
            (body_marker, row)
        })
        .collect();
    let live_row = by_body.get("live-claimed").expect("live row present");
    assert_eq!(live_row.flushed_in_session.as_ref(), Some(&live_session));
    assert_eq!(live_row.outbound_sequence, Some(7));
    let dead_a = by_body.get("dead-claimed-a").expect("dead-a present");
    assert!(dead_a.flushed_in_session.is_none(), "released by janitor");
    assert!(
        dead_a.outbound_sequence.is_none(),
        "release_row clears outbound_sequence"
    );
    let dead_b = by_body
        .get("dead-claimed-b-no-seq")
        .expect("dead-b present");
    assert!(dead_b.flushed_in_session.is_none());
    let unclaimed = by_body.get("unclaimed").expect("unclaimed present");
    assert!(unclaimed.flushed_in_session.is_none());
}

// #1124: an in-flight non-SM flush claims rows under a synthetic
// `transient:` session id that is never in the janitor's live-set.
// The claim recency floor must keep the janitor's hands off those
// claims while the flush is running, and still release them once
// they age past the floor (post-crash orphan recovery).
#[tokio::test]
async fn janitor_floor_protects_in_flight_transient_claims() {
    let storage = InMemoryPendingDeliveryStorage::unlimited();
    let alice = bare("alice@example.com");
    for body in ["a", "b"] {
        storage
            .insert(transient_row("alice@example.com", body))
            .await
            .unwrap();
    }
    let transient_session = SmSessionId::new("transient:web-resource:0197fe2a");
    let claimed = storage
        .claim_batch_for_session(&alice, &transient_session, None, 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 2);

    // Janitor pass with a floor 3 intervals in the past (the
    // production shape): the just-claimed rows are fresh → skipped,
    // even though `transient:` is not in the (empty) live-set.
    let floor_in_past = chrono::Utc::now().timestamp_millis() - 180_000;
    let orphans = storage
        .list_orphaned_claims(&[], floor_in_past)
        .await
        .unwrap();
    assert!(
        orphans.is_empty(),
        "#1124: an overlapping janitor pass must not release in-flight transient claims"
    );

    // Once the claim ages past the floor (post-crash scenario,
    // simulated with a floor in the future), it is release-eligible.
    let orphans = storage.list_orphaned_claims(&[], i64::MAX).await.unwrap();
    assert_eq!(
        orphans.len(),
        2,
        "genuinely orphaned claims must still be released after the floor"
    );
}

// #1124 DB-backend mirror of the floor contract, including the
// release path clearing the recency stamp so a released-then-
// re-claimed row gets a fresh stamp.
#[tokio::test]
async fn db_storage_orphan_listing_respects_claim_recency_floor() {
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .unwrap();
    let alice = bare("alice@example.com");
    for body in ["a", "b"] {
        storage
            .insert(transient_row("alice@example.com", body))
            .await
            .unwrap();
    }
    let transient_session = SmSessionId::new("transient:web-resource:0197fe2a");
    let claimed = storage
        .claim_batch_for_session(&alice, &transient_session, None, 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 2);

    let floor_in_past = chrono::Utc::now().timestamp_millis() - 180_000;
    let orphans = storage
        .list_orphaned_claims(&[], floor_in_past)
        .await
        .unwrap();
    assert!(
        orphans.is_empty(),
        "#1124: fresh transient claims must be invisible to the janitor"
    );

    let orphans = storage.list_orphaned_claims(&[], i64::MAX).await.unwrap();
    assert_eq!(orphans.len(), 2, "aged claims are release-eligible");
    for (row_id, session) in &orphans {
        assert_eq!(
            storage
                .release_row_if_session(row_id, session)
                .await
                .unwrap(),
            1
        );
    }
    // Released rows carry no claim (and no stale recency stamp): a
    // repeat janitor pass sees nothing.
    let orphans = storage.list_orphaned_claims(&[], i64::MAX).await.unwrap();
    assert!(orphans.is_empty(), "release clears the claim + stamp");
    // And the rows are re-claimable by a recovering resource.
    let reclaimed = storage
        .claim_batch_for_session(&alice, &SmSessionId::new("sm-stream-fresh"), None, 10)
        .await
        .unwrap();
    assert_eq!(reclaimed.len(), 2);
}

// #1124 mixed-version guard, DB backend: a claim row written by a
// pre-#1124 binary (flushed_in_session set, claimed_at_ms NULL) must
// be invisible to the janitor until adopted, then age normally.
#[tokio::test]
async fn db_storage_unstamped_claim_is_skipped_until_adopted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir
        .path()
        .join("pending_delivery-unstamped.sqlite")
        .to_str()
        .expect("utf-8 path")
        .to_string();
    let url = format!("sqlite://{path}");
    let storage = DatabasePendingDeliveryStorage::open(Some(&url), QuotaPolicy::Unlimited)
        .await
        .unwrap();
    let raw = crate::db::Database::from_config(
        "pending_delivery_unstamped_raw",
        &crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Sqlite, url.clone()),
    )
    .await
    .expect("raw db handle");
    storage
        .insert(transient_row("alice@example.com", "legacy"))
        .await
        .unwrap();
    // Simulate the old binary's claim: session tag without a stamp
    // (claim_batch always stamps in this binary, so write it raw).
    let conn = raw.guard().await.expect("db guard");
    conn.execute(
        "UPDATE pending_delivery SET flushed_in_session = 'transient:web:legacy'",
        (),
    )
    .await
    .unwrap();
    drop(conn);

    assert!(
        storage
            .list_orphaned_claims(&[], i64::MAX)
            .await
            .unwrap()
            .is_empty(),
        "unstamped claim must be invisible until adopted"
    );
    let now_ms = chrono::Utc::now().timestamp_millis();
    assert_eq!(storage.stamp_unstamped_claims(now_ms).await.unwrap(), 1);
    assert!(
        storage
            .list_orphaned_claims(&[], now_ms - 180_000)
            .await
            .unwrap()
            .is_empty(),
        "adopted claim is fresh under the production floor"
    );
    assert_eq!(
        storage
            .list_orphaned_claims(&[], i64::MAX)
            .await
            .unwrap()
            .len(),
        1,
        "adopted claim ages into release-eligibility"
    );
}

#[tokio::test]
async fn flush_blocked_row_releases_claim_when_delete_fails() {
    // Copilot review on PR #360: if `delete_row` fails for a
    // blocked row, the row would otherwise stay tagged with the
    // current (still-live) SM session id. The SM-expiry janitor
    // wouldn't see it as orphaned, the SM ack wouldn't delete it
    // (NULL outbound_sequence), and the next flush wouldn't
    // re-claim it. Permanent wedge + quota leak. Fix: fall back
    // to `release_row` so the next flush can re-check the block.
    use async_trait::async_trait;
    use waddle_xmpp::pending_delivery::storage::PendingStorageError;
    use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};

    // Wrap an in-memory storage so `delete_row` fails once but
    // every other operation passes through.
    struct DeleteRowFails {
        inner: InMemoryPendingDeliveryStorage,
    }
    #[async_trait]
    impl PendingDeliveryStorage for DeleteRowFails {
        async fn insert(&self, row: PendingRow) -> Result<InsertOutcome, PendingStorageError> {
            self.inner.insert(row).await
        }
        async fn list(&self, recipient: &BareJid) -> Result<Vec<PendingRow>, PendingStorageError> {
            self.inner.list(recipient).await
        }
        async fn claim_for_session(
            &self,
            recipient: &BareJid,
            session: &waddle_xmpp::pending_delivery::SmSessionId,
        ) -> Result<Vec<PendingRow>, PendingStorageError> {
            self.inner.claim_for_session(recipient, session).await
        }
        async fn claim_batch_for_session(
            &self,
            recipient: &BareJid,
            session: &waddle_xmpp::pending_delivery::SmSessionId,
            after: Option<&PendingRowId>,
            limit: usize,
        ) -> Result<Vec<PendingRow>, PendingStorageError> {
            self.inner
                .claim_batch_for_session(recipient, session, after, limit)
                .await
        }
        async fn delete_claimed(
            &self,
            session: &waddle_xmpp::pending_delivery::SmSessionId,
        ) -> Result<u64, PendingStorageError> {
            self.inner.delete_claimed(session).await
        }
        async fn delete_row(&self, _id: &PendingRowId) -> Result<u64, PendingStorageError> {
            Err(PendingStorageError::Other(
                "simulated delete failure".into(),
            ))
        }
        async fn release_claim(
            &self,
            session: &waddle_xmpp::pending_delivery::SmSessionId,
        ) -> Result<u64, PendingStorageError> {
            self.inner.release_claim(session).await
        }
        async fn release_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError> {
            self.inner.release_row(id).await
        }
        async fn record_pushed_at(
            &self,
            id: &PendingRowId,
            sequence: u32,
        ) -> Result<u64, PendingStorageError> {
            self.inner.record_pushed_at(id, sequence).await
        }
        async fn delete_acked_in_window(
            &self,
            session: &waddle_xmpp::pending_delivery::SmSessionId,
            from_exclusive: u32,
            to_inclusive: u32,
        ) -> Result<u64, PendingStorageError> {
            self.inner
                .delete_acked_in_window(session, from_exclusive, to_inclusive)
                .await
        }
        async fn list_orphaned_claims(
            &self,
            live: &[waddle_xmpp::pending_delivery::SmSessionId],
            claimed_before_ms: i64,
        ) -> Result<
            Vec<(PendingRowId, waddle_xmpp::pending_delivery::SmSessionId)>,
            PendingStorageError,
        > {
            self.inner
                .list_orphaned_claims(live, claimed_before_ms)
                .await
        }
        async fn count(&self, recipient: &BareJid) -> Result<u32, PendingStorageError> {
            self.inner.count(recipient).await
        }
        async fn delete_older_than(
            &self,
            cutoff: chrono::DateTime<chrono::Utc>,
        ) -> Result<u64, PendingStorageError> {
            self.inner.delete_older_than(cutoff).await
        }
        async fn scrub_for_tombstone(
            &self,
            target: &waddle_xmpp::tombstone::TombstoneTarget,
        ) -> Result<u64, PendingStorageError> {
            self.inner.scrub_for_tombstone(target).await
        }
    }

    let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(DeleteRowFails {
        inner: InMemoryPendingDeliveryStorage::unlimited(),
    });
    storage
        .insert(transient_row("alice@example.com", "blocked-row"))
        .await
        .unwrap();
    let blocking = InMemoryBlockingStorage::new();
    blocking.set_blocklist(bare("alice@example.com"), vec![bare("bob@elsewhere")]);
    let blocking_arc: Arc<dyn BlockingStorage> = Arc::new(blocking);
    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let sm_session = SmSessionId::new("sm-stream-wedge-test");

    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: Some(&blocking_arc),
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(outcome.dropped_blocked, 1);

    // Row stays in storage (delete_row failed), but the claim
    // MUST be cleared by the release_row fallback so a future
    // flush can re-evaluate the blocklist or push it.
    let rows = storage.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].flushed_in_session.is_none(),
        "release_row fallback cleared the wedged claim"
    );
    assert!(rows[0].outbound_sequence.is_none());
}

#[tokio::test]
async fn xep0160_promoted_stanzas_carry_original_receipt_time_in_delay() {
    // Issue #209 PR #361 dedicated XEP-0160 test (Greptile +
    // Copilot + Qodo P1 review): the SM-promoted-then-replayed
    // path MUST carry the ORIGINAL pending_delivery row's
    // `original_receipt_at` all the way through to the eventual
    // XEP-0203 `<delay/>` stamp on the offline replay, even
    // when the stanza was flushed to a live SM session that
    // disconnected pre-ack and the SM session later expired
    // (Q6 promotion re-creates the pending row).
    //
    // Failure mode this guards against: stamping `Utc::now()`
    // anywhere along the path (flush time, drain time, expiry
    // time) would mean the recipient sees the wrong delivery
    // time on their reconnect.
    //
    // End-to-end flow exercised:
    //   1. Insert pending row with original_receipt_at = T1.
    //   2. flush_for_resource sends OutboundStanza carrying T1.
    //   3. Recipient's main loop records into SM unacked queue
    //      with T1 (via record_outbound_with_receipt_at).
    //   4. Convert SM state → DetachedSession (simulates
    //      disconnect + detach at T2 >> T1).
    //   5. promote_session_unacked re-creates a pending row.
    //   6. Verify the new row's original_receipt_at == T1.
    use waddle_xmpp::stream_management::{DetachedSessionSnapshot, StreamManagementState};
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let registry = ConnectionRegistry::new();
    let alice_bare = bare("alice@example.com");
    let alice_jid = full("alice@example.com/laptop");

    // T1 = the original failed-delivery time (a year ago).
    let t1 = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1_700_000_000_000)
        .expect("valid millis");
    let mut row = transient_row("alice@example.com", "missed-while-offline");
    row.original_receipt_at = t1;
    let row_id = row.id.clone();
    storage.insert(row).await.unwrap();

    // Wire alice's recovering resource as the recipient.
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(alice_jid.clone(), tx);

    // Step 2: flush_for_resource through the SM-enabled path.
    let sm_session_id = waddle_xmpp::pending_delivery::SmSessionId::new("sm-stream-receipt-e2e");
    let outcome = flush_for_resource(
        &storage,
        &registry,
        &alice_bare,
        &alice_jid,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session_id),
            blocking_storage: None,
            owner: None,
            archive_resolver: &NullArchiveResolver,
        },
    )
    .await;
    assert_eq!(outcome.pushed, 1);

    // Step 3: pluck the OutboundStanza from the channel — it
    // MUST carry T1 as pending_row_original_receipt_at.
    let pushed = rx.try_recv().expect("flush stanza pushed");
    assert_eq!(
        pushed.pending_row_id.as_ref(),
        Some(&row_id),
        "OutboundStanza tagged with source row id"
    );
    assert_eq!(
        pushed.pending_row_original_receipt_at,
        Some(t1),
        "OutboundStanza carries the source row's original_receipt_at"
    );

    // Step 4: simulate the recipient's main loop recording the
    // outbound stanza into its SM unacked queue WITH T1, then
    // converting state → DetachedSession (i.e. transport drops).
    let mut sm_state = StreamManagementState::new();
    sm_state.enable("sm-stream-receipt-e2e".to_string(), true, Some(300));
    let xml = match &pushed.stanza {
        waddle_xmpp::Stanza::Message(m) => {
            let element: xmpp_parsers::minidom::Element = m.clone().into();
            let mut buf = Vec::new();
            element.write_to(&mut buf).unwrap();
            String::from_utf8(buf).unwrap()
        }
        _ => panic!("expected Message"),
    };
    let _ = sm_state.record_outbound_with_receipt_at(xml, t1);

    // Convert to detached session (simulates transport drop).
    let detached = sm_state
        .to_detached_session(DetachedSessionSnapshot {
            user_id: "alice".to_string(),
            jid: alice_jid.clone(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .expect("session resumable");

    // Verify the detached snapshot preserved T1.
    assert_eq!(detached.unacked_stanzas.len(), 1);
    assert_eq!(detached.unacked_stanzas[0].original_receipt_at, t1);

    // Clear the original pending row so we observe only the
    // promoted row (the original would have been deleted by
    // SM-ack in production; here we simulate).
    storage.delete_row(&row_id).await.unwrap();

    // Step 5: SM-expiry promotion re-creates the pending row.
    let user_registry = test_user_registry();
    let summary = crate::sm_promotion::promote_session_unacked(
        &detached,
        &registry,
        &user_registry,
        &storage,
        &waddle_xmpp::protocol::session_state::Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;
    assert_eq!(summary.queued, 1);

    // Step 6: the new pending row carries T1, NOT flush/expiry time.
    let rows = storage.list(&alice_bare).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].original_receipt_at, t1,
        "promoted row's original_receipt_at MUST be the source row's T1, \
         NOT the flush/drain/expiry wall-clock"
    );
}

#[tokio::test]
async fn xep0160_pending_delivery_survives_server_restart() {
    // Locked Q8 = B (issue #209): `pending_delivery` rows MUST
    // survive a process restart. This is the actual restart-
    // durability test (Codex P2 review on PR #362: the
    // waddle-xmpp pointer test only exercised read-after-write
    // through the same in-memory handle, which is not a
    // restart-equivalent).
    //
    // Real restart simulation: open a SQLite-backed storage
    // against a tempdir path, insert a row, drop the storage
    // handle (closes the connection), reopen against the SAME
    // path, assert the row is still present.
    //
    // Use `tempdir()` + `path.join()` rather than
    // `NamedTempFile`: NamedTempFile keeps an open OS file
    // handle alive for its lifetime, which can interfere with
    // SQLite's file-locking semantics on some platforms
    // (Copilot review on PR #362). The tempdir version creates
    // only a directory; the SQLite file inside it has no other
    // open handles.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir
        .path()
        .join("pending_delivery.sqlite")
        .to_str()
        .expect("utf-8 path")
        .to_string();
    let url = format!("sqlite://{path}");

    // Boot 1: write a row + drop the handle to close the connection.
    {
        let storage = DatabasePendingDeliveryStorage::open(Some(&url), QuotaPolicy::Unlimited)
            .await
            .expect("open file-backed storage");
        let outcome = storage
            .insert(transient_row("alice@example.com", "across-restart"))
            .await
            .expect("insert before restart");
        assert_eq!(outcome, InsertOutcome::Inserted);
    }

    // Boot 2: reopen against the SAME path (process restart
    // semantics). The row MUST still be there.
    let storage = DatabasePendingDeliveryStorage::open(Some(&url), QuotaPolicy::Unlimited)
        .await
        .expect("reopen file-backed storage");
    let rows = storage
        .list(&bare("alice@example.com"))
        .await
        .expect("list after restart");
    assert_eq!(
        rows.len(),
        1,
        "row durably persisted across the process-restart boundary"
    );
    let body = match &rows[0].payload {
        PendingPayload::Transient(m) => m.bodies.get("").map(|b| b.as_str()),
        _ => None,
    };
    assert_eq!(body, Some("across-restart"));
}

/// Regression: `original_receipt_at` stores `timestamp_millis()` (i64
/// ms-since-epoch). On Postgres, the column MUST be `BIGINT` — using
/// `INTEGER` (i32, max ~2.1B) overflowed on every write past
/// 2001-09-09, breaking XEP-0160 offline DM delivery on SM session
/// resume / detach promotion (production logs spammed
/// `numeric_value_out_of_range`).
///
/// CI runs only against SQLite (where `INTEGER` is dynamic-width), so
/// the bug slipped past the existing SQLite-backed coverage. This test
/// opts in to a real Postgres via `WADDLE_TEST_POSTGRES_URL` and proves
/// the round-trip with a 2026-era millisecond value that does not fit
/// in i32.
#[tokio::test]
async fn db_storage_postgres_handles_i32_overflow_receipt_ms() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (postgres-backed regression for pending_delivery BIGINT)"
        );
        return;
    };

    let storage = DatabasePendingDeliveryStorage::open(Some(&database_url), QuotaPolicy::Unlimited)
        .await
        .expect("open postgres storage");

    // 2026-era ms timestamp — comfortably past i32::MAX (~2.147B). A
    // pre-fix `INTEGER` column would reject this with
    // `22003 numeric_value_out_of_range`.
    let receipt_ms: i64 = 1_778_000_000_000;
    assert!(
        receipt_ms > i64::from(i32::MAX),
        "test value must exceed i32::MAX to exercise the regression"
    );
    let receipt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(receipt_ms)
        .expect("valid timestamp");

    // Unique recipient per run so concurrent test runs against the
    // same Postgres do not collide on the archived-id unique index.
    let recipient_local = format!("alice-{}", uuid::Uuid::new_v4());
    let recipient = bare(&format!("{recipient_local}@example.com"));

    let row_id = PendingRowId::fresh();
    let row = PendingRow {
        id: row_id.clone(),
        recipient: recipient.clone(),
        original_receipt_at: receipt,
        payload: PendingPayload::Archived(StanzaId::new(
            uuid::Uuid::new_v4().to_string(),
            jid::Jid::from(recipient.clone()),
        )),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    assert_eq!(
        storage.insert(row).await.expect("insert"),
        InsertOutcome::Inserted,
        "BIGINT column must accept i64 ms past i32::MAX"
    );

    let rows = storage.list(&recipient).await.expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].original_receipt_at, receipt);

    // Clean up by the exact row id we just inserted so repeated runs
    // are idempotent without touching unrelated rows. `delete_older_than`
    // is a global cutoff across all recipients — using it for test
    // cleanup against a `WADDLE_TEST_POSTGRES_URL` pointed at a shared
    // dev/CI database could wipe production-shaped test fixtures from
    // other suites.
    let deleted = storage.delete_row(&row_id).await.expect("cleanup");
    assert_eq!(deleted, 1, "test row must be deleted by id");
}

// ── ADR-0017 Phase 3 Slice 5 FIX 3 (council-adjudicated): fenced Q6
// promotion insert — duplicate-promotion (double-janitor) prevention ──
//
// Element 9's locked text: "promotion executes under the row-locked
// fenced epoch." This proves `insert_fenced`'s wiring end-to-end against
// real Postgres: two nodes attempting to promote the SAME SM session's
// unacked queue under different claim states — one holds a now-stale
// (deposed) claim, the other holds the current one — must have exactly
// one succeed; the deposed node's attempt aborts fenced
// (`PendingStorageError::NotOwner`) before writing anything.
#[cfg(feature = "clustering")]
struct RotateIncarnationAfterEnsureClaimStore {
    inner: crate::clustering::claims::PostgresClaimStore,
    db: crate::db::Database,
    replacement: waddle_xmpp::ownership::NodeIdentity,
    rotate_once: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "clustering")]
#[async_trait::async_trait]
impl waddle_xmpp::ownership::ClaimStore for RotateIncarnationAfterEnsureClaimStore {
    async fn ensure_schema(&self) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.ensure_schema().await
    }

    async fn acquire(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner.acquire(entity, me).await
    }

    async fn ensure_claimed(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        let epoch = self.inner.ensure_claimed(entity, me).await?;
        if self
            .rotate_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.db
                .guard()
                .await
                .map_err(|error| waddle_xmpp::ownership::ClaimError::Backend(error.to_string()))?
                .execute(
                    "UPDATE clustering_claims SET node_epoch = ? WHERE entity = ?",
                    crate::db_params![
                        self.replacement.node_epoch.clone(),
                        format!("{}:{}", entity.entity_type.as_db_str(), entity.id),
                    ],
                )
                .await
                .map_err(|error| waddle_xmpp::ownership::ClaimError::Backend(error.to_string()))?;
        }
        Ok(epoch)
    }

    async fn steal_stale(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        observed: waddle_xmpp::ownership::ClaimEpoch,
        staleness: waddle_xmpp::ownership::StalePredicate,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner
            .steal_stale(entity, observed, staleness, me)
            .await
    }

    async fn steal_for_resume(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        observed: waddle_xmpp::ownership::ClaimEpoch,
        witness: waddle_xmpp::ownership::ResumeIdentityProof,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner
            .steal_for_resume(entity, observed, witness, me)
            .await
    }

    async fn current_claim(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
    ) -> Result<Option<waddle_xmpp::ownership::ClaimSnapshot>, waddle_xmpp::ownership::ClaimError>
    {
        self.inner.current_claim(entity).await
    }

    async fn fence(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
        mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<bool, waddle_xmpp::ownership::ClaimError> {
        self.inner.fence(entity, me, mine).await
    }

    async fn release(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
        mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.release(entity, me, mine).await
    }

    async fn release_many(
        &self,
        entities: &[waddle_xmpp::ownership::Entity],
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.release_many(entities, me).await
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn insert_fenced_rejects_same_node_id_new_incarnation_at_the_same_claim_epoch() {
    use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
    use crate::db::{Database, DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE};
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
    };

    let _guard = clustering_control_plane_table_lock().lock().await;
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        return;
    };
    let db = Database::from_config(
        "pending-delivery-incarnation-fence-test",
        &DatabaseConfig::new(DatabaseDriver::Postgres, database_url.clone())
            .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
    )
    .await
    .expect("open test postgres");
    let old = NodeIdentity::new(uuid::Uuid::new_v4().to_string(), "old-incarnation");
    let replacement = NodeIdentity::new(old.node_id.clone(), "new-incarnation");
    let stream_id = format!("stream-incarnation-{}", uuid::Uuid::new_v4());
    let entity = Entity::new(EntityType::SmSession, stream_id.clone());
    let schema_store = PostgresClaimStore::new(db.clone());
    schema_store.ensure_schema().await.expect("claim schema");
    schema_store
        .acquire(&entity, &old)
        .await
        .expect("old claim");
    let rotating_store: std::sync::Arc<dyn ClaimStore> =
        std::sync::Arc::new(RotateIncarnationAfterEnsureClaimStore {
            inner: PostgresClaimStore::new(db.clone()),
            db: db.clone(),
            replacement,
            rotate_once: std::sync::atomic::AtomicBool::new(true),
        });
    let storage = crate::pending_delivery::open_for_cluster_mode(
        Some(&database_url),
        QuotaPolicy::Unlimited,
        true,
        Some((rotating_store, SharedNodeIdentity::new(old))),
        &db,
    )
    .await
    .expect("open fenced storage");
    let recipient_text = format!("incarnation-{}@example.com", uuid::Uuid::new_v4());
    let recipient = bare(&recipient_text);

    let outcome = storage
        .insert_fenced(transient_row(&recipient_text, "must not land"), &stream_id)
        .await;
    assert!(matches!(outcome, Err(PendingStorageError::NotOwner { .. })));
    assert!(storage.list(&recipient).await.expect("list").is_empty());
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn insert_fenced_prevents_duplicate_promotion_across_claim_states() {
    use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
    use crate::db::{Database, DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE};
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
    };

    let _guard = clustering_control_plane_table_lock().lock().await;
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (pending_delivery fenced-insert duplicate-promotion regression)"
        );
        return;
    };

    let db = Database::from_config(
        "pending-delivery-fenced-test",
        &DatabaseConfig::new(DatabaseDriver::Postgres, database_url.clone())
            .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
    )
    .await
    .expect("open test postgres");

    let schema_store = PostgresClaimStore::new(db.clone());
    schema_store
        .ensure_schema()
        .await
        .expect("ensure claims schema");

    let stream_id = format!("stream-dup-{}", uuid::Uuid::new_v4());
    let entity = Entity::new(EntityType::SmSession, stream_id.clone());
    let entity_key = format!("{}:{}", EntityType::SmSession.as_db_str(), stream_id);

    let node_a = NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    let node_b = NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );

    // Node A originally holds the claim — the ordinary "self-claimed
    // session" state a Q6 promotion runs under.
    schema_store
        .acquire(&entity, &node_a)
        .await
        .expect("node A acquires");

    // Simulate a concurrent double-janitor: another node's own
    // `steal_stale`/orphan-reaper sweep won this exact entity's claim out
    // from under node A between node A's own claim and this promotion
    // attempt (element 9's "any node may steal such claims" text) — bump
    // the row directly to node B's identity/epoch, the same observable
    // end state a real concurrent steal would leave, without depending on
    // real-time interleaving for a deterministic test.
    {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_claims SET node_id = ?, node_epoch = ?, \
             claim_epoch = claim_epoch + 1 WHERE entity = ?",
            crate::db_params![
                node_b.node_id.clone(),
                node_b.node_epoch.clone(),
                entity_key.clone(),
            ],
        )
        .await
        .expect("simulate concurrent steal to node B");
    }

    let recipient_str = format!("dup-promo-{}@example.com", uuid::Uuid::new_v4());
    let recipient = bare(&recipient_str);
    let row_for_a = transient_row(&recipient_str, "node A's promotion attempt");
    let row_for_b = transient_row(&recipient_str, "node B's promotion attempt");

    // Node A's storage: fenced against its own (now-stale/deposed) identity.
    let storage_a = crate::pending_delivery::open_for_cluster_mode(
        Some(&database_url),
        QuotaPolicy::Unlimited,
        true,
        Some((
            std::sync::Arc::new(PostgresClaimStore::new(db.clone()))
                as std::sync::Arc<dyn ClaimStore>,
            SharedNodeIdentity::new(node_a.clone()),
        )),
        &db,
    )
    .await
    .expect("open node A's fenced pending_delivery storage");

    // Node B's storage: fenced against the identity that actually won the
    // claim.
    let storage_b = crate::pending_delivery::open_for_cluster_mode(
        Some(&database_url),
        QuotaPolicy::Unlimited,
        true,
        Some((
            std::sync::Arc::new(PostgresClaimStore::new(db.clone()))
                as std::sync::Arc<dyn ClaimStore>,
            SharedNodeIdentity::new(node_b.clone()),
        )),
        &db,
    )
    .await
    .expect("open node B's fenced pending_delivery storage");

    // Node A's promotion aborts fenced — its own `ensure_claimed` observes
    // a genuinely different node/epoch now on the row and refuses before
    // any write.
    let outcome_a = storage_a.insert_fenced(row_for_a, &stream_id).await;
    assert!(
        matches!(outcome_a, Err(PendingStorageError::NotOwner { .. })),
        "the deposed node's promotion attempt must abort fenced (NotOwner), got {outcome_a:?}"
    );

    // Node B's promotion succeeds — it is the current, genuine owner.
    let outcome_b = storage_b
        .insert_fenced(row_for_b, &stream_id)
        .await
        .expect("node B's promotion succeeds");
    assert_eq!(outcome_b, InsertOutcome::Inserted);

    // Exactly one row landed — the deposed node's attempt never wrote
    // anything, proving the fence closed the double-promotion window, not
    // merely raced it.
    let rows = storage_b.list(&recipient).await.expect("list");
    assert_eq!(
        rows.len(),
        1,
        "exactly one promotion attempt's row must land"
    );
    let landed_body = match &rows[0].payload {
        PendingPayload::Transient(message) => message.bodies.values().next().cloned(),
        PendingPayload::Archived(_) => None,
    };
    assert_eq!(
        landed_body.as_deref(),
        Some("node B's promotion attempt"),
        "the landed row must be the current owner's, never the deposed node's"
    );

    // Cleanup — scoped to this test's own unique stream id/recipient, never
    // a global cutoff (see the i32-overflow test above for why).
    for row in rows {
        let _ = storage_b.delete_row(&row.id).await;
    }
    let conn = db.guard().await.expect("guard");
    let _ = conn
        .execute(
            "DELETE FROM clustering_claims WHERE entity = ?",
            crate::db_params![entity_key],
        )
        .await;
}

// ── Issue #1122: transient MAM failure vs genuine tombstone miss ────
//
// The archive resolver must distinguish a transient MAM storage
// error (release the row so the next flush retries) from a genuine
// miss / unparseable row (poison pill: delete). Collapsing both into
// "unresolved" meant a momentary MAM outage permanently destroyed
// queued offline mail.

use std::sync::atomic::{AtomicU32, Ordering};
use waddle_xmpp::mam::storage::{InMemoryMamStorage, MamStorage, MamStorageError, StoreOutcome};
use waddle_xmpp_core::mam::ArchivedMessage;

/// MAM storage wrapper that fails `get_message_by_archive_or_stanza_id`
/// with a configurable error (default: transient `Database`) for the
/// first `failures_remaining` lookups, then delegates to the inner
/// in-memory storage. Every other operation delegates unconditionally.
struct LookupOutageMamStorage {
    inner: InMemoryMamStorage,
    failures_remaining: AtomicU32,
    make_error: fn() -> MamStorageError,
}

impl LookupOutageMamStorage {
    fn new(inner: InMemoryMamStorage, failures: u32) -> Self {
        Self::with_error(inner, failures, || {
            MamStorageError::Database("simulated transient MAM outage".to_string())
        })
    }

    /// Like [`Self::new`] but failing lookups return the error produced
    /// by `make_error` (e.g. `Serialization` to simulate a corrupt
    /// archive row whose column decode fails on every attempt).
    fn with_error(
        inner: InMemoryMamStorage,
        failures: u32,
        make_error: fn() -> MamStorageError,
    ) -> Self {
        Self {
            inner,
            failures_remaining: AtomicU32::new(failures),
            make_error,
        }
    }
}

#[async_trait::async_trait]
impl MamStorage for LookupOutageMamStorage {
    async fn store_message(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<StoreOutcome, MamStorageError> {
        self.inner.store_message(archive_jid, message).await
    }
    async fn query_messages(
        &self,
        archive_jid: &BareJid,
        archive_kind: waddle_xmpp::mam::MamArchiveKind,
        query: &waddle_xmpp_core::mam::MamQuery,
    ) -> Result<waddle_xmpp_core::mam::MamResult, MamStorageError> {
        self.inner
            .query_messages(archive_jid, archive_kind, query)
            .await
    }
    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        self.inner.get_message(archive_id).await
    }
    async fn replace_with_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        self.inner
            .replace_with_tombstone(archive_id, tombstone)
            .await
    }
    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        self.inner
            .get_message_by_stanza_id(archive_jid, stanza_id)
            .await
    }
    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        self.inner
            .get_message_by_message_id(archive_jid, message_id)
            .await
    }
    async fn get_message_by_sender_and_origin_id(
        &self,
        archive_jid: &BareJid,
        archive_kind: waddle_xmpp::mam::MamArchiveKind,
        sender: &jid::Jid,
        origin_id: &waddle_xmpp_core::xep0359::OriginId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        self.inner
            .get_message_by_sender_and_origin_id(archive_jid, archive_kind, sender, origin_id)
            .await
    }
    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let remaining = self.failures_remaining.load(Ordering::Acquire);
        if remaining > 0 {
            self.failures_remaining
                .store(remaining - 1, Ordering::Release);
            return Err((self.make_error)());
        }
        self.inner
            .get_message_by_archive_or_stanza_id(archive_jid, stanza_id)
            .await
    }
    async fn count_messages(&self, room_jid: &BareJid) -> Result<u32, MamStorageError> {
        self.inner.count_messages(room_jid).await
    }
    async fn delete_before(
        &self,
        room_jid: &BareJid,
        before: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, MamStorageError> {
        self.inner.delete_before(room_jid, before).await
    }
}

/// Seed the given MAM storage with an archived copy of a chat message
/// addressed to `recipient`, retrievable by `archive_id` via
/// `get_message_by_archive_or_stanza_id`. `stanza_xml` overrides the
/// preserved wire XML (pass `None` to leave the column empty, or
/// garbage to simulate a corrupt row).
async fn seed_archived_message(
    mam: &dyn MamStorage,
    recipient: &str,
    archive_id: &str,
    stanza_xml: Option<String>,
) {
    let recipient_bare = bare(recipient);
    let mut archived = ArchivedMessage::for_test(
        "bob@elsewhere/x".parse::<jid::Jid>().expect("jid"),
        jid::Jid::from(recipient_bare.clone()),
    );
    archived.id = archive_id.to_string();
    archived.stanza_id = Some(StanzaId::new(
        archive_id,
        jid::Jid::from(recipient_bare.clone()),
    ));
    archived.stanza_xml = stanza_xml;
    mam.store_message(&recipient_bare, &archived)
        .await
        .expect("seed archived message");
}

fn valid_archived_stanza_xml(recipient: &str, body: &str) -> String {
    transient_message_xml(recipient, body)
}

#[tokio::test]
async fn flush_archived_row_transient_resolver_error_releases_row_for_retry() {
    // Issue #1122 core repro: MAM lookup errors (outage), the row
    // must be RELEASED for the next flush — not poison-pill-deleted.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(archived_row("alice@example.com", "archive-1"))
        .await
        .unwrap();

    // MAM storage that always fails the lookup.
    let mam: Arc<dyn MamStorage> = Arc::new(LookupOutageMamStorage::new(
        InMemoryMamStorage::new(),
        u32::MAX,
    ));
    let resolver = MamArchiveResolver { mam_storage: mam };

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let sm_session = SmSessionId::new("sm-stream-mam-outage");

    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &resolver,
        },
    )
    .await;

    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.pushed, 0);
    assert_eq!(
        outcome.unresolved, 0,
        "transient MAM error must NOT count as an unresolved poison pill"
    );
    assert_eq!(
        outcome.deferred_transient, 1,
        "transient MAM error must be counted as a deferred row"
    );
    assert!(rx.try_recv().is_err(), "nothing pushed during the outage");

    // The row survives AND its claim is released so the next flush
    // (or another recovering resource) can re-claim it.
    let rows = storage.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(rows.len(), 1, "row must not be deleted on transient error");
    assert!(
        rows[0].flushed_in_session.is_none(),
        "row must be released for re-claim on the next flush"
    );
}

#[tokio::test]
async fn flush_archived_row_genuine_mam_miss_is_poison_pill_deleted() {
    // Genuine tombstone miss (`Ok(None)` from MAM): the original
    // stanza is unrecoverable — keep the poison-pill delete so the
    // flush loop never wedges on the row.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(archived_row("alice@example.com", "archive-missing"))
        .await
        .unwrap();

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new()); // empty archive
    let resolver = MamArchiveResolver { mam_storage: mam };

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let sm_session = SmSessionId::new("sm-stream-mam-miss");

    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &resolver,
        },
    )
    .await;

    assert_eq!(outcome.unresolved, 1);
    assert_eq!(outcome.deferred_transient, 0);
    assert_eq!(
        storage.count(&bare("alice@example.com")).await.unwrap(),
        0,
        "genuine miss is a poison pill: row deleted"
    );
}

#[tokio::test]
async fn flush_archived_row_unparseable_stanza_xml_is_poison_pill() {
    // Corrupt archive rows (garbage / absent stanza_xml) are
    // unrecoverable — poison-pill delete, never retry-loop.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(archived_row("alice@example.com", "archive-garbage"))
        .await
        .unwrap();
    storage
        .insert(archived_row("alice@example.com", "archive-no-xml"))
        .await
        .unwrap();

    let mam_inner = InMemoryMamStorage::new();
    seed_archived_message(
        &mam_inner,
        "alice@example.com",
        "archive-garbage",
        Some("<<<definitely-not-xml".to_string()),
    )
    .await;
    seed_archived_message(&mam_inner, "alice@example.com", "archive-no-xml", None).await;
    let mam: Arc<dyn MamStorage> = Arc::new(mam_inner);
    let resolver = MamArchiveResolver { mam_storage: mam };

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let sm_session = SmSessionId::new("sm-stream-mam-corrupt");

    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &resolver,
        },
    )
    .await;

    assert_eq!(outcome.unresolved, 2);
    assert_eq!(outcome.deferred_transient, 0);
    assert_eq!(
        storage.count(&bare("alice@example.com")).await.unwrap(),
        0,
        "unparseable rows are poison pills: deleted, not retried"
    );
}

#[tokio::test]
async fn flush_brief_mam_outage_preserves_offline_message() {
    // End-to-end issue #1122 guarantee: a MAM outage during one flush
    // loses no mail — the next flush re-claims the released row,
    // resolves it, and delivers it via the normal SM-ack lifecycle.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(archived_row("alice@example.com", "archive-1"))
        .await
        .unwrap();

    let mam_inner = InMemoryMamStorage::new();
    seed_archived_message(
        &mam_inner,
        "alice@example.com",
        "archive-1",
        Some(valid_archived_stanza_xml(
            "alice@example.com",
            "survived-the-outage",
        )),
    )
    .await;
    // Fail exactly the first lookup, then recover.
    let mam: Arc<dyn MamStorage> = Arc::new(LookupOutageMamStorage::new(mam_inner, 1));
    let resolver = MamArchiveResolver { mam_storage: mam };

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let sm_session = SmSessionId::new("sm-stream-brief-outage");

    // Flush #1: outage — nothing delivered, nothing lost.
    let first = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &resolver,
        },
    )
    .await;
    assert_eq!(first.deferred_transient, 1);
    assert_eq!(first.pushed, 0);
    assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);

    // Flush #2: MAM is back — the released row is re-claimed and
    // delivered.
    let second = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &resolver,
        },
    )
    .await;
    assert_eq!(second.claimed, 1);
    assert_eq!(second.pushed, 1);
    assert_eq!(second.deferred_transient, 0);
    assert_eq!(second.unresolved, 0);

    let pushed = rx.try_recv().expect("replay stanza delivered");
    match &pushed.stanza {
        waddle_xmpp::Stanza::Message(m) => {
            assert_eq!(
                m.bodies.get("").map(|b| b.as_str()),
                Some("survived-the-outage")
            );
        }
        other => panic!("expected Message replay, got {other:?}"),
    }

    // Locked Q7b: after delivery the row stays claimed by the SM
    // session; only the normal SM-ack path removes it.
    let rows = storage.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].flushed_in_session.as_ref(), Some(&sm_session));
    storage
        .delete_acked_in_window(&sm_session, 0, u32::MAX)
        .await
        .unwrap();
}

// ── Adversarial review R1 (#1122 follow-up): transient MAM failure is
// batch-fatal. pending_delivery is FIFO (XEP-0160 §3 order of
// receipt): releasing a failing Archived row while delivering later
// rows would break delivery order, and a hard MAM outage would mean
// one failing lookup per archived row awaited inline in the presence
// handler. The first Err aborts the flush and releases everything
// still undelivered.

fn body_of(stanza: &waddle_xmpp::Stanza) -> String {
    match stanza {
        waddle_xmpp::Stanza::Message(m) => m
            .bodies
            .get("")
            .map(|b| b.as_str().to_string())
            .unwrap_or_default(),
        other => panic!("expected Message replay, got {other:?}"),
    }
}

#[tokio::test]
async fn flush_transient_error_aborts_batch_releases_remaining_rows_and_preserves_fifo() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(transient_row("alice@example.com", "first"))
        .await
        .unwrap();
    storage
        .insert(archived_row("alice@example.com", "archive-mid"))
        .await
        .unwrap();
    storage
        .insert(transient_row("alice@example.com", "third"))
        .await
        .unwrap();

    let mam_inner = InMemoryMamStorage::new();
    seed_archived_message(
        &mam_inner,
        "alice@example.com",
        "archive-mid",
        Some(valid_archived_stanza_xml("alice@example.com", "second")),
    )
    .await;
    // Fail exactly the first lookup (the "archive-mid" row), then recover.
    let mam: Arc<dyn MamStorage> = Arc::new(LookupOutageMamStorage::new(mam_inner, 1));
    let resolver = MamArchiveResolver { mam_storage: mam };

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let sm_session = SmSessionId::new("sm-stream-batch-abort");

    let first = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &resolver,
        },
    )
    .await;

    assert_eq!(first.claimed, 3);
    assert_eq!(
        first.pushed, 1,
        "row delivered before the failure stays delivered"
    );
    assert_eq!(
        first.unresolved, 0,
        "transient failure must never poison-pill"
    );
    assert_eq!(
        first.deferred_transient, 2,
        "failing row AND all later claimed rows are deferred"
    );
    assert_eq!(
        body_of(&rx.try_recv().expect("first row delivered").stanza),
        "first"
    );
    assert!(
        rx.try_recv().is_err(),
        "batch aborted: no row after the failure is delivered in this flush"
    );

    // Nothing deleted; the failing row and its successors are released
    // (re-claimable), the delivered row stays claimed for the SM ack.
    let rows = storage.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(rows.len(), 3, "no row deleted on transient failure");
    let released = rows
        .iter()
        .filter(|r| r.flushed_in_session.is_none())
        .count();
    assert_eq!(released, 2, "failing + subsequent rows released for retry");

    // Retry flush with MAM recovered: FIFO preserved — "second" is
    // delivered before "third".
    let second = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &resolver,
        },
    )
    .await;
    assert_eq!(second.claimed, 2);
    assert_eq!(second.pushed, 2);
    assert_eq!(second.deferred_transient, 0);
    assert_eq!(second.unresolved, 0);
    assert_eq!(
        body_of(
            &rx.try_recv()
                .expect("archived row delivered on retry")
                .stanza
        ),
        "second"
    );
    assert_eq!(
        body_of(&rx.try_recv().expect("later row delivered on retry").stanza),
        "third"
    );
}

// ── Adversarial review R3 (#1122 follow-up): decode corruption is
// permanent, not transient. The production lookup surfaces row-content
// corruption (decode_sqlite_message_row / decode_postgres_message_row
// on a bad timestamp/JID column) as MamStorageError::Serialization;
// retrying can never succeed, so it must take the loud poison-pill
// path — not the "no mail lost" transient counter.

#[tokio::test]
async fn flush_archived_row_serialization_error_is_poison_pill_not_transient() {
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(archived_row("alice@example.com", "archive-corrupt"))
        .await
        .unwrap();

    let mam: Arc<dyn MamStorage> = Arc::new(LookupOutageMamStorage::with_error(
        InMemoryMamStorage::new(),
        u32::MAX,
        || MamStorageError::Serialization("bad timestamp column".to_string()),
    ));
    let resolver = MamArchiveResolver { mam_storage: mam };

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let sm_session = SmSessionId::new("sm-stream-corrupt-decode");

    let outcome = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &resolver,
        },
    )
    .await;

    assert_eq!(outcome.unresolved, 1, "decode corruption is a poison pill");
    assert_eq!(
        outcome.deferred_transient, 0,
        "decode corruption must NOT count as transient"
    );
    assert!(rx.try_recv().is_err(), "nothing deliverable");
    assert_eq!(
        storage.count(&bare("alice@example.com")).await.unwrap(),
        0,
        "corrupt row deleted, never released-and-retried forever"
    );
}

#[tokio::test]
async fn archive_resolver_maps_invalid_query_to_permanent_miss() {
    // InvalidQuery, like Serialization, cannot succeed on retry — the
    // resolver reports a definitive miss (poison pill), not Err.
    let mam: Arc<dyn MamStorage> = Arc::new(LookupOutageMamStorage::with_error(
        InMemoryMamStorage::new(),
        u32::MAX,
        || MamStorageError::InvalidQuery("unusable stored id".to_string()),
    ));
    let resolver = MamArchiveResolver { mam_storage: mam };
    let recipient = bare("alice@example.com");
    let stanza_id = StanzaId::new("archive-bad-query", jid::Jid::from(recipient));
    let resolved = resolver
        .resolve(&stanza_id)
        .await
        .expect("permanent decode failure must be Ok(None), not a retryable Err");
    assert!(resolved.is_none());
}

// ── Adversarial review R2 (#1122 follow-up): deferred rows must be
// retryable within a live session. `claim_offline_flush()` is a
// once-per-connection CAS, so after a transient MAM blip the presence
// handler resets it (when `deferred_transient > 0`) and the client's
// next presence update re-attempts the flush.

#[tokio::test]
async fn reset_offline_flush_reopens_the_once_per_session_cas() {
    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let entry = registry.get_entry(&resource).expect("registered entry");

    assert!(entry.claim_offline_flush(), "first claim wins");
    assert!(!entry.claim_offline_flush(), "CAS: second claim loses");
    entry.reset_offline_flush();
    assert!(
        entry.claim_offline_flush(),
        "reset re-opens the CAS for the next presence update"
    );
    assert!(!entry.claim_offline_flush(), "and it is again once-only");
}

#[tokio::test]
async fn transient_deferral_plus_cas_reset_delivers_on_next_presence_flush() {
    // Mirrors `maybe_flush_pending_delivery`: claim the CAS, flush
    // (MAM outage → deferral), reset the CAS because
    // `deferred_transient > 0`, then the next presence update's claim
    // succeeds and the retry flush delivers.
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    storage
        .insert(archived_row("alice@example.com", "archive-1"))
        .await
        .unwrap();

    let mam_inner = InMemoryMamStorage::new();
    seed_archived_message(
        &mam_inner,
        "alice@example.com",
        "archive-1",
        Some(valid_archived_stanza_xml(
            "alice@example.com",
            "after-the-blip",
        )),
    )
    .await;
    // Fail exactly the first lookup, then recover.
    let mam: Arc<dyn MamStorage> = Arc::new(LookupOutageMamStorage::new(mam_inner, 1));
    let resolver = MamArchiveResolver { mam_storage: mam };

    let registry = ConnectionRegistry::new();
    let resource = full("alice@example.com/web");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), tx);
    let entry = registry.get_entry(&resource).expect("registered entry");
    let sm_session = SmSessionId::new("sm-stream-cas-reset");

    assert!(
        entry.claim_offline_flush(),
        "fresh session claims the flush"
    );
    let first = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &resolver,
        },
    )
    .await;
    assert_eq!(first.deferred_transient, 1);
    assert_eq!(first.pushed, 0);

    // The presence handler's contract: deferral re-opens the CAS.
    entry.reset_offline_flush();
    assert!(
        entry.claim_offline_flush(),
        "next presence update re-claims after a deferral"
    );

    let second = flush_for_resource(
        &storage,
        &registry,
        &bare("alice@example.com"),
        &resource,
        FlushContext {
            server_domain: "example.com",
            sm_session: Some(&sm_session),
            blocking_storage: None,
            owner: None,
            archive_resolver: &resolver,
        },
    )
    .await;
    assert_eq!(second.pushed, 1);
    assert_eq!(second.deferred_transient, 0);
    assert_eq!(
        body_of(&rx.try_recv().expect("delivered on retry").stanza),
        "after-the-blip"
    );
}

fn transient_dm_row_with_id(recipient: &str, wire_id: &str, body: &str) -> PendingRow {
    let mut m = Message::new(Some(recipient.parse::<jid::Jid>().expect("jid")));
    m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().expect("jid"));
    m.id = Some(xmpp_parsers::message::Id(wire_id.to_string()));
    m.type_ = MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang(String::new()), body.to_string());
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: bare(recipient),
        original_receipt_at: Utc::now(),
        payload: PendingPayload::Transient(Box::new(m)),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

#[tokio::test]
async fn database_scrub_for_tombstone_removes_transient_and_archived_matches() {
    // F2: a XEP-0424/0425 retraction must scrub promoted pending rows
    // in the SQL backend too — a Transient row would otherwise deliver
    // the retracted content verbatim on the recipient's next login,
    // and an Archived pointer would flush a tombstone stub.
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("open");
    storage
        .insert(transient_dm_row_with_id(
            "alice@example.com",
            "retract-me",
            "secret",
        ))
        .await
        .expect("insert");
    storage
        .insert(transient_dm_row_with_id(
            "alice@example.com",
            "keep-me",
            "safe",
        ))
        .await
        .expect("insert");
    // Same wire id in another conversation: scope guard keeps it.
    storage
        .insert(transient_dm_row_with_id(
            "carol@example.com",
            "retract-me",
            "unrelated",
        ))
        .await
        .expect("insert");
    storage
        .insert(archived_row("alice@example.com", "retract-me"))
        .await
        .expect("insert");
    storage
        .insert(archived_row("alice@example.com", "other-archive"))
        .await
        .expect("insert");

    let removed = storage
        .scrub_for_tombstone(&waddle_xmpp::tombstone::TombstoneTarget::Direct {
            wire_id: "retract-me".to_string(),
            author: bare("bob@elsewhere"),
            archive: bare("alice@example.com"),
        })
        .await
        .expect("scrub");
    assert_eq!(
        removed, 2,
        "one transient + one archived in-scope match removed"
    );

    // Next-login flush proxy: the claim (what a fresh session flushes)
    // must no longer surface the scrubbed rows.
    let claimed = storage
        .claim_for_session(&bare("alice@example.com"), &SmSessionId::new("s-next"))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 2);
    for row in &claimed {
        match &row.payload {
            PendingPayload::Transient(m) => {
                assert_eq!(m.id.as_ref().map(|id| id.0.as_str()), Some("keep-me"));
            }
            PendingPayload::Archived(r) => assert_eq!(r.id.as_str(), "other-archive"),
        }
    }
    // The out-of-scope conversation is untouched.
    assert_eq!(
        storage
            .list(&bare("carol@example.com"))
            .await
            .expect("list")
            .len(),
        1
    );
}
