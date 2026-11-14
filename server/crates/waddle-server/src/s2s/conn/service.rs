//! The running S2S service: the inbound STARTTLS listener, the outbound
//! STARTTLS dialer and the shutdown join.
//!
//! Every connection runs in its own task on a task tracker. S2S tasks hold
//! no lock, permit or registry shared with C2S or any local service, so a
//! slow, stalled or hostile peer cannot hold up unrelated local traffic;
//! certificate verification runs on the bounded verifier workers, off the
//! async runtime.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::s2s::{
    DialbackMode, InitiatingConfig, ReceivingConfig, S2sStream, SaslLimits, StreamLanguage,
};
use waddle_xmpp::telemetry::attributes::{S2sOutcome, S2sResource, S2sTransport};

use super::admission::ConnectionAdmission;
use super::capacity::{Capacity, InboundSlot, OutboundSlot};
use super::endpoints::{ConfiguredEndpoints, EndpointResolver, Endpoints, ResolveError};
use super::peer_stream::{StreamDirection, VerifiedPeerStream};
use super::registry::{Activity, DialRefusal, DialSlot, Registry};
use super::session::{is_peer_failure, EstablishedSender, Role, Session, SessionStart};
use super::sink::{RefuseAllStanzas, StanzaSink};
use super::telemetry;
use super::transport::Transport;
use crate::s2s::config::{AdmissionRefusal, S2sConfig, STREAM_CODEC_LIMITS};
use crate::s2s::verify::VerifierWorkers;

/// Pause after a failed `accept`, such as when the process is out of file
/// descriptors, so the loop does not spin.
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(100);

/// What the service runs with besides its configuration.
pub struct ServiceOptions {
    /// Where outbound connections go; the configured endpoints by default.
    pub resolver: Option<Arc<dyn EndpointResolver>>,
    /// Where stanzas from established inbound streams go.
    pub sink: Arc<dyn StanzaSink>,
    /// The workers inbound TLS handshakes run on; by default as many as
    /// the inbound handshake cap, with as many waiters.
    pub inbound_workers: Option<VerifierWorkers>,
    /// The workers outbound TLS handshakes run on; by default as many as
    /// the outbound handshake cap, with as many waiters.
    pub outbound_workers: Option<VerifierWorkers>,
    /// Cancelled when the process starts to shut down; the service then
    /// stops accepting and drains its streams.
    pub stop: Option<CancellationToken>,
}

impl Default for ServiceOptions {
    fn default() -> Self {
        Self {
            resolver: None,
            sink: Arc::new(RefuseAllStanzas),
            inbound_workers: None,
            outbound_workers: None,
            stop: None,
        }
    }
}

/// Verifier workers for a handshake cap: one worker per handshake, and as
/// many waiters, because a worker whose handshake timed out stays busy until
/// its job ends.
fn workers_for(handshakes: usize) -> VerifierWorkers {
    VerifierWorkers::new(handshakes, handshakes).unwrap_or_default()
}

/// State shared by the listener, the dialer and every connection task.
pub(super) struct Shared {
    pub(super) config: Arc<S2sConfig>,
    pub(super) capacity: Arc<Capacity>,
    pub(super) registry: Arc<Registry>,
    pub(super) inbound_workers: VerifierWorkers,
    pub(super) outbound_workers: VerifierWorkers,
    pub(super) sink: Arc<dyn StanzaSink>,
    pub(super) resolver: Arc<dyn EndpointResolver>,
    /// Cancelled to drain: connections send `system-shutdown` and close.
    pub(super) shutdown: CancellationToken,
    /// Cancelled when the drain time is up: connections drop their sockets.
    pub(super) abort: CancellationToken,
    tasks: TaskTracker,
    /// Whether tasks may still be spawned. Shutdown clears it under the
    /// lock before closing the tracker, and every spawn happens under the
    /// lock, so no task starts after the join has begun.
    open: Mutex<bool>,
    next_connection: AtomicU64,
}

impl Shared {
    fn next_connection(&self) -> u64 {
        self.next_connection.fetch_add(1, Ordering::Relaxed)
    }

    fn lock_open(&self) -> std::sync::MutexGuard<'_, bool> {
        // A bool is always usable, so a poisoned lock is too.
        match self.open.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Spawn `task` on the tracker unless shutdown has begun; `false` when
    /// it was not spawned.
    fn spawn<F>(&self, task: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let open = self.lock_open();
        if !*open {
            return false;
        }
        self.tasks.spawn(task);
        true
    }

    /// Stop new tasks, tell every connection to drain and close the
    /// tracker.
    fn close(&self) {
        let mut open = self.lock_open();
        *open = false;
        self.shutdown.cancel();
        self.tasks.close();
    }

