//! Dark RFC 7395 and SASL1 transcript over an actual Axum TCP listener.
//! Scripted and session-validated modes never create a production binding.
#![cfg(feature = "test-support")]

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequest, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use waddle_server::config::MaxWebSocketConnections;
use waddle_server::test_support::connection_actor::{
    canonical_actor_gate, CompletePrebindAuth, CompleteValidatedPrebindAuth,
    ConnectionActorController, ConnectionActorStartHooks, PrebindAuthAttempt, PrebindAuthOutcome,
    PrebindPhase, PrebindValidationAttempt, SnapshotConnection, SnapshotPrebind,
};
use waddle_server::test_support::{
    create_test_session, websocket_state_with_admission, WebSocketAdmissionTestConfig,
    WebSocketState,
};
use waddle_server::websocket_transport::{
    ActorWriteProbe, AdmissionTestBarriers, CustodianState, Limits, StopReason, TransportLimits,
};
use waddle_xmpp_client::{AccessToken, OAuthBearerRequest};
use xmpp_parsers::minidom::Element;

const TIMEOUT: Duration = Duration::from_secs(5);
const OPEN: &str =
    "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' to='example.com' version='1.0'/>";
const AUTH: &str = "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='OAUTHBEARER'>biwsAWF1dGg9QmVhcmVyIHRva2VuAQE=</auth>";
const AUTH_AS_BOB: &str = "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='OAUTHBEARER'>bixhPWJvYkBleGFtcGxlLmNvbSwBYXV0aD1CZWFyZXIgdG9rZW4BAQ==</auth>";
const ENABLE: &str = "<enable xmlns='urn:xmpp:sm:3'/>";

struct RouteState {
    state: Arc<WebSocketState>,
    started: mpsc::Sender<ConnectionActorController>,
    auth: FixtureAuth,
    probe: Option<Arc<ActorWriteProbe>>,
}

enum FixtureAuth {
    Scripted(mpsc::Sender<PrebindAuthAttempt>),
    Validated(mpsc::Sender<PrebindValidationAttempt>),
}

