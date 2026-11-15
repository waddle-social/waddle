//! Dialback verification connections (XEP-0220 §2.1.2; ADR-0020 D2, gate
//! G5).
//!
//! When an inbound peer authenticates by dialback, this server, as the
//! Receiving Server, opens a separate connection to the Authoritative
//! Server of the peer's claimed domain, found like any other outbound
//! destination (configured endpoints or discovery, with the blocked
//! address classes), and asks it about the key.
//!
//! - The connection is TLS only, STARTTLS or direct TLS as the endpoint
//!   says, and the Authoritative Server's certificate is verified for the
//!   claimed domain under that domain's trust profile, exactly as on any
//!   outbound stream: DNS selects where to ask, never whom to believe. A
//!   certificate that fails is an active certificate failure; the key is
//!   then not verified and the inbound peer receives a dialback error.
//! - The connection carries one `<db:verify/>` request with the inbound
//!   stream's pair and stream ID, accepts only the answer correlated to it,
//!   and closes. It never authenticates and never carries stanzas.
//! - It has a budget of its own: a slot under the cap on verifications in
//!   progress and a token of the verification rate, both taken before the
//!   Authoritative Server is looked up. Its socket is outside the socket
//!   caps and its handshake outside the handshake caps of ordinary
//!   connections, and runs on the verification workers, so it never
//!   evicts, delays or refuses an ordinary stream, dial or handshake.
//! - It ends by the inbound stream's dialback deadline, when the inbound
//!   session stops waiting for it, or when the service drains. Any
//!   shortage or failure is a dialback error, never a valid key.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::net::TcpStream;
use waddle_xmpp::s2s::{
    DialbackError, DialbackMode, DialbackOutcome, DialbackRequest, InitiatingConfig, Output,
    S2sStream, StreamLanguage, TlsEntry, VerifyRequest, VerifyVerdict,
};
use waddle_xmpp::telemetry::attributes::S2sOutcome;

use super::endpoints::ResolveError;
use super::peer_stream::StreamDirection;
use super::registry::Activity;
use super::service::Shared;
use super::session::{establish_tls, HandshakeBudget, TlsOutcome, TlsRequest};
use super::telemetry;
use super::transport::{Transport, TransportError};
use crate::s2s::config::STREAM_CODEC_LIMITS;
use crate::s2s::verify::OutboundTransport;

/// Plaintext handed to the codec at once.
const READ_CHUNK: usize = 16 * 1024;

