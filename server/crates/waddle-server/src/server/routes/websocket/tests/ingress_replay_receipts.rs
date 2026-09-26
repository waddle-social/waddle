//! A committed response batch that dies in the transport keeps its ingress
//! receipt obligations on the retained XEP-0198 replay entry, and discharges
//! them only once the replay reaches the wire (RFC 0018 §3: receipts follow
//! confirmed effects, and a canonical row terminalizes when they are complete).
use super::*;
use crate::server::routes::websocket::tests::{
    create_test_session, create_test_websocket_state, create_test_websocket_state_with_db_pool,
};
use std::sync::Arc;
use waddle_xmpp::pending_delivery::SmSessionId;

async fn connection(state: &WebSocketState) -> WsConnState {
    let mut conn = WsConnState::new();
    let jid: jid::FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(create_test_session(state, "alice").await);
    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        jid,
        false,
        Default::default(),
    );
    let id = SmSessionId::new(uuid::Uuid::new_v4().to_string());
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim(id.as_str())
            .await
            .expect("SM claim"),
    );
    state
        .deps
        .protocol
        .ingress
        .enroll_stream(&id)
        .await
        .expect("enroll");
    conn.sm_ingress_fence = state
        .deps
        .protocol
        .sm_session_registry
        .current_sm_claim_fence(id.as_str());
    conn.sm_state
        .enable(id.as_str().to_owned(), true, Some(300));
    conn
}

/// Offered stanza whose committed disposition is a semantic rejection: exactly
/// one response frame, one receipt obligation, no recipient-side writes.
fn offered_message() -> String {
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("target")));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .payloads
        .push(minidom::Element::builder("result", waddle_xmpp::xep::NS_INBOX).build());
    super::super::transport_xml::stanza_to_xml(&Stanza::Message(message))
}

/// `(receipt rows, terminalized canonical rows)` for the single committed row.
async fn receipt_state(state: &WebSocketState) -> (i64, i64) {
    let db = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database");
    let mut rows = db
        .query(
            "SELECT (SELECT COUNT(*) FROM ingress_effect_receipts), \
             (SELECT COUNT(*) FROM ingress_messages WHERE terminal_at IS NOT NULL), \
             (SELECT COUNT(*) FROM ingress_messages)",
            (),
        )
        .await
        .expect("receipt state");
    let row = rows.next().await.expect("row").expect("counts");
    assert_eq!(row.get::<i64>(2).expect("canonical rows"), 1);
    (
        row.get(0).expect("receipts"),
        row.get(1).expect("terminal rows"),
    )
}

async fn wait_for_receipts(state: &WebSocketState) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while receipt_state(state).await != (1, 1) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("retained receipt proofs must settle after storage recovery");
}

async fn replayed_frame_receipt_case(state: Arc<WebSocketState>) {
    let mut conn = connection(&state).await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut responses = super::super::frame::handle_xmpp_frame_with_admission(
        &offered_message(),
        "example.com",
        &state,
        &mut conn,
        &permit,
        &shutdown,
    )
    .await;
    assert_eq!(responses.frames.len(), 1);
    assert_eq!(responses.ingress_reports.len(), 1);
    assert_eq!(
        responses.frames[0].ingress_receipts().len(),
        1,
        "the batch's last frame must carry the receipt obligation"
    );

    let mut broken = Box::pin(futures::sink::unfold((), |(), _: Message| async {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }));
    let mut reader = futures::stream::pending::<Result<Message, std::io::Error>>();
    let report = write_ingress_response_batch_with_admission(
        &mut broken,
        &mut reader,
        &state,
        &mut conn,
        &mut responses,
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;
    assert!(matches!(report.outcome, BatchWriteOutcome::TransportClosed));
    assert_eq!(
        receipt_state(&state).await,
        (0, 0),
        "an unwritten frame proves nothing"
    );
    drop(responses);

    let retained = conn.sm_state.get_stanzas_to_resend(0);
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0].ingress_receipts.len(),
        1,
        "the replay entry must carry the dropped report's obligation"
    );
    let replay: Vec<ResponseFrame> = retained
        .into_iter()
        .map(|entry| {
            ResponseFrame::from_serialized_xml(entry.stanza_xml)
                .with_ingress_receipts(entry.ingress_receipts)
        })
        .collect();

    let mut socket = Box::pin(futures::sink::unfold((), |(), _: Message| async {
        Ok::<(), std::io::Error>(())
    }));
    let outcome = write_response_batch_with_admission(
        &mut socket,
        &mut reader,
        &state,
        &mut conn,
        replay,
        BatchSmPolicy::ReplaySuppressed,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;
    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    wait_for_receipts(&state).await;
}

