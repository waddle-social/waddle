//! Dormant ownership core for the eventual WebSocket connection actor.
//!
//! The active `/ws` loop still owns its own state. This core consumes an
//! existing state without exposing it and serializes stop with operations that
//! hold the lifecycle gate. The actor fixture exercises this same core.

use super::connection_actor_driver_task::ReservedSmRequest;
use super::frame::{
    handle_classified_sm_request_with_admission, handle_classified_xmpp_frame_with_admission,
    ordered_relay_origin_from_sm, ClassifiedInboundFrame, ResponseBatch,
};
use super::interpret_loop::build_interpret_deps;
use super::replay::{drive_interpret_loop, DriveOutcome};
use super::state::WsConnState;
use super::supervised_io::StopReason;
use super::{DeliveryKind, InboundEvent, OutboundStanza, WebSocketState};
use crate::clustering::NodeAdmissionPermit;
#[cfg(feature = "test-support")]
use std::collections::HashSet;
use std::future::Future;
#[cfg(feature = "test-support")]
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard};
#[cfg(feature = "test-support")]
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::ConnectionRef;

#[cfg(feature = "test-support")]
#[path = "connection_actor.rs"]
pub mod fixture;

/// The unique protocol-state owner and its independently shared lifecycle gate.
pub(super) struct ConnectionActorCore {
    state: WsConnState,
    pub(super) lifecycle: Arc<ConnectionActorLifecycle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnectionActorDriverCompletionUnknownReason {
    Stopped,
    AuthorityRevoked,
}

/// An operation result whose side effects must remain represented even if
/// authority or actor lifecycle changes while the handler is awaited.
///
/// `CurrentAtCompletion` is only a snapshot taken after the handler returns;
/// a future transport adapter must revalidate authority before emitting it.
/// `CompletionUnknown` retains the returned value so callers cannot mistake a
/// late stop for a pre-handler rejection. Dropping the driver future while it
/// is awaiting the handler still drops that handler future, so this type does
/// not establish cancellation-safe custody or make the driver safe to activate.
#[derive(Debug)]
pub(super) enum ConnectionActorDriverCompletion<T> {
    CurrentAtCompletion(T),
    CompletionUnknown {
        retained: T,
        reason: ConnectionActorDriverCompletionUnknownReason,
    },
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ConnectionActorDriverError {
    #[error("connection actor is stopping")]
    Stopped,
    #[cfg_attr(
        all(feature = "test-support", not(test)),
        expect(
            dead_code,
            reason = "the generic recipient seam awaits a bounded producer"
        )
    )]
    #[error("connection state machine is not initialized")]
    StateMachineUnavailable,
    #[cfg_attr(
        all(feature = "test-support", not(test)),
        expect(
            dead_code,
            reason = "the generic recipient seam awaits a bounded producer"
        )
    )]
    #[error("recipient interpretation requires a peer stanza")]
    NotPeerStanza,
    #[error("connection admission authority was revoked")]
    AuthorityRevoked,
    #[error("SM request lacks its output reservation")]
    OutputReservationMissing,
}

impl ConnectionActorCore {
    pub(super) fn new(state: WsConnState) -> Self {
        Self {
            state,
            lifecycle: Arc::new(ConnectionActorLifecycle::new()),
        }
    }

    pub(super) fn connection_ref(&self) -> &ConnectionRef {
        &self.state.connection_ref
    }

    #[cfg(feature = "test-support")]
    pub(super) fn complete_fixture_handled(
        &mut self,
        sequence: crate::server::routes::interpret::OrderedRelayInboundSequence,
    ) {
        self.state
            .sm_inbound_completion
            .complete(sequence, &mut self.state.sm_state);
    }

    /// Stop admitting new driver work after the current handler returns.
    pub(super) fn request_stop(&self, reason: StopReason) -> bool {
        self.lifecycle.request_stop(reason)
    }

