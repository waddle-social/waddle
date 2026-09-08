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
    assert_eq!(
        receipt_state(&state).await,
        (1, 1),
        "the replayed frame receipts its obligation and terminalizes the row"
    );
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
            assert_eq!(conn.sm_state.last_acked, 0);
            assert_eq!(conn.sm_state.queue_len(), 1);
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
            super::super::stream_management::apply_sm_ack(
                &state,
                &mut conn.sm_state,
                &mut conn.phase,
                1,
            )
            .await;
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
    assert_eq!(receipt_state(&state).await, (1, 1));
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
async fn ingress_ack_receipt_failure_retains_carrier_sqlite() {
    receipt_boundary_case(
        create_test_websocket_state().await,
        ReceiptBoundary::AckPersistenceFailure,
    )
    .await;
}

#[tokio::test]
async fn ingress_ack_receipt_failure_retains_carrier_postgres() {
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
    let failed: minidom::Element = response[0].parse().expect("failed XML");
    assert_eq!(failed.name(), "failed");
    assert!(failed
        .get_child("internal-server-error", xmpp_parsers::ns::XMPP_STANZAS)
        .is_some());
    assert!(!resumed.sm_state.enabled);
    assert_eq!(resumed.sm_state.queue_len(), 0);
    assert!(resumed.sm_ingress_fence.is_none());
    assert!(resumed.pending_resume_claim.is_none());
    assert!(matches!(
        resumed.phase,
        ConnectionPhase::Authenticated { .. }
    ));
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
    let response =
        super::super::frame::handle_xmpp_frame(&xml, "example.com", state, &mut resumed).await;
    let success: minidom::Element = response[0].parse().expect("resumed XML");
    assert_eq!(
        success.name(),
        "resumed",
        "released claim remains retryable"
    );
    assert!(resumed.sm_state.enabled);
    assert_eq!(resumed.sm_state.queue_len(), 0);
}

#[tokio::test]
async fn ingress_resume_receipt_failure_preserves_staged_state_sqlite() {
    receipt_boundary_case(
        create_test_websocket_state().await,
        ReceiptBoundary::ResumeStorageFailure,
    )
    .await;
}

#[tokio::test]
async fn ingress_resume_receipt_failure_preserves_staged_state_postgres() {
    postgres_receipt_boundary(ReceiptBoundary::ResumeStorageFailure).await;
}
