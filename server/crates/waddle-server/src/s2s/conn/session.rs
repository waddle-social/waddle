//! One S2S connection's task: it drives the codec and negotiation over the
//! transport, runs the TLS handshake on the verifier workers, decides SASL
//! EXTERNAL authorization and enforces the deadlines.
//!
//! Negotiation deadlines run from fixed instants and are never renewed by
//! partial input: each stage (initial header, TLS handshake, each expected
//! restart) has its own deadline, the SASL phase has one from its start,
//! and the whole negotiation has one from the TCP accept or connect. An
//! established stream ends when its certificate evidence or its maximum
//! age runs out, when it has been idle too long, or when an unfinished
//! element stays unfinished too long.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::UnixTime;
use rustls::{ClientConnection, Connection, ServerConnection};
use tokio::sync::oneshot;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::s2s::{
    AuthorizationDecision, AuthorizationRequest, CloseReason, NegotiationPhase, Output, S2sStream,
    Signal, StreamErrorCondition,
};
use waddle_xmpp::telemetry::attributes::{S2sOutcome, S2sResource, S2sTransport};

use super::admission::{Claim, ConnectionAdmission};
use super::capacity::{InboundSlot, OutboundSlot};
use super::peer_stream::{StreamDirection, TlsParameters, VerifiedPeerStream};
use super::registry::{
    Activity, DialSlot, NegotiationSlot, RegisterRefusal, StreamSlot, StreamTokens,
};
use super::service::Shared;
use super::sink::SinkVerdict;
use super::telemetry;
use super::transport::{Transport, TransportError};
use crate::s2s::verify::{
    initiating_config, receiving_config, OutboundTransport, PeerVerifier, Verification,
    VerifiedPeer,
};

/// Plaintext handed to the codec at once; below its per-push bound.
const PLAINTEXT_CHUNK: usize = 16 * 1024;

/// How an outbound session reports its first result to the dialer.
pub(super) type EstablishedSender = oneshot::Sender<Result<VerifiedPeerStream, S2sOutcome>>;

/// The role-specific part of a session.
pub(super) enum Role {
    Inbound {
        admission: Arc<ConnectionAdmission>,
        slot: InboundSlot,
    },
    Outbound {
        local: CanonicalDomain,
        remote: CanonicalDomain,
        _slot: OutboundSlot,
        /// Held until the attempt succeeds or fails, so a second dial for
        /// the pair is refused meanwhile.
        dial: Option<DialSlot>,
        established: Option<EstablishedSender>,
    },
}

/// Everything a new session starts from.
pub(super) struct SessionStart {
    pub(super) connection: u64,
    pub(super) transport: Transport,
    pub(super) stream: S2sStream,
    pub(super) role: Role,
    pub(super) activity: Arc<Activity>,
    /// When the TCP connection was accepted or opened.
    pub(super) started: Instant,
    pub(super) tls_transport: S2sTransport,
}

/// Which deadline fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineKind {
    Timeout,
    Expired,
}

enum Event {
    Shutdown,
    Evicted,
    Replaced,
    Deadline(DeadlineKind),
    Read(Result<usize, TransportError>),
}

/// The state of one connection.
pub(super) struct Session {
    shared: Arc<Shared>,
    connection: u64,
    direction: StreamDirection,
    transport: Transport,
    stream: S2sStream,
    role: Role,
    activity: Arc<Activity>,
    tokens: StreamTokens,
    started: Instant,
    negotiation_deadline: Instant,
    stage_deadline: Instant,
    sasl_deadline: Option<Instant>,
    phase: NegotiationPhase,
    incomplete_since: Option<Instant>,
    expiry: Option<Instant>,
    verified: Option<VerifiedPeer>,
    negotiation: Option<NegotiationSlot>,
    registered: Option<StreamSlot>,
    peer_stream: Option<VerifiedPeerStream>,
    tls_transport: S2sTransport,
    shutdown_sent: bool,
    evicted: bool,
    replaced: bool,
    admission_refused: bool,
    capacity_refusal: Option<S2sResource>,
}