    /// Delegate one XMPP frame to the same typed protocol/frame handler used
    /// by the legacy WebSocket loop. The actor keeps sole mutable ownership of
    /// `WsConnState`; the returned batch is still only local output work.
    #[cfg_attr(
        all(feature = "test-support", not(test)),
        expect(dead_code, reason = "generic inbound work awaits bounded producers")
    )]
    pub(super) async fn process_inbound_frame(
        &mut self,
        input: ClassifiedInboundFrame,
        domain: &jid::BareJid,
        state: &WebSocketState,
        permit: &NodeAdmissionPermit,
        shutdown: &CancellationToken,
    ) -> Result<ConnectionActorDriverCompletion<ResponseBatch>, ConnectionActorDriverError> {
        let lifecycle = Arc::clone(&self.lifecycle);
        run_with_driver_completion(lifecycle, permit, shutdown, || async {
            handle_classified_xmpp_frame_with_admission(
                input,
                domain,
                state,
                &mut self.state,
                permit,
                shutdown,
            )
            .await
        })
        .await
    }

    /// Run the bounded SM request while retaining sole ownership of state.
    pub(super) async fn process_sm_request(
        &mut self,
        _request: ReservedSmRequest<'_>,
        state: &WebSocketState,
        permit: &NodeAdmissionPermit,
        shutdown: &CancellationToken,
        #[cfg(test)] after_checkpoint: Option<&super::frame::SmCheckpointTestGate>,
    ) -> Result<
        ConnectionActorDriverCompletion<Option<waddle_xmpp::stream_management::SmAck>>,
        ConnectionActorDriverError,
    > {
        let lifecycle = Arc::clone(&self.lifecycle);
        run_with_driver_completion(lifecycle, permit, shutdown, || async {
            handle_classified_sm_request_with_admission(
                state,
                &mut self.state,
                permit,
                shutdown,
                #[cfg(test)]
                after_checkpoint,
            )
            .await
        })
        .await
    }

    #[cfg(test)]
    pub(super) fn advance_handled_for_test(&mut self) {
        self.state.sm_state.increment_inbound();
    }

    /// Run one retained peer envelope through the existing state-machine
    /// recipient pass. Borrowing the envelope leaves its exact custody record
    /// intact; the caller still owns writing, SM acceptance, and terminal
    /// settlement for the returned driver outcome.
    #[cfg_attr(
        all(feature = "test-support", not(test)),
        expect(dead_code, reason = "generic recipient work awaits a bounded producer")
    )]
    pub(super) async fn interpret_peer_stanza(
        &mut self,
        state: &WebSocketState,
        outbound: &OutboundStanza,
        permit: &NodeAdmissionPermit,
        shutdown: &CancellationToken,
    ) -> Result<ConnectionActorDriverCompletion<DriveOutcome>, ConnectionActorDriverError> {
        if outbound.kind != DeliveryKind::PeerStanza {
            return Err(ConnectionActorDriverError::NotPeerStanza);
        }
        let ordered_relay_origin =
            ordered_relay_origin_from_sm(&self.state.sm_state, self.state.phase.bound_jid());
        let session = self.state.authenticated_session.clone();
        let principal = session
            .as_ref()
            .map(super::ResolvedPrincipal::from_authenticated_session);
        let sm = self
            .state
            .state_machine
            .as_mut()
            .ok_or(ConnectionActorDriverError::StateMachineUnavailable)?;
        let lifecycle = Arc::clone(&self.lifecycle);
        let stanza = outbound.stanza.clone();
        run_with_driver_completion(lifecycle, permit, shutdown, || async move {
            let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(stanza)));
            let deps = build_interpret_deps(state, principal)
                .with_ordered_relay_origin(ordered_relay_origin);
            drive_interpret_loop(events, sm, &deps).await
        })
        .await
    }
}

