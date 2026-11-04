use super::super::super::cleanup::POST_DETACH_STORE_GATE;
use super::super::super::connection::{
    finish_connection, ConnectionShutdownChannels, FrameAuthority,
};
use super::super::super::send::InterruptibleSink;
use super::{
    create_test_session, create_test_websocket_state, mpsc, publish_stream_id_and_presence,
    register_bound_connection_after_frame, Arc, ConnectionPhase, FullJid, OutboundStanza,
    RegistrationAfterFrame, WsConnState,
};
use axum::extract::ws::Message;
use futures::Sink;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use tokio::sync::{oneshot, Notify};
use tokio::time::{timeout, Duration};
use waddle_xmpp::registry::{
    ForceDetachOrigin, ForceDetachOutcome, ForceDetachRequest, LocalSocketIo, SocketCleanupState,
};

#[derive(Default)]
struct SocketObservation {
    events: Vec<&'static str>,
    frames: Vec<Message>,
}

struct ObservedSink {
    observation: Arc<Mutex<SocketObservation>>,
    closed: Arc<Notify>,
}

impl Sink<Message> for ObservedSink {
    type Error = &'static str;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, frame: Message) -> Result<(), Self::Error> {
        let mut observation = self.observation.lock().expect("observation lock");
        observation.events.push("frame");
        observation.frames.push(frame);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.observation
            .lock()
            .expect("observation lock")
            .events
            .push("close");
        self.closed.notify_one();
        Poll::Ready(Ok(()))
    }
}

impl Drop for ObservedSink {
    fn drop(&mut self) {
        self.observation
            .lock()
            .expect("observation lock")
            .events
            .push("sender dropped");
    }
}

struct ObservedReceiver(Arc<Mutex<SocketObservation>>);

impl Drop for ObservedReceiver {
    fn drop(&mut self) {
        self.0
            .lock()
            .expect("observation lock")
            .events
            .push("receiver dropped");
    }
}

#[tokio::test]
async fn socket_is_relinquished_after_writes_and_settlement_before_cleanup_ack() {
    assert_shutdown_order(false, RequestTiming::Initially).await;
}

#[tokio::test]
async fn cancelling_blocked_cleanup_keeps_relinquishment_distinct_from_completion() {
    assert_shutdown_order(true, RequestTiming::Initially).await;
}

#[tokio::test]
async fn force_detach_during_settlement_waits_for_lifecycle_and_is_inconclusive() {
    assert_shutdown_order(false, RequestTiming::DuringSettlement).await;
}

#[tokio::test]
async fn force_detach_during_ordinary_cleanup_waits_for_lifecycle_and_is_inconclusive() {
    assert_shutdown_order(false, RequestTiming::DuringCleanup).await;
}