impl Session {
    /// A session for the connection `start` describes.
    pub(super) fn new(shared: Arc<Shared>, start: SessionStart) -> Self {
        let deadlines = *shared.config.deadlines();
        let direction = match start.role {
            Role::Inbound { .. } => StreamDirection::Inbound,
            Role::Outbound { .. } => StreamDirection::Outbound,
        };
        let phase = start.stream.phase();
        Self {
            shared,
            connection: start.connection,
            direction,
            transport: start.transport,
            stream: start.stream,
            role: start.role,
            activity: start.activity,
            tokens: StreamTokens::default(),
            started: start.started,
            negotiation_deadline: start.started + deadlines.negotiation,
            stage_deadline: Instant::now() + deadlines.stage,
            sasl_deadline: None,
            phase,
            incomplete_since: None,
            expiry: None,
            verified: None,
            negotiation: None,
            registered: None,
            peer_stream: None,
            tls_transport: start.tls_transport,
            shutdown_sent: false,
            evicted: false,
            replaced: false,
            admission_refused: false,
            capacity_refusal: None,
        }
    }

    /// Hold the per-peer negotiation slot taken before the session started.
    pub(super) fn with_negotiation(mut self, slot: NegotiationSlot) -> Self {
        self.negotiation = Some(slot);
        self
    }

    /// Run the connection to its end, starting with `initial` output.
    pub(super) async fn run(mut self, initial: Vec<Output>) {
        let outcome = self.drive(initial).await;
        self.end(outcome).await;
    }

    async fn drive(&mut self, initial: Vec<Output>) -> S2sOutcome {
        let mut queue: VecDeque<Output> = initial.into();
        let mut buffer = vec![0u8; PLAINTEXT_CHUNK];
        loop {
            while let Some(output) = queue.pop_front() {
                match self.execute(output).await {
                    Ok(more) => queue.extend(more),
                    Err(outcome) => return outcome,
                }
            }
            self.track_phase();
            let (at, kind) = self.deadline();
            let shutdown = self.shared.shutdown.clone();
            let abort = self.shared.abort.clone();
            let evict = self.tokens.evict.clone();
            let replace = self.tokens.replace.clone();
            let event = tokio::select! {
                biased;
                () = abort.cancelled() => return S2sOutcome::Shutdown,
                () = shutdown.cancelled(), if !self.shutdown_sent => Event::Shutdown,
                () = evict.cancelled(), if !self.evicted => Event::Evicted,
                () = replace.cancelled(), if !self.replaced => Event::Replaced,
                () = tokio::time::sleep_until(at.into()) => Event::Deadline(kind),
                read = self.transport.read(&mut buffer) => Event::Read(read),
            };
            let outputs = match event {
                Event::Shutdown => {
                    self.shutdown_sent = true;
                    self.stream.signal(Signal::Shutdown)
                }
                Event::Evicted => {
                    self.evicted = true;
                    self.stream.signal(Signal::ResourceExhausted)
                }
                // A newer stream for the same pair took over.
                Event::Replaced => {
                    self.replaced = true;
                    self.stream.signal(Signal::Conflict)
                }
                Event::Deadline(DeadlineKind::Timeout) => self.stream.signal(Signal::Timeout),
                Event::Deadline(DeadlineKind::Expired) => self.stream.signal(Signal::Expired),
                Event::Read(Ok(0)) => return S2sOutcome::PeerClosed,
                Event::Read(Ok(read)) => {
                    let outputs = self.stream.receive(&buffer[..read]);
                    if let Err(outcome) = self.after_receive() {
                        return outcome;
                    }
                    outputs
                }
                Event::Read(Err(TransportError::Tls(_))) => return S2sOutcome::Tls,
                Event::Read(Err(_)) => return S2sOutcome::Io,
            };
            queue.extend(outputs);
        }
    }

    /// Checks after the codec consumed input: per-peer capacity once the
    /// inbound header named the remote domain, and the unfinished-element
    /// deadline on an established stream.
    fn after_receive(&mut self) -> Result<(), S2sOutcome> {
        let negotiating = self.negotiation.is_none() && self.peer_stream.is_none();
        if let (Role::Inbound { admission, .. }, true) = (&self.role, negotiating) {
            match admission.claim() {
                Claim::Admitted { remote, .. } if self.negotiation.is_none() => {
                    match self.shared.registry.begin_negotiation(&remote) {
                        Ok(slot) => self.negotiation = Some(slot),
                        // Capacity is unavailable before the stream is
                        // safe to continue: close without further XML.
                        Err(resource) => {
                            self.capacity_refusal = Some(resource);
                            return Err(S2sOutcome::Capacity);
                        }
                    }
                }
                Claim::Refused => self.admission_refused = true,
                Claim::Admitted { .. } | Claim::None => {}
            }
        }
        if self.phase == NegotiationPhase::Established {
            if self.stream.retained_bytes() > 0 {
                self.incomplete_since.get_or_insert_with(Instant::now);
            } else {
                self.incomplete_since = None;
            }
        }
        Ok(())
    }

