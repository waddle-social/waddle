//! Issue #1789: a frame queued on a registered socket carries its origin's ingress
//! obligation, and the production detach cleanup keys the XEP-0198 replay append
//! with it. Driven through `cleanup_connection_shutdown`, not the drain helper, so
//! both drains and the session store are the real ones.

use super::super::cleanup::{cleanup_connection_shutdown, ConnectionShutdownOutcome};
use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent, MessageKey};
use waddle_xmpp::stream_management::{
    SmIngressAppendKey, SmIngressReceiptKind, SmKeyedAppendOutcome, SmRelayedAppendObligation,
    SmSessionRegistry as _,
};

const STREAM: &str = "keyed-detach-drain";

struct DetachingSocket {
    fixture: IngressFixture,
    state: Arc<WebSocketState>,
    sm: Arc<InMemorySmSessionRegistry>,
    recipient: FullJid,
    conn: WsConnState,
    tx: mpsc::Sender<OutboundStanza>,
    rx: mpsc::Receiver<OutboundStanza>,
}

async fn detaching_socket(fixture: IngressFixture) -> DetachingSocket {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let pool = DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared database");
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(Arc::new(pool)),
            ingress: Some(Arc::new(fixture.authority().await)),
            sm_session_registry: Some(Arc::clone(&sm)),
            ..TestStateOverrides::default()
        },
    )
    .await;
    let recipient: FullJid = "juliet@example.com/web".parse().expect("recipient");
    let (tx, rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(recipient.clone(), tx.clone());
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(recipient.clone(), false);
    conn.authenticated_session = Some(create_test_session(state.as_ref(), "juliet").await);
    conn.registry_owner = Some(owner);
    conn.sm_state.enable(STREAM.to_string(), true, Some(300));
    state
        .deps
        .protocol
        .ingress
        .enroll_stream(&waddle_xmpp::pending_delivery::SmSessionId::new(STREAM))
        .await
        .expect("enabled stream ingress enrollment");
    drop(sm.ensure_session_claim(STREAM).await.expect("enable claim"));
    DetachingSocket {
        fixture,
        state,
        sm,
        recipient,
        conn,
        tx,
        rx,
    }
}

/// Commit a canonical direct route to the socket's resource, as the origin would.
async fn committed_obligation(socket: &DetachingSocket) -> (Stanza, SmRelayedAppendObligation) {
    let mut submission = socket.fixture.submission(None, "drained once");
    let intent = IngressEffectIntent::RouteDirect {
        recipient: socket.recipient.to_bare(),
        fanout: vec![socket.recipient.clone()],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    };
    let receipt = crate::ingress::receipt_key(&intent).expect("receipt");
    submission.plan.intents = vec![intent];
    let decision = commit_submission(&socket.fixture.uow, &submission, 1)
        .await
        .expect("canonical row");
    let mut message = submission.plan.sanitized_message.clone();
    message.to = Some(socket.recipient.clone().into());
    let obligation = SmRelayedAppendObligation {
        key: SmIngressAppendKey {
            message_key: decision.message_key.expect("canonical key"),
            kind: SmIngressReceiptKind::from_storage(receipt.kind.to_storage()),
            semantic_identity_hash: receipt.semantic_identity_hash,
            resource: socket.recipient.clone(),
        },
        sender_bare: submission.sender.to_bare(),
        received_at: None,
    };
    (Stanza::Message(message), obligation)
}

async fn detach(socket: &mut DetachingSocket) -> waddle_xmpp::stream_management::DetachedSession {
    assert_eq!(
        cleanup_connection_shutdown(
            socket.state.as_ref(),
            &mut socket.rx,
            &mut socket.conn,
            false
        )
        .await,
        ConnectionShutdownOutcome::Detached
    );
    socket
        .sm
        .peek_session(STREAM)
        .await
        .expect("registry lookup")
        .expect("resumable snapshot")
}

/// The issue's failure path: the socket node accepted the frame, the origin lost its
/// receipt, the socket detached unwritten, and recovery re-executes the obligation.
async fn recovery_after_drain_finds_the_proof(fixture: IngressFixture) {
    let mut socket = detaching_socket(fixture).await;
    let (stanza, obligation) = committed_obligation(&socket).await;
    socket
        .tx
        .send(OutboundStanza::new(stanza.clone()).with_ingress_append(obligation.clone()))
        .await
        .expect("socket node queue acceptance");

    let detached = detach(&mut socket).await;
    assert_eq!(detached.unacked_stanzas.len(), 1);
    assert_eq!(socket.fixture.count("sm_ingress_appends").await, 1);

    let retried = socket
        .sm
        .record_keyed_stanza_for_detached_bound_resource(
            &socket.recipient,
            &stanza,
            chrono::Utc::now(),
            obligation.key,
        )
        .await
        .expect("recovery re-execution");
    assert!(matches!(
        retried,
        SmKeyedAppendOutcome::AlreadyAppended { .. }
    ));
    let after = socket.sm.peek_session(STREAM).await.unwrap().unwrap();
    assert_eq!(after.unacked_stanzas.len(), 1, "exactly one queue entry");
    assert_eq!(socket.fixture.count("sm_ingress_appends").await, 1);
}