    /// Admit an inbound socket from `address`. When the inbound socket cap
    /// is full, the inbound stream that has carried no traffic the longest
    /// is evicted if that has lasted the eviction time, and its socket
    /// makes room for this one.
    fn admit_inbound(&self, address: std::net::IpAddr) -> Result<InboundSlot, S2sResource> {
        match self.capacity.admit_inbound(address) {
            Err(S2sResource::Sockets) => {
                let socket = self
                    .registry
                    .evict_for_socket(StreamDirection::Inbound, Instant::now())
                    .ok_or(S2sResource::Sockets)?;
                socket.release();
                self.capacity.admit_inbound(address)
            }
            admitted => admitted,
        }
    }

    /// Admit an outbound socket, evicting an outbound stream without
    /// traffic as [`Shared::admit_inbound`] does for inbound ones.
    fn admit_outbound(&self) -> Result<OutboundSlot, S2sResource> {
        match self.capacity.admit_outbound() {
            Err(S2sResource::Sockets) => {
                let socket = self
                    .registry
                    .evict_for_socket(StreamDirection::Outbound, Instant::now())
                    .ok_or(S2sResource::Sockets)?;
                socket.release();
                self.capacity.admit_outbound()
            }
            admitted => admitted,
        }
    }
}

/// Why an outbound stream could not be provided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConnectError {
    #[error("the S2S service is shutting down")]
    ShuttingDown,
    #[error("the source domain is not a local domain")]
    NotLocal,
    #[error("the remote domain is not admitted: {}", .0.code())]
    NotAdmitted(AdmissionRefusal),
    #[error("an earlier attempt failed; the next one waits")]
    BackingOff,
    #[error("an attempt to this domain is already in progress")]
    InProgress,
    #[error("the cap on outbound attempts in progress is reached")]
    Busy,
    #[error("no endpoint: {0}")]
    Resolve(ResolveError),
    #[error("the attempt failed: {}", outcome_code(*.0))]
    Failed(S2sOutcome),
}

fn outcome_code(outcome: S2sOutcome) -> &'static str {
    use waddle_xmpp::telemetry::attributes::MetricAttribute;
    outcome.value()
}

/// How the shutdown join ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Every connection closed within the drain time.
    pub drained: bool,
    /// Every task had ended when the join returned.
    pub joined: bool,
}

/// The running S2S service.
pub struct S2sService {
    shared: Arc<Shared>,
    local_address: Option<SocketAddr>,
}

/// A cheap handle for opening outbound streams and inspecting state.
#[derive(Clone)]
pub struct S2sHandle {
    shared: Arc<Shared>,
}

impl S2sService {
    /// Start the service. `listener` is the inbound STARTTLS listener,
    /// present exactly when the configuration has a `listen` address.
    pub fn start(
        config: Arc<S2sConfig>,
        listener: Option<TcpListener>,
        options: ServiceOptions,
    ) -> Self {
        let shutdown = options
            .stop
            .map_or_else(CancellationToken::new, |stop| stop.child_token());
        let resolver = options
            .resolver
            .unwrap_or_else(|| Arc::new(ConfiguredEndpoints::new(Arc::clone(&config))));
        let limits = *config.limits();
        let inbound_workers = options
            .inbound_workers
            .unwrap_or_else(|| workers_for(limits.max_inbound_handshakes));
        let outbound_workers = options
            .outbound_workers
            .unwrap_or_else(|| workers_for(limits.max_outbound_handshakes));
        let shared = Arc::new(Shared {
            capacity: Capacity::new(*config.limits()),
            registry: Registry::new(*config.limits(), *config.deadlines()),
            config,
            inbound_workers,
            outbound_workers,
            sink: options.sink,
            resolver,
            shutdown,
            abort: CancellationToken::new(),
            tasks: TaskTracker::new(),
            open: Mutex::new(true),
            next_connection: AtomicU64::new(1),
        });
        let local_address = listener
            .as_ref()
            .and_then(|listener| listener.local_addr().ok());
        if let Some(listener) = listener {
            shared.spawn(accept_loop(Arc::clone(&shared), listener));
        }
        tracing::info!(
            generation = %shared.config.generation(),
            mode = shared.config.admission().mode().code(),
            listening = local_address.is_some(),
            "s2s service started"
        );
        Self {
            shared,
            local_address,
        }
    }

    /// The address the listener is bound to.
    pub fn local_address(&self) -> Option<SocketAddr> {
        self.local_address
    }