    /// Execute one codec output; `Err` ends the connection.
    async fn execute(&mut self, output: Output) -> Result<Vec<Output>, S2sOutcome> {
        match output {
            Output::Transmit(bytes) => {
                let write = self.shared.config.deadlines().write;
                match tokio::time::timeout(write, self.transport.write_all(&bytes)).await {
                    Ok(Ok(())) => Ok(Vec::new()),
                    Ok(Err(TransportError::Tls(_))) => Err(S2sOutcome::Tls),
                    Ok(Err(_)) => Err(S2sOutcome::Io),
                    Err(_) => Err(S2sOutcome::Timeout),
                }
            }
            Output::StartTls(handoff) => {
                let (local, remote) = self.pair().ok_or(S2sOutcome::Protocol)?;
                let request = TlsRequest {
                    direction: self.direction,
                    local,
                    remote,
                    handoff: handoff.into_bytes(),
                    deadline: self.handshake_deadline(),
                };
                let verified = establish_tls(&self.shared, &mut self.transport, request)
                    .await
                    .map_err(|failure| {
                        self.capacity_refusal = failure.resource;
                        failure.outcome
                    })?;
                let peer = verified.tls_peer();
                self.verified = Some(verified);
                Ok(self.stream.tls_established(peer))
            }
            Output::Authorize(request) => Ok(self.authorize(&request)),
            Output::Established(_) => self.established(),
            Output::Stanza(stanza) => {
                let verdict = match &self.peer_stream {
                    Some(stream) => self.shared.sink.offer(stream, stanza),
                    None => SinkVerdict::Refuse,
                };
                match verdict {
                    SinkVerdict::Refuse => Ok(self.stream.signal(Signal::PolicyViolation)),
                }
            }
            Output::Close(reason) => Err(self.close_outcome(reason)),
        }
    }

    /// The local and remote domain of this connection, once known.
    fn pair(&self) -> Option<(CanonicalDomain, CanonicalDomain)> {
        match &self.role {
            Role::Inbound { admission, .. } => match admission.claim() {
                Claim::Admitted { local, remote } => Some((local, remote)),
                Claim::Refused | Claim::None => None,
            },
            Role::Outbound { local, remote, .. } => Some((local.clone(), remote.clone())),
        }
    }

    fn handshake_deadline(&self) -> Instant {
        let stage = Instant::now() + self.shared.config.deadlines().stage;
        stage.min(self.negotiation_deadline)
    }

    /// Decide SASL EXTERNAL for the receiving role: the certificate still
    /// holds, the pair is the one TLS verified and admission still admits
    /// the remote domain; then the stream is registered, which refuses a
    /// duplicate pair and may evict an idle stream.
    fn authorize(&mut self, request: &AuthorizationRequest) -> Vec<Output> {
        let Some(verified) = self.verified.clone() else {
            return self
                .stream
                .authorization(request.exchange, AuthorizationDecision::Denied);
        };
        let Some(now) = self.shared.config.settings().now() else {
            // A clock that cannot tell the time fails the connection.
            return self
                .stream
                .authorization(request.exchange, AuthorizationDecision::Denied);
        };
        if !verified.is_current(now) {
            return self.stream.signal(Signal::Expired);
        }
        let remote = request.source.clone();
        let local = request.destination.clone();
        if &remote != verified.domain() || !self.shared.config.is_local(&local) {
            return self
                .stream
                .authorization(request.exchange, AuthorizationDecision::Denied);
        }
        if self.shared.config.admission().decide(&remote).is_err() {
            self.admission_refused = true;
            return self
                .stream
                .authorization(request.exchange, AuthorizationDecision::Denied);
        }
        match self.register(local, &verified) {
            Ok(()) => self
                .stream
                .authorization(request.exchange, AuthorizationDecision::Granted),
            Err(signal) => self.stream.signal(signal),
        }
    }