async fn postgres_state() -> Option<(Arc<WebSocketState>, sqlx::PgPool, String)> {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        return None;
    };
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("postgres");
    let schema = format!("ingress_replay_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("schema");
    let mut url = url::Url::parse(&database_url).expect("URL");
    url.query_pairs_mut()
        .append_pair("options", &format!("-c search_path={schema}"));
    let pool = Arc::new(
        crate::db::DatabasePool::new(
            crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, url.to_string()),
            crate::db::PoolConfig,
        )
        .await
        .expect("pool"),
    );
    Some((
        create_test_websocket_state_with_db_pool(pool).await,
        admin,
        schema,
    ))
}

#[tokio::test]
async fn ingress_replayed_frame_completes_receipts_sqlite() {
    replayed_frame_receipt_case(create_test_websocket_state().await).await;
}

#[tokio::test]
async fn ingress_replayed_frame_completes_receipts_postgres() {
    let Some((state, admin, schema)) = postgres_state().await else {
        return;
    };
    replayed_frame_receipt_case(state).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
    admin.close().await;
}

#[derive(Clone, Copy)]
enum ReceiptBoundary {
    LaterWriteFailure,
    Ack,
    AckPersistenceFailure,
    ResumeStorageFailure,
    Resume,
}

async fn receipt_boundary_case(state: Arc<WebSocketState>, boundary: ReceiptBoundary) {
    let mut conn = connection(&state).await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut responses = super::super::frame::handle_xmpp_frame_with_admission(
        &offered_message(),
        "example.com",
        &state,
        &mut conn,
        &permit,
        &shutdown,
    )
    .await;
    let mut reader = futures::stream::pending::<Result<Message, std::io::Error>>();
    let successful_writes = usize::from(matches!(boundary, ReceiptBoundary::LaterWriteFailure));
    if matches!(boundary, ReceiptBoundary::LaterWriteFailure) {
        responses.frames.push(ResponseFrame::from(
            super::super::transport_xml::websocket_stream_close_element(),
        ));
    }
    let mut socket = Box::pin(futures::sink::unfold(
        0usize,
        move |count, _: Message| async move {
            if count < successful_writes {
                Ok(count + 1)
            } else {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            }
        },
    ));
    let report = write_ingress_response_batch_with_admission(
        &mut socket,
        &mut reader,
        &state,
        &mut conn,
        &mut responses,
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;
    assert!(matches!(report.outcome, BatchWriteOutcome::TransportClosed));
    match boundary {
        ReceiptBoundary::LaterWriteFailure => {
            assert_eq!(report.written_frame_count, 1);
        }
        ReceiptBoundary::Ack => {
            assert_eq!(receipt_state(&state).await, (0, 0));
            // A peer acknowledgement is authoritative proof even when the
            // original writer never recorded completion locally.
            let frames = super::super::stream_management::apply_sm_ack(
                &state,
                &mut conn.sm_state,
                &mut conn.phase,
                1,
            )
            .await;
            assert!(frames.is_empty());
            assert_eq!(conn.sm_state.queue_len(), 0);
        }
        ReceiptBoundary::AckPersistenceFailure => {
            let db = state
                .deps
                .app_state
                .db_pool
                .global()
                .guard()
                .await
                .expect("database");
            db.execute(
                "ALTER TABLE ingress_effect_receipts RENAME TO held_receipts",
                (),
            )
            .await
            .expect("hide receipt storage");
            drop(db);
            let frames = super::super::stream_management::apply_sm_ack(
                &state,
                &mut conn.sm_state,
                &mut conn.phase,
                1,
            )
            .await;
            assert!(frames.is_empty());
            assert_eq!(conn.sm_state.last_acked, 1);
            assert_eq!(conn.sm_state.queue_len(), 0);
            assert!(!matches!(conn.phase, ConnectionPhase::Closing { .. }));
            let db = state
                .deps
                .app_state
                .db_pool
                .global()
                .guard()
                .await
                .expect("database");
            db.execute(
                "ALTER TABLE held_receipts RENAME TO ingress_effect_receipts",
                (),
            )
            .await
            .expect("restore receipt storage");
            drop(db);
            // A fresh connection has no replay carrier or repeated ACK. A write
            // tick on the shared authority still settles the retained proof.
            conn.sm_state = Default::default();
            assert!(
                super::super::batch_write::complete_replayed_ingress_receipts(&state, &[]).await
            );
            assert_eq!(conn.sm_state.queue_len(), 0);
        }
        ReceiptBoundary::ResumeStorageFailure => {
            resume_storage_failure(&state, &mut conn).await;
        }
        ReceiptBoundary::Resume => {
            assert_eq!(receipt_state(&state).await, (0, 0));
            // Exercise the admission gate shared by resume.h and live ACK.
            assert!(
                super::super::stream_management::complete_acknowledged_ingress_receipts(
                    &state,
                    &conn.sm_state,
                    1,
                )
                .await
            );
            conn.sm_state.acknowledge(1);
            assert_eq!(conn.sm_state.queue_len(), 0);
        }
    }
    wait_for_receipts(&state).await;
}

#[tokio::test]
async fn ingress_written_carrier_survives_later_batch_failure_sqlite() {
    receipt_boundary_case(
        create_test_websocket_state().await,
        ReceiptBoundary::LaterWriteFailure,
    )
    .await;
}

#[tokio::test]
async fn ingress_ack_completes_carrier_before_removal_sqlite() {
    receipt_boundary_case(create_test_websocket_state().await, ReceiptBoundary::Ack).await;
}

#[tokio::test]
async fn ingress_resume_h_completes_carrier_before_removal_sqlite() {
    receipt_boundary_case(create_test_websocket_state().await, ReceiptBoundary::Resume).await;
}

async fn postgres_receipt_boundary(boundary: ReceiptBoundary) {
    let Some((state, admin, schema)) = postgres_state().await else {
        return;
    };
    receipt_boundary_case(state, boundary).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
    admin.close().await;
}

#[tokio::test]
async fn ingress_written_carrier_survives_later_batch_failure_postgres() {
    postgres_receipt_boundary(ReceiptBoundary::LaterWriteFailure).await;
}

#[tokio::test]
async fn ingress_ack_completes_carrier_before_removal_postgres() {
    postgres_receipt_boundary(ReceiptBoundary::Ack).await;
}

#[tokio::test]
async fn ingress_resume_h_completes_carrier_before_removal_postgres() {
    postgres_receipt_boundary(ReceiptBoundary::Resume).await;
}

#[tokio::test]
async fn ingress_ack_receipt_failure_advances_and_retries_sqlite() {
    receipt_boundary_case(
        create_test_websocket_state().await,
        ReceiptBoundary::AckPersistenceFailure,
    )
    .await;
}

#[tokio::test]
async fn ingress_ack_receipt_failure_advances_and_retries_postgres() {
    postgres_receipt_boundary(ReceiptBoundary::AckPersistenceFailure).await;
}

async fn resume_storage_failure(state: &WebSocketState, conn: &mut WsConnState) {
    use waddle_xmpp::stream_management::DetachedSessionSnapshot;
    let session = conn.authenticated_session.clone().expect("session");
    let jid = conn.phase.bound_jid().cloned().expect("bound jid");
    let detached = conn
        .sm_state
        .to_detached_session(DetachedSessionSnapshot {
            user_id: session.user_jid.clone(),
            jid: jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
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
        .expect("detached");
    let stream_id = detached.stream_id.clone();
    crate::server::routes::websocket::tests::store_resumable_detached_session(
        state, &session, detached,
    )
    .await;
    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&jid);
    let resume = minidom::Element::builder("resume", waddle_xmpp::stream_management::SM_NS)
        .attr(minidom::rxml::xml_ncname!("previd").to_owned(), stream_id)
        .attr(minidom::rxml::xml_ncname!("h").to_owned(), "1")
        .build();
    let xml = super::super::transport_xml::element_to_xml(resume);
    let db = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database");
    db.execute(
        "ALTER TABLE ingress_effect_receipts RENAME TO held_receipts",
        (),
    )
    .await
    .expect("hide receipts");
    drop(db);
    let response =
        super::super::frame::handle_xmpp_frame(&xml, "example.com", state, &mut resumed).await;
    let success: minidom::Element = response[0].parse().expect("resumed XML");
    assert_eq!(
        success.name(),
        "resumed",
        "retained proofs authorize resume despite receipt outage"
    );
    assert!(resumed.sm_state.enabled);
    assert_eq!(resumed.sm_state.queue_len(), 0);
    let db = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database");
    db.execute(
        "ALTER TABLE held_receipts RENAME TO ingress_effect_receipts",
        (),
    )
    .await
    .expect("restore receipts");
    drop(db);
    wait_for_receipts(state).await;
}

#[tokio::test]
async fn ingress_resume_receipt_failure_retains_proof_sqlite() {
    receipt_boundary_case(
        create_test_websocket_state().await,
        ReceiptBoundary::ResumeStorageFailure,
    )
    .await;
}

#[tokio::test]
async fn ingress_resume_receipt_failure_retains_proof_postgres() {
    postgres_receipt_boundary(ReceiptBoundary::ResumeStorageFailure).await;
}

async fn ack_deletion_failure_case(mut state: Arc<WebSocketState>) {
    use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId, QuotaPolicy};
    let pending = Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
            state.deps.app_state.db_pool.global().clone(),
            QuotaPolicy::Unlimited,
        )
        .await
        .expect("shared pending store"),
    );
    Arc::get_mut(&mut state)
        .expect("unshared test state")
        .deps
        .protocol
        .pending_delivery_storage = pending.clone();
    let mut conn = connection(&state).await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut responses = super::super::frame::handle_xmpp_frame_with_admission(
        &offered_message(),
        "example.com",
        &state,
        &mut conn,
        &permit,
        &shutdown,
    )
    .await;
    let mut broken = Box::pin(futures::sink::unfold((), |(), _: Message| async {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }));
    let mut reader = futures::stream::pending::<Result<Message, std::io::Error>>();
    let report = write_ingress_response_batch_with_admission(
        &mut broken,
        &mut reader,
        &state,
        &mut conn,
        &mut responses,
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;
    assert!(matches!(report.outcome, BatchWriteOutcome::TransportClosed));
    let stanza = Stanza::Message(xmpp_parsers::message::Message::new(None));
    for _ in 0..2 {
        let _ = conn.sm_state.record_outbound(
            super::super::transport_xml::stanza_to_xml(&stanza),
            waddle_xmpp::telemetry::attributes::SmEvictionPath::Batch,
        );
    }
    let recipient: jid::BareJid = "alice@example.com".parse().expect("recipient");
    let old_session = SmSessionId::new(conn.sm_state.stream_id.clone().expect("stream"));
    let mut row_ids = Vec::new();
    for _ in 0..4 {
        let id = PendingRowId::fresh();
        pending
            .insert(PendingRow {
                id: id.clone(),
                recipient: recipient.clone(),
                original_receipt_at: chrono::Utc::now(),
                payload: PendingPayload::Transient(Box::new(xmpp_parsers::message::Message::new(
                    None,
                ))),
                flushed_in_session: None,
                outbound_sequence: None,
            })
            .await
            .expect("pending row");
        row_ids.push(id);
    }
    pending
        .claim_for_session(&recipient, &old_session)
        .await
        .expect("claim rows");
    for (index, id) in row_ids.iter().take(3).enumerate() {
        pending
            .record_pushed_at(id, u32::try_from(index + 1).expect("sequence"))
            .await
            .expect("stamp row");
    }
    let db = pending.database();
    let guard = db.guard().await.expect("database");
    guard
        .execute(
            "ALTER TABLE ingress_effect_receipts RENAME TO held_receipts",
            (),
        )
        .await
        .expect("receipt outage");
    guard
        .execute(
            "ALTER TABLE pending_delivery RENAME TO held_pending_delivery",
            (),
        )
        .await
        .expect("pending outage");
    drop(guard);
    for h in [2, 2] {
        assert!(super::super::stream_management::apply_sm_ack(
            &state,
            &mut conn.sm_state,
            &mut conn.phase,
            h
        )
        .await
        .is_empty());
        assert_eq!(conn.sm_state.last_acked, 2);
        assert_eq!(conn.sm_state.queue_len(), 1);
    }
    // Persistent receipt failure must not serialize an unrelated socket's ACK
    // or output behind the authority retry worker's database attempts.
    let mut unrelated = connection(&state).await;
    tokio::time::timeout(std::time::Duration::from_millis(500), async {
        assert!(super::super::stream_management::apply_sm_ack(
            &state,
            &mut unrelated.sm_state,
            &mut unrelated.phase,
            0
        )
        .await
        .is_empty());
        let mut socket = Box::pin(futures::sink::unfold((), |(), _: Message| async {
            Ok::<(), std::io::Error>(())
        }));
        let outcome = write_response_batch_with_admission(
            &mut socket,
            &mut reader,
            &state,
            &mut unrelated,
            vec![ResponseFrame::from(
                super::super::transport_xml::websocket_stream_close_element(),
            )],
            BatchSmPolicy::Record,
            BatchAuthority {
                permit: &permit,
                shutdown: &shutdown,
            },
        )
        .await;
        assert!(matches!(outcome, BatchWriteOutcome::Continue));
    })
    .await
    .expect("unrelated ACK and batch must not await receipt persistence");
    drop(conn);
    // Fresh-session cleanup uses the shared store after the old connection and
    // its ACK floor have gone. It must retain ownership throughout the outage.
    assert!(pending.release_claim(&old_session).await.is_err());
    let guard = db.guard().await.expect("database");
    guard
        .execute(
            "ALTER TABLE held_pending_delivery RENAME TO pending_delivery",
            (),
        )
        .await
        .expect("recover pending store");
    guard
        .execute(
            "ALTER TABLE held_receipts RENAME TO ingress_effect_receipts",
            (),
        )
        .await
        .expect("recover receipts");
    drop(guard);
    assert_eq!(
        pending
            .release_claim(&old_session)
            .await
            .expect("fresh-session cleanup"),
        2
    );
    let remaining = pending
        .list(&recipient)
        .await
        .expect("remaining pending rows");
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().all(|row| row_ids[2..].contains(&row.id)
        && row.flushed_in_session.is_none()
        && row.outbound_sequence.is_none()));
    assert_eq!(
        pending
            .claim_for_session(&recipient, &SmSessionId::new("fresh-session"))
            .await
            .expect("fresh delivery")
            .len(),
        2
    );
    wait_for_receipts(&state).await;
}