    pub fn handle(&self) -> S2sHandle {
        S2sHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Stop accepting, tell every stream `system-shutdown`, wait up to the
    /// drain time, then drop the remaining connections and join every task.
    /// `budget` caps the whole wait, for a process whose own drain budget
    /// is shorter.
    pub async fn shutdown(self, budget: Duration) -> ShutdownReport {
        let started = Instant::now();
        let deadlines = *self.shared.config.deadlines();
        self.shared.close();
        let drain = deadlines.drain.min(budget);
        let drained = tokio::time::timeout(drain, self.shared.tasks.wait())
            .await
            .is_ok();
        let joined = if drained {
            true
        } else {
            self.shared.abort.cancel();
            let remaining = budget.saturating_sub(started.elapsed());
            tokio::time::timeout(remaining.min(deadlines.write), self.shared.tasks.wait())
                .await
                .is_ok()
        };
        tracing::info!(drained, joined, "s2s service stopped");
        ShutdownReport { drained, joined }
    }
}

impl S2sHandle {
    /// An established outbound stream from `local` to `remote`: the existing
    /// one, or a new one from a fresh attempt. Admission is checked before
    /// anything is dialled; a failed attempt makes the next one wait.
    pub async fn connect(
        &self,
        local: &CanonicalDomain,
        remote: &CanonicalDomain,
    ) -> Result<VerifiedPeerStream, ConnectError> {
        let shared = &self.shared;
        if shared.shutdown.is_cancelled() {
            return Err(ConnectError::ShuttingDown);
        }
        if !shared.config.is_local(local) {
            return Err(ConnectError::NotLocal);
        }
        shared
            .config
            .admission()
            .decide(remote)
            .map_err(ConnectError::NotAdmitted)?;
        if let Some(stream) = shared.registry.outbound(local, remote) {
            return Ok(stream);
        }
        let dial = shared
            .registry
            .begin_dial(local, remote, Instant::now())
            .map_err(|refusal| match refusal {
                DialRefusal::BackingOff => ConnectError::BackingOff,
                DialRefusal::InProgress => ConnectError::InProgress,
                DialRefusal::Busy => ConnectError::Busy,
            })?;
        let endpoints = shared
            .resolver
            .resolve(remote)
            .await
            .map_err(ConnectError::Resolve)?;
        let (sender, receiver) = oneshot::channel();
        let attempt = Attempt {
            local: local.clone(),
            remote: remote.clone(),
            endpoints,
            dial,
            sender,
        };
        if !shared.spawn(run_attempt(Arc::clone(shared), attempt)) {
            return Err(ConnectError::ShuttingDown);
        }
        match receiver.await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(outcome)) => Err(ConnectError::Failed(outcome)),
            Err(_) => Err(ConnectError::Failed(S2sOutcome::Shutdown)),
        }
    }

    /// Every established stream.
    pub fn streams(&self) -> Vec<VerifiedPeerStream> {
        self.shared.registry.streams()
    }

    /// Open S2S sockets in every stage.
    pub fn open_sockets(&self) -> usize {
        self.shared.capacity.open_sockets()
    }

    /// Verifier workers not running a handshake job.
    pub fn idle_workers(&self, direction: StreamDirection) -> usize {
        match direction {
            StreamDirection::Inbound => self.shared.inbound_workers.available(),
            StreamDirection::Outbound => self.shared.outbound_workers.available(),
        }
    }
}

/// Accept inbound connections until shutdown. A socket over a limit is
/// closed at once, before any byte is read.
async fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    loop {
        let accepted = tokio::select! {
            () = shared.shutdown.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        let (socket, address) = match accepted {
            Ok(accepted) => accepted,
            Err(_) => {
                tokio::time::sleep(ACCEPT_ERROR_PAUSE).await;
                continue;
            }
        };
        match shared.admit_inbound(address.ip()) {
            Ok(slot) => {
                let started = Instant::now();
                let _ = socket.set_nodelay(true);
                let connection = shared.next_connection();
                let activity = Activity::new();
                let transport = Transport::new(
                    socket,
                    shared.config.limits().input_rate_per_socket,
                    Arc::clone(&shared.capacity),
                    Arc::clone(&activity),
                );
                let Some((stream, admission)) = receiving_stream(&shared) else {
                    continue;
                };
                let session = Session::new(
                    Arc::clone(&shared),
                    SessionStart {
                        connection,
                        transport,
                        stream,
                        role: Role::Inbound { admission, slot },
                        activity,
                        started,
                        tls_transport: S2sTransport::StartTls,
                    },
                );
                // A session not spawned because shutdown began drops its
                // socket here.
                shared.spawn(session.run(Vec::new()));
            }
            Err(resource) => {
                drop(socket);
                telemetry::capacity_refused(StreamDirection::Inbound, resource);
                telemetry::ended(StreamDirection::Inbound, S2sOutcome::Capacity);
            }
        }
    }
}