    /// Register the authenticated stream for `local` and the verified peer.
    fn register(&mut self, local: CanonicalDomain, verified: &VerifiedPeer) -> Result<(), Signal> {
        let tls = TlsParameters {
            version: self
                .transport
                .tls()
                .and_then(|connection| connection.protocol_version()),
            cipher_suite: self
                .transport
                .tls()
                .and_then(|connection| connection.negotiated_cipher_suite())
                .map(|suite| suite.suite()),
        };
        let stream = VerifiedPeerStream::pkix(
            self.connection,
            self.direction,
            local,
            verified,
            tls,
            self.shared.config.generation(),
        );
        match self.shared.registry.register(
            stream.clone(),
            Arc::clone(&self.activity),
            self.tokens.clone(),
        ) {
            Ok(slot) => {
                self.registered = Some(slot);
                self.peer_stream = Some(stream);
                Ok(())
            }
            Err(RegisterRefusal::Conflict) => Err(Signal::Conflict),
            Err(RegisterRefusal::Full(resource)) => {
                self.capacity_refusal = Some(resource);
                Err(Signal::ResourceExhausted)
            }
        }
    }

    /// The codec completed negotiation.
    fn established(&mut self) -> Result<Vec<Output>, S2sOutcome> {
        let verified = self.verified.clone().ok_or(S2sOutcome::Protocol)?;
        if let Role::Outbound { local, .. } = &self.role {
            // The initiating role has no authorization step: the receiving
            // server's certificate is checked again here.
            let current = self
                .shared
                .config
                .settings()
                .now()
                .is_some_and(|now| verified.is_current(now));
            if !current {
                return Ok(self.stream.signal(Signal::Expired));
            }
            let local = local.clone();
            if let Err(signal) = self.register(local, &verified) {
                return Ok(self.stream.signal(signal));
            }
        }
        let Some(stream) = self.peer_stream.clone() else {
            return Err(S2sOutcome::Protocol);
        };
        self.negotiation = None;
        self.expiry = Some(self.expiry_instant(&verified));
        match &mut self.role {
            Role::Inbound { slot, .. } => slot.authenticated(),
            Role::Outbound {
                remote,
                dial,
                established,
                ..
            } => {
                self.shared.registry.record_success(remote);
                *dial = None;
                if let Some(sender) = established.take() {
                    // The dialer may have stopped waiting.
                    let _ = sender.send(Ok(stream.clone()));
                }
            }
        }
        telemetry::established(
            self.direction,
            self.tls_transport,
            self.started.elapsed(),
            &self.shared.registry,
        );
        tracing::debug!(
            connection = self.connection,
            direction = ?self.direction,
            strength = stream.strength().code(),
            "s2s stream established"
        );
        Ok(Vec::new())
    }

    /// When an established stream must end: its certificate evidence or its
    /// maximum age, whichever comes first.
    fn expiry_instant(&self, verified: &VerifiedPeer) -> Instant {
        let now = Instant::now();
        let max_age = now + self.shared.config.deadlines().max_age;
        let remaining = self
            .shared
            .config
            .settings()
            .now()
            .map_or(Duration::ZERO, |wall| until(wall, verified.expires_at()));
        max_age.min(now + remaining)
    }

    /// Follow the negotiation phase to start stage and SASL deadlines.
    fn track_phase(&mut self) {
        let phase = self.stream.phase();
        if phase == self.phase {
            return;
        }
        let now = Instant::now();
        let deadlines = self.shared.config.deadlines();
        if is_sasl(phase) {
            if self.sasl_deadline.is_none() {
                self.sasl_deadline = Some(now + deadlines.sasl);
            }
        } else if phase != NegotiationPhase::Established {
            self.stage_deadline = now + deadlines.stage;
        }
        self.phase = phase;
    }

