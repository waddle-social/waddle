//! Established streams, per-peer negotiation counts, outbound dials in
//! progress and outbound backoff (ADR-0020 gate G7).
//!
//! Outbound, one stream per exact directional domain pair: a new
//! authenticated outbound stream for a pair replaces the existing one,
//! which closes with `conflict`, unless the existing one is younger than
//! the replacement interval, in which case the new one is refused with
//! `conflict`. Inbound, a pair may hold up to
//! [`ResourceLimits::max_inbound_streams_per_peer`] streams, since a pooled
//! or clustered peer opens several and each carries its stanzas; one more
//! takes the place of the pair's inbound stream that has carried no
//! traffic the longest, if that has lasted the eviction idle time, and is
//! refused with `resource-constraint` otherwise. A stream never replaces
//! or joins one of greater authentication strength: a `dialback` stream is
//! refused with `conflict` while the pair has a `pkix` one in the same
//! direction, whatever its age, so a weaker identity proof can never
//! displace or sit beside a stronger one.
//!
//! When the stream or peer cap is reached, the stream (or, for the peer
//! cap, every stream of the peer) that has carried no traffic the longest
//! is evicted, but only once that has lasted the configured eviction time;
//! otherwise the new stream is refused. Traffic is an element or stream
//! header received, or anything sent; whitespace keepalives keep a stream
//! open but are not traffic, so a peer that keeps its streams busy is never
//! displaced, and an attacker who opens streams from many domains can
//! displace only streams that carry nothing. In `open` mode a quarter of
//! each cap is reserved for allowlisted and trusted-home peers
//! ([`crate::s2s::config::AdmissionPolicy::is_reserved_peer`]): other peers
//! share the rest and can evict only each other, whichever cap is full.
//! A new socket whose stream is not yet known, at accept, never evicts a
//! reserved peer's stream in `open` mode either.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::telemetry::attributes::S2sResource;

use super::capacity::{SocketCount, TokenBucket};
use super::endpoints::ResolveError;
use super::outbox::Outbox;
use super::peer_stream::{AuthenticationStrength, StreamDirection, VerifiedPeerStream};
use crate::s2s::config::{Deadlines, ResourceLimits, NO_ENDPOINT_WAIT};

/// When a connection last carried anything, and when it last carried
/// traffic, shared between its task and the registry's eviction choice.
#[derive(Debug)]
pub(super) struct Activity {
    epoch: Instant,
    /// Milliseconds after `epoch` when anything was last received or sent,
    /// whitespace keepalives included.
    alive_millis: AtomicU64,
    /// Milliseconds after `epoch` when an element or stream header was last
    /// received, or anything was last sent.
    traffic_millis: AtomicU64,
}

impl Activity {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: Instant::now(),
            alive_millis: AtomicU64::new(0),
            traffic_millis: AtomicU64::new(0),
        })
    }

    fn now_millis(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn since(&self, millis: &AtomicU64, now: Instant) -> Duration {
        let last = self.epoch + Duration::from_millis(millis.load(Ordering::Relaxed));
        now.saturating_duration_since(last)
    }

    /// Bytes arrived from the peer, which may be only whitespace.
    pub(super) fn input(&self) {
        self.alive_millis
            .store(self.now_millis(), Ordering::Relaxed);
    }

    /// An element or stream header was received, or something was sent.
    pub(super) fn traffic(&self) {
        let now = self.now_millis();
        self.alive_millis.store(now, Ordering::Relaxed);
        self.traffic_millis.store(now, Ordering::Relaxed);
    }

    /// How long since anything was received or sent; the idle timeout.
    pub(super) fn idle(&self, now: Instant) -> Duration {
        self.since(&self.alive_millis, now)
    }

    /// How long since the last traffic; the eviction order.
    pub(super) fn quiet(&self, now: Instant) -> Duration {
        self.since(&self.traffic_millis, now)
    }
}