/// Ask the Authoritative Server of `request.source` whether `request.key`
/// is the key it issued for the stream `request.stream_id` from
/// `request.source` to `request.destination`, by `deadline`.
///
/// The verification's slot and rate token are taken first, before the
/// DNS lookup, and the slot is held until the connection closes. A drain
/// or abort of the service ends the verification at any step.
pub(super) async fn verify_key(
    shared: &Arc<Shared>,
    request: DialbackRequest,
    deadline: Instant,
) -> DialbackOutcome {
    let _slot = match shared.capacity.admit_verification() {
        Ok(slot) => slot,
        Err(resource) => {
            telemetry::capacity_refused(StreamDirection::Outbound, resource);
            return DialbackOutcome::Error(DialbackError::ResourceConstraint);
        }
    };
    let resolve =
        tokio::time::timeout_at(deadline.into(), shared.resolver.resolve(&request.source));
    let resolved = tokio::select! {
        biased;
        () = shared.abort.cancelled() => return DialbackOutcome::Error(DialbackError::RemoteServerTimeout),
        () = shared.shutdown.cancelled() => return DialbackOutcome::Error(DialbackError::RemoteServerTimeout),
        resolved = resolve => resolved,
    };
    let endpoints = match resolved {
        Ok(Ok(endpoints)) => endpoints,
        Ok(Err(ResolveError::LookupFailed)) => {
            return DialbackOutcome::Error(DialbackError::RemoteConnectionFailed)
        }
        Err(_) => return DialbackOutcome::Error(DialbackError::RemoteServerTimeout),
        Ok(Err(
            ResolveError::NoEndpoints | ResolveError::Blocked | ResolveError::NotAPeerDomain,
        )) => return DialbackOutcome::Error(DialbackError::RemoteServerNotFound),
    };
    // A DNS-derived address is checked again on the address connected, with
    // the operator's own blocks; a domain whose every address is refused
    // has no Authoritative Server this deployment may reach.
    let blocked = Arc::clone(shared.config.blocked_addresses());
    let candidates: Vec<_> = endpoints
        .iter()
        .filter(|endpoint| !(endpoint.origin.is_dns() && blocked.is_blocked(endpoint.address.ip())))
        .map(|endpoint| (endpoint.address, endpoint.transport))
        .collect();
    if candidates.is_empty() {
        return DialbackOutcome::Error(DialbackError::RemoteServerNotFound);
    }
    let connect = *shared.config.deadlines();
    let mut connected = None;
    for (address, transport) in candidates {
        let attempt_deadline = (Instant::now() + connect.connect).min(deadline);
        let socket = connect_to(shared, address, attempt_deadline).await;
        if let Some(socket) = socket {
            connected = Some((socket, transport));
            break;
        }
    }
    let Some((socket, transport)) = connected else {
        telemetry::ended(StreamDirection::Outbound, S2sOutcome::Io);
        return DialbackOutcome::Error(DialbackError::RemoteConnectionFailed);
    };
    let _ = socket.set_nodelay(true);
    let transport_layer = Transport::new(
        socket,
        shared.config.limits().input_rate_per_socket,
        Arc::clone(&shared.capacity),
        Activity::new(),
    );
    let config = InitiatingConfig {
        source: request.destination.clone(),
        destination: request.source.clone(),
        language: StreamLanguage::english(),
        dialback: DialbackMode::Disabled,
    };
    let entry = match transport {
        OutboundTransport::StartTls => TlsEntry::StartTls,
        OutboundTransport::DirectTls => TlsEntry::DirectTls,
    };
    let (stream, initial) = S2sStream::verify(
        config,
        VerifyRequest {
            stream_id: request.stream_id.clone(),
            key: request.key.clone(),
        },
        entry,
        STREAM_CODEC_LIMITS,
    );
    let mut connection = Verification {
        shared,
        transport: transport_layer,
        stream,
        request: &request,
        tls: transport,
        deadline,
        verdict: None,
    };
    let (outcome, ended) = connection.run(initial).await;
    let abort = shared.abort.clone();
    if !abort.is_cancelled() {
        let write = shared.config.deadlines().write;
        tokio::select! {
            biased;
            () = abort.cancelled() => {}
            _ = tokio::time::timeout(write, connection.transport.close()) => {}
        }
    }
    telemetry::ended(StreamDirection::Outbound, ended);
    outcome
}

/// Open a TCP connection to `address` by `deadline`, unless the service
/// starts to drain or aborts first.
async fn connect_to(shared: &Shared, address: SocketAddr, deadline: Instant) -> Option<TcpStream> {
    let connect = tokio::time::timeout_at(deadline.into(), TcpStream::connect(address));
    tokio::select! {
        biased;
        () = shared.abort.cancelled() => None,
        () = shared.shutdown.cancelled() => None,
        connect = connect => connect.ok().and_then(Result::ok),
    }
}

/// One verification connection after its TCP connect.
struct Verification<'a> {
    shared: &'a Arc<Shared>,
    transport: Transport,
    stream: S2sStream,
    request: &'a DialbackRequest,
    tls: OutboundTransport,
    deadline: Instant,
    verdict: Option<VerifyVerdict>,
}

