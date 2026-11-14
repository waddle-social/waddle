//! One S2S connection's task: it drives the codec and negotiation over the
//! transport, runs the TLS handshake on the verifier workers, decides SASL
//! EXTERNAL authorization and enforces the deadlines.
//!
//! Negotiation deadlines run from fixed instants and are never renewed by
//! partial input: each stage (initial header, TLS handshake, each expected
//! restart) has its own deadline, the SASL phase has one from its start,
//! and the whole negotiation has one from the TCP accept or connect. An
//! established stream ends when its certificate evidence or its maximum
//! age runs out, when it has received and sent nothing, not even
//! whitespace, for the idle time, or when one unfinished element stays
//! unfinished too long. That last deadline charges the peer only for time
//! the server was willing to read: time its reads waited for the input
//! rates is added back, up to [`MAX_INCOMPLETE_ELEMENT_TIME`] for the
//! element in all, so a large stanza from a peer that is being read slowly
//! still arrives while a peer that stops sending is still closed.
//!
//! The end of the shutdown drain (abort) interrupts every step, including
//! a TLS handshake or a write in progress.
//!
//! An established outbound stream also writes the stanzas handed to it
//! through its outbox, one at a time, and reports each write; it stops
//! taking them once it starts to close, and returns those still waiting
//! unwritten.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::UnixTime;
use rustls::{ClientConnection, Connection, ServerConnection};
use tokio::sync::oneshot;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::s2s::{
    AuthorizationDecision, AuthorizationRequest, CloseReason, NegotiatedStream, NegotiationPhase,
    Output, S2sStream, Signal, StreamErrorCondition,
};
use waddle_xmpp::telemetry::attributes::{S2sOutcome, S2sResource, S2sTransport};

use super::admission::{Claim, ConnectionAdmission};
use super::capacity::{InboundSlot, OutboundSlot, SocketCount};
use super::outbox::{self, OutboxReceiver, Transmission, Untransmitted};
use super::peer_stream::{StreamDirection, TlsParameters, VerifiedPeerStream};
use super::registry::{
    Activity, DialSlot, NegotiationSlot, RegisterRefusal, Registration, StreamSlot, StreamTokens,
};
use super::service::Shared;
use super::sink::SinkVerdict;
use super::telemetry;
use super::transport::{Transport, TransportError};
use crate::s2s::config::MAX_INCOMPLETE_ELEMENT_TIME;
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
        slot: OutboundSlot,
        /// Held until the attempt succeeds or fails, so a second dial for
        /// the pair is refused meanwhile.
        dial: Option<DialSlot>,
        established: Option<EstablishedSender>,
    },
}