/// Run one effectful handler and preserve its returned value across a late
/// stop or authority revocation. Cancellation of this future before the
/// handler returns is not captured; activation requires an actor-owned task
/// or another retained completion mechanism.
async fn run_with_driver_completion<T, F, Fut>(
    lifecycle: Arc<ConnectionActorLifecycle>,
    permit: &NodeAdmissionPermit,
    shutdown: &CancellationToken,
    operation: F,
) -> Result<ConnectionActorDriverCompletion<T>, ConnectionActorDriverError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    if shutdown.is_cancelled() || permit.revalidate().is_err() {
        return Err(ConnectionActorDriverError::AuthorityRevoked);
    }
    drop(
        lifecycle
            .lock_running()
            .map_err(|_| ConnectionActorDriverError::Stopped)?,
    );

    let retained = operation().await;
    let reason = if shutdown.is_cancelled() || permit.revalidate().is_err() {
        Some(ConnectionActorDriverCompletionUnknownReason::AuthorityRevoked)
    } else if lifecycle.lock_running().is_err() {
        Some(ConnectionActorDriverCompletionUnknownReason::Stopped)
    } else {
        None
    };

    Ok(match reason {
        Some(reason) => ConnectionActorDriverCompletion::CompletionUnknown { retained, reason },
        None => ConnectionActorDriverCompletion::CurrentAtCompletion(retained),
    })
}

pub(super) struct ConnectionActorLifecycle {
    state: Mutex<ConnectionActorLifecycleState>,
    #[cfg(feature = "test-support")]
    stop_callback_count: AtomicU64,
    #[cfg(feature = "test-support")]
    stop_callback_watch: watch::Sender<u64>,
}

#[derive(Default)]
struct ConnectionActorLifecycleState {
    stop_reason: Option<StopReason>,
    #[cfg(feature = "test-support")]
    pending_handled_operations: HashSet<fixture::ConnectionActorOperationId>,
    #[cfg(feature = "test-support")]
    pending_binding_handoff: Option<u64>,
}

struct ConnectionActorStopped;

struct ConnectionActorLifecycleGuard<'a> {
    state: MutexGuard<'a, ConnectionActorLifecycleState>,
}

impl ConnectionActorLifecycle {
    fn new() -> Self {
        #[cfg(feature = "test-support")]
        let (stop_callback_watch, _) = watch::channel(0);
        Self {
            state: Mutex::new(ConnectionActorLifecycleState::default()),
            #[cfg(feature = "test-support")]
            stop_callback_count: AtomicU64::new(0),
            #[cfg(feature = "test-support")]
            stop_callback_watch,
        }
    }

