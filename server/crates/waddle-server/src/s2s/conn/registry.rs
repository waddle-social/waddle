//! Established streams, per-peer negotiation counts, outbound dials in
//! progress and outbound backoff (ADR-0020 gate G7).
//!
//! One stream per exact directional domain pair: a new authenticated stream
//! for a pair replaces the existing one, which closes with `conflict`,
//! unless the existing one is younger than the replacement interval, in
//! which case the new one is refused with `conflict`. When the
//! stream or peer cap is reached, the stream (or, for the peer cap, the
//! peer) idle the longest is evicted, but only once it has been idle for
//! the configured eviction time; otherwise the new stream is refused. A
//! peer that keeps its streams busy is therefore never displaced by a new
//! one, and an attacker who opens streams from many domains can displace
//! only streams that carry nothing.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::telemetry::attributes::S2sResource;

use super::peer_stream::{StreamDirection, VerifiedPeerStream};
use crate::s2s::config::{Deadlines, ResourceLimits};

/// The last time a connection received input, shared between its task and
/// the registry's eviction choice.
#[derive(Debug)]
pub(super) struct Activity {
    epoch: Instant,
    last_millis: AtomicU64,
}

impl Activity {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: Instant::now(),
            last_millis: AtomicU64::new(0),
        })
    }

    /// Record input now.
    pub(super) fn touch(&self) {
        let millis = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_millis.store(millis, Ordering::Relaxed);
    }

    /// How long since the last input.
    pub(super) fn idle(&self, now: Instant) -> Duration {
        let last = self.epoch + Duration::from_millis(self.last_millis.load(Ordering::Relaxed));
        now.saturating_duration_since(last)
    }
}

/// Why a stream could not be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RegisterRefusal {
    /// Another stream carries the same directional pair and is too young
    /// to be replaced.
    Conflict,
    /// A cap is reached and nothing can be evicted.
    Full(S2sResource),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StreamKey {
    direction: StreamDirection,
    local: CanonicalDomain,
    remote: CanonicalDomain,
}

impl StreamKey {
    fn of(stream: &VerifiedPeerStream) -> Self {
        Self {
            direction: stream.direction(),
            local: stream.local().clone(),
            remote: stream.remote().clone(),
        }
    }
}

/// How the registry ends a registered stream's life early.
#[derive(Debug, Clone, Default)]
pub(super) struct StreamTokens {
    /// Cancelled when the stream is evicted for capacity.
    pub(super) evict: CancellationToken,
    /// Cancelled when a newer stream for the same pair replaces it.
    pub(super) replace: CancellationToken,
}

#[derive(Debug)]
struct Entry {
    stream: VerifiedPeerStream,
    activity: Arc<Activity>,
    tokens: StreamTokens,
    registered_at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct Backoff {
    failures: u32,
    retry_at: Instant,
}

#[derive(Debug, Default)]
struct State {
    streams: BTreeMap<StreamKey, Entry>,
    negotiating: HashMap<CanonicalDomain, usize>,
    /// Remote domains with an outbound attempt in progress.
    dialing: HashSet<CanonicalDomain>,
    /// Outbound backoff by remote domain.
    backoff: HashMap<CanonicalDomain, Backoff>,
}

/// Why an outbound attempt may not start now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DialRefusal {
    /// An earlier attempt to the domain failed and its wait has not passed.
    BackingOff,
    /// An attempt to the domain is in progress.
    InProgress,
    /// The cap on attempts in progress is reached.
    Busy,
    /// The backoff table is full of domains still waiting.
    TooManyDestinations,
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
            state: Mutex::new(State::default()),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Every update leaves the maps consistent, so a poisoned lock still
        // holds a usable state.
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Count one more stream negotiating for `remote`, in either direction.
    pub(super) fn begin_negotiation(
        self: &Arc<Self>,
        remote: &CanonicalDomain,
    ) -> Result<NegotiationSlot, S2sResource> {
        let mut state = self.state();
        let count = state.negotiating.entry(remote.clone()).or_insert(0);
        if *count >= self.limits.max_negotiating_per_peer {
            return Err(S2sResource::PerPeer);
        }
        *count += 1;
        Ok(NegotiationSlot {
            registry: Arc::clone(self),
            remote: remote.clone(),
        })
    }

