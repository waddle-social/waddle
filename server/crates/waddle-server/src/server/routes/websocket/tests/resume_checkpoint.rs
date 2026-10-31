use std::sync::Arc;
use std::time::Duration;

use super::{
    create_test_session, create_test_websocket_state_with_extension_manager,
    empty_extension_manager, handle_xmpp_frame, store_resumable_detached_session,
    TestStateOverrides, WebSocketState, WsConnState,
};
use crate::db::{Database, MigrationRunner};
use crate::ingress::IngressAuthority;
use crate::server::routes::websocket::transport_xml::{element_to_xml, stanza_to_xml};
use jid::FullJid;
use minidom::Element;
use waddle_xmpp::protocol::ConnectionPhase;
use waddle_xmpp::stream_management::{
    DetachedSession, DetachedUnackedStanza, PromotionCaptureCustody, SmSessionRegistry, SM_NS,
};
use waddle_xmpp::Stanza;

const STREAM: &str = "checkpoint-acquisition-resume";

async fn fixture() -> (
    tempfile::TempDir,
    Database,
    Arc<WebSocketState>,
    WsConnState,
) {
    let directory = tempfile::tempdir().expect("database directory");
    // Keep checkpoint contention separate from authentication and registry I/O,
    // so the held writer can only stall this resume at checkpoint acquisition.
    let database = Database::open_local("resume-checkpoint", directory.path().join("ingress.db"))
        .await
        .expect("checkpoint database");
    MigrationRunner::global()
        .run(&database)
        .await
        .expect("migrate");
    let ingress = Arc::new(IngressAuthority::for_test(database.clone()).await);
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            ingress: Some(ingress),
            ..Default::default()
        },
    )
    .await;
    let session = create_test_session(&state, "alice").await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let mut message = xmpp_parsers::message::Message::new(Some(jid.clone().into()));
    message.id = Some(xmpp_parsers::message::Id(
        "checkpoint-replay-must-wait".to_owned(),
    ));
    store_resumable_detached_session(
        &state,
        &session,
        DetachedSession {
            promotion_custody: PromotionCaptureCustody::never_claimed(),
            stream_id: STREAM.to_owned(),
            user_id: session.user_jid.clone(),
            jid: jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 1,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![DetachedUnackedStanza {
                ingress_receipts: Vec::new(),
                sequence: 1,
                stanza_xml: stanza_to_xml(&Stanza::Message(message)),
                original_receipt_at: chrono::Utc::now(),
            }],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    (directory, database, state, conn)
}

fn resume_xml() -> String {
    element_to_xml(
        Element::builder("resume", SM_NS)
            .attr(minidom::rxml::xml_ncname!("previd").to_owned(), STREAM)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
            .build(),
    )
}

async fn assert_unresumed_and_reclaimable(state: &WebSocketState, conn: &WsConnState) {
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert!(!conn.sm_state.enabled);
    assert_eq!(conn.sm_state.queue_len(), 0);
    assert!(conn.pending_resume_claim.is_none());
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    assert!(state
        .deps
        .protocol
        .connection_registry
        .entry(&jid)
        .is_none());
    let snapshot = state
        .deps
        .protocol
        .sm_session_registry
        .claim_session(STREAM)
        .await
        .expect("claim released snapshot")
        .expect("snapshot retained");
    assert_eq!(snapshot.unacked_stanzas.len(), 1, "replay remains queued");
}

#[tokio::test]
async fn checkpoint_acquisition_timeout_releases_resume_claim_without_replay() {
    let (_directory, database, state, mut conn) = fixture().await;
    let writer = database
        .begin_immediate()
        .await
        .expect("hold checkpoint writer");
    let wire = resume_xml();
    let responses = {
        let resume = handle_xmpp_frame(&wire, "example.com", &state, &mut conn);
        tokio::pin!(resume);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut resume)
                .await
                .is_err()
        );
        assert!(
            state
                .deps
                .protocol
                .sm_session_registry
                .peek_session(STREAM)
                .await
                .expect("peek claimed snapshot")
                .is_none(),
            "resume holds its claim"
        );
        tokio::time::timeout(Duration::from_secs(2), &mut resume)
            .await
            .expect("bounded checkpoint failure")
    };
    assert_eq!(
        responses.len(),
        1,
        "only the failed response, without resumed or replay"
    );
    let failed: Element = responses[0].parse().expect("failed XML");
    assert!(failed.is("failed", SM_NS));
    assert!(failed
        .get_child(
            "internal-server-error",
            "urn:ietf:params:xml:ns:xmpp-stanzas"
        )
        .is_some());
    assert_unresumed_and_reclaimable(&state, &conn).await;
    writer.commit().await.expect("release writer");
}

#[tokio::test]
async fn cancelling_checkpoint_acquisition_releases_resume_claim_without_replay() {
    let (_directory, database, state, mut conn) = fixture().await;
    let writer = database
        .begin_immediate()
        .await
        .expect("hold checkpoint writer");
    let wire = resume_xml();
    {
        let resume = handle_xmpp_frame(&wire, "example.com", &state, &mut conn);
        tokio::pin!(resume);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut resume)
                .await
                .is_err()
        );
        assert!(
            state
                .deps
                .protocol
                .sm_session_registry
                .peek_session(STREAM)
                .await
                .expect("peek claimed snapshot")
                .is_none(),
            "resume holds its claim"
        );
        // Drop while restore_ingress_checkpoint is waiting for BEGIN IMMEDIATE.
        // The handler returns frames only after completion, so no response or
        // replay has escaped this cancelled future.
    }
    assert_unresumed_and_reclaimable(&state, &conn).await;
    writer.commit().await.expect("release writer");
    assert!(
        state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(1))
            .await
    );
}
