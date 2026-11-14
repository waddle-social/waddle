//! Global socket, handshake and rate limits (ADR-0020 gate G7).
//!
//! Every admitted socket and handshake holds a slot whose drop releases it,
//! so a connection task that ends in any way, including cancellation,
//! returns its capacity. The lock is never held across an await.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use waddle_xmpp::telemetry::attributes::S2sResource;

use super::peer_stream::StreamDirection;
use crate::s2s::config::{RateLimit, ResourceLimits};

/// How long a source address with no unauthenticated socket stays in the
/// table after its last connection.
const ADDRESS_EXPIRY: Duration = Duration::from_secs(60);

/// A token bucket. Tokens may go negative when more is consumed than was
/// available, which delays the next use instead of refusing it.
#[derive(Debug, Clone)]
pub(super) struct TokenBucket {
    per_second: f64,
    burst: f64,
    tokens: f64,
    updated: Instant,
}

impl TokenBucket {
    pub(super) fn new(limit: RateLimit, now: Instant) -> Self {
        Self {
            per_second: f64::from(limit.per_second),
            burst: f64::from(limit.burst),
            tokens: f64::from(limit.burst),
            updated: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.burst);
        self.updated = now;
    }

    /// Take one token if one is available.
    pub(super) fn try_take(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Consume `amount` tokens, going negative if needed.
    pub(super) fn consume(&mut self, amount: usize, now: Instant) {
        self.refill(now);
        self.tokens -= f64::from(u32::try_from(amount).unwrap_or(u32::MAX));
    }

    /// How long until the balance is no longer negative.
    pub(super) fn delay(&mut self, now: Instant) -> Duration {
        self.refill(now);
        if self.tokens >= 0.0 || self.per_second <= 0.0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(-self.tokens / self.per_second)
    }
}

#[derive(Debug)]
struct AddressState {
    bucket: TokenBucket,
    unauthenticated: usize,
    last_seen: Instant,
}

#[derive(Debug)]
struct State {
    sockets: usize,
    inbound: usize,
    outbound: usize,
    unauthenticated: usize,
    inbound_handshakes: usize,
    outbound_handshakes: usize,
    accept: TokenBucket,
    input: TokenBucket,
    addresses: HashMap<IpAddr, AddressState>,
}

/// The process-wide S2S socket, handshake and rate limits.
#[derive(Debug)]
pub(super) struct Capacity {
    limits: ResourceLimits,
    state: Mutex<State>,
}

impl Capacity {
    pub(super) fn new(limits: ResourceLimits) -> Arc<Self> {
        let now = Instant::now();
        Arc::new(Self {
            limits,
            state: Mutex::new(State {
                sockets: 0,
                inbound: 0,
                outbound: 0,
                unauthenticated: 0,
                inbound_handshakes: 0,
                outbound_handshakes: 0,
                accept: TokenBucket::new(limits.accept_rate, now),
                input: TokenBucket::new(limits.input_rate_global, now),
                addresses: HashMap::new(),
            }),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Every update leaves the counters consistent, so a poisoned lock
        // still holds a usable state.
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Admit a new inbound socket from `address`, or name the limit that
    /// refuses it. A refused socket is closed without any XML.
    pub(super) fn admit_inbound(
        self: &Arc<Self>,
        address: IpAddr,
    ) -> Result<InboundSlot, S2sResource> {
        let now = Instant::now();
        let limits = self.limits;
        let mut state = self.state();
        if state.sockets >= limits.max_sockets || state.inbound >= limits.max_inbound_sockets {
            return Err(S2sResource::Sockets);
        }
        if state.unauthenticated >= limits.max_unauthenticated {
            return Err(S2sResource::Unauthenticated);
        }
        if !state.accept.try_take(now) {
            return Err(S2sResource::AcceptRate);
        }
        if !state.addresses.contains_key(&address) {
            state.addresses.retain(|_, entry| {
                entry.unauthenticated > 0
                    || now.saturating_duration_since(entry.last_seen) < ADDRESS_EXPIRY
            });
            if state.addresses.len() >= limits.max_tracked_addresses {
                return Err(S2sResource::SourceAddress);
            }
        }
        let entry = state
            .addresses
            .entry(address)
            .or_insert_with(|| AddressState {
                bucket: TokenBucket::new(limits.accept_rate_per_address, now),
                unauthenticated: 0,
                last_seen: now,
            });
        entry.last_seen = now;
        if entry.unauthenticated >= limits.max_unauthenticated_per_address {
            return Err(S2sResource::SourceAddress);
        }
        if !entry.bucket.try_take(now) {
            return Err(S2sResource::AcceptRate);
        }
        entry.unauthenticated += 1;
        state.sockets += 1;
        state.inbound += 1;
        state.unauthenticated += 1;
        Ok(InboundSlot {
            capacity: Arc::clone(self),
            address,
            authenticated: false,
        })
    }

    /// Admit a new outbound socket.
    pub(super) fn admit_outbound(self: &Arc<Self>) -> Result<OutboundSlot, S2sResource> {
        let mut state = self.state();
        if state.sockets >= self.limits.max_sockets
            || state.outbound >= self.limits.max_outbound_sockets
        {
            return Err(S2sResource::Sockets);
        }
        state.sockets += 1;
        state.outbound += 1;
        Ok(OutboundSlot {
            capacity: Arc::clone(self),
        })
    }

    /// Start a TLS handshake in `direction` if fewer than that direction's
    /// limit are in progress.
    pub(super) fn handshake(
        self: &Arc<Self>,
        direction: StreamDirection,
    ) -> Result<HandshakeSlot, S2sResource> {
        let mut state = self.state();
        let (count, limit) = match direction {
            StreamDirection::Inbound => (
                &mut state.inbound_handshakes,
                self.limits.max_inbound_handshakes,
            ),
            StreamDirection::Outbound => (
                &mut state.outbound_handshakes,
                self.limits.max_outbound_handshakes,
            ),
        };
        if *count >= limit {
            return Err(S2sResource::Handshakes);
        }
        *count += 1;
        Ok(HandshakeSlot {
            capacity: Arc::clone(self),
            direction,
        })
    }

    /// Account `bytes` read from any S2S socket against the global input
    /// rate.
    pub(super) fn consume_input(&self, bytes: usize) {
        self.state().input.consume(bytes, Instant::now());
    }

    /// How long every S2S socket waits before its next read, because the
    /// global input rate is exhausted.
    pub(super) fn input_delay(&self) -> Duration {
        self.state().input.delay(Instant::now())
    }

    /// Open sockets, for tests and logs.
    pub(super) fn open_sockets(&self) -> usize {
        self.state().sockets
    }
}

/// An admitted inbound socket.
#[derive(Debug)]
pub(super) struct InboundSlot {
    capacity: Arc<Capacity>,
    address: IpAddr,
    authenticated: bool,
}

impl InboundSlot {
    /// The socket's stream authenticated: it no longer counts against the
    /// unauthenticated limits.
    pub(super) fn authenticated(&mut self) {
        if self.authenticated {
            return;
        }
        self.authenticated = true;
        let mut state = self.capacity.state();
        state.unauthenticated = state.unauthenticated.saturating_sub(1);
        if let Some(entry) = state.addresses.get_mut(&self.address) {
            entry.unauthenticated = entry.unauthenticated.saturating_sub(1);
        }
    }
}

impl Drop for InboundSlot {
    fn drop(&mut self) {
        self.authenticated();
        let mut state = self.capacity.state();
        state.sockets = state.sockets.saturating_sub(1);
        state.inbound = state.inbound.saturating_sub(1);
    }
}

/// An admitted outbound socket.
#[derive(Debug)]
pub(super) struct OutboundSlot {
    capacity: Arc<Capacity>,
}

impl Drop for OutboundSlot {
    fn drop(&mut self) {
        let mut state = self.capacity.state();
        state.sockets = state.sockets.saturating_sub(1);
        state.outbound = state.outbound.saturating_sub(1);
    }
}

/// A TLS handshake in progress.
#[derive(Debug)]
pub(super) struct HandshakeSlot {
    capacity: Arc<Capacity>,
    direction: StreamDirection,
}

impl Drop for HandshakeSlot {
    fn drop(&mut self) {
        let mut state = self.capacity.state();
        let count = match self.direction {
            StreamDirection::Inbound => &mut state.inbound_handshakes,
            StreamDirection::Outbound => &mut state.outbound_handshakes,
        };
        *count = count.saturating_sub(1);
    }
}