    /// Register an authenticated stream, replacing the pair's existing
    /// stream or evicting an idle one if a cap requires it.
    pub(super) fn register(
        self: &Arc<Self>,
        stream: VerifiedPeerStream,
        activity: Arc<Activity>,
        tokens: StreamTokens,
    ) -> Result<StreamSlot, RegisterRefusal> {
        let key = StreamKey::of(&stream);
        let now = Instant::now();
        let mut state = self.state();
        if let Some(existing) = state.streams.get(&key) {
            let age = now.saturating_duration_since(existing.registered_at);
            if age < self.deadlines.replacement_interval {
                return Err(RegisterRefusal::Conflict);
            }
            if let Some(replaced) = state.streams.remove(&key) {
                replaced.tokens.replace.cancel();
            }
        }
        let new_peer = !state
            .streams
            .keys()
            .any(|existing| existing.remote == key.remote);
        loop {
            let peers = distinct_peers(&state.streams);
            if new_peer && peers >= self.limits.max_peers {
                if !self.evict_peer(&mut state, &key.remote, now) {
                    return Err(RegisterRefusal::Full(S2sResource::Peers));
                }
                continue;
            }
            if state.streams.len() >= self.limits.max_streams {
                if !self.evict_stream(&mut state, now) {
                    return Err(RegisterRefusal::Full(S2sResource::Streams));
                }
                continue;
            }
            break;
        }
        let connection = stream.connection();
        state.streams.insert(
            key.clone(),
            Entry {
                stream,
                activity,
                tokens,
                registered_at: now,
            },
        );
        Ok(StreamSlot {
            registry: Arc::clone(self),
            key,
            connection,
        })
    }

    fn evictable(&self, entry: &Entry, now: Instant) -> bool {
        entry.activity.idle(now) >= self.deadlines.eviction_idle
    }

    /// Evict the stream idle the longest, if one has been idle long enough.
    fn evict_stream(&self, state: &mut State, now: Instant) -> bool {
        let victim = state
            .streams
            .iter()
            .filter(|(_, entry)| self.evictable(entry, now))
            .max_by_key(|(_, entry)| entry.activity.idle(now))
            .map(|(key, _)| key.clone());
        match victim.and_then(|key| state.streams.remove(&key)) {
            Some(entry) => {
                entry.tokens.evict.cancel();
                true
            }
            None => false,
        }
    }

    /// Evict every stream of the peer whose most recent input is the
    /// oldest, among peers other than `keep` whose streams have all been
    /// idle long enough.
    fn evict_peer(&self, state: &mut State, keep: &CanonicalDomain, now: Instant) -> bool {
        let mut peers: BTreeMap<&CanonicalDomain, (bool, Duration)> = BTreeMap::new();
        for (key, entry) in &state.streams {
            if &key.remote == keep {
                continue;
            }
            let idle = entry.activity.idle(now);
            let evictable = self.evictable(entry, now);
            let slot = peers.entry(&key.remote).or_insert((true, idle));
            slot.0 &= evictable;
            slot.1 = slot.1.min(idle);
        }
        let victim = peers
            .into_iter()
            .filter(|(_, (evictable, _))| *evictable)
            .max_by_key(|(_, (_, idle))| *idle)
            .map(|(domain, _)| domain.clone());
        let Some(victim) = victim else {
            return false;
        };
        let keys: Vec<StreamKey> = state
            .streams
            .keys()
            .filter(|key| key.remote == victim)
            .cloned()
            .collect();
        for key in keys {
            if let Some(entry) = state.streams.remove(&key) {
                entry.tokens.evict.cancel();
            }
        }
        true
    }

    /// The established outbound stream for a pair, if there is one.
    pub(super) fn outbound(
        &self,
        local: &CanonicalDomain,
        remote: &CanonicalDomain,
    ) -> Option<VerifiedPeerStream> {
        let key = StreamKey {
            direction: StreamDirection::Outbound,
            local: local.clone(),
            remote: remote.clone(),
        };
        self.state()
            .streams
            .get(&key)
            .map(|entry| entry.stream.clone())
    }