    /// The next deadline and what it means.
    fn deadline(&self) -> (Instant, DeadlineKind) {
        let deadlines = self.shared.config.deadlines();
        let now = Instant::now();
        if self.phase == NegotiationPhase::Established {
            let idle = now + deadlines.idle.saturating_sub(self.activity.idle(now));
            let incomplete = self
                .incomplete_since
                .map(|since| since + deadlines.incomplete_element);
            let timeout = incomplete.map_or(idle, |incomplete| incomplete.min(idle));
            return match self.expiry {
                Some(expiry) if expiry <= timeout => (expiry, DeadlineKind::Expired),
                _ => (timeout, DeadlineKind::Timeout),
            };
        }
        let stage = if is_sasl(self.phase) {
            self.sasl_deadline.unwrap_or(self.stage_deadline)
        } else {
            self.stage_deadline
        };
        (stage.min(self.negotiation_deadline), DeadlineKind::Timeout)
    }

    fn close_outcome(&self, reason: CloseReason) -> S2sOutcome {
        match reason {
            CloseReason::StreamError(condition) => match condition {
                StreamErrorCondition::ConnectionTimeout => S2sOutcome::Timeout,
                StreamErrorCondition::Reset => S2sOutcome::Expired,
                StreamErrorCondition::SystemShutdown => S2sOutcome::Shutdown,
                StreamErrorCondition::Conflict => S2sOutcome::Conflict,
                StreamErrorCondition::ResourceConstraint if self.evicted => S2sOutcome::Evicted,
                StreamErrorCondition::ResourceConstraint => S2sOutcome::Capacity,
                _ if self.admission_refused => S2sOutcome::Admission,
                _ => S2sOutcome::Protocol,
            },
            CloseReason::PeerStreamError(StreamErrorCondition::SeeOtherHost) => {
                S2sOutcome::Redirected
            }
            CloseReason::PeerStreamError(_) => S2sOutcome::PeerError,
            CloseReason::PeerClosed => S2sOutcome::PeerClosed,
            CloseReason::SaslFailure(_) if self.admission_refused => S2sOutcome::Admission,
            CloseReason::TlsRefused => S2sOutcome::Tls,
            CloseReason::SaslFailure(_)
            | CloseReason::SaslExhausted
            | CloseReason::LocalFailure
            | CloseReason::NotXml => S2sOutcome::Protocol,
        }
    }

    /// Close the transport, report the outcome and release every slot.
    async fn end(mut self, outcome: S2sOutcome) {
        if outcome != S2sOutcome::Shutdown || !self.shared.abort.is_cancelled() {
            let write = self.shared.config.deadlines().write;
            let _ = tokio::time::timeout(write, self.transport.close()).await;
        }
        if let Role::Outbound {
            remote,
            established,
            ..
        } = &mut self.role
        {
            // An attempt that never established backs off when the peer or
            // the path to it failed; one that did was recorded as a
            // success.
            if let Some(sender) = established.take() {
                if is_peer_failure(outcome) {
                    self.shared.registry.record_failure(remote);
                }
                let _ = sender.send(Err(outcome));
            }
        }
        if let Some(resource) = self.capacity_refusal {
            telemetry::capacity_refused(self.direction, resource);
        }
        telemetry::ended(self.direction, outcome);
        tracing::debug!(
            connection = self.connection,
            direction = ?self.direction,
            outcome = ?outcome,
            "s2s connection ended"
        );
        // Unregister before reporting the new stream count.
        self.registered = None;
        telemetry::stream_counts(&self.shared.registry);
    }
}

/// Whether an outbound attempt that ended with `outcome` failed because of
/// the peer or the path to it, which backs the domain off. Local capacity
/// and shutdown do not, and neither does a refusal of the domain's own
/// admission, which no retry changes.
pub(super) fn is_peer_failure(outcome: S2sOutcome) -> bool {
    matches!(
        outcome,
        S2sOutcome::Certificate
            | S2sOutcome::Tls
            | S2sOutcome::Protocol
            | S2sOutcome::PeerError
            | S2sOutcome::Redirected
            | S2sOutcome::PeerClosed
            | S2sOutcome::Timeout
            | S2sOutcome::Expired
            | S2sOutcome::Conflict
            | S2sOutcome::Io
    )
}

fn is_sasl(phase: NegotiationPhase) -> bool {
    matches!(
        phase,
        NegotiationPhase::SaslAwaitAuth
            | NegotiationPhase::SaslAwaitResponse
            | NegotiationPhase::SaslAuthorizing
            | NegotiationPhase::SaslAwaitOutcome
    )
}