impl Verification<'_> {
    /// Drive the connection until it closes: the outcome for the inbound
    /// peer and how the connection ended.
    async fn run(&mut self, initial: Vec<Output>) -> (DialbackOutcome, S2sOutcome) {
        let mut queue: std::collections::VecDeque<Output> = initial.into();
        let mut buffer = vec![0u8; READ_CHUNK];
        let abort = self.shared.abort.clone();
        let shutdown = self.shared.shutdown.clone();
        loop {
            while let Some(output) = queue.pop_front() {
                match self.execute(output).await {
                    Ok(more) => queue.extend(more),
                    Err(ended) => return (self.outcome(ended), ended),
                }
            }
            let read = tokio::select! {
                biased;
                () = abort.cancelled() => return (self.outcome(S2sOutcome::Shutdown), S2sOutcome::Shutdown),
                () = shutdown.cancelled() => return (self.outcome(S2sOutcome::Shutdown), S2sOutcome::Shutdown),
                () = tokio::time::sleep_until(self.deadline.into()) => {
                    return (self.outcome(S2sOutcome::Timeout), S2sOutcome::Timeout)
                }
                read = self.transport.read(&mut buffer) => read,
            };
            match read {
                Ok(0) => return (self.outcome(S2sOutcome::PeerClosed), S2sOutcome::PeerClosed),
                Ok(read) => queue.extend(self.stream.receive(&buffer[..read])),
                Err(TransportError::Tls(_)) => {
                    return (self.outcome(S2sOutcome::Tls), S2sOutcome::Tls)
                }
                Err(_) => return (self.outcome(S2sOutcome::Io), S2sOutcome::Io),
            }
        }
    }

    /// Execute one codec output; `Err` ends the connection with that
    /// outcome.
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
                let stage = Instant::now() + self.shared.config.deadlines().stage;
                let request = TlsRequest {
                    direction: StreamDirection::Outbound,
                    local: self.request.destination.clone(),
                    remote: self.request.source.clone(),
                    handoff: handoff.into_bytes(),
                    deadline: stage.min(self.deadline),
                    transport: self.tls,
                    client_certificate_optional: false,
                    budget: HandshakeBudget::Verification,
                };
                let shutdown = self.shared.shutdown.clone();
                let handshake = tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return Err(S2sOutcome::Shutdown),
                    handshake = establish_tls(self.shared, &mut self.transport, request) => handshake,
                };
                match handshake {
                    Ok(TlsOutcome::Verified(verified)) => {
                        Ok(self.stream.tls_established(verified.tls_peer()))
                    }
                    // An outbound handshake never completes without the
                    // receiving server's certificate.
                    Ok(TlsOutcome::NoCertificate) => Err(S2sOutcome::Certificate),
                    Err(failure) => {
                        if let Some(resource) = failure.resource {
                            telemetry::capacity_refused(StreamDirection::Outbound, resource);
                        }
                        Err(failure.outcome)
                    }
                }
            }
            Output::VerifyVerdict(verdict) => {
                self.verdict = Some(verdict);
                Ok(Vec::new())
            }
            Output::Close(_) if self.verdict.is_some() => Err(S2sOutcome::Verified),
            Output::Close(_) => Err(S2sOutcome::Protocol),
            // A verification connection never authorizes, establishes,
            // delivers or answers anything.
            Output::Authorize(_)
            | Output::Established(_)
            | Output::Stanza(_)
            | Output::VerifyDialback(_)
            | Output::DialbackAnswered { .. } => Err(S2sOutcome::Protocol),
        }
    }

    /// The outcome for the inbound peer once the connection ended with
    /// `ended`. Only the Authoritative Server's correlated answer makes a
    /// key valid or invalid. An active certificate failure of the
    /// Authoritative Server leaves the claimed identity unestablished.
    fn outcome(&self, ended: S2sOutcome) -> DialbackOutcome {
        match self.verdict {
            Some(VerifyVerdict::Valid) => DialbackOutcome::Valid,
            Some(VerifyVerdict::Invalid) => DialbackOutcome::Invalid,
            Some(VerifyVerdict::Error) => {
                DialbackOutcome::Error(DialbackError::RemoteServerNotFound)
            }
            None => DialbackOutcome::Error(match ended {
                S2sOutcome::Certificate => DialbackError::NotAcceptable,
                S2sOutcome::Capacity => DialbackError::ResourceConstraint,
                S2sOutcome::Tls | S2sOutcome::Io => DialbackError::RemoteConnectionFailed,
                _ => DialbackError::RemoteServerTimeout,
            }),
        }
    }
}