async fn handler(State(route): State<Arc<RouteState>>, request: Request) -> Response {
    let gate = match canonical_actor_gate(&route.state) {
        Ok(gate) => gate,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let upgrade = match WebSocketUpgrade::from_request(request, &route).await {
        Ok(upgrade) => upgrade,
        Err(error) => return error.into_response(),
    };
    let admission = match gate.reserve(route.probe.clone()) {
        Ok(admission) => admission,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    upgrade
        .protocols(["xmpp"])
        .on_upgrade(move |socket| async move {
            let limits = TransportLimits::new(
                Limits::new(32, 65536).expect("inbound"),
                Limits::new(8, 65536).expect("outbound"),
            );
            let controller = match &route.auth {
                FixtureAuth::Scripted(attempts) => {
                    admission
                        .start_prebind(
                            socket,
                            limits,
                            Duration::from_millis(100),
                            TIMEOUT,
                            ConnectionActorStartHooks::default(),
                            attempts.clone(),
                        )
                        .await
                }
                FixtureAuth::Validated(attempts) => {
                    admission
                        .start_prebind_validated(
                            socket,
                            limits,
                            Duration::from_millis(100),
                            TIMEOUT,
                            ConnectionActorStartHooks::default(),
                            attempts.clone(),
                        )
                        .await
                }
            }
            .expect("start fixture");
            let _ = route.started.send(controller).await;
        })
}

struct Fixture {
    peer: WebSocketStream<TcpStream>,
    actor: ConnectionActorController,
    attempts: mpsc::Receiver<PrebindAuthAttempt>,
    validated_attempts: mpsc::Receiver<PrebindValidationAttempt>,
    state: Arc<WebSocketState>,
    server: JoinHandle<()>,
}

struct ReleaseProbe(Arc<ActorWriteProbe>);
impl Drop for ReleaseProbe {
    fn drop(&mut self) {
        self.0.release();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn start(probe: Option<Arc<ActorWriteProbe>>) -> Self {
        Self::start_with_auth(probe, false).await
    }
    async fn start_validated() -> Self {
        Self::start_with_auth(None, true).await
    }
    async fn start_validated_with_probe(probe: Arc<ActorWriteProbe>) -> Self {
        Self::start_with_auth(Some(probe), true).await
    }
    async fn start_with_auth(probe: Option<Arc<ActorWriteProbe>>, validated: bool) -> Self {
        let state = websocket_state_with_admission(WebSocketAdmissionTestConfig {
            capacity: MaxWebSocketConnections::from_raw(Some("1")).expect("capacity"),
            clustered_identity: None,
            barriers: AdmissionTestBarriers::default(),
        })
        .await;
        let (started_tx, mut started_rx) = mpsc::channel(1);
        let (attempts_tx, attempts) = mpsc::channel(4);
        let (validated_tx, validated_attempts) = mpsc::channel(4);
        let route = Arc::new(RouteState {
            state: Arc::clone(&state),
            started: started_tx,
            auth: if validated {
                FixtureAuth::Validated(validated_tx)
            } else {
                FixtureAuth::Scripted(attempts_tx)
            },
            probe,
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("address");
        let app = Router::new().route("/dark", get(handler)).with_state(route);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        let stream = TcpStream::connect(addr).await.expect("connect");
        let (peer, _) = tokio_tungstenite::client_async("ws://localhost/dark", stream)
            .await
            .expect("upgrade");
        let actor = timeout(TIMEOUT, started_rx.recv())
            .await
            .expect("started deadline")
            .expect("started");
        Self {
            peer,
            actor,
            attempts,
            validated_attempts,
            state,
            server,
        }
    }
    async fn send(&mut self, text: &str) {
        self.peer
            .send(Message::Text(text.into()))
            .await
            .expect("send");
    }
    async fn element(&mut self) -> Element {
        match timeout(TIMEOUT, self.peer.next())
            .await
            .expect("frame deadline")
            .expect("socket open")
            .expect("frame")
        {
            Message::Text(text) => text.parse().expect("element"),
            other => panic!("expected XML element, got {other:?}"),
        }
    }
    async fn ping(&mut self, payload: &[u8]) {
        self.peer
            .send(Message::Ping(payload.to_vec().into()))
            .await
            .expect("send ping");
        assert_eq!(
            timeout(TIMEOUT, self.peer.next())
                .await
                .expect("pong deadline")
                .expect("socket open")
                .expect("frame"),
            Message::Pong(payload.to_vec().into())
        );
    }
    async fn phase(&self) -> PrebindPhase {
        self.actor
            .actor()
            .ask(SnapshotPrebind)
            .send()
            .await
            .expect("actor")
            .expect("prebind")
            .phase
    }
    async fn wait_phase(&self, expected: PrebindPhase) {
        timeout(TIMEOUT, async {
            loop {
                if self.phase().await == expected {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("phase deadline");
    }
    async fn initial_open(&mut self) {
        self.send(OPEN).await;
        let open = self.element().await;
        assert_eq!(
            (open.name(), open.ns().as_str()),
            ("open", "urn:ietf:params:xml:ns:xmpp-framing")
        );
        let features = self.element().await;
        assert_eq!(features.name(), "features");
        let mechanisms = features
            .get_child("mechanisms", waddle_xmpp::ns::SASL)
            .expect("mechanisms");
        assert_eq!(
            mechanisms
                .children()
                .map(|element| element.text())
                .collect::<Vec<_>>(),
            vec!["OAUTHBEARER"]
        );
    }
    async fn auth_attempt(&mut self) -> PrebindAuthAttempt {
        timeout(TIMEOUT, self.attempts.recv())
            .await
            .expect("attempt deadline")
            .expect("attempt")
    }
    async fn validation_attempt(&mut self) -> PrebindValidationAttempt {
        timeout(TIMEOUT, self.validated_attempts.recv())
            .await
            .expect("attempt deadline")
            .expect("attempt")
    }
    async fn complete_validated(&self, result: CompleteValidatedPrebindAuth) {
        self.actor
            .actor()
            .tell(result)
            .send()
            .await
            .expect("validated auth completion");
    }
    async fn send_bearer(&mut self, token: &str, account: &str) {
        let element = OAuthBearerRequest {
            account: account.parse().expect("account"),
            access_token: AccessToken::new(token),
        }
        .to_element();
        self.send(&String::from(&element)).await;
    }
    async fn complete(
        &self,
        token: waddle_server::test_support::connection_actor::PrebindAuthToken,
        outcome: PrebindAuthOutcome,
    ) {
        self.actor
            .actor()
            .tell(CompletePrebindAuth { token, outcome })
            .send()
            .await
            .expect("auth completion");
    }
}

fn condition(element: &Element) -> Option<&str> {
    element.children().next().map(Element::name)
}

#[tokio::test]
async fn validated_auth_installs_exact_session_and_rejects_scripted_success() {
    let mut fixture = Fixture::start_validated().await;
    let session = create_test_session(&fixture.state, "alice").await;
    fixture.initial_open().await;

    fixture
        .send_bearer("not-a-session", "alice@example.com")
        .await;
    let invalid = fixture
        .validation_attempt()
        .await
        .validate()
        .await
        .expect("result");
    fixture.complete_validated(invalid).await;
    assert_eq!(condition(&fixture.element().await), Some("not-authorized"));
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert!(!snapshot.state_authenticated);
    assert_eq!(snapshot.validated_principal, None);

    fixture.send_bearer(&session.id, "alice@example.com").await;
    let attempt = fixture.validation_attempt().await;
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    fixture
        .complete(
            snapshot.pending_auth.expect("pending token"),
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert_eq!(snapshot.phase, PrebindPhase::AuthPending);
    assert_eq!(snapshot.validated_principal, None);
    assert!(!snapshot.state_authenticated);

    let result = attempt.validate().await.expect("validated result");
    fixture.complete_validated(result.clone()).await;
    assert_eq!(fixture.element().await.name(), "success");
    fixture.complete_validated(result).await;
    fixture.wait_phase(PrebindPhase::AwaitRestart).await;
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert_eq!(snapshot.phase, PrebindPhase::AwaitRestart);
    assert!(snapshot.state_authenticated);
    assert_eq!(
        snapshot.validated_principal,
        Some(session.authenticated_principal_ref().expect("principal"))
    );
    let connection = fixture
        .actor
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("actor");
    assert_eq!(connection.binding_ref, None);
    assert!(!connection.stream_management_enabled);
    fixture.send(OPEN).await;
    assert_eq!(fixture.element().await.name(), "open");
    assert_eq!(fixture.element().await.name(), "features");
    fixture.wait_phase(PrebindPhase::AuthenticatedUnbound).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validated_auth_checks_authzid_and_initial_open_hint_before_installing() {
    let mut fixture = Fixture::start_validated().await;
    let session = create_test_session(&fixture.state, "alice").await;
    fixture.initial_open().await;
    fixture.send_bearer(&session.id, "bob@example.com").await;
    let result = fixture
        .validation_attempt()
        .await
        .validate()
        .await
        .expect("result");
    fixture.complete_validated(result).await;
    assert_eq!(condition(&fixture.element().await), Some("invalid-authzid"));
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert!(!snapshot.state_authenticated);
    assert_eq!(snapshot.validated_principal, None);

    let probe = Arc::new(ActorWriteProbe::default());
    let _release = ReleaseProbe(Arc::clone(&probe));
    let mut other = Fixture::start_validated_with_probe(Arc::clone(&probe)).await;
    let session = create_test_session(&other.state, "alice").await;
    other.send("<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' to='example.com' from='bob@example.com' version='1.0'/>").await;
    assert_eq!(other.element().await.name(), "open");
    assert_eq!(other.element().await.name(), "features");
    other.send_bearer(&session.id, "alice@example.com").await;
    let result = other
        .validation_attempt()
        .await
        .validate()
        .await
        .expect("result");
    probe.pause_next();
    other.complete_validated(result).await;
    timeout(TIMEOUT, probe.wait_for_entry())
        .await
        .expect("failure write");
    let snapshot = other
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert_eq!(snapshot.phase, PrebindPhase::Closing);
    assert!(!snapshot.state_authenticated);
    assert_eq!(snapshot.validated_principal, None);
    probe.release();
    assert_eq!(condition(&other.element().await), Some("not-authorized"));
    assert_eq!(other.element().await.name(), "close");
}

#[tokio::test]
async fn validated_auth_discards_replaced_cancelled_foreign_and_duplicate_results() {
    let mut fixture = Fixture::start_validated().await;
    let session = create_test_session(&fixture.state, "alice").await;
    fixture.initial_open().await;
    fixture.send_bearer(&session.id, "alice@example.com").await;
    let stale = fixture
        .validation_attempt()
        .await
        .validate()
        .await
        .expect("first result");
    fixture.send_bearer(&session.id, "alice@example.com").await;
    let cancelled = fixture.validation_attempt().await;
    fixture.send_bearer(&session.id, "alice@example.com").await;
    let current = fixture.validation_attempt().await;
    assert!(cancelled.is_cancelled());
    assert!(cancelled.validate().await.is_none());

    let mut foreign_fixture = Fixture::start_validated().await;
    let foreign_session = create_test_session(&foreign_fixture.state, "alice").await;
    foreign_fixture.initial_open().await;
    foreign_fixture
        .send_bearer(&foreign_session.id, "alice@example.com")
        .await;
    let foreign = foreign_fixture
        .validation_attempt()
        .await
        .validate()
        .await
        .expect("foreign result");
    fixture.complete_validated(stale).await;
    fixture.complete_validated(foreign).await;
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert_eq!(snapshot.phase, PrebindPhase::AuthPending);
    assert_eq!(snapshot.validated_principal, None);
    assert!(!snapshot.state_authenticated);

    let result = current.validate().await.expect("current result");
    fixture.complete_validated(result.clone()).await;
    fixture.complete_validated(result).await;
    assert_eq!(fixture.element().await.name(), "success");
    fixture.wait_phase(PrebindPhase::AwaitRestart).await;
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert_eq!(snapshot.phase, PrebindPhase::AwaitRestart);
    assert_eq!(
        snapshot.validated_principal,
        Some(session.authenticated_principal_ref().expect("principal"))
    );
}

#[tokio::test]
async fn validated_auth_abort_cancels_attempt_and_rejects_completed_late_result() {
    let mut fixture = Fixture::start_validated().await;
    let session = create_test_session(&fixture.state, "alice").await;
    fixture.initial_open().await;
    fixture.send_bearer(&session.id, "alice@example.com").await;
    let late = fixture
        .validation_attempt()
        .await
        .validate()
        .await
        .expect("result");
    fixture
        .send("<abort xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>")
        .await;
    assert_eq!(condition(&fixture.element().await), Some("aborted"));
    fixture.complete_validated(late).await;
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert_eq!(snapshot.phase, PrebindPhase::PreAuth);
    assert!(!snapshot.state_authenticated);
    assert_eq!(snapshot.validated_principal, None);

    fixture.send_bearer(&session.id, "alice@example.com").await;
    let cancelled = fixture.validation_attempt().await;
    fixture
        .send("<abort xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>")
        .await;
    assert_eq!(condition(&fixture.element().await), Some("aborted"));
    assert!(cancelled.is_cancelled());
    assert!(cancelled.validate().await.is_none());
}

#[tokio::test]
async fn revoked_session_fails_fresh_validation_and_shutdown_cancels_pending_validation() {
    let mut fixture = Fixture::start_validated().await;
    let session = create_test_session(&fixture.state, "alice").await;
    fixture.initial_open().await;
    fixture.send_bearer(&session.id, "alice@example.com").await;
    let attempt = fixture.validation_attempt().await;
    fixture
        .state
        .deps
        .auth_state
        .session_manager
        .delete_session(&session.id)
        .await
        .expect("revoke");
    let result = attempt.validate().await.expect("result");
    fixture.complete_validated(result).await;
    assert_eq!(condition(&fixture.element().await), Some("not-authorized"));
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert!(!snapshot.state_authenticated);
    assert_eq!(snapshot.validated_principal, None);

    fixture.send_bearer(&session.id, "alice@example.com").await;
    let attempt = fixture.validation_attempt().await;
    let actor = fixture.actor.actor().clone();
    fixture.actor.request_stop(StopReason::Requested);
    timeout(TIMEOUT, actor.wait_for_shutdown_result())
        .await
        .expect("actor shutdown deadline")
        .expect("actor shutdown");
    assert!(attempt.is_cancelled());
    assert!(attempt.validate().await.is_none());
    assert_eq!(
        timeout(TIMEOUT, fixture.actor.wait_until_joined())
            .await
            .expect("pump join deadline"),
        Some(CustodianState::Joined)
    );
}

#[tokio::test]
async fn revocation_after_lookup_does_not_revalidate_the_unbound_actor_snapshot() {
    let mut fixture = Fixture::start_validated().await;
    let session = create_test_session(&fixture.state, "alice").await;
    fixture.initial_open().await;
    fixture.send_bearer(&session.id, "alice@example.com").await;

    // Hold the already-validated result outside the actor while revocation
    // completes. The test characterizes lookup-time, not delivery-time,
    // authority; this dark fixture cannot bind or publish the stale result.
    let result = fixture
        .validation_attempt()
        .await
        .validate()
        .await
        .expect("validation completed before revocation");
    let manager = &fixture.state.deps.auth_state.session_manager;
    manager
        .delete_session(&session.id)
        .await
        .expect("revoke before completion delivery");
    assert!(manager
        .get_session(&session.id)
        .await
        .expect("read revoked session")
        .is_none());

    fixture.complete_validated(result).await;
    assert_eq!(fixture.element().await.name(), "success");
    fixture.wait_phase(PrebindPhase::AwaitRestart).await;
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert!(snapshot.state_authenticated);
    assert_eq!(
        snapshot.validated_principal,
        Some(session.authenticated_principal_ref().expect("principal"))
    );
    let connection = fixture
        .actor
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("actor");
    assert_eq!(connection.binding_ref, None);
    assert!(!connection.stream_management_enabled);
}

#[tokio::test]
async fn initial_open_sm_disabled_scripted_auth_and_restart() {
    let mut fixture = Fixture::start(None).await;
    let initial = fixture
        .actor
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("snapshot");
    assert!(!initial.stream_management_enabled);
    assert_eq!(initial.binding_ref, None);
    assert_eq!(fixture.phase().await, PrebindPhase::AwaitInitialOpen);
    fixture.initial_open().await;
    fixture.send(ENABLE).await;
    let failed = fixture.element().await;
    assert_eq!(
        (failed.name(), failed.ns().as_str(), condition(&failed)),
        ("failed", "urn:xmpp:sm:3", Some("unexpected-request"))
    );
    fixture.send(AUTH).await;
    let attempt = fixture.auth_attempt().await;
    assert_eq!(fixture.phase().await, PrebindPhase::AuthPending);
    fixture
        .complete(
            attempt.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    assert_eq!(fixture.element().await.name(), "success");
    fixture.send(OPEN).await;
    let open = fixture.element().await;
    assert_eq!(open.name(), "open");
    let features = fixture.element().await;
    assert_eq!(features.name(), "features");
    assert!(features.children().next().is_none());
    assert_eq!(fixture.phase().await, PrebindPhase::AuthenticatedUnbound);
    fixture.send(ENABLE).await;
    assert_eq!(
        condition(&fixture.element().await),
        Some("unexpected-request")
    );
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotConnection)
        .send()
        .await
        .expect("snapshot");
    assert!(!snapshot.stream_management_enabled);
    assert_eq!(snapshot.handled_h, 0);
    assert_eq!(snapshot.binding_ref, None);
}

#[tokio::test]
async fn opening_errors_emit_open_error_close_in_order() {
    for (input, expected) in [
        ("<open xmlns='urn:wrong'/>", "invalid-namespace"),
        (
            "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' from='domain.example'/>",
            "invalid-from",
        ),
    ] {
        let mut fixture = Fixture::start(None).await;
        fixture.send(input).await;
        assert_eq!(fixture.element().await.name(), "open");
        let error = fixture.element().await;
        assert_eq!(error.name(), "error");
        assert_eq!(condition(&error), Some(expected));
        assert_eq!(fixture.element().await.name(), "close");
        assert!(matches!(
            timeout(TIMEOUT, fixture.peer.next())
                .await
                .expect("close deadline"),
            Some(Ok(Message::Close(_))) | None
        ));
    }
}

#[tokio::test]
async fn wrong_namespace_close_fails_closed() {
    let mut fixture = Fixture::start(None).await;
    fixture.initial_open().await;
    fixture.send("<close xmlns='urn:wrong'/>").await;
    assert_eq!(fixture.element().await.name(), "error");
    assert_eq!(fixture.element().await.name(), "close");
    assert!(matches!(
        timeout(TIMEOUT, fixture.peer.next())
            .await
            .expect("close deadline"),
        Some(Ok(Message::Close(_))) | None
    ));
}

#[tokio::test]
async fn malformed_and_unsupported_auth_and_pipelined_attempts_are_explicit() {
    let mut fixture = Fixture::start(None).await;
    fixture.initial_open().await;
    for (input, expected) in [
        (
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='SCRAM-SHA-256'/>",
            "invalid-mechanism",
        ),
        (
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='OAUTHBEARER'>!!!</auth>",
            "incorrect-encoding",
        ),
        (
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='OAUTHBEARER'>YQ==</auth>",
            "malformed-request",
        ),
    ] {
        fixture.send(input).await;
        let failure = fixture.element().await;
        assert_eq!(failure.name(), "failure");
        assert_eq!(condition(&failure), Some(expected));
    }
    fixture.send(AUTH).await;
    let first = fixture.auth_attempt().await;
    fixture.send(AUTH).await;
    let second = fixture.auth_attempt().await;
    fixture
        .complete(
            first.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    assert_eq!(fixture.phase().await, PrebindPhase::AuthPending);
    fixture
        .complete(
            second.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    assert_eq!(fixture.element().await.name(), "success");
    assert_eq!(fixture.actor.handled_h(), 0);
}

#[tokio::test]
async fn stale_and_duplicate_scripted_results_do_not_complete_new_attempt() {
    let mut fixture = Fixture::start(None).await;
    fixture.initial_open().await;
    fixture.send(AUTH).await;
    let first = fixture.auth_attempt().await;
    fixture
        .complete(first.token.clone(), PrebindAuthOutcome::Rejected)
        .await;
    assert_eq!(fixture.element().await.name(), "failure");
    fixture.send(AUTH).await;
    let second = fixture.auth_attempt().await;
    fixture
        .complete(
            first.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    assert_eq!(fixture.phase().await, PrebindPhase::AuthPending);
    fixture
        .complete(
            second.token.clone(),
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    assert_eq!(fixture.element().await.name(), "success");
    fixture
        .complete(second.token, PrebindAuthOutcome::Rejected)
        .await;
    assert_eq!(fixture.phase().await, PrebindPhase::AwaitRestart);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_replacement_auth_invalidates_pending_attempt_and_allows_retry() {
    for replacement in [
        "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>",
        "<s:auth xmlns:s='urn:ietf:params:xml:ns:xmpp-sasl'/>",
    ] {
        let probe = Arc::new(ActorWriteProbe::default());
        let _release = ReleaseProbe(Arc::clone(&probe));
        let mut fixture = Fixture::start(Some(Arc::clone(&probe))).await;
        fixture.initial_open().await;
        fixture.send(AUTH).await;
        let first = fixture.auth_attempt().await;
        probe.pause_next();
        fixture.send(replacement).await;
        timeout(TIMEOUT, probe.wait_for_entry())
            .await
            .expect("replacement failure sink entry");
        fixture
            .complete(
                first.token.clone(),
                PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
            )
            .await;
        let snapshot = fixture
            .actor
            .actor()
            .ask(SnapshotPrebind)
            .send()
            .await
            .expect("actor")
            .expect("prebind");
        assert_eq!(snapshot.phase, PrebindPhase::PreAuth);
        assert_eq!(snapshot.pending_auth, None);
        assert!(snapshot.output_pending);
        probe.release();
        let failure = fixture.element().await;
        assert_eq!(failure.name(), "failure");
        assert_eq!(condition(&failure), Some("invalid-mechanism"));
        fixture.send(AUTH).await;
        let second = fixture.auth_attempt().await;
        fixture
            .complete(
                first.token,
                PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
            )
            .await;
        assert_eq!(fixture.phase().await, PrebindPhase::AuthPending);
        fixture
            .complete(
                second.token,
                PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
            )
            .await;
        assert_eq!(fixture.element().await.name(), "success");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_control_frames_do_not_interrupt_open_or_pending_auth() {
    let probe = Arc::new(ActorWriteProbe::default());
    let _release = ReleaseProbe(Arc::clone(&probe));
    probe.pause_next();
    let mut fixture = Fixture::start(Some(Arc::clone(&probe))).await;
    fixture.send(OPEN).await;
    timeout(TIMEOUT, probe.wait_for_entry())
        .await
        .expect("open sink entry");
    fixture.ping(b"opening\0ping").await;
    fixture
        .peer
        .send(Message::Pong(b"unsolicited".to_vec().into()))
        .await
        .expect("send pong");
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("actor")
        .expect("prebind");
    assert_eq!(snapshot.phase, PrebindPhase::AwaitInitialOpen);
    assert!(snapshot.output_pending);
    probe.release();
    assert_eq!(fixture.element().await.name(), "open");
    assert_eq!(fixture.element().await.name(), "features");

    fixture.send(AUTH).await;
    let attempt = fixture.auth_attempt().await;
    assert_eq!(fixture.phase().await, PrebindPhase::AuthPending);
    fixture.ping(&[0, 1, 255]).await;
    fixture
        .peer
        .send(Message::Pong(b"ignored".to_vec().into()))
        .await
        .expect("send pong");
    fixture
        .complete(
            attempt.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    assert_eq!(fixture.element().await.name(), "success");
    assert_eq!(fixture.phase().await, PrebindPhase::AwaitRestart);
    fixture.send(OPEN).await;
    assert_eq!(fixture.element().await.name(), "open");
    assert_eq!(fixture.element().await.name(), "features");
    assert_eq!(fixture.phase().await, PrebindPhase::AuthenticatedUnbound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_invalidates_pending_auth_and_allows_retry() {
    for abort in [
        "<abort xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>",
        "<s:abort xmlns:s='urn:ietf:params:xml:ns:xmpp-sasl'/>",
    ] {
        let probe = Arc::new(ActorWriteProbe::default());
        let _release = ReleaseProbe(Arc::clone(&probe));
        let mut fixture = Fixture::start(Some(Arc::clone(&probe))).await;
        fixture.initial_open().await;
        fixture.send(AUTH).await;
        let first = fixture.auth_attempt().await;
        probe.pause_next();
        fixture.send(abort).await;
        timeout(TIMEOUT, probe.wait_for_entry())
            .await
            .expect("abort failure sink entry");
        fixture
            .complete(
                first.token.clone(),
                PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
            )
            .await;
        let snapshot = fixture
            .actor
            .actor()
            .ask(SnapshotPrebind)
            .send()
            .await
            .expect("actor")
            .expect("prebind");
        assert_eq!(snapshot.phase, PrebindPhase::PreAuth);
        assert_eq!(snapshot.pending_auth, None);
        assert!(snapshot.output_pending);
        probe.release();
        let failure = fixture.element().await;
        assert_eq!(failure.name(), "failure");
        assert_eq!(failure.ns(), waddle_xmpp::ns::SASL);
        assert_eq!(condition(&failure), Some("aborted"));
        fixture.send(AUTH).await;
        let second = fixture.auth_attempt().await;
        fixture
            .complete(
                first.token,
                PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
            )
            .await;
        assert_eq!(fixture.phase().await, PrebindPhase::AuthPending);
        fixture
            .complete(
                second.token,
                PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
            )
            .await;
        assert_eq!(fixture.element().await.name(), "success");
        assert_eq!(fixture.phase().await, PrebindPhase::AwaitRestart);
    }
}

#[tokio::test]
async fn abort_with_wrong_namespace_is_not_an_abort() {
    let mut fixture = Fixture::start(None).await;
    fixture.initial_open().await;
    fixture.send(AUTH).await;
    let _attempt = fixture.auth_attempt().await;
    fixture.send("<s:abort xmlns:s='urn:wrong'/>").await;
    assert_eq!(fixture.element().await.name(), "error");
    assert_eq!(fixture.element().await.name(), "close");
}

#[tokio::test]
async fn prefixed_framing_and_sasl_auth_complete_a_transcript() {
    let mut fixture = Fixture::start(None).await;
    fixture
        .send("<f:open xmlns:f='urn:ietf:params:xml:ns:xmpp-framing'/>")
        .await;
    assert_eq!(fixture.element().await.name(), "open");
    assert_eq!(fixture.element().await.name(), "features");
    fixture.send("<s:auth xmlns:s='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='OAUTHBEARER'>biwsAWF1dGg9QmVhcmVyIHRva2VuAQE=</s:auth>").await;
    let attempt = fixture.auth_attempt().await;
    fixture
        .complete(
            attempt.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    assert_eq!(fixture.element().await.name(), "success");
    fixture
        .send("<f:open xmlns:f='urn:ietf:params:xml:ns:xmpp-framing'/>")
        .await;
    assert_eq!(fixture.element().await.name(), "open");
    assert_eq!(fixture.element().await.name(), "features");
    fixture
        .send("<f:close xmlns:f='urn:ietf:params:xml:ns:xmpp-framing'/>")
        .await;
    assert_eq!(fixture.element().await.name(), "close");
    assert!(matches!(
        timeout(TIMEOUT, fixture.peer.next())
            .await
            .expect("close deadline"),
        Some(Ok(Message::Close(_))) | None
    ));
}

#[tokio::test]
async fn unauthorized_authzid_is_reported_and_retry_remains_possible() {
    let mut fixture = Fixture::start(None).await;
    fixture.initial_open().await;
    fixture.send(AUTH_AS_BOB).await;
    let attempt = fixture.auth_attempt().await;
    assert_eq!(
        attempt
            .credentials
            .authzid
            .as_ref()
            .map(ToString::to_string),
        Some("bob@example.com".to_string())
    );
    fixture
        .complete(
            attempt.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    let failure = fixture.element().await;
    assert_eq!(failure.name(), "failure");
    assert_eq!(condition(&failure), Some("invalid-authzid"));
    fixture.send(AUTH).await;
    let retry = fixture.auth_attempt().await;
    fixture
        .complete(
            retry.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    assert_eq!(fixture.element().await.name(), "success");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_open_receipt_keeps_phase_and_independent_stop_progresses() {
    let probe = Arc::new(ActorWriteProbe::default());
    let _release = ReleaseProbe(Arc::clone(&probe));
    probe.pause_next();
    let mut fixture = Fixture::start(Some(Arc::clone(&probe))).await;
    fixture.send(OPEN).await;
    timeout(TIMEOUT, probe.wait_for_entry())
        .await
        .expect("sink entry");
    fixture.send(AUTH).await;
    assert!(fixture.actor.wait_for_inbound_forwarded(2).await);
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("snapshot")
        .expect("prebind");
    assert_eq!(snapshot.phase, PrebindPhase::AwaitInitialOpen);
    assert!(snapshot.output_pending);
    assert!(timeout(Duration::from_millis(50), fixture.attempts.recv())
        .await
        .is_err());
    fixture.actor.request_stop(StopReason::Requested);
    assert_eq!(fixture.actor.stop_reason(), Some(StopReason::Requested));
    probe.release();
    timeout(TIMEOUT, fixture.actor.wait_until_joined())
        .await
        .expect("joined deadline")
        .expect("observer");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preauth_stanza_is_rejected_without_sm_ack_or_count() {
    let probe = Arc::new(ActorWriteProbe::default());
    let _release = ReleaseProbe(Arc::clone(&probe));
    let mut fixture = Fixture::start(Some(Arc::clone(&probe))).await;
    fixture.initial_open().await;
    probe.pause_next();
    fixture
        .send("<message xmlns='jabber:client' xml:lang='en'><body>hello</body></message>")
        .await;
    timeout(TIMEOUT, probe.wait_for_entry())
        .await
        .expect("sink entry");
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("snapshot")
        .expect("prebind");
    assert_eq!(
        snapshot
            .last_stanza_language
            .as_ref()
            .map(|lang| lang.0.as_str()),
        Some("en")
    );
    probe.release();
    let error = fixture.element().await;
    assert_eq!(error.name(), "error");
    assert_eq!(condition(&error), Some("not-authorized"));
    assert_eq!(fixture.element().await.name(), "close");
    assert_eq!(fixture.actor.handled_h(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn success_receipt_precedes_restart_open_and_phase_transition() {
    let probe = Arc::new(ActorWriteProbe::default());
    let _release = ReleaseProbe(Arc::clone(&probe));
    let mut fixture = Fixture::start(Some(Arc::clone(&probe))).await;
    fixture.initial_open().await;
    fixture.send(AUTH).await;
    let attempt = fixture.auth_attempt().await;
    probe.pause_next();
    fixture
        .complete(
            attempt.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    timeout(TIMEOUT, probe.wait_for_entry())
        .await
        .expect("success sink entry");
    fixture.send(OPEN).await;
    assert!(fixture.actor.wait_for_inbound_forwarded(3).await);
    let snapshot = fixture
        .actor
        .actor()
        .ask(SnapshotPrebind)
        .send()
        .await
        .expect("snapshot")
        .expect("prebind");
    assert_eq!(snapshot.phase, PrebindPhase::AuthPending);
    assert!(snapshot.output_pending);
    probe.release();
    assert_eq!(fixture.element().await.name(), "success");
    assert_eq!(fixture.element().await.name(), "open");
    assert_eq!(fixture.element().await.name(), "features");
    assert_eq!(fixture.phase().await, PrebindPhase::AuthenticatedUnbound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_identity_must_match_open_from_hint() {
    let probe = Arc::new(ActorWriteProbe::default());
    let _release = ReleaseProbe(Arc::clone(&probe));
    let mut fixture = Fixture::start(Some(Arc::clone(&probe))).await;
    fixture
        .send("<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' from='bob@example.com'/>")
        .await;
    let response = fixture.element().await;
    assert_eq!(response.name(), "open");
    assert_eq!(response.attr("to"), Some("bob@example.com"));
    assert_eq!(fixture.element().await.name(), "features");
    fixture.send(AUTH).await;
    let attempt = fixture.auth_attempt().await;
    probe.pause_next();
    fixture
        .complete(
            attempt.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    timeout(TIMEOUT, probe.wait_for_entry())
        .await
        .expect("failure sink entry");
    assert_eq!(fixture.phase().await, PrebindPhase::Closing);
    probe.release();
    let failure = fixture.element().await;
    assert_eq!(failure.name(), "failure");
    assert_eq!(condition(&failure), Some("not-authorized"));
    assert_eq!(fixture.element().await.name(), "close");
    assert!(matches!(
        timeout(TIMEOUT, fixture.peer.next())
            .await
            .expect("close deadline"),
        Some(Ok(Message::Close(_))) | None
    ));
}

#[tokio::test]
async fn malformed_restart_does_not_reuse_previous_open_hint() {
    let mut fixture = Fixture::start(None).await;
    fixture
        .send("<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' from='alice@example.com'/>")
        .await;
    assert_eq!(
        fixture.element().await.attr("to"),
        Some("alice@example.com")
    );
    assert_eq!(fixture.element().await.name(), "features");
    fixture.send(AUTH).await;
    let attempt = fixture.auth_attempt().await;
    fixture
        .complete(
            attempt.token,
            PrebindAuthOutcome::Authenticated("alice@example.com".parse().expect("jid")),
        )
        .await;
    assert_eq!(fixture.element().await.name(), "success");
    fixture.send("<open xmlns='urn:wrong'/>").await;
    let response = fixture.element().await;
    assert_eq!(response.name(), "open");
    assert_eq!(response.attr("to"), None);
    let error = fixture.element().await;
    assert_eq!(condition(&error), Some("invalid-namespace"));
    assert_eq!(fixture.element().await.name(), "close");
}

#[tokio::test]
async fn preauth_ack_request_cannot_emit_acknowledgement() {
    let mut fixture = Fixture::start(None).await;
    fixture.initial_open().await;
    fixture.send("<r xmlns='urn:xmpp:sm:3'/>").await;
    assert_eq!(fixture.element().await.name(), "error");
    assert_eq!(fixture.element().await.name(), "close");
    assert_eq!(fixture.actor.handled_h(), 0);
}