    fn lock_running(&self) -> Result<ConnectionActorLifecycleGuard<'_>, ConnectionActorStopped> {
        let state = self.lock_state();
        if state.stop_reason.is_some() {
            return Err(ConnectionActorStopped);
        }
        Ok(ConnectionActorLifecycleGuard { state })
    }

    pub(super) fn request_stop(&self, reason: StopReason) -> bool {
        let mut state = self.lock_state();
        if state.stop_reason.is_some() {
            return false;
        }
        state.stop_reason = Some(reason);
        true
    }

    fn lock_state(&self) -> MutexGuard<'_, ConnectionActorLifecycleState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::protocol::Blocklist;
    use waddle_xmpp::Stanza;

    #[tokio::test]
    async fn actor_owner_delegates_inbound_frame_to_shared_protocol_handler() {
        use std::str::FromStr;

        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = CancellationToken::new();
        let mut owner = ConnectionActorCore::new(WsConnState::new());
        let frame =
            r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.com" version="1.0"/>"#;
        let input = ClassifiedInboundFrame::classify(frame);
        let domain = jid::BareJid::from_str("example.com").expect("typed XMPP domain");

        let ConnectionActorDriverCompletion::CurrentAtCompletion(batch) = owner
            .process_inbound_frame(input, &domain, state.as_ref(), &permit, &shutdown)
            .await
            .expect("current actor can process a frame")
        else {
            panic!("actor remained current after handling the frame");
        };

        assert_eq!(batch.frames.len(), 2);
        let frames = batch
            .frames
            .iter()
            .map(|frame| frame.clone().into_serialized_xml())
            .collect::<Vec<_>>();
        assert!(frames[0].contains("urn:ietf:params:xml:ns:xmpp-framing"));
        assert!(frames[1].contains("<features"));
    }

    async fn owner_frames(
        owner: &mut ConnectionActorCore,
        frame: &str,
        state: &WebSocketState,
    ) -> Vec<String> {
        let permit = crate::clustering::NodeLifecycle::new()
            .admit()
            .expect("serving permit");
        let domain = "example.com".parse().expect("domain");
        let input = ClassifiedInboundFrame::classify(frame);
        assert_eq!(input.wire_bytes(), frame.len());
        let ConnectionActorDriverCompletion::CurrentAtCompletion(batch) = owner
            .process_inbound_frame(input, &domain, state, &permit, &CancellationToken::new())
            .await
            .expect("owner handler completed")
        else {
            panic!("owner remained current")
        };
        batch.into_serialized_frames()
    }

    fn without_generated_stream_ids(frames: &[String]) -> Vec<String> {
        use std::str::FromStr;
        use xmpp_parsers::minidom::Element;

        frames
            .iter()
            .map(|frame| {
                let mut element = Element::from_str(frame).expect("response XML");
                if element.is("open", "urn:ietf:params:xml:ns:xmpp-framing") {
                    element.set_attr(
                        minidom::rxml::Namespace::NONE,
                        minidom::rxml::xml_ncname!("id").to_owned(),
                        "generated-stream-id",
                    );
                    super::super::transport_xml::element_to_xml(element)
                } else {
                    frame.clone()
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn typed_owner_xep0198_dispatch_matches_transport_bytes_and_state() {
        use super::super::frame::handle_xmpp_frame;
        use super::super::ConnectionPhase;

        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        // Rejections prove enable and resume reach their SM handlers, rather
        // than disappearing as unknown roots in ParsedFrame classification.
        for frame in [
            "<enable xmlns='urn:xmpp:sm:3'/>",
            "<resume xmlns='urn:xmpp:sm:3' previd='absent' h='0'/>",
        ] {
            let mut owner = ConnectionActorCore::new(WsConnState::new());
            let mut transport = WsConnState::new();
            let frames = owner_frames(&mut owner, frame, state.as_ref()).await;
            assert_eq!(
                frames,
                handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut transport).await
            );
            assert_eq!(frames.len(), 1);
            assert!(frames[0].contains("unexpected-request"));
            assert!(!owner.state.sm_state.enabled);
        }

        let ready = || {
            let mut conn = WsConnState::new();
            conn.phase =
                ConnectionPhase::ready("alice@example.com/web".parse().expect("bound JID"), false);
            conn.sm_state
                .enable("typed-owner-sm".to_owned(), false, None);
            conn
        };
        let mut owner = ConnectionActorCore::new(ready());
        let mut transport = ready();
        for frame in [
            "<r xmlns='urn:xmpp:sm:3'/>",
            "<a xmlns='urn:xmpp:sm:3' h='0'/>",
        ] {
            let frames = owner_frames(&mut owner, frame, state.as_ref()).await;
            assert_eq!(
                frames,
                handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut transport).await
            );
            if frame.starts_with("<r ") {
                assert_eq!(frames.len(), 1);
                assert_eq!(
                    frames[0]
                        .parse::<xmpp_parsers::minidom::Element>()
                        .expect("ack XML")
                        .attr("h"),
                    Some("0"),
                );
            } else {
                assert!(frames.is_empty());
            }
            assert!(owner.state.sm_state.enabled);
            assert_eq!(
                owner.state.sm_state.inbound_count,
                transport.sm_state.inbound_count
            );
        }
    }

    #[tokio::test]
    async fn typed_owner_malformed_and_empty_input_preserves_transport_disposition() {
        use super::super::frame::handle_xmpp_frame;
        use super::super::ConnectionPhase;

        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let cases = [
            ("<a xmlns='urn:xmpp:sm:3' h='invalid'/>", None),
            ("<resume xmlns='urn:xmpp:sm:3' h='0'/>", None),
            (
                "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>",
                Some("malformed-request"),
            ),
            (
                "<s:response xmlns:s='urn:ietf:params:xml:ns:xmpp-sasl'><broken/>",
                Some("malformed-request"),
            ),
            (
                "<response xmlns='urn:xmpp:sasl:2'>!</response>",
                Some("malformed-request"),
            ),
            (
                "<iq type='get' id='a&amp;b'><bad>",
                Some("feature-not-implemented"),
            ),
            (
                "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' from='example.com'/>",
                Some("invalid-from"),
            ),
            (
                "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing'",
                Some("not-well-formed"),
            ),
            ("<open xmlns='urn:wrong'/>", Some("invalid-namespace")),
            (
                "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing'>text</open>",
                Some("invalid-xml"),
            ),
            (" \t\n", None),
        ];
        for pending_sasl2 in [false, true] {
            for (frame, expected) in cases {
                let initial = || {
                    let mut conn = WsConnState::new();
                    if pending_sasl2 {
                        conn.phase = ConnectionPhase::sasl2_oauth_error_pending();
                    }
                    conn
                };
                let mut owner = ConnectionActorCore::new(initial());
                let mut transport = initial();
                let frames = owner_frames(&mut owner, frame, state.as_ref()).await;
                assert_eq!(
                    without_generated_stream_ids(&frames),
                    without_generated_stream_ids(
                        &handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut transport)
                            .await
                    ),
                    "{frame}, pending={pending_sasl2}"
                );
                assert_eq!(
                    format!("{:?}", owner.state.phase),
                    format!("{:?}", transport.phase)
                );
                assert!(!owner.state.phase.is_sasl2_oauth_error_pending());
                assert!(owner.state.authenticated_session.is_none());
                if !pending_sasl2 {
                    if let Some(expected) = expected {
                        assert!(
                            frames.iter().any(|response| response.contains(expected)),
                            "{frame}: {frames:?}"
                        );
                    } else {
                        assert!(frames.is_empty(), "{frame}: {frames:?}");
                    }
                }
            }
        }
        let oversized = " ".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE + 1);
        for pending in [false, true] {
            let initial = || {
                let mut conn = WsConnState::new();
                if pending {
                    conn.phase = ConnectionPhase::sasl2_oauth_error_pending();
                }
                conn
            };
            let mut owner = ConnectionActorCore::new(initial());
            let mut transport = initial();
            let frames = owner_frames(&mut owner, &oversized, state.as_ref()).await;
            assert_eq!(
                frames,
                handle_xmpp_frame(&oversized, "example.com", state.as_ref(), &mut transport).await
            );
            assert_eq!(frames.len(), usize::from(pending));
            assert_eq!(
                format!("{:?}", owner.state.phase),
                format!("{:?}", transport.phase)
            );
        }
    }

    #[tokio::test]
    async fn typed_owner_malformed_sasl2_clears_sasl1_scram_like_transport() {
        use super::super::frame::handle_xmpp_frame;
        use waddle_xmpp::auth::ScramServer;
        use waddle_xmpp::protocol::{ConnectionPhase, ScramPendingState};

        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        for frame in [
            "<response xmlns='urn:xmpp:sasl:2'>!</response>",
            "<s:authenticate xmlns:s='urn:xmpp:sasl:2'/>",
        ] {
            let initial = || {
                let mut conn = WsConnState::new();
                conn.phase = ConnectionPhase::scram_pending(ScramPendingState::new(
                    ScramServer::new(),
                    vec![],
                    vec![],
                    "alice",
                ));
                conn
            };
            let mut owner = ConnectionActorCore::new(initial());
            let mut transport = initial();
            let frames = owner_frames(&mut owner, frame, state.as_ref()).await;
            assert_eq!(
                frames,
                handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut transport).await
            );
            assert_eq!(frames.len(), 1);
            assert!(frames[0].contains("malformed-request"));
            assert!(matches!(
                owner.state.phase,
                ConnectionPhase::Unauthenticated
            ));
            assert!(matches!(transport.phase, ConnectionPhase::Unauthenticated));
        }
    }

    #[tokio::test]
    async fn actor_owner_runs_retained_peer_stanza_through_recipient_interpreter() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = CancellationToken::new();
        let bob_full: jid::FullJid = "bob@example.com/desk".parse().expect("recipient JID");
        let mut owner = ConnectionActorCore::new(WsConnState::new());
        owner.state.ensure_state_machine(
            "example.com",
            &state.deps.protocol.dispatcher,
            bob_full,
            false,
            Blocklist::empty(),
        );

        let mut message = xmpp_parsers::message::Message::new(Some(
            "bob@example.com".parse().expect("bare recipient"),
        ));
        message.from = Some("alice@example.com/web".parse().expect("sender JID"));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), "hi bob".to_owned());
        let outbound = OutboundStanza::peer_stanza(Stanza::Message(message));
        let original = outbound.stanza.clone();

        let ConnectionActorDriverCompletion::CurrentAtCompletion(drive) = owner
            .interpret_peer_stanza(state.as_ref(), &outbound, &permit, &shutdown)
            .await
            .expect("current actor can run the recipient pass")
        else {
            panic!("actor remained current after recipient interpretation");
        };

        assert_eq!(outbound.kind, DeliveryKind::PeerStanza);
        assert_eq!(
            crate::server::routes::websocket::stanza_to_xml(&outbound.stanza),
            crate::server::routes::websocket::stanza_to_xml(&original),
            "actor custody keeps its envelope"
        );
        assert!(!drive.close);
        let frames = drive.frames.join("\n");
        assert!(frames.contains("by='bob@example.com'"), "{frames}");
        assert!(frames.contains("hi bob"), "{frames}");
    }

    #[tokio::test]
    async fn actor_owner_retains_handler_result_when_stop_arrives_during_handler() {
        use tokio::sync::Notify;

        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = CancellationToken::new();
        let owner = ConnectionActorCore::new(WsConnState::new());
        let actor_lifecycle = Arc::clone(&owner.lifecycle);
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let entered_handler = Arc::clone(&entered);
        let release_handler = Arc::clone(&release);
        let operation = run_with_driver_completion(
            Arc::clone(&actor_lifecycle),
            &permit,
            &shutdown,
            || async move {
                entered_handler.notify_one();
                release_handler.notified().await;
                "retained handler result"
            },
        );
        tokio::pin!(operation);

        tokio::select! {
            result = &mut operation => panic!("handler unexpectedly completed: {result:?}"),
            _ = entered.notified() => {}
        }
        assert!(owner.request_stop(StopReason::Requested));
        release.notify_one();

        assert!(matches!(
            operation.await,
            Ok(ConnectionActorDriverCompletion::CompletionUnknown {
                retained: "retained handler result",
                reason: ConnectionActorDriverCompletionUnknownReason::Stopped,
            })
        ));
    }

    #[tokio::test]
    async fn actor_owner_retains_handler_result_when_shutdown_arrives_during_handler() {
        use tokio::sync::Notify;

        let lifecycle = crate::clustering::NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        let shutdown = CancellationToken::new();
        let actor_lifecycle = Arc::new(ConnectionActorLifecycle::new());
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let entered_handler = Arc::clone(&entered);
        let release_handler = Arc::clone(&release);
        let operation = run_with_driver_completion(
            Arc::clone(&actor_lifecycle),
            &permit,
            &shutdown,
            || async move {
                entered_handler.notify_one();
                release_handler.notified().await;
                "retained handler result"
            },
        );
        tokio::pin!(operation);

        tokio::select! {
            result = &mut operation => panic!("handler unexpectedly completed: {result:?}"),
            _ = entered.notified() => {}
        }
        shutdown.cancel();
        release.notify_one();

        assert!(matches!(
            operation.await,
            Ok(ConnectionActorDriverCompletion::CompletionUnknown {
                retained: "retained handler result",
                reason: ConnectionActorDriverCompletionUnknownReason::AuthorityRevoked,
            })
        ));
    }
}