/// The codec of a new inbound connection, which hands stanzas from the
/// established stream to the sink.
fn receiving_stream(shared: &Arc<Shared>) -> Option<(S2sStream, Arc<ConnectionAdmission>)> {
    let default_domain = shared.config.default_domain()?.clone();
    let admission = ConnectionAdmission::new(Arc::clone(&shared.config));
    let mut stream = S2sStream::accept(
        ReceivingConfig {
            admission: admission.clone(),
            default_domain,
            language: StreamLanguage::english(),
            sasl: SaslLimits::PILOT,
            dialback: DialbackMode::Disabled,
        },
        STREAM_CODEC_LIMITS,
    );
    stream.deliver_stanzas();
    Some((stream, admission))
}

/// One outbound attempt.
struct Attempt {
    local: CanonicalDomain,
    remote: CanonicalDomain,
    endpoints: Endpoints,
    dial: DialSlot,
    sender: EstablishedSender,
}

/// Fail an attempt before its session exists.
fn fail_attempt(shared: &Shared, attempt: Attempt, outcome: S2sOutcome) {
    if is_peer_failure(outcome) {
        shared.registry.record_failure(&attempt.remote);
    }
    telemetry::ended(StreamDirection::Outbound, outcome);
    let _ = attempt.sender.send(Err(outcome));
}

/// Dial the attempt's endpoints in order and run a STARTTLS stream on the
/// first one that accepts the TCP connection. Any failure after a
/// connection is open, including a `see-other-host` redirect, ends the
/// attempt: the redirect is never followed.
async fn run_attempt(shared: Arc<Shared>, attempt: Attempt) {
    let started = Instant::now();
    let deadlines = *shared.config.deadlines();
    let negotiation_deadline = started + deadlines.negotiation;
    let negotiation = match shared
        .registry
        .begin_negotiation(StreamDirection::Outbound, &attempt.remote)
    {
        Ok(slot) => slot,
        Err(resource) => {
            telemetry::capacity_refused(StreamDirection::Outbound, resource);
            return fail_attempt(&shared, attempt, S2sOutcome::Capacity);
        }
    };
    let slot = match shared.admit_outbound() {
        Ok(slot) => slot,
        Err(resource) => {
            telemetry::capacity_refused(StreamDirection::Outbound, resource);
            return fail_attempt(&shared, attempt, S2sOutcome::Capacity);
        }
    };
    let addresses: Vec<SocketAddr> = attempt
        .endpoints
        .iter()
        .map(|endpoint| endpoint.address)
        .collect();
    let mut connected: Option<TcpStream> = None;
    for address in addresses {
        let deadline = (Instant::now() + deadlines.connect).min(negotiation_deadline);
        let connect = tokio::time::timeout_at(deadline.into(), TcpStream::connect(address));
        // A drain does not wait for a TCP connect.
        let connect = tokio::select! {
            biased;
            () = shared.shutdown.cancelled() => {
                return fail_attempt(&shared, attempt, S2sOutcome::Shutdown);
            }
            connect = connect => connect,
        };
        if let Ok(Ok(socket)) = connect {
            connected = Some(socket);
            break;
        }
    }
    let Some(socket) = connected else {
        return fail_attempt(&shared, attempt, S2sOutcome::Io);
    };
    let _ = socket.set_nodelay(true);
    let activity = Activity::new();
    let transport = Transport::new(
        socket,
        shared.config.limits().input_rate_per_socket,
        Arc::clone(&shared.capacity),
        Arc::clone(&activity),
    );
    let config = InitiatingConfig {
        source: attempt.local.clone(),
        destination: attempt.remote.clone(),
        language: StreamLanguage::english(),
        dialback: DialbackMode::Disabled,
    };
    let (stream, initial) = S2sStream::initiate(config, STREAM_CODEC_LIMITS);
    let connection = shared.next_connection();
    let Attempt {
        local,
        remote,
        dial,
        sender,
        ..
    } = attempt;
    let session = Session::new(
        Arc::clone(&shared),
        SessionStart {
            connection,
            transport,
            stream,
            role: Role::Outbound {
                local,
                remote,
                slot,
                dial: Some(dial),
                established: Some(sender),
            },
            activity,
            started,
            tls_transport: S2sTransport::StartTls,
        },
    )
    .with_negotiation(negotiation);
    session.run(initial).await;
}