#[tokio::test]
async fn cancelling_ordinary_cleanup_closes_its_force_detach_receiver() {
    assert_shutdown_order(true, RequestTiming::DuringCleanup).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestTiming {
    Initially,
    DuringSettlement,
    DuringCleanup,
}

async fn assert_shutdown_order(cancel_cleanup: bool, timing: RequestTiming) {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/shutdown-order".parse().expect("JID");
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(create_test_session(&state, "alice").await);
    conn.begin_server_stream_open_response();
    conn.commit_server_stream_open_response();
    let observation = Arc::new(Mutex::new(SocketObservation::default()));
    let closed = Arc::new(Notify::new());
    let sink = InterruptibleSink::new(
        ObservedSink {
            observation: observation.clone(),
            closed: closed.clone(),
        },
        conn.write_interruption.clone(),
    );
    let (socket_io, witness) = LocalSocketIo::new(sink, ObservedReceiver(observation.clone()));
    conn.local_io_relinquishment = Some(witness.clone());
    let (tx, mut outbound_rx) = mpsc::channel::<OutboundStanza>(4);
    let mut pending_tx = Some(tx);
    let registered = register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;
    assert!(matches!(registered, RegistrationAfterFrame::Registered(_)));
    let lifecycle = conn.socket_lifecycle.clone().expect("socket lifecycle");
    let entry = lifecycle.entry.clone();
    let stream_id = "shutdown-order-stream";
    conn.sm_state.enable(stream_id.to_owned(), true, Some(300));
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim(stream_id)
            .await
            .expect("stream claim"),
    );
    conn.sm_ingress_fence = state
        .deps
        .protocol
        .sm_session_registry
        .current_sm_claim_fence(stream_id);
    assert!(conn.sm_ingress_fence.is_some());
    state
        .deps
        .protocol
        .ingress
        .enroll_stream(&waddle_xmpp::pending_delivery::SmSessionId::new(stream_id))
        .await
        .expect("enroll stream");
    publish_stream_id_and_presence(
        &state,
        &jid,
        conn.registry_owner.as_ref().expect("binding owner"),
        &conn,
    );

    // A force detach can arrive after a live write was interrupted or while
    // shutdown is waiting for an already-admitted relay handoff to settle.
    conn.write_interruption.cancel();
    let force_tx = entry.force_detach_sender();
    let mut force_detach_rx = Some(entry.take_force_detach_rx().expect("control receiver"));
    let (ack, mut ack_rx) = oneshot::channel();
    let mut request = Some(ForceDetachRequest {
        origin: ForceDetachOrigin::CrossNodeResume,
        target_binding: None,
        requester_bare_jid: jid.to_bare(),
        ack,
    });
    if timing == RequestTiming::Initially {
        force_tx
            .send(request.take().expect("initial force detach"))
            .await
            .expect("queue force detach");
    }
    let mut pending_force_detach = Vec::new();
    let (handoff_tx, mut handoff_rx) = mpsc::unbounded_channel();
    if timing == RequestTiming::DuringSettlement {
        let ping = xmpp_parsers::iq::Iq::from_get("parked-ping", xmpp_parsers::ping::Ping);
        conn.deferred_inbound.push_back(
            waddle_xmpp::parser::stanza_to_string(ping)
                .expect("serialize parked ping")
                .into(),
        );
    }
    let inbound_sequence = conn.sm_inbound_completion.reserve(&conn.sm_state);
    let permit = state.deps.app_state.node_lifecycle.admit().expect("permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let task_state = state.clone();
    let mut shutdown_future = Box::pin(POST_DETACH_STORE_GATE.scope(
        (reached.clone(), release.clone()),
        async move {
            finish_connection(
                &task_state,
                &mut conn,
                socket_io,
                FrameAuthority {
                    permit: &permit,
                    shutdown: &shutdown,
                },
                false,
                ConnectionShutdownChannels {
                    outbound_rx: &mut outbound_rx,
                    handoff_rx: &mut handoff_rx,
                    force_detach_rx: &mut force_detach_rx,
                    pending_force_detach: &mut pending_force_detach,
                },
            )
            .await;
            conn
        },
    ));

    // The future reaches the outstanding handoff with its initial control
    // drain complete. Polling it here makes the later enqueue deterministic.
    assert!(futures::poll!(shutdown_future.as_mut()).is_pending());
    if timing == RequestTiming::DuringSettlement {
        assert!(observation
            .lock()
            .expect("observation lock")
            .events
            .is_empty());
        force_tx
            .send(request.take().expect("late force detach"))
            .await
            .expect("queue force detach during settlement");
        assert!(matches!(
            ack_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }
    let finish_handoff = || {
        handoff_tx
            .send(
                crate::server::routes::interpret::OrderedRelayHandoffCompletion {
                    inbound_sequence,
                    replies: Vec::new(),
                },
            )
            .expect("complete pending relay handoff");
    };
    if timing != RequestTiming::Initially {
        finish_handoff();
    }
    let mut task = tokio::spawn(shutdown_future);

    if timing == RequestTiming::Initially {
        timeout(Duration::from_secs(5), closed.notified())
            .await
            .expect("conflict close attempted");
        assert!(
            !witness.is_relinquished(),
            "relay settlement is still pending"
        );
        assert_eq!(lifecycle.state(), SocketCleanupState::Running);
        assert!(matches!(
            ack_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let observation = observation.lock().expect("observation lock");
        assert_eq!(&observation.events[..3], ["frame", "frame", "close"]);
        let Message::Text(conflict) = &observation.frames[0] else {
            panic!("expected conflict stream error");
        };
        assert!(conflict.contains("conflict"));
        let Message::Text(close) = &observation.frames[1] else {
            panic!("expected stream close");
        };
        assert!(close.contains("close"));
    }

    if timing == RequestTiming::Initially {
        finish_handoff();
    }
    tokio::select! {
        reached = timeout(Duration::from_secs(5), reached.notified()) => {
            reached.expect("cleanup reached post-store gate");
        }
        result = &mut task => {
            match result {
                Ok(conn) => panic!(
                    "cleanup completed before post-store gate: lifecycle={:?}, inbound_count={}",
                    lifecycle.state(),
                    conn.sm_state.inbound_count(),
                ),
                Err(error) => panic!("shutdown task failed before post-store gate: {error}"),
            }
        }
    }
    assert!(witness.is_relinquished());
    assert!(entry
        .local_io_relinquishment()
        .expect("published local witness")
        .is_relinquished());
    let expected_events: &[&str] = if timing == RequestTiming::Initially {
        &[
            "frame",
            "frame",
            "close",
            "sender dropped",
            "receiver dropped",
        ]
    } else {
        &["sender dropped", "receiver dropped"]
    };
    assert_eq!(
        observation.lock().expect("observation lock").events,
        expected_events
    );
    assert_eq!(lifecycle.state(), SocketCleanupState::Running);
    if timing == RequestTiming::DuringCleanup {
        force_tx
            .send(request.take().expect("cleanup force detach"))
            .await
            .expect("queue force detach while cleanup is blocked");
    }
    assert!(matches!(
        ack_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));

    if cancel_cleanup {
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert!(witness.is_relinquished());
        assert_eq!(lifecycle.state(), SocketCleanupState::Running);
        assert!(
            timeout(Duration::from_secs(5), ack_rx)
                .await
                .expect("acknowledgement sender dropped")
                .is_err(),
            "cancellation must not claim detach completion"
        );
        let (ack, _ack_rx) = oneshot::channel();
        assert!(
            force_tx
                .try_send(ForceDetachRequest {
                    origin: ForceDetachOrigin::CrossNodeResume,
                    target_binding: None,
                    requester_bare_jid: jid.to_bare(),
                    ack,
                })
                .is_err(),
            "cancelled cleanup must close the control receiver"
        );
    } else {
        release.notify_one();
        let conn = timeout(Duration::from_secs(5), task)
            .await
            .expect("cleanup finishes")
            .expect("shutdown task");
        assert_eq!(
            conn.sm_state.inbound_count(),
            if timing == RequestTiming::DuringSettlement {
                2
            } else {
                1
            }
        );
        assert_eq!(lifecycle.state(), SocketCleanupState::Detached);
        assert_eq!(
            timeout(Duration::from_secs(5), ack_rx)
                .await
                .expect("acknowledgement arrives")
                .expect("detach acknowledgement"),
            if timing == RequestTiming::Initially {
                ForceDetachOutcome::Detached
            } else {
                ForceDetachOutcome::NotPersisted
            }
        );
    }
}