/// Recovery re-executed while the first frame was still queued: both sit in the
/// socket's queue at detach. The second is dropped uncounted (XEP-0198 §5: nothing
/// on the drain path reached a wire, so `h` can never include it).
async fn duplicate_frames_in_one_queue_drain_once(fixture: IngressFixture) {
    let mut socket = detaching_socket(fixture).await;
    let (stanza, obligation) = committed_obligation(&socket).await;
    for _ in 0..2 {
        socket
            .tx
            .send(OutboundStanza::new(stanza.clone()).with_ingress_append(obligation.clone()))
            .await
            .expect("socket node queue acceptance");
    }

    let detached = detach(&mut socket).await;
    assert_eq!(detached.unacked_stanzas.len(), 1);
    assert_eq!(
        detached.outbound_count, 1,
        "the duplicate was never counted"
    );
    assert_eq!(socket.fixture.count("sm_ingress_appends").await, 1);
}

/// Decision recorded on #1789: an obligation the socket node cannot authorize loses
/// its dedupe key, never its delivery. The origin was already told `Delivered`.
async fn unauthorized_obligation_drains_unkeyed(fixture: IngressFixture) {
    let mut socket = detaching_socket(fixture).await;
    let (stanza, mut obligation) = committed_obligation(&socket).await;
    obligation.key.message_key = MessageKey::new();
    socket
        .tx
        .send(OutboundStanza::new(stanza).with_ingress_append(obligation))
        .await
        .expect("socket node queue acceptance");

    let detached = detach(&mut socket).await;
    assert_eq!(detached.unacked_stanzas.len(), 1, "delivery is preserved");
    assert_eq!(socket.fixture.count("sm_ingress_appends").await, 0);
}

/// The second detach drain: a frame lands after the detached session is stored but
/// before the registry unregisters the socket. It is keyed at the connection's own
/// sequence, in one write with its proof, and a repeat is dropped uncounted.
async fn late_frames_are_keyed_into_the_detached_stream(fixture: IngressFixture) {
    use super::super::replay::{drain_outbound_into_replay, PendingRowDrainPolicy};

    let mut socket = detaching_socket(fixture).await;
    let (stanza, obligation) = committed_obligation(&socket).await;
    detach(&mut socket).await;

    for _ in 0..2 {
        let (late_tx, mut late_rx) = mpsc::channel::<OutboundStanza>(1);
        late_tx
            .send(OutboundStanza::new(stanza.clone()).with_ingress_append(obligation.clone()))
            .await
            .expect("late frame");
        drain_outbound_into_replay(
            socket.state.as_ref(),
            None,
            &mut socket.conn.sm_state,
            None,
            &mut late_rx,
            super::super::replay::ReplayDrainSink {
                detached_stream_id: Some(STREAM),
                pending_row_policy: PendingRowDrainPolicy::PreserveForReplay,
                drained_appends: &mut Vec::new(),
            },
        )
        .await;
    }

    let detached = socket.sm.peek_session(STREAM).await.unwrap().unwrap();
    assert_eq!(detached.unacked_stanzas.len(), 1);
    assert_eq!(detached.outbound_count, 1);
    assert_eq!(
        socket.conn.sm_state.outbound_count, 1,
        "the connection-local counter stays aligned with the detached stream"
    );
    assert_eq!(socket.fixture.count("sm_ingress_appends").await, 1);
}

macro_rules! paired {
    ($case:ident, $sqlite:ident, $postgres:ident) => {
        #[tokio::test]
        async fn $sqlite() {
            $case(IngressFixture::sqlite().await).await;
        }

        #[tokio::test]
        async fn $postgres() {
            if let Some(fixture) = IngressFixture::postgres(stringify!($case)).await {
                $case(fixture).await;
            }
        }
    };
}

paired!(
    recovery_after_drain_finds_the_proof,
    sqlite_recovery_after_drain_finds_the_proof,
    postgres_recovery_after_drain_finds_the_proof
);
paired!(
    duplicate_frames_in_one_queue_drain_once,
    sqlite_duplicate_frames_in_one_queue_drain_once,
    postgres_duplicate_frames_in_one_queue_drain_once
);
paired!(
    unauthorized_obligation_drains_unkeyed,
    sqlite_unauthorized_obligation_drains_unkeyed,
    postgres_unauthorized_obligation_drains_unkeyed
);
paired!(
    late_frames_are_keyed_into_the_detached_stream,
    sqlite_late_frames_are_keyed_into_the_detached_stream,
    postgres_late_frames_are_keyed_into_the_detached_stream
);