/// The wall-clock time from `now` to `then`, zero when it has passed.
fn until(now: UnixTime, then: UnixTime) -> Duration {
    Duration::from_secs(then.as_secs().saturating_sub(now.as_secs()))
}

/// Why TLS could not be established.
pub(super) struct TlsFailure {
    pub(super) outcome: S2sOutcome,
    pub(super) resource: Option<S2sResource>,
}

impl TlsFailure {
    fn of(outcome: S2sOutcome) -> Self {
        Self {
            outcome,
            resource: None,
        }
    }
}

/// One TLS handshake to run.
pub(super) struct TlsRequest {
    pub(super) direction: StreamDirection,
    pub(super) local: CanonicalDomain,
    /// The reference identity: the stream source on an inbound connection
    /// and the stream destination on an outbound one.
    pub(super) remote: CanonicalDomain,
    /// Ciphertext that arrived with the plaintext stream.
    pub(super) handoff: Vec<u8>,
    pub(super) deadline: Instant,
}

/// Run the TLS handshake `request` describes and return what the verifier
/// established for the completed handshake. A presented certificate that
/// fails verification is an active certificate failure (ADR-0020 D2).
pub(super) async fn establish_tls(
    shared: &Shared,
    transport: &mut Transport,
    request: TlsRequest,
) -> Result<VerifiedPeer, TlsFailure> {
    let _handshake = shared
        .capacity
        .handshake(request.direction)
        .map_err(|resource| TlsFailure {
            outcome: S2sOutcome::Capacity,
            resource: Some(resource),
        })?;
    let settings = shared.config.settings();
    let verifier = PeerVerifier::new(
        shared.config.trust(),
        request.remote.clone(),
        settings.algorithms(),
    )
    .map(Arc::new)
    .map_err(|_| TlsFailure::of(S2sOutcome::Admission))?;
    let credentials = shared
        .config
        .credentials(&request.local)
        .ok_or(TlsFailure::of(S2sOutcome::Protocol))?;
    let tls: Connection = match request.direction {
        StreamDirection::Inbound => {
            let config = receiving_config(settings, credentials, Arc::clone(&verifier))
                .map_err(|_| TlsFailure::of(S2sOutcome::Tls))?;
            ServerConnection::new(Arc::new(config))
                .map_err(|_| TlsFailure::of(S2sOutcome::Tls))?
                .into()
        }
        StreamDirection::Outbound => {
            let config = initiating_config(
                settings,
                credentials,
                Arc::clone(&verifier),
                OutboundTransport::StartTls,
            )
            .map_err(|_| TlsFailure::of(S2sOutcome::Tls))?;
            ClientConnection::new(Arc::new(config), verifier.reference().server_name())
                .map_err(|_| TlsFailure::of(S2sOutcome::Tls))?
                .into()
        }
    };
    // Each direction has its own workers, so a flood of inbound handshakes
    // cannot hold every worker an outbound handshake needs.
    let workers = match request.direction {
        StreamDirection::Inbound => &shared.inbound_workers,
        StreamDirection::Outbound => &shared.outbound_workers,
    };
    let handshake = transport.handshake(tls, request.handoff, workers);
    let failed = |verifier: &PeerVerifier, outcome: S2sOutcome| {
        if let Verification::Failed(error) = verifier.verification() {
            tracing::debug!(code = error.code(), "s2s peer certificate refused");
            TlsFailure::of(S2sOutcome::Certificate)
        } else {
            TlsFailure::of(outcome)
        }
    };
    match tokio::time::timeout_at(request.deadline.into(), handshake).await {
        Err(_) => Err(TlsFailure::of(S2sOutcome::Timeout)),
        Ok(Err(TransportError::Workers(_))) => Err(TlsFailure {
            outcome: S2sOutcome::Capacity,
            resource: Some(S2sResource::Handshakes),
        }),
        Ok(Err(TransportError::Io(_) | TransportError::HandshakeEof)) => {
            Err(failed(&verifier, S2sOutcome::Io))
        }
        Ok(Err(_)) => Err(failed(&verifier, S2sOutcome::Tls)),
        Ok(Ok(())) => {
            let evidence = transport
                .tls()
                .and_then(|connection| verifier.established(connection));
            evidence.ok_or_else(|| failed(&verifier, S2sOutcome::Certificate))
        }
    }
}