impl Role {
    /// The connection's place in the socket counts.
    fn socket(&self) -> Arc<SocketCount> {
        match self {
            Self::Inbound { slot, .. } => slot.socket(),
            Self::Outbound { slot, .. } => slot.socket(),
        }
    }
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

/// An unfinished first-level element on an established stream.
#[derive(Debug, Clone, Copy)]
struct Unfinished {
    /// When the element began, or the element before it completed.
    since: Instant,
    /// The transport's total wait for the input rates at `since`.
    waited: Duration,
}

impl Unfinished {
    /// When the element has had `allowed` of time the server was willing
    /// to read, given that reads have waited `waited` in total for the
    /// rates by now and the next read will wait `upcoming` before it
    /// starts; at the latest [`MAX_INCOMPLETE_ELEMENT_TIME`] after `since`.
    fn deadline(&self, allowed: Duration, waited: Duration, upcoming: Duration) -> Instant {
        let held_back = waited.saturating_sub(self.waited) + upcoming;
        (self.since + allowed + held_back).min(self.since + MAX_INCOMPLETE_ELEMENT_TIME)
    }
}

enum Event {
    Shutdown,
    Evicted,
    Replaced,
    Deadline(DeadlineKind),
    Read(Result<usize, TransportError>),
    /// A stanza for an established outbound stream, or `None` when its
    /// outbox can no longer receive any.
    Transmit(Option<Transmission>),
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
    incomplete: Option<Unfinished>,
    expiry: Option<Instant>,
    verified: Option<VerifiedPeer>,
    negotiation: Option<NegotiationSlot>,
    registered: Option<StreamSlot>,
    peer_stream: Option<VerifiedPeerStream>,
    /// Stanzas for an established outbound stream.
    outbox: Option<OutboxReceiver>,
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
            incomplete: None,
            expiry: None,
            verified: None,
            negotiation: None,
            registered: None,
            peer_stream: None,
            outbox: None,
            tls_transport: start.tls_transport,
            shutdown_sent: false,
            evicted: false,
            replaced: false,
            admission_refused: false,
            capacity_refusal: None,
        }
    }

    /// Hold the per-peer negotiation slot an outbound attempt took before
    /// dialling.
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
        let abort = self.shared.abort.clone();
        loop {
            while let Some(output) = queue.pop_front() {
                let executed = tokio::select! {
                    biased;
                    () = abort.cancelled() => return S2sOutcome::Shutdown,
                    executed = self.execute(output) => executed,
                };
                match executed {
                    Ok(more) => queue.extend(more),
                    Err(outcome) => return outcome,
                }
            }
            self.track_phase();
            let (at, kind) = self.deadline();
            let shutdown = self.shared.shutdown.clone();
            let evict = self.tokens.evict.clone();
            let replace = self.tokens.replace.clone();
            let transmitting = self.takes_transmissions();
            let event = tokio::select! {
                biased;
                () = abort.cancelled() => return S2sOutcome::Shutdown,
                () = shutdown.cancelled(), if !self.shutdown_sent => Event::Shutdown,
                () = evict.cancelled(), if !self.evicted => Event::Evicted,
                () = replace.cancelled(), if !self.replaced => Event::Replaced,
                () = tokio::time::sleep_until(at.into()) => Event::Deadline(kind),
                read = self.transport.read(&mut buffer) => Event::Read(read),
                transmission = next_transmission(&mut self.outbox), if transmitting => {
                    Event::Transmit(transmission)
                }
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
                // A read that waited for the input rates after the
                // deadline was computed moved the incomplete-element
                // deadline later; one that is no longer due is not acted on.
                Event::Deadline(_) if self.deadline().0 > Instant::now() => continue,
                Event::Deadline(DeadlineKind::Timeout) => self.stream.signal(Signal::Timeout),
                Event::Deadline(DeadlineKind::Expired) => self.stream.signal(Signal::Expired),
                Event::Read(Ok(0)) => return S2sOutcome::PeerClosed,
                Event::Read(Ok(read)) => {
                    let outputs = self.stream.receive(&buffer[..read]);
                    self.after_receive(!outputs.is_empty());
                    outputs
                }
                Event::Read(Err(TransportError::Tls(_))) => return S2sOutcome::Tls,
                Event::Read(Err(_)) => return S2sOutcome::Io,
                Event::Transmit(None) => {
                    self.outbox = None;
                    continue;
                }
                Event::Transmit(Some(transmission)) => {
                    let written = tokio::select! {
                        biased;
                        () = abort.cancelled() => return S2sOutcome::Shutdown,
                        written = self.transmit(transmission) => written,
                    };
                    match written {
                        Ok(()) => continue,
                        Err(outcome) => return outcome,
                    }
                }
            };
            queue.extend(outputs);
        }
    }

    /// Whether the stream takes stanzas from its outbox now: an established
    /// outbound stream that is not closing.
    fn takes_transmissions(&self) -> bool {
        self.outbox.is_some()
            && self.phase == NegotiationPhase::Established
            && !self.stream.is_closed()
            && !self.shutdown_sent
            && !self.evicted
            && !self.replaced
    }

    /// Write one stanza from the outbox and report the write to its sender.
    /// A stanza the codec refuses is reported and the stream continues; a
    /// failed write ends the connection, and its sender learns only that
    /// the outcome is unknown.
    async fn transmit(&mut self, transmission: Transmission) -> Result<(), S2sOutcome> {
        let Transmission { stanza, written } = transmission;
        let outputs = match self.stream.send_stanza(stanza) {
            Ok(outputs) => outputs,
            Err(error) => {
                let _ = written.send(Err(Untransmitted::Refused(error)));
                return Ok(());
            }
        };
        for output in outputs {
            // Dropping `written` on failure tells the sender the outcome is
            // unknown.
            self.execute(output).await?;
        }
        let _ = written.send(Ok(()));
        Ok(())
    }

    /// Bookkeeping after the codec consumed input. `progressed` is whether
    /// the input completed an element or a stream header or ended the
    /// stream, which is traffic; whitespace and partial elements are not.
    /// On an established stream, completing an element restarts the
    /// unfinished-element deadline for the element that follows it; partial
    /// input never does.
    fn after_receive(&mut self, progressed: bool) {
        if let Role::Inbound { admission, .. } = &self.role {
            if admission.claim() == Claim::Refused {
                self.admission_refused = true;
            }
        }
        if progressed {
            self.activity.traffic();
        }
        if self.stream.phase() == NegotiationPhase::Established {
            if self.stream.retained_bytes() == 0 {
                self.incomplete = None;
            } else if progressed || self.incomplete.is_none() {
                let now = Instant::now();
                self.incomplete = Some(Unfinished {
                    since: now,
                    waited: self.transport.throttled(now),
                });
            }
        }
    }

    /// Execute one codec output; `Err` ends the connection.
    async fn execute(&mut self, output: Output) -> Result<Vec<Output>, S2sOutcome> {
        match output {
            Output::Transmit(bytes) => {
                let write = self.shared.config.deadlines().write;
                match tokio::time::timeout(write, self.transport.write_all(&bytes)).await {
                    Ok(Ok(())) => {
                        self.activity.traffic();
                        Ok(Vec::new())
                    }
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
                // A drain does not wait for a handshake: no XML, and so no
                // `system-shutdown`, can be sent during one.
                let shutdown = self.shared.shutdown.clone();
                let handshake = tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return Err(S2sOutcome::Shutdown),
                    handshake = establish_tls(&self.shared, &mut self.transport, request) => handshake,
                };
                let verified = handshake.map_err(|failure| {
                    self.capacity_refusal = failure.resource;
                    failure.outcome
                })?;
                let peer = verified.tls_peer();
                let remote = verified.domain().clone();
                self.verified = Some(verified);
                let mut outputs = self.stream.tls_established(peer);
                if self.direction == StreamDirection::Inbound {
                    // TLS has proven the remote domain, so its per-peer
                    // budget applies from here; a claim that was never
                    // verified did not count against it.
                    match self
                        .shared
                        .registry
                        .begin_negotiation(StreamDirection::Inbound, &remote)
                    {
                        Ok(slot) => self.negotiation = Some(slot),
                        Err(resource) => {
                            self.capacity_refusal = Some(resource);
                            outputs.extend(self.stream.signal(Signal::ResourceExhausted));
                        }
                    }
                }
                Ok(outputs)
            }
            Output::Authorize(request) => Ok(self.authorize(&request)),
            Output::Established(negotiated) => self.established(negotiated),
            Output::Stanza(stanza) => {
                let verdict = match &self.peer_stream {
                    Some(stream) => self.shared.sink.offer(stream, stanza),
                    None => SinkVerdict::Refuse,
                };
                match verdict {
                    SinkVerdict::Accepted => Ok(Vec::new()),
                    SinkVerdict::Refuse => Ok(self.stream.signal(Signal::PolicyViolation)),
                }
            }
            Output::MalformedAddress(stanza) => {
                let verdict = match &self.peer_stream {
                    Some(stream) => self.shared.sink.offer_malformed(stream, stanza),
                    None => SinkVerdict::Refuse,
                };
                match verdict {
                    SinkVerdict::Accepted => Ok(Vec::new()),
                    SinkVerdict::Refuse => Ok(self.stream.signal(Signal::ImproperAddressing)),
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
    /// holds, the pair is the one TLS verified, admission still admits the
    /// remote domain and the registry could take the stream. The stream is
    /// registered only after the post-SASL header, so a peer that stops
    /// after SASL never replaces or evicts another stream; one the registry
    /// would refuse now is refused before SASL succeeds.
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
        let admission = self.shared.config.admission();
        if admission.decide(&remote).is_err() {
            self.admission_refused = true;
            return self
                .stream
                .authorization(request.exchange, AuthorizationDecision::Denied);
        }
        let reserved = admission.is_reserved_peer(&remote);
        match self
            .shared
            .registry
            .check(self.direction, &local, &remote, reserved)
        {
            Ok(()) => self
                .stream
                .authorization(request.exchange, AuthorizationDecision::Granted),
            Err(refusal) => {
                let signal = self.refused(refusal);
                self.stream.signal(signal)
            }
        }
    }

    /// The signal that closes a stream the registry refused.
    fn refused(&mut self, refusal: RegisterRefusal) -> Signal {
        match refusal {
            RegisterRefusal::Conflict => Signal::Conflict,
            RegisterRefusal::Full(resource) => {
                self.capacity_refusal = Some(resource);
                Signal::ResourceExhausted
            }
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
        let reserved = self
            .shared
            .config
            .admission()
            .is_reserved_peer(stream.remote());
        let (outbox, receiver) = match self.direction {
            StreamDirection::Outbound => {
                let (outbox, receiver) = outbox::outbox();
                (Some(outbox), Some(receiver))
            }
            StreamDirection::Inbound => (None, None),
        };
        let registration = Registration {
            stream: stream.clone(),
            activity: Arc::clone(&self.activity),
            tokens: self.tokens.clone(),
            socket: self.role.socket(),
            reserved,
            outbox,
        };
        match self.shared.registry.register(registration) {
            Ok(slot) => {
                self.registered = Some(slot);
                self.peer_stream = Some(stream);
                self.outbox = receiver;
                Ok(())
            }
            Err(refusal) => Err(self.refused(refusal)),
        }
    }

    /// The codec completed negotiation with the post-SASL stream header.
    /// The certificate is checked again for currency, which is the only
    /// check an outbound stream gets after TLS, and the stream is
    /// registered in either direction.
    fn established(&mut self, negotiated: NegotiatedStream) -> Result<Vec<Output>, S2sOutcome> {
        let verified = self.verified.clone().ok_or(S2sOutcome::Protocol)?;
        let current = self
            .shared
            .config
            .settings()
            .now()
            .is_some_and(|now| verified.is_current(now));
        if !current {
            return Ok(self.stream.signal(Signal::Expired));
        }
        let local = match self.direction {
            StreamDirection::Inbound => negotiated.destination,
            StreamDirection::Outbound => negotiated.source,
        };
        if let Err(signal) = self.register(local, &verified) {
            return Ok(self.stream.signal(signal));
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
            let incomplete = self.incomplete.map(|unfinished| {
                unfinished.deadline(
                    deadlines.incomplete_element,
                    self.transport.throttled(now),
                    self.transport.next_wait(now),
                )
            });
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
        // Stanzas still waiting were never written; their senders may send
        // them on another stream.
        if let Some(mut receiver) = self.outbox.take() {
            outbox::drain(&mut receiver);
        }
        let abort = self.shared.abort.clone();
        if !abort.is_cancelled() {
            // Abort interrupts the close too; the socket is dropped anyway.
            let write = self.shared.config.deadlines().write;
            tokio::select! {
                biased;
                () = abort.cancelled() => {}
                _ = tokio::time::timeout(write, self.transport.close()) => {}
            }
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

/// The next stanza from an outbox, or never when there is none.
async fn next_transmission(outbox: &mut Option<OutboxReceiver>) -> Option<Transmission> {
    match outbox {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// Whether an outbound attempt that ended with `outcome` failed because of
/// the peer or the path to it, which backs the domain off. Local capacity
/// and shutdown do not, nor does a refusal of the domain's own admission,
/// which no retry changes, nor a local `conflict`, which another stream for
/// the same pair caused and which says nothing about the peer.
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