    /// Every established stream.
    pub(super) fn streams(&self) -> Vec<VerifiedPeerStream> {
        self.state()
            .streams
            .values()
            .map(|entry| entry.stream.clone())
            .collect()
    }

    /// Established streams in `direction`.
    pub(super) fn count(&self, direction: StreamDirection) -> usize {
        self.state()
            .streams
            .keys()
            .filter(|key| key.direction == direction)
            .count()
    }

    /// Start an outbound attempt to `remote` if its backoff has passed, no
    /// attempt to it is in progress and the caps allow one.
    pub(super) fn begin_dial(
        self: &Arc<Self>,
        remote: &CanonicalDomain,
        now: Instant,
    ) -> Result<DialSlot, DialRefusal> {
        let mut state = self.state();
        match state.backoff.get(remote) {
            Some(backoff) if backoff.retry_at > now => return Err(DialRefusal::BackingOff),
            Some(_) => {}
            None => {
                if state.backoff.len() >= self.limits.max_backoff_destinations {
                    self.expire_backoff(&mut state, now);
                    if state.backoff.len() >= self.limits.max_backoff_destinations {
                        return Err(DialRefusal::TooManyDestinations);
                    }
                }
            }
        }
        if state.dialing.contains(remote) {
            return Err(DialRefusal::InProgress);
        }
        if state.dialing.len() >= self.limits.max_dialing {
            return Err(DialRefusal::Busy);
        }
        state.dialing.insert(remote.clone());
        Ok(DialSlot {
            registry: Arc::clone(self),
            remote: remote.clone(),
        })
    }

    /// Forget backoff entries whose wait passed longer ago than the
    /// longest backoff; a later failure starts again from the initial wait.
    fn expire_backoff(&self, state: &mut State, now: Instant) {
        let horizon = self.deadlines.backoff_max;
        state
            .backoff
            .retain(|_, backoff| now.saturating_duration_since(backoff.retry_at) < horizon);
    }

    /// Record a failed outbound attempt to `remote` that the peer or the
    /// path to it caused: the wait doubles from the initial backoff up to
    /// the maximum. A domain already in the table keeps its entry; a new
    /// one is added only below the destination cap, which
    /// [`Registry::begin_dial`] already enforced.
    pub(super) fn record_failure(&self, remote: &CanonicalDomain) {
        let now = Instant::now();
        let mut state = self.state();
        if !state.backoff.contains_key(remote) {
            self.expire_backoff(&mut state, now);
            if state.backoff.len() >= self.limits.max_backoff_destinations {
                return;
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
            },
        );
    }

    /// Forget the backoff of `remote` after a successful attempt.
    pub(super) fn record_success(&self, remote: &CanonicalDomain) {
        self.state().backoff.remove(remote);
    }
}

fn distinct_peers(streams: &BTreeMap<StreamKey, Entry>) -> usize {
    let mut peers: Vec<&CanonicalDomain> = streams.keys().map(|key| &key.remote).collect();
    peers.sort();
    peers.dedup();
    peers.len()
}

/// One stream negotiating for a remote domain.
#[derive(Debug)]
pub(super) struct NegotiationSlot {
    registry: Arc<Registry>,
    remote: CanonicalDomain,
}

impl Drop for NegotiationSlot {
    fn drop(&mut self) {
        let mut state = self.registry.state();
        if let Some(count) = state.negotiating.get_mut(&self.remote) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.negotiating.remove(&self.remote);
            }
        }
    }
}

/// A registered stream; dropping it unregisters the stream unless it was
/// evicted.
#[derive(Debug)]
pub(super) struct StreamSlot {
    registry: Arc<Registry>,
    key: StreamKey,
    connection: u64,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut state = self.registry.state();
        let owned = state
            .streams
            .get(&self.key)
            .is_some_and(|entry| entry.stream.connection() == self.connection);
        if owned {
            state.streams.remove(&self.key);
        }
    }
}

/// An outbound attempt in progress to one remote domain.
#[derive(Debug)]
pub(super) struct DialSlot {
    registry: Arc<Registry>,
    remote: CanonicalDomain,
}

impl Drop for DialSlot {
    fn drop(&mut self) {
        self.registry.state().dialing.remove(&self.remote);
    }
}