/// Why a stream could not be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RegisterRefusal {
    /// Another stream carries the same directional pair and is stronger, or
    /// is an outbound stream too young to be replaced.
    Conflict,
    /// A cap is reached and nothing can be evicted.
    Full(S2sResource),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StreamKey {
    direction: StreamDirection,
    local: CanonicalDomain,
    remote: CanonicalDomain,
    /// The connection carrying the stream: a pair has at most one outbound
    /// stream but may have several inbound ones.
    connection: u64,
}

impl StreamKey {
    /// Whether `other` carries the same directional pair.
    fn same_pair(&self, other: &StreamKey) -> bool {
        self.direction == other.direction
            && self.local == other.local
            && self.remote == other.remote
    }
}

/// How the registry ends a registered stream's life early.
#[derive(Debug, Clone, Default)]
pub(super) struct StreamTokens {
    /// Cancelled when the stream is evicted for capacity, including for a
    /// newer inbound stream of the same pair beyond the per-pair cap.
    pub(super) evict: CancellationToken,
    /// Cancelled when a newer outbound stream for the same pair replaces
    /// it.
    pub(super) replace: CancellationToken,
}

/// An authenticated stream to register.
#[derive(Debug)]
pub(super) struct Registration {
    pub(super) stream: VerifiedPeerStream,
    pub(super) activity: Arc<Activity>,
    pub(super) tokens: StreamTokens,
    /// The connection's socket, released early if the stream is evicted to
    /// make room for a new socket.
    pub(super) socket: Arc<SocketCount>,
    /// Whether the peer may use the reserved share of the caps.
    pub(super) reserved: bool,
    /// Where stanzas for an outbound stream go; `None` for an inbound one.
    pub(super) outbox: Option<Outbox>,
}

#[derive(Debug)]
struct Entry {
    stream: VerifiedPeerStream,
    activity: Arc<Activity>,
    tokens: StreamTokens,
    socket: Arc<SocketCount>,
    reserved: bool,
    registered_at: Instant,
    outbox: Option<Outbox>,
}

/// What registering a stream would do to the registered ones.
#[derive(Debug, Default)]
struct Plan {
    /// The pair's existing outbound stream, which the new one replaces.
    replaced: Option<StreamKey>,
    /// Streams evicted to stay within the caps.
    evicted: BTreeSet<StreamKey>,
}

impl Plan {
    fn removes(&self, key: &StreamKey) -> bool {
        self.replaced.as_ref() == Some(key) || self.evicted.contains(key)
    }
}

/// Which cap a registration must make room under.
#[derive(Debug, Clone, Copy)]
enum Need {
    /// A peer slot; `general` when only peers outside the reserve count.
    Peer { general: bool },
    /// A stream slot; `general` when only streams outside the reserve count.
    Stream { general: bool },
}

#[derive(Debug, Clone, Copy)]
struct Backoff {
    failures: u32,
    retry_at: Instant,
    /// When the entry last recorded a failure.
    updated: Instant,
}

#[derive(Debug)]
struct State {
    streams: BTreeMap<StreamKey, Entry>,
    /// Streams negotiating with each remote domain, per direction.
    negotiating: HashMap<(StreamDirection, CanonicalDomain), usize>,
    /// Directional `(local, remote)` pairs with an outbound attempt in
    /// progress.
    dialing: HashSet<(CanonicalDomain, CanonicalDomain)>,
    /// Outbound backoff of failing remote domains.
    backoff: HashMap<CanonicalDomain, Backoff>,
    /// Remote domains whose endpoint lookup found nothing, until when that
    /// answer is repeated without a new lookup.
    no_endpoints: HashMap<CanonicalDomain, NoEndpoint>,
    /// New outbound connections across every remote domain.
    dial_rate: TokenBucket,
}

/// A remembered lookup that found no endpoint.
#[derive(Debug, Clone, Copy)]
struct NoEndpoint {
    until: Instant,
    error: ResolveError,
}

/// What [`Registry::begin_dial`] found for a directional pair.
#[derive(Debug)]
pub(super) enum DialStart {
    /// The pair's established outbound stream.
    Established(VerifiedPeerStream),
    /// No stream: the caller dials, holding the pair's attempt.
    Dial(DialSlot),
}