#[tokio::test]
async fn ingress_ack_deletion_failure_gates_fresh_cleanup_sqlite() {
    ack_deletion_failure_case(create_test_websocket_state().await).await;
}

#[tokio::test]
async fn ingress_ack_deletion_failure_gates_fresh_cleanup_postgres() {
    let Some((state, admin, schema)) = postgres_state().await else {
        return;
    };
    ack_deletion_failure_case(state).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
    admin.close().await;
}

#[tokio::test]
async fn ingress_post_registration_resume_settles_replayed_receipt() {
    use super::super::connection::{
        handle_inbound_text, ConnectionIo, FrameAuthority, RegistrationChannels,
    };
    use waddle_xmpp::stream_management::DetachedSessionSnapshot;
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state).await;
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut responses = super::super::frame::handle_xmpp_frame_with_admission(
        &offered_message(),
        "example.com",
        &state,
        &mut conn,
        &permit,
        &shutdown,
    )
    .await;
    assert_eq!(responses.frames.len(), 1);
    assert_eq!(responses.ingress_reports.len(), 1);
    assert_eq!(
        responses.frames[0].ingress_receipts().len(),
        1,
        "the batch's last frame must carry the receipt obligation"
    );

    let mut broken = Box::pin(futures::sink::unfold((), |(), _: Message| async {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }));
    let mut reader = futures::stream::pending::<Result<Message, std::io::Error>>();
    let report = write_ingress_response_batch_with_admission(
        &mut broken,
        &mut reader,
        &state,
        &mut conn,
        &mut responses,
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;
    assert!(matches!(report.outcome, BatchWriteOutcome::TransportClosed));
    assert_eq!(
        receipt_state(&state).await,
        (0, 0),
        "an unwritten frame proves nothing"
    );
    drop(responses);

    let retained = conn.sm_state.get_stanzas_to_resend(0);
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0].ingress_receipts.len(),
        1,
        "the replay entry must carry the dropped report's obligation"
    );
    let session = conn.authenticated_session.clone().expect("session");
    let jid = conn.phase.bound_jid().cloned().expect("bound jid");
    let detached = conn
        .sm_state
        .to_detached_session(DetachedSessionSnapshot {
            user_id: session.user_jid.clone(),
            jid: jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
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
        .expect("detached");
    assert_eq!(detached.unacked_stanzas.len(), 1);
    assert_eq!(detached.unacked_stanzas[0].ingress_receipts.len(), 1);
    let stream_id = detached.stream_id.clone();
    crate::server::routes::websocket::tests::store_resumable_detached_session(
        &state, &session, detached,
    )
    .await;

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&jid);
    resumed.authenticated_session = Some(session);
    let resume = minidom::Element::builder("resume", waddle_xmpp::stream_management::SM_NS)
        .attr(
            minidom::rxml::xml_ncname!("previd").to_owned(),
            stream_id.as_str(),
        )
        .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
        .build();
    let xml = super::super::transport_xml::element_to_xml(resume);
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut socket = Box::pin(futures::sink::unfold(
        captured.clone(),
        |captured, frame: Message| async move {
            captured.lock().expect("captured frames").push(frame);
            Ok::<_, std::io::Error>(captured)
        },
    ));
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let mut pending_tx = Some(tx);
    let mut force_detach_rx = None;
    assert!(
        handle_inbound_text(
            &xml,
            "example.com",
            &state,
            &mut resumed,
            RegistrationChannels {
                pending_tx: &mut pending_tx,
                force_detach_rx: &mut force_detach_rx
            },
            ConnectionIo {
                sender: &mut socket,
                receiver: &mut reader
            },
            FrameAuthority {
                permit: &permit,
                shutdown: &shutdown
            },
        )
        .await
    );
    assert!(
        pending_tx.is_none(),
        "production handler must register the resumed connection"
    );
    assert!(resumed.pending_resume_stream_id.is_none());
    assert!(resumed.registry_owner.is_some());
    wait_for_receipts(&state).await;
    assert_eq!(
        receipt_state(&state).await,
        (1, 1),
        "replay reaching the wire must settle the receipt and terminalize its canonical row"
    );
    let frames = captured.lock().expect("captured frames");
    let elements: Vec<minidom::Element> = frames
        .iter()
        .map(|frame| {
            let Message::Text(xml) = frame else {
                panic!("expected XML text frame")
            };
            xml.parse().expect("wire XML")
        })
        .collect();
    assert_eq!(elements.len(), 2, "resumed followed by the retained stanza");
    assert!(elements[0].is("resumed", waddle_xmpp::stream_management::SM_NS));
    assert!(elements[1].is("message", waddle_xmpp::ns::JABBER_CLIENT));
}
