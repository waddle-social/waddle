//! One S2S connection's task: it drives the codec and negotiation over the
//! transport, runs the TLS handshake on the verifier workers, decides SASL
//! EXTERNAL authorization, verifies an inbound peer's dialback key through
//! a verification connection and enforces the deadlines.
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
    AuthorizationDecision, AuthorizationRequest, CloseReason, DialbackError, DialbackOutcome,
    DialbackRequest, NegotiatedStream, NegotiationPhase, Output, S2sStream, Signal,
    StreamAuthentication, StreamErrorCondition, TlsPeer,
};
use waddle_xmpp::telemetry::attributes::{
    S2sDialbackResult, S2sDialbackRole, S2sOutcome, S2sResource, S2sTransport,
};

use super::admission::{Claim, ConnectionAdmission};
use super::capacity::{InboundSlot, OutboundSlot, SocketCount};
use super::outbox::{self, OutboxReceiver, Transmission, Untransmitted};
use super::peer_stream::{
    AuthenticationStrength, PeerStreamParts, StreamDirection, TlsParameters, VerifiedPeerStream,
};
use super::registry::{
    Activity, DialSlot, NegotiationSlot, RegisterRefusal, Registration, StreamSlot, StreamTokens,
};
use super::service::Shared;
use super::sink::{SinkVerdict, SinkWait, StreamEnd, Waited};
use super::telemetry;
use super::transport::{Transport, TransportError};
use super::verification;
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
    /// The idle timeout, or a negotiation stage's deadline.
    Timeout,
    /// An established stream's first-level element stayed unfinished for
    /// longer than the incomplete-element deadline allows.
    Incomplete,
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
    /// The verification of the inbound peer's dialback key ended; `None`
    /// when its task ended without an answer.
    Dialback(Option<DialbackOutcome>),
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
    /// The end of a dialback exchange, from when it started.
    dialback_deadline: Option<Instant>,
    phase: NegotiationPhase,
    incomplete: Option<Unfinished>,
    expiry: Option<Instant>,
    verified: Option<VerifiedPeer>,
    /// The verification of the inbound peer's dialback key, in progress.
    /// Dropping the receiver cancels the verification.
    pending_dialback: Option<oneshot::Receiver<DialbackOutcome>>,
    /// An outbound stream sent its dialback key and awaits the answer, which
    /// telemetry has not counted yet. Telemetry counts the answer itself
    /// when it arrives; this covers a stream that ends without one.
    awaiting_dialback_result: bool,
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
    /// The stream ended without an orderly close and may have lost stanzas
    /// in flight: the transport reached end of file, the idle timeout or
    /// the maximum age while the peer had a first-level element unfinished,
    /// a write did not complete within its deadline, the peer left a
    /// first-level element unfinished past its deadline, the stanza sink
    /// could not take a stanza within its deadline, or the stream ended for
    /// a deadline, eviction or replacement before reading again after a
    /// wait for the sink.
    lost: bool,
    /// The connection waited for the stanza sink and has not read from the
    /// peer since. The peer may have written stanzas meanwhile that are
    /// still unread in the transport, so ending the stream now for a
    /// deadline, eviction or replacement may lose them.
    waited_since_read: bool,
    /// The peer ended the stream with `connection-timeout`: it closed an
    /// idle stream, as this server does, and nothing is known to be lost.
    peer_idle_close: bool,
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
            dialback_deadline: None,
            phase,
            incomplete: None,
            expiry: None,
            verified: None,
            pending_dialback: None,
            awaiting_dialback_result: false,
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
            lost: false,
            waited_since_read: false,
            peer_idle_close: false,
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
            let mut pending = self.pending_dialback.take();
            let event = {
                let dialback = async {
                    match pending.as_mut() {
                        Some(receiver) => receiver.await.ok(),
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    biased;
                    () = abort.cancelled() => return S2sOutcome::Shutdown,
                    () = shutdown.cancelled(), if !self.shutdown_sent => Event::Shutdown,
                    () = evict.cancelled(), if !self.evicted => Event::Evicted,
                    () = replace.cancelled(), if !self.replaced => Event::Replaced,
                    outcome = dialback => Event::Dialback(outcome),
                    () = tokio::time::sleep_until(at.into()) => Event::Deadline(kind),
                    read = self.transport.read(&mut buffer) => Event::Read(read),
                    transmission = next_transmission(&mut self.outbox), if transmitting => {
                        Event::Transmit(transmission)
                    }
                }
            };
            if !matches!(event, Event::Dialback(_)) {
                self.pending_dialback = pending;
            }
            let outputs = match event {
                Event::Shutdown => {
                    self.shutdown_sent = true;
                    self.stream.signal(Signal::Shutdown)
                }
                Event::Evicted => {
                    self.lose_unread();
                    self.evicted = true;
                    self.stream.signal(Signal::ResourceExhausted)
                }
                // A newer stream for the same pair took over.
                Event::Replaced => {
                    self.lose_unread();
                    self.replaced = true;
                    self.stream.signal(Signal::Conflict)
                }
                // A read that waited for the input rates after the
                // deadline was computed moved the incomplete-element
                // deadline later; one that is no longer due is not acted on.
                Event::Deadline(_) if self.deadline().0 > Instant::now() => continue,
                // The idle timeout and the maximum age end the stream in an
                // orderly way unless the peer was in the middle of an
                // element, which is then lost, or the connection has not
                // read since it waited for the sink, so that what the peer
                // wrote meanwhile is lost unread.
                Event::Deadline(DeadlineKind::Timeout) => {
                    self.lose_unread();
                    self.lose_unfinished();
                    self.stream.signal(Signal::Timeout)
                }
                Event::Deadline(DeadlineKind::Incomplete) => {
                    self.lost = true;
                    self.stream.signal(Signal::Timeout)
                }
                Event::Deadline(DeadlineKind::Expired) => {
                    self.lose_unread();
                    self.lose_unfinished();
                    self.stream.signal(Signal::Expired)
                }
                // Either side's closing tag ends the connection as soon as
                // the codec reports it, so end of file here arrived before
                // any close: the peer dropped the connection. Peers drop
                // idle connections this way, so it loses something only
                // when the peer was in the middle of an element. Nothing
                // the peer wrote is left unread behind an end of file,
                // whether or not the connection waited for the sink before.
                Event::Read(Ok(0)) => {
                    self.lose_unfinished();
                    return S2sOutcome::PeerClosed;
                }
                Event::Read(Ok(read)) => {
                    self.waited_since_read = false;
                    let outputs = self.stream.receive(&buffer[..read]);
                    self.after_receive(!outputs.is_empty());
                    outputs
                }
                Event::Read(Err(TransportError::Tls(_))) => return S2sOutcome::Tls,
                Event::Read(Err(_)) => return S2sOutcome::Io,
                Event::Dialback(outcome) => self.dialback_verified(
                    outcome.unwrap_or(DialbackOutcome::Error(DialbackError::RemoteServerTimeout)),
                ),
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

    /// Record that the stream lost a stanza if it ends now while the peer
    /// has a first-level element unfinished.
    fn lose_unfinished(&mut self) {
        if self.incomplete.is_some() {
            self.lost = true;
        }
    }

    /// Record that the stream may have lost stanzas if it ends now without
    /// having read since it waited for the sink: the peer may have written
    /// more while the connection read nothing, and those stanzas would be
    /// dropped unread. A peer that wrote nothing more is reported the same
    /// way, since nothing tells the two apart without reading.
    fn lose_unread(&mut self) {
        if self.waited_since_read {
            self.lost = true;
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
                    // The peer did not read in time: what was being written
                    // may be partly sent.
                    Err(_) => {
                        self.lost = true;
                        Err(S2sOutcome::Timeout)
                    }
                }
            }
            Output::StartTls(handoff) => {
                let (local, remote) = self.pair().ok_or(S2sOutcome::Protocol)?;
                // A receiving connection lets the peer present no
                // certificate only where it may then use dialback: as an
                // Initiating Server for a listed domain, or as a Receiving
                // Server asking this deployment to verify a key.
                let config = &self.shared.config;
                let client_certificate_optional = self.direction == StreamDirection::Inbound
                    && (config.dialback().outbound() || config.permits_inbound_dialback(&remote));
                let request = TlsRequest {
                    direction: self.direction,
                    local,
                    remote,
                    handoff: handoff.into_bytes(),
                    deadline: self.handshake_deadline(),
                    transport: match self.tls_transport {
                        S2sTransport::StartTls => OutboundTransport::StartTls,
                        S2sTransport::DirectTls => OutboundTransport::DirectTls,
                    },
                    client_certificate_optional,
                    budget: HandshakeBudget::Connection,
                };
                // A drain does not wait for a handshake: no XML, and so no
                // `system-shutdown`, can be sent during one.
                let shutdown = self.shared.shutdown.clone();
                let handshake = tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return Err(S2sOutcome::Shutdown),
                    handshake = establish_tls(&self.shared, &mut self.transport, request) => handshake,
                };
                let outcome = handshake.map_err(|failure| {
                    self.capacity_refusal = failure.resource;
                    failure.outcome
                })?;
                let verified = match outcome {
                    TlsOutcome::Verified(verified) => verified,
                    // No certificate: the claimed domain is unproven, so
                    // its per-peer budget is not touched; the connection
                    // stays within the unauthenticated limits.
                    TlsOutcome::NoCertificate => {
                        return Ok(self.stream.tls_established(TlsPeer::NoCertificate))
                    }
                };
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
            Output::VerifyDialback(request) => Ok(self.verify_dialback(request)),
            Output::DialbackAnswered { valid } => {
                telemetry::dialback(S2sDialbackRole::Authoritative, verdict_result(valid));
                Ok(Vec::new())
            }
            // Only a verification connection reports a verdict, and it runs
            // without a session.
            Output::VerifyVerdict(_) => Ok(Vec::new()),
            Output::Established(negotiated) => self.established(negotiated),
            Output::Stanza(stanza) => {
                let verdict = match &self.peer_stream {
                    Some(stream) => self.shared.sink.offer(stream, stanza),
                    None => SinkVerdict::Refuse,
                };
                self.sink_verdict(verdict, Signal::PolicyViolation).await
            }
            Output::MalformedAddress(stanza) => {
                let verdict = match &self.peer_stream {
                    Some(stream) => self.shared.sink.offer_malformed(stream, stanza),
                    None => SinkVerdict::Refuse,
                };
                self.sink_verdict(verdict, Signal::ImproperAddressing).await
            }
            Output::Close(reason) => {
                if reason == CloseReason::PeerStreamError(StreamErrorCondition::ConnectionTimeout) {
                    self.peer_idle_close = true;
                }
                if self.direction == StreamDirection::Outbound {
                    // The receiving server's answer, also when it arrived
                    // with the features in one read; otherwise a stream that
                    // ends after its key was sent counts as an error.
                    let result = match reason {
                        CloseReason::DialbackRefused => Some(S2sDialbackResult::Invalid),
                        CloseReason::DialbackFailed => Some(S2sDialbackResult::Error),
                        _ if self.awaiting_dialback_result => Some(S2sDialbackResult::Error),
                        _ => None,
                    };
                    if let Some(result) = result {
                        telemetry::dialback(S2sDialbackRole::Initiating, result);
                    }
                    self.awaiting_dialback_result = false;
                }
                Err(self.close_outcome(reason))
            }
        }
    }

    /// Act on what the sink did with a stanza: a refusal closes the stream
    /// with `refusal`, and a wait holds the connection, reading nothing
    /// more from the peer, until the sink takes the stanza.
    async fn sink_verdict(
        &mut self,
        verdict: SinkVerdict,
        refusal: Signal,
    ) -> Result<Vec<Output>, S2sOutcome> {
        match verdict {
            SinkVerdict::Accepted => Ok(Vec::new()),
            SinkVerdict::Refuse => Ok(self.stream.signal(refusal)),
            SinkVerdict::Wait(wait) => Ok(self.wait_for_sink(wait).await),
        }
    }

    /// Wait for the sink to take a stanza. The stream's own ends still
    /// apply meanwhile: shutdown, eviction and replacement end the wait and
    /// the stream, and the stanza is lost. A wait the sink gives up on ends
    /// the stream with `resource-constraint`, and one the sink ends because
    /// it is stopping with `system-shutdown`; either loses the stanza. Time
    /// spent waiting is not counted against the element the peer may have
    /// left unfinished behind the stanza, since nothing was read. A stanza
    /// the sink takes counts as traffic, so a stream that waited is neither
    /// idle nor a candidate for eviction because of the wait.
    async fn wait_for_sink(&mut self, wait: SinkWait) -> Vec<Output> {
        /// What ended a wait before the sink did.
        enum Interrupted {
            Shutdown,
            Evicted,
            Replaced,
        }
        self.waited_since_read = true;
        let started = Instant::now();
        let shutdown = self.shared.shutdown.clone();
        let evict = self.tokens.evict.clone();
        let replace = self.tokens.replace.clone();
        let interrupted = tokio::select! {
            biased;
            () = shutdown.cancelled(), if !self.shutdown_sent => Some(Interrupted::Shutdown),
            () = evict.cancelled(), if !self.evicted => Some(Interrupted::Evicted),
            () = replace.cancelled(), if !self.replaced => Some(Interrupted::Replaced),
            waited = wait => match waited {
                Waited::Taken => {
                    self.activity.traffic();
                    None
                }
                Waited::Overloaded => {
                    self.lost = true;
                    return self.stream.signal(Signal::ResourceExhausted);
                }
                Waited::Stopping => Some(Interrupted::Shutdown),
            },
        };
        if let Some(unfinished) = &mut self.incomplete {
            unfinished.since += started.elapsed();
        }
        let Some(interrupted) = interrupted else {
            return Vec::new();
        };
        self.lost = true;
        match interrupted {
            Interrupted::Shutdown => {
                self.shutdown_sent = true;
                self.stream.signal(Signal::Shutdown)
            }
            Interrupted::Evicted => {
                self.evicted = true;
                self.stream.signal(Signal::ResourceExhausted)
            }
            Interrupted::Replaced => {
                self.replaced = true;
                self.stream.signal(Signal::Conflict)
            }
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
        match self.shared.registry.check(
            self.direction,
            &local,
            &remote,
            AuthenticationStrength::Pkix,
            reserved,
        ) {
            Ok(()) => self
                .stream
                .authorization(request.exchange, AuthorizationDecision::Granted),
            Err(refusal) => {
                let signal = self.refused(refusal);
                self.stream.signal(signal)
            }
        }
    }

    /// Verify an inbound peer's dialback key (XEP-0220 §2.2.1) with the
    /// Authoritative Server of its claimed domain, on a verification
    /// connection that runs in its own task. The domain is checked first:
    /// still admitted, inbound dialback still permitted for it, and a
    /// stream the registry could take, so nothing is dialled for a stream
    /// that would be refused anyway. The task ends as soon as this session
    /// stops waiting for it, which happens when the session ends for any
    /// reason, so no verification outlives the stream it is for.
    fn verify_dialback(&mut self, request: DialbackRequest) -> Vec<Output> {
        if self.direction != StreamDirection::Inbound {
            return self.stream.signal(Signal::PolicyViolation);
        }
        if let Err(signal) = self.dialback_admissible(&request.destination, &request.source) {
            return self.stream.signal(signal);
        }
        let deadlines = self.shared.config.deadlines();
        let deadline = (Instant::now() + deadlines.dialback).min(self.negotiation_deadline);
        let (sender, receiver) = oneshot::channel();
        let shared = Arc::clone(&self.shared);
        let verification = async move {
            let mut sender = sender;
            let outcome = tokio::select! {
                biased;
                // The session ended: drop the lookup, the connection and
                // its handshake wait.
                () = sender.closed() => return,
                outcome = verification::verify_key(&shared, request, deadline) => outcome,
            };
            let _ = sender.send(outcome);
        };
        if !self.shared.spawn(verification) {
            return self.stream.signal(Signal::Shutdown);
        }
        self.pending_dialback = Some(receiver);
        Vec::new()
    }

    /// Answer the inbound peer's dialback request with the verification's
    /// outcome. A valid key is accepted only if the domain is still
    /// admissible when the answer arrives; one that no longer is counts as
    /// an error, not as a valid key.
    fn dialback_verified(&mut self, outcome: DialbackOutcome) -> Vec<Output> {
        if outcome == DialbackOutcome::Valid {
            let admissible = match self.pair() {
                Some((local, remote)) => self.dialback_admissible(&local, &remote),
                None => Err(Signal::PolicyViolation),
            };
            if let Err(signal) = admissible {
                telemetry::dialback(S2sDialbackRole::Receiving, S2sDialbackResult::Error);
                return self.stream.signal(signal);
            }
        }
        telemetry::dialback(S2sDialbackRole::Receiving, outcome_result(outcome));
        self.stream.dialback(outcome)
    }

    /// Whether an inbound stream from `remote` to `local` may still be
    /// authenticated by dialback and registered.
    fn dialback_admissible(
        &mut self,
        local: &CanonicalDomain,
        remote: &CanonicalDomain,
    ) -> Result<(), Signal> {
        let config = Arc::clone(&self.shared.config);
        if !config.is_local(local)
            || config.admission().decide(remote).is_err()
            || !config.permits_inbound_dialback(remote)
        {
            self.admission_refused = true;
            return Err(Signal::PolicyViolation);
        }
        let reserved = config.admission().is_reserved_peer(remote);
        self.shared
            .registry
            .check(
                self.direction,
                local,
                remote,
                AuthenticationStrength::Dialback,
                reserved,
            )
            .map_err(|refusal| self.refused(refusal))
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

    /// Register the authenticated stream for the pair, with the verifier's
    /// evidence for the remote certificate where there is one.
    fn register(
        &mut self,
        local: CanonicalDomain,
        remote: CanonicalDomain,
        strength: AuthenticationStrength,
        verified: Option<&VerifiedPeer>,
    ) -> Result<(), Signal> {
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
        let stream = VerifiedPeerStream::new(PeerStreamParts {
            connection: self.connection,
            direction: self.direction,
            local,
            remote,
            strength,
            peer: verified,
            tls,
            generation: self.shared.config.generation(),
        });
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

    /// The codec completed negotiation: with the post-SASL stream header,
    /// or by dialback. A certificate, which every outbound stream and every
    /// SASL EXTERNAL stream has, is checked again for currency, which is
    /// the only check an outbound stream gets after TLS, and the stream is
    /// registered in either direction. Only an inbound dialback stream may
    /// lack a certificate.
    fn established(&mut self, negotiated: NegotiatedStream) -> Result<Vec<Output>, S2sOutcome> {
        let strength = match negotiated.authentication {
            StreamAuthentication::SaslExternal => AuthenticationStrength::Pkix,
            StreamAuthentication::Dialback => AuthenticationStrength::Dialback,
        };
        let (local, remote) = match self.direction {
            StreamDirection::Inbound => (negotiated.destination, negotiated.source),
            StreamDirection::Outbound => (negotiated.source, negotiated.destination),
        };
        let verified = self.verified.clone();
        let certificate_required =
            strength == AuthenticationStrength::Pkix || self.direction == StreamDirection::Outbound;
        match &verified {
            None if certificate_required => return Err(S2sOutcome::Protocol),
            None => {}
            Some(verified) => {
                if verified.domain() != &remote {
                    return Err(S2sOutcome::Protocol);
                }
                let current = self
                    .shared
                    .config
                    .settings()
                    .now()
                    .is_some_and(|now| verified.is_current(now));
                if !current {
                    return Ok(self.stream.signal(Signal::Expired));
                }
            }
        }
        if let Err(signal) = self.register(local, remote, strength, verified.as_ref()) {
            return Ok(self.stream.signal(signal));
        }
        let Some(stream) = self.peer_stream.clone() else {
            return Err(S2sOutcome::Protocol);
        };
        self.negotiation = None;
        self.expiry = Some(self.expiry_instant(verified.as_ref()));
        if strength == AuthenticationStrength::Dialback
            && self.direction == StreamDirection::Outbound
        {
            telemetry::dialback(S2sDialbackRole::Initiating, S2sDialbackResult::Valid);
        }
        self.awaiting_dialback_result = false;
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
            stream.strength(),
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

    /// When an established stream must end: its certificate evidence, if it
    /// has a certificate, or its maximum age, whichever comes first.
    fn expiry_instant(&self, verified: Option<&VerifiedPeer>) -> Instant {
        let now = Instant::now();
        let max_age = now + self.shared.config.deadlines().max_age;
        let Some(verified) = verified else {
            return max_age;
        };
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
        } else if is_dialback_exchange(phase) {
            if self.dialback_deadline.is_none() {
                self.dialback_deadline = Some(now + deadlines.dialback);
            }
            if phase == NegotiationPhase::DialbackAwaitResult {
                self.awaiting_dialback_result = true;
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
            let (timeout, kind) = match incomplete {
                Some(incomplete) if incomplete < idle => (incomplete, DeadlineKind::Incomplete),
                _ => (idle, DeadlineKind::Timeout),
            };
            return match self.expiry {
                Some(expiry) if expiry <= timeout => (expiry, DeadlineKind::Expired),
                _ => (timeout, kind),
            };
        }
        let stage = if is_sasl(self.phase) {
            self.sasl_deadline.unwrap_or(self.stage_deadline)
        } else if is_dialback_exchange(self.phase) {
            self.dialback_deadline.unwrap_or(self.stage_deadline)
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
            // The peer keeps one stream per pair and refused or replaced
            // this one for another stream of the same pair, such as another
            // replica's: a local conflict seen from the other side, which
            // says nothing about the peer's health.
            CloseReason::PeerStreamError(StreamErrorCondition::Conflict) => S2sOutcome::Conflict,
            CloseReason::PeerStreamError(_) => S2sOutcome::PeerError,
            CloseReason::PeerClosed => S2sOutcome::PeerClosed,
            CloseReason::SaslFailure(_) if self.admission_refused => S2sOutcome::Admission,
            CloseReason::TlsRefused => S2sOutcome::Tls,
            CloseReason::DialbackRefused | CloseReason::DialbackFailed => S2sOutcome::Dialback,
            CloseReason::VerificationComplete => S2sOutcome::Verified,
            CloseReason::SaslFailure(_)
            | CloseReason::SaslExhausted
            | CloseReason::LocalFailure
            | CloseReason::NotXml => S2sOutcome::Protocol,
        }
    }

    /// Close the transport, report the outcome and release every slot.
    async fn end(mut self, outcome: S2sOutcome) {
        // Stop a verification of the peer's dialback key at once.
        self.pending_dialback = None;
        if self.awaiting_dialback_result {
            self.awaiting_dialback_result = false;
            telemetry::dialback(S2sDialbackRole::Initiating, S2sDialbackResult::Error);
        }
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
        if let Some(stream) = self.peer_stream.take() {
            self.shared.sink.stream_ended(
                &stream,
                stream_end(outcome, self.lost, self.peer_idle_close),
            );
        }
    }
}

/// How an established stream that ended with `outcome` ended, for the
/// sink. `lost` is whether it ended without an orderly close (end of file,
/// the idle timeout or the maximum age with an element unfinished, a write
/// past its deadline, an element left unfinished past its deadline, a
/// stanza the sink could not take in time or took while stopping, or the
/// idle timeout, the maximum age, eviction or replacement before the
/// connection read again after waiting for the sink), which may have lost
/// stanzas in flight whatever the outcome. `peer_idle_close` is whether the peer
/// closed the stream with `connection-timeout`, which is orderly. Otherwise
/// the idle timeout, the maximum age, end of file and either side's close
/// are orderly, and a stream error, a transport failure or a withdrawn
/// admission may have lost stanzas.
fn stream_end(outcome: S2sOutcome, lost: bool, peer_idle_close: bool) -> StreamEnd {
    if lost {
        return StreamEnd::Lost;
    }
    if peer_idle_close {
        return StreamEnd::Closed;
    }
    match outcome {
        S2sOutcome::Io
        | S2sOutcome::Tls
        | S2sOutcome::Protocol
        | S2sOutcome::PeerError
        | S2sOutcome::Admission
        | S2sOutcome::Certificate
        | S2sOutcome::Dialback => StreamEnd::Lost,
        S2sOutcome::Established
        | S2sOutcome::Capacity
        | S2sOutcome::Redirected
        | S2sOutcome::PeerClosed
        | S2sOutcome::Timeout
        | S2sOutcome::Expired
        | S2sOutcome::Evicted
        | S2sOutcome::Conflict
        | S2sOutcome::Shutdown
        | S2sOutcome::Verified => StreamEnd::Closed,
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
/// which no retry changes, nor a `conflict` sent by either side, which
/// another stream for the same pair caused and which says nothing about the
/// peer.
pub(super) fn is_peer_failure(outcome: S2sOutcome) -> bool {
    matches!(
        outcome,
        S2sOutcome::Certificate
            | S2sOutcome::Tls
            | S2sOutcome::Dialback
            | S2sOutcome::Protocol
            | S2sOutcome::PeerError
            | S2sOutcome::Redirected
            | S2sOutcome::PeerClosed
            | S2sOutcome::Timeout
            | S2sOutcome::Expired
            | S2sOutcome::Io
    )
}

/// Phases that wait for the other side of a dialback exchange, which may
/// involve a verification connection, under the dialback deadline.
fn is_dialback_exchange(phase: NegotiationPhase) -> bool {
    matches!(
        phase,
        NegotiationPhase::DialbackVerifying | NegotiationPhase::DialbackAwaitResult
    )
}

fn outcome_result(outcome: DialbackOutcome) -> S2sDialbackResult {
    match outcome {
        DialbackOutcome::Valid => S2sDialbackResult::Valid,
        DialbackOutcome::Invalid => S2sDialbackResult::Invalid,
        DialbackOutcome::Error(_) => S2sDialbackResult::Error,
    }
}

fn verdict_result(valid: bool) -> S2sDialbackResult {
    if valid {
        S2sDialbackResult::Valid
    } else {
        S2sDialbackResult::Invalid
    }
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
    /// and the stream destination on an outbound one. An outbound
    /// connection also sends it as the Server Name Indication.
    pub(super) remote: CanonicalDomain,
    /// Ciphertext that arrived with the plaintext stream.
    pub(super) handoff: Vec<u8>,
    pub(super) deadline: Instant,
    /// How an outbound connection reached TLS; direct TLS sends the
    /// `xmpp-server` ALPN protocol. Ignored inbound.
    pub(super) transport: OutboundTransport,
    /// Whether an inbound peer may complete the handshake without a
    /// certificate. Ignored outbound, where the receiving server must
    /// always present one.
    pub(super) client_certificate_optional: bool,
    pub(super) budget: HandshakeBudget,
}

/// Whose capacity a TLS handshake uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HandshakeBudget {
    /// An ordinary connection: its direction's handshake cap and verifier
    /// workers.
    Connection,
    /// A dialback verification connection: the verification cap, which the
    /// connection already holds, and the verification workers, so it never
    /// takes a handshake or a worker from an ordinary connection.
    Verification,
}

/// What a completed TLS handshake established about the peer.
pub(super) enum TlsOutcome {
    /// The peer's certificate verified for the reference identity.
    Verified(VerifiedPeer),
    /// The inbound peer presented no certificate, which the request
    /// allowed.
    NoCertificate,
}

/// Run the TLS handshake `request` describes and return what the verifier
/// established for the completed handshake.
///
/// The outcomes are classified for ADR-0020 D2 (gate G5): a presented
/// certificate that fails verification for the reference identity under
/// its trust profile is an active certificate failure, which fails the
/// handshake ([`S2sOutcome::Certificate`]), so no weaker authentication is
/// ever offered after it; a completed handshake without any certificate is
/// [`TlsOutcome::NoCertificate`], possible only where the request allows
/// it; everything else is a TLS, I/O, capacity or timeout failure.
pub(super) async fn establish_tls(
    shared: &Shared,
    transport: &mut Transport,
    request: TlsRequest,
) -> Result<TlsOutcome, TlsFailure> {
    let _handshake = match request.budget {
        HandshakeBudget::Connection => Some(shared.capacity.handshake(request.direction).map_err(
            |resource| TlsFailure {
                outcome: S2sOutcome::Capacity,
                resource: Some(resource),
            },
        )?),
        HandshakeBudget::Verification => None,
    };
    let settings = shared.config.settings();
    let client_certificate_optional =
        request.direction == StreamDirection::Inbound && request.client_certificate_optional;
    let verifier = PeerVerifier::new(
        shared.config.trust(),
        request.remote.clone(),
        settings.algorithms(),
    )
    .map(|verifier| {
        if client_certificate_optional {
            verifier.with_optional_client_certificate()
        } else {
            verifier
        }
    })
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
                request.transport,
            )
            .map_err(|_| TlsFailure::of(S2sOutcome::Tls))?;
            ClientConnection::new(Arc::new(config), verifier.reference().server_name())
                .map_err(|_| TlsFailure::of(S2sOutcome::Tls))?
                .into()
        }
    };
    // Each direction, and verification connections, have their own
    // workers, so a flood of handshakes of one kind cannot hold every
    // worker another kind needs.
    let workers = match (request.budget, request.direction) {
        (HandshakeBudget::Verification, _) => &shared.verification_workers,
        (HandshakeBudget::Connection, StreamDirection::Inbound) => &shared.inbound_workers,
        (HandshakeBudget::Connection, StreamDirection::Outbound) => &shared.outbound_workers,
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
            resource: Some(match request.budget {
                HandshakeBudget::Connection => S2sResource::Handshakes,
                HandshakeBudget::Verification => S2sResource::Verifications,
            }),
        }),
        Ok(Err(TransportError::Io(_) | TransportError::HandshakeEof)) => {
            Err(failed(&verifier, S2sOutcome::Io))
        }
        Ok(Err(_)) => Err(failed(&verifier, S2sOutcome::Tls)),
        Ok(Ok(())) => {
            if let Some(evidence) = transport
                .tls()
                .and_then(|connection| verifier.established(connection))
            {
                return Ok(TlsOutcome::Verified(evidence));
            }
            let absent = client_certificate_optional
                && transport
                    .tls()
                    .is_some_and(|connection| verifier.completed_without_certificate(connection));
            if absent {
                Ok(TlsOutcome::NoCertificate)
            } else {
                Err(failed(&verifier, S2sOutcome::Certificate))
            }
        }
    }
}