/// Why an outbound attempt may not start now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DialRefusal {
    /// An earlier attempt to the domain failed and its wait has not passed.
    BackingOff,
    /// An attempt for the pair is in progress.
    InProgress,
    /// The cap on attempts in progress is reached.
    Busy,
    /// The global rate of new outbound connections is exhausted.
    RateLimited,
    /// A recent lookup found no endpoint for the domain; this is its
    /// answer, repeated without spending a dial token.
    NoEndpoint(ResolveError),
}

/// The share of `cap` that peers outside the reserve may use: all but a
/// quarter, rounded so that a cap of one to three reserves nothing.
fn general_share(cap: usize) -> usize {
    cap - cap / 4
}

/// The process's established streams and per-peer state.
#[derive(Debug)]
pub(super) struct Registry {
    limits: ResourceLimits,
    deadlines: Deadlines,
    state: Mutex<State>,
}

impl Registry {
    pub(super) fn new(limits: ResourceLimits, deadlines: Deadlines) -> Arc<Self> {
        Arc::new(Self {
            limits,
            deadlines,
            state: Mutex::new(State {
                streams: BTreeMap::new(),
                negotiating: HashMap::new(),
                dialing: HashSet::new(),
                backoff: HashMap::new(),
                no_endpoints: HashMap::new(),
                dial_rate: TokenBucket::new(limits.dial_rate, Instant::now()),
            }),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Nothing fails while the lock is held. A poisoned lock means the
        // maps may no longer match the streams they hold, and a registry
        // that miscounts would route to or admit the wrong stream: the
        // process stops instead.
        self.state
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }

    /// Count one more stream negotiating with `remote` in `direction`. An
    /// inbound connection calls this only once TLS has verified `remote`,
    /// so an unauthenticated claim can never use up a peer's budget, and
    /// the two directions' budgets are separate.
    pub(super) fn begin_negotiation(
        self: &Arc<Self>,
        direction: StreamDirection,
        remote: &CanonicalDomain,
    ) -> Result<NegotiationSlot, S2sResource> {
        let key = (direction, remote.clone());
        let mut state = self.state();
        let count = state.negotiating.entry(key.clone()).or_insert(0);
        if *count >= self.limits.max_negotiating_per_peer {
            return Err(S2sResource::PerPeer);
        }
        *count += 1;
        Ok(NegotiationSlot {
            registry: Arc::clone(self),
            key,
        })
    }

    /// Whether a stream of `strength` for the pair could be registered
    /// now, without changing anything: the receiving role asks before SASL
    /// EXTERNAL succeeds or a dialback key is verified, so a stream that
    /// would be refused learns it before its restart and before any
    /// verification connection is opened for it. Registration itself
    /// waits for the authenticated stream.
    pub(super) fn check(
        &self,
        direction: StreamDirection,
        local: &CanonicalDomain,
        remote: &CanonicalDomain,
        strength: AuthenticationStrength,
        reserved: bool,
    ) -> Result<(), RegisterRefusal> {
        // The connection is not registered yet; its id plays no part in
        // the plan beyond telling its key apart from registered ones.
        let key = StreamKey {
            direction,
            local: local.clone(),
            remote: remote.clone(),
            connection: u64::MAX,
        };
        let state = self.state();
        self.plan(&state, &key, strength, reserved, Instant::now())
            .map(|_| ())
    }

    /// Register an authenticated stream, replacing the pair's existing
    /// outbound stream or evicting idle ones if a cap requires it.
    pub(super) fn register(
        self: &Arc<Self>,
        registration: Registration,
    ) -> Result<StreamSlot, RegisterRefusal> {
        let stream = &registration.stream;
        let key = StreamKey {
            direction: stream.direction(),
            local: stream.local().clone(),
            remote: stream.remote().clone(),
            connection: stream.connection(),
        };
        let now = Instant::now();
        let mut state = self.state();
        let plan = self.plan(
            &state,
            &key,
            registration.stream.strength(),
            registration.reserved,
            now,
        )?;
        if let Some(replaced) = plan.replaced.and_then(|key| state.streams.remove(&key)) {
            replaced.tokens.replace.cancel();
        }
        for key in &plan.evicted {
            if let Some(evicted) = state.streams.remove(key) {
                evicted.tokens.evict.cancel();
            }
        }
        state.streams.insert(
            key.clone(),
            Entry {
                stream: registration.stream,
                activity: registration.activity,
                tokens: registration.tokens,
                socket: registration.socket,
                reserved: registration.reserved,
                registered_at: now,
                outbox: registration.outbox,
            },
        );
        Ok(StreamSlot {
            registry: Arc::clone(self),
            key,
        })
    }

    /// Decide what registering `key` with `strength` replaces and evicts,
    /// or why it is refused. A pair's existing outbound stream is replaced
    /// only by a stream at least as strong, and only once it is older than
    /// the replacement interval. A pair's inbound streams are joined by one
    /// at least as strong as each of them, up to the per-pair cap; at the
    /// cap, the one without traffic the longest makes room if it has had
    /// none for the eviction idle time.
    fn plan(
        &self,
        state: &State,
        key: &StreamKey,
        strength: AuthenticationStrength,
        reserved: bool,
        now: Instant,
    ) -> Result<Plan, RegisterRefusal> {
        let mut plan = Plan::default();
        let pair: Vec<(&StreamKey, &Entry)> = state
            .streams
            .iter()
            .filter(|(existing, _)| existing.same_pair(key))
            .collect();
        if pair
            .iter()
            .any(|(_, existing)| strength.is_weaker_than(existing.stream.strength()))
        {
            return Err(RegisterRefusal::Conflict);
        }
        match key.direction {
            StreamDirection::Outbound => {
                if let Some((existing_key, existing)) = pair.first() {
                    let age = now.saturating_duration_since(existing.registered_at);
                    if age < self.deadlines.replacement_interval {
                        return Err(RegisterRefusal::Conflict);
                    }
                    plan.replaced = Some((*existing_key).clone());
                }
            }
            StreamDirection::Inbound => {
                if pair.len() >= self.limits.max_inbound_streams_per_peer {
                    let victim = pair
                        .iter()
                        .filter(|(_, entry)| self.evictable(entry, now))
                        .max_by_key(|(_, entry)| entry.activity.quiet(now))
                        .map(|(existing, _)| (*existing).clone());
                    let Some(victim) = victim else {
                        return Err(RegisterRefusal::Full(S2sResource::PeerStreams));
                    };
                    plan.evicted.insert(victim);
                }
            }
        }
        // Each pass evicts at least one stream or ends, so this terminates.
        loop {
            let live: Vec<(&StreamKey, &Entry)> = state
                .streams
                .iter()
                .filter(|(existing, _)| !plan.removes(existing))
                .collect();
            let Some(need) = self.need(&live, key, reserved) else {
                return Ok(plan);
            };
            let victims = match need {
                Need::Peer { general } => self.peer_victim(&live, &key.remote, general, now),
                Need::Stream { general } => self.stream_victim(&live, general, now),
            };
            if victims.is_empty() {
                let resource = match need {
                    Need::Peer { .. } => S2sResource::Peers,
                    Need::Stream { .. } => S2sResource::Streams,
                };
                return Err(RegisterRefusal::Full(resource));
            }
            plan.evicted.extend(victims);
        }
    }

    /// The first cap a new stream for `key` would exceed among the `live`
    /// streams, if any.
    fn need(&self, live: &[(&StreamKey, &Entry)], key: &StreamKey, reserved: bool) -> Option<Need> {
        let new_peer = !live
            .iter()
            .any(|(existing, _)| existing.remote == key.remote);
        // A stream outside the reserve makes room only among streams
        // outside it, also when the whole cap rather than its general share
        // is full: it never evicts a reserved peer's stream.
        if new_peer {
            if !reserved && peers(live, true) >= general_share(self.limits.max_peers) {
                return Some(Need::Peer { general: true });
            }
            if peers(live, false) >= self.limits.max_peers {
                return Some(Need::Peer { general: !reserved });
            }
        }
        let general_streams = live.iter().filter(|(_, entry)| !entry.reserved).count();
        if !reserved && general_streams >= general_share(self.limits.max_streams) {
            return Some(Need::Stream { general: true });
        }
        if live.len() >= self.limits.max_streams {
            return Some(Need::Stream { general: !reserved });
        }
        None
    }

    fn evictable(&self, entry: &Entry, now: Instant) -> bool {
        entry.activity.quiet(now) >= self.deadlines.eviction_idle
    }

    /// The stream without traffic the longest, if long enough to evict;
    /// only among streams outside the reserve when `general`.
    fn stream_victim(
        &self,
        live: &[(&StreamKey, &Entry)],
        general: bool,
        now: Instant,
    ) -> Vec<StreamKey> {
        live.iter()
            .filter(|(_, entry)| !general || !entry.reserved)
            .filter(|(_, entry)| self.evictable(entry, now))
            .max_by_key(|(_, entry)| entry.activity.quiet(now))
            .map(|(key, _)| (*key).clone())
            .into_iter()
            .collect()
    }

    /// Every stream of the peer, other than `keep`, whose latest traffic is
    /// the oldest, when all its streams can be evicted; only among peers
    /// outside the reserve when `general`.
    fn peer_victim(
        &self,
        live: &[(&StreamKey, &Entry)],
        keep: &CanonicalDomain,
        general: bool,
        now: Instant,
    ) -> Vec<StreamKey> {
        let mut peers: BTreeMap<&CanonicalDomain, (bool, Duration)> = BTreeMap::new();
        for (key, entry) in live {
            if &key.remote == keep || (general && entry.reserved) {
                continue;
            }
            let quiet = entry.activity.quiet(now);
            let evictable = self.evictable(entry, now);
            let peer = peers.entry(&key.remote).or_insert((true, quiet));
            peer.0 &= evictable;
            peer.1 = peer.1.min(quiet);
        }
        let Some(victim) = peers
            .into_iter()
            .filter(|(_, (evictable, _))| *evictable)
            .max_by_key(|(_, (_, quiet))| *quiet)
            .map(|(domain, _)| domain)
        else {
            return Vec::new();
        };
        live.iter()
            .filter(|(key, _)| &key.remote == victim)
            .map(|(key, _)| (*key).clone())
            .collect()
    }

    /// Make room for a new socket in `direction` when its socket cap is
    /// full: evict the established stream in that direction that has
    /// carried no traffic the longest, if that has lasted the eviction
    /// time, and return its socket for the new one to take over. The
    /// evicted connection sends `resource-constraint` and closes.
    ///
    /// A stream outside the reserve is evicted first. A reserved peer's
    /// stream is evicted only when no other stream can be and
    /// `newcomer_reserved` says the new socket may use the reserve: an
    /// outbound socket whose remote domain is a reserved peer, or any
    /// socket where admission holds nothing in reserve. An inbound socket
    /// is anonymous at accept, so in `open` mode it never evicts a reserved
    /// peer's stream.
    pub(super) fn evict_for_socket(
        &self,
        direction: StreamDirection,
        newcomer_reserved: bool,
        now: Instant,
    ) -> Option<Arc<SocketCount>> {
        let mut state = self.state();
        let quietest = |reserved: bool| {
            state
                .streams
                .iter()
                .filter(|(key, entry)| {
                    key.direction == direction
                        && entry.reserved == reserved
                        && self.evictable(entry, now)
                })
                .max_by_key(|(_, entry)| entry.activity.quiet(now))
                .map(|(key, _)| key.clone())
        };
        let victim = quietest(false).or_else(|| {
            if newcomer_reserved {
                quietest(true)
            } else {
                None
            }
        })?;
        let entry = state.streams.remove(&victim)?;
        entry.tokens.evict.cancel();
        Some(entry.socket)
    }

    /// Every established stream.
    pub(super) fn streams(&self) -> Vec<VerifiedPeerStream> {
        self.state()
            .streams
            .values()
            .map(|entry| entry.stream.clone())
            .collect()
    }

    /// The channel of the registered outbound `stream`, while the registry
    /// still holds that exact connection for its pair.
    pub(super) fn outbox(&self, stream: &VerifiedPeerStream) -> Option<Outbox> {
        if stream.direction() != StreamDirection::Outbound {
            return None;
        }
        let key = StreamKey {
            direction: StreamDirection::Outbound,
            local: stream.local().clone(),
            remote: stream.remote().clone(),
            connection: stream.connection(),
        };
        self.state()
            .streams
            .get(&key)
            .and_then(|entry| entry.outbox.clone())
    }

    /// Established streams in `direction`.
    pub(super) fn count(&self, direction: StreamDirection) -> usize {
        self.state()
            .streams
            .keys()
            .filter(|key| key.direction == direction)
            .count()
    }

    /// The established outbound stream from `local` to `remote`, or else a
    /// new outbound attempt if the remote domain's backoff has passed, no
    /// recent lookup found it without endpoints, no attempt for the pair is
    /// in progress, the cap on attempts allows one and the global dial rate
    /// has a token. Both are decided under one lock: an attempt registers
    /// its stream before it releases its dial slot, so a caller always
    /// finds either the stream or the attempt in progress, never neither. A
    /// domain without a backoff entry is never refused for the table's
    /// size; the dial rate bounds how often such domains are dialled.
    ///
    /// The token is taken before the endpoints are looked up, so a domain
    /// without endpoints costs one token per [`NO_ENDPOINT_WAIT`]; a sender
    /// who names many distinct such domains still spends one token for
    /// each, which the dial rate bounds.
    pub(super) fn begin_dial(
        self: &Arc<Self>,
        local: &CanonicalDomain,
        remote: &CanonicalDomain,
        now: Instant,
    ) -> Result<DialStart, DialRefusal> {
        let mut state = self.state();
        let established = state.streams.iter().find(|(key, _)| {
            key.direction == StreamDirection::Outbound
                && &key.local == local
                && &key.remote == remote
        });
        if let Some((_, entry)) = established {
            return Ok(DialStart::Established(entry.stream.clone()));
        }
        if state
            .backoff
            .get(remote)
            .is_some_and(|backoff| backoff.retry_at > now)
        {
            return Err(DialRefusal::BackingOff);
        }
        if let Some(entry) = state
            .no_endpoints
            .get(remote)
            .filter(|entry| entry.until > now)
        {
            return Err(DialRefusal::NoEndpoint(entry.error));
        }
        let pair = (local.clone(), remote.clone());
        if state.dialing.contains(&pair) {
            return Err(DialRefusal::InProgress);
        }
        if state.dialing.len() >= self.limits.max_dialing {
            return Err(DialRefusal::Busy);
        }
        if !state.dial_rate.try_take(now) {
            return Err(DialRefusal::RateLimited);
        }
        state.dialing.insert(pair.clone());
        Ok(DialStart::Dial(DialSlot {
            registry: Arc::clone(self),
            pair,
        }))
    }

    /// Remember for [`NO_ENDPOINT_WAIT`] that `remote` has no endpoint, as
    /// `error` says, so repeated requests for it spend no dial token. The
    /// table holds at most as many domains as the backoff table; when it is
    /// full of current entries, the one that expires first makes room,
    /// which costs that domain only one more token later.
    pub(super) fn record_no_endpoints(&self, remote: &CanonicalDomain, error: ResolveError) {
        let now = Instant::now();
        let mut state = self.state();
        state.no_endpoints.retain(|_, entry| entry.until > now);
        if !state.no_endpoints.contains_key(remote)
            && state.no_endpoints.len() >= self.limits.max_backoff_destinations
        {
            let first = state
                .no_endpoints
                .iter()
                .min_by_key(|(_, entry)| entry.until)
                .map(|(domain, _)| domain.clone());
            if let Some(first) = first {
                state.no_endpoints.remove(&first);
            }
        }
        state.no_endpoints.insert(
            remote.clone(),
            NoEndpoint {
                until: now + NO_ENDPOINT_WAIT,
                error,
            },
        );
    }

    /// Record a failed outbound attempt to `remote` that the peer or the
    /// path to it caused: the wait doubles from the initial backoff up to
    /// the maximum.
    ///
    /// An entry whose wait passed longer ago than the maximum backoff is
    /// forgotten first, so a later failure starts again from the initial
    /// wait. An entry is kept at least until its wait has passed: when the
    /// table is full, a new domain's entry replaces only the entry updated
    /// longest ago among those whose wait has passed, and when every entry
    /// is still waiting the new failure is not recorded. Failures that
    /// local users cause, to as many domains as they like, therefore never
    /// shorten another domain's wait; a domain whose failure was not
    /// recorded is dialled again only within the global dial rate. The
    /// table bounds memory, never which domains can be dialled.
    pub(super) fn record_failure(&self, remote: &CanonicalDomain) {
        let now = Instant::now();
        let horizon = self.deadlines.backoff_max;
        let mut state = self.state();
        state
            .backoff
            .retain(|_, backoff| now.saturating_duration_since(backoff.retry_at) < horizon);
        if !state.backoff.contains_key(remote)
            && state.backoff.len() >= self.limits.max_backoff_destinations
        {
            match oldest_backoff(&state.backoff, now) {
                Some(victim) => {
                    state.backoff.remove(&victim);
                }
                None => {
                    tracing::debug!(
                        "s2s backoff table full of waiting entries; failure not recorded"
                    );
                    return;
                }
            }
        }
        let failures = state
            .backoff
            .get(remote)
            .map_or(1, |backoff| backoff.failures.saturating_add(1));
        let exponent = failures.saturating_sub(1).min(31);
        let wait = self
            .deadlines
            .backoff_initial
            .saturating_mul(1u32 << exponent)
            .min(self.deadlines.backoff_max);
        state.backoff.insert(
            remote.clone(),
            Backoff {
                failures,
                retry_at: now + wait,
                updated: now,
            },
        );
    }

    /// Forget the backoff of `remote` after a successful attempt.
    pub(super) fn record_success(&self, remote: &CanonicalDomain) {
        self.state().backoff.remove(remote);
    }
}

/// The backoff entry updated longest ago among those whose wait has passed
/// by `now`.
fn oldest_backoff(
    backoff: &HashMap<CanonicalDomain, Backoff>,
    now: Instant,
) -> Option<CanonicalDomain> {
    backoff
        .iter()
        .filter(|(_, entry)| entry.retry_at <= now)
        .min_by_key(|(_, entry)| entry.updated)
        .map(|(domain, _)| domain.clone())
}

/// Distinct remote domains among `live` streams; only those outside the
/// reserve when `general`.
fn peers(live: &[(&StreamKey, &Entry)], general: bool) -> usize {
    live.iter()
        .filter(|(_, entry)| !general || !entry.reserved)
        .map(|(key, _)| &key.remote)
        .collect::<BTreeSet<_>>()
        .len()
}

/// One stream negotiating with a remote domain in one direction.
#[derive(Debug)]
pub(super) struct NegotiationSlot {
    registry: Arc<Registry>,
    key: (StreamDirection, CanonicalDomain),
}

impl Drop for NegotiationSlot {
    fn drop(&mut self) {
        let mut state = self.registry.state();
        if let Some(count) = state.negotiating.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.negotiating.remove(&self.key);
            }
        }
    }
}

/// A registered stream; dropping it unregisters the stream unless it was
/// replaced or evicted. Its key names its own connection, so only it is
/// removed.
#[derive(Debug)]
pub(super) struct StreamSlot {
    registry: Arc<Registry>,
    key: StreamKey,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        self.registry.state().streams.remove(&self.key);
    }
}

/// An outbound attempt in progress for one directional pair.
#[derive(Debug)]
pub(super) struct DialSlot {
    registry: Arc<Registry>,
    pair: (CanonicalDomain, CanonicalDomain),
}

impl Drop for DialSlot {
    fn drop(&mut self) {
        self.registry.state().dialing.remove(&self.pair);
    }
}
