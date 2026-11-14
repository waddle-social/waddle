//! Global socket, handshake and rate limits (ADR-0020 gate G7).
//!
//! Every admitted socket and handshake holds a slot whose drop releases it,
//! so a connection task that ends in any way, including cancellation,
//! returns its capacity. The lock is never held across an await.
//!
//! A socket's place in the socket counts is a [`SocketCount`], which the
//! registry also holds for an established stream: evicting an idle stream
//! to make room for a new socket releases the evicted socket's count at
//! once, while its connection sends `resource-constraint` and closes within
//! the write deadline.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use waddle_xmpp::telemetry::attributes::S2sResource;

use super::peer_stream::StreamDirection;
use crate::s2s::config::{RateLimit, ResourceLimits};

/// How long a source address with no unauthenticated socket stays in the
/// table after its last connection.
const ADDRESS_EXPIRY: Duration = Duration::from_secs(60);

/// The bits of an IPv6 address that name its /64 prefix.
const IPV6_PREFIX_64: u128 = !((1u128 << 64) - 1);

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

    /// Whether a token is available, without taking it.
    fn has_token(&mut self, now: Instant) -> bool {
        self.refill(now);
        self.tokens >= 1.0
    }

    /// Take one token if one is available.
    pub(super) fn try_take(&mut self, now: Instant) -> bool {
        if self.has_token(now) {
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

/// The key a source address is counted under: an IPv4 address as itself
/// (also when written as an IPv4-mapped IPv6 address), and an IPv6 address
/// by its /64 prefix, the smallest block a site is usually given, so a
/// source cannot multiply its per-address allowance by rotating through
/// the addresses of its own prefix.
fn address_key(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & IPV6_PREFIX_64)),
        },
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
    ///
    /// The socket caps are checked first, then the global unauthenticated
    /// cap, then every per-address limit, and only then is a global accept
    /// token spent: a source over its own limits cannot drain the allowance
    /// every other source shares.
    pub(super) fn admit_inbound(
        self: &Arc<Self>,
        address: IpAddr,
    ) -> Result<InboundSlot, S2sResource> {
        let now = Instant::now();
        let limits = self.limits;
        let key = address_key(address);
        let mut state = self.state();
        if state.sockets >= limits.max_sockets || state.inbound >= limits.max_inbound_sockets {
            return Err(S2sResource::Sockets);
        }
        if state.unauthenticated >= limits.max_unauthenticated {
            return Err(S2sResource::Unauthenticated);
        }
        if !state.addresses.contains_key(&key) {
            state.addresses.retain(|_, entry| {
                entry.unauthenticated > 0
                    || now.saturating_duration_since(entry.last_seen) < ADDRESS_EXPIRY
            });
            if state.addresses.len() >= limits.max_tracked_addresses {
                return Err(S2sResource::SourceAddress);
            }
        }
        let entry = state.addresses.entry(key).or_insert_with(|| AddressState {
            bucket: TokenBucket::new(limits.accept_rate_per_address, now),
            unauthenticated: 0,
            last_seen: now,
        });
        entry.last_seen = now;
        if entry.unauthenticated >= limits.max_unauthenticated_per_address {
            return Err(S2sResource::SourceAddress);
        }
        if !entry.bucket.has_token(now) {
            return Err(S2sResource::AcceptRate);
        }
        if !state.accept.try_take(now) {
            return Err(S2sResource::AcceptRate);
        }
        if let Some(entry) = state.addresses.get_mut(&key) {
            // The token was available above, under the same lock and at
            // the same instant.
            entry.bucket.try_take(now);
            entry.unauthenticated += 1;
        }
        state.sockets += 1;
        state.inbound += 1;
        state.unauthenticated += 1;
        Ok(InboundSlot {
            socket: SocketCount::new(Arc::clone(self), StreamDirection::Inbound),
            address: key,
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
            socket: SocketCount::new(Arc::clone(self), StreamDirection::Outbound),
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

    fn release_socket(&self, direction: StreamDirection) {
        let mut state = self.state();
        state.sockets = state.sockets.saturating_sub(1);
        let count = match direction {
            StreamDirection::Inbound => &mut state.inbound,
            StreamDirection::Outbound => &mut state.outbound,
        };
        *count = count.saturating_sub(1);
    }
}

/// One admitted socket's place in the socket counts. It is released
/// exactly once: when its slot drops, or earlier when its established
/// stream is evicted to make room for a new socket.
#[derive(Debug)]
pub(super) struct SocketCount {
    capacity: Arc<Capacity>,
    direction: StreamDirection,
    released: AtomicBool,
}

impl SocketCount {
    fn new(capacity: Arc<Capacity>, direction: StreamDirection) -> Arc<Self> {
        Arc::new(Self {
            capacity,
            direction,
            released: AtomicBool::new(false),
        })
    }

    /// Stop counting this socket. Later calls do nothing.
    pub(super) fn release(&self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            self.capacity.release_socket(self.direction);
        }
    }
}

/// An admitted inbound socket.
#[derive(Debug)]
pub(super) struct InboundSlot {
    socket: Arc<SocketCount>,
    /// The source address key the socket counts under.
    address: IpAddr,
    authenticated: bool,
}

impl InboundSlot {
    /// The socket's place in the socket counts.
    pub(super) fn socket(&self) -> Arc<SocketCount> {
        Arc::clone(&self.socket)
    }

    /// The socket's stream authenticated: it no longer counts against the
    /// unauthenticated limits.
    pub(super) fn authenticated(&mut self) {
        if self.authenticated {
            return;
        }
        self.authenticated = true;
        let mut state = self.socket.capacity.state();
        state.unauthenticated = state.unauthenticated.saturating_sub(1);
        if let Some(entry) = state.addresses.get_mut(&self.address) {
            entry.unauthenticated = entry.unauthenticated.saturating_sub(1);
        }
    }
}

impl Drop for InboundSlot {
    fn drop(&mut self) {
        self.authenticated();
        self.socket.release();
    }
}

/// An admitted outbound socket.
#[derive(Debug)]
pub(super) struct OutboundSlot {
    socket: Arc<SocketCount>,
}

impl OutboundSlot {
    /// The socket's place in the socket counts.
    pub(super) fn socket(&self) -> Arc<SocketCount> {
        Arc::clone(&self.socket)
    }
}

impl Drop for OutboundSlot {
    fn drop(&mut self) {
        self.socket.release();
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
