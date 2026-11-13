//! Unwired custody primitive. This does not bound the live outbound queues.
//!
//! An adapter must supply a proven upper bound for the retained payload's
//! lifetime; no such bound is established here for `OutboundStanza` or
//! `Stanza`. The budget covers that supplied charge, not allocator overhead,
//! unrelated copies, transport buffers, or durable storage. A future caller
//! must also keep the custody owner alive independently of its offer futures.
//! The local scope token is not yet bound to a real connection, binding, or
//! remote registration; that binding is a prerequisite for any integration.
//! Budgets are per custody owner; separately created owners do not share a
//! global limit. Offer replies are assertions from an adapter, not proof of a
//! real transport result. In particular, a future `NotEnqueued` adapter must
//! prove that result for the exact offer before using it for a handoff.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
}

#[derive(Default)]
struct Usage {
    count: usize,
    bytes: usize,
}

struct Budget {
    max_count: usize,
    max_bytes: usize,
    usage: Mutex<Usage>,
}

impl Budget {
    fn reserve(self: &Arc<Self>, bytes: usize) -> Option<Lease> {
        let mut usage = lock(&self.usage);
        let next_count = usage.count.checked_add(1)?;
        let next_bytes = usage.bytes.checked_add(bytes)?;
        if next_count > self.max_count || next_bytes > self.max_bytes {
            return None;
        }
        usage.count = next_count;
        usage.bytes = next_bytes;
        Some(Lease {
            budget: Arc::clone(self),
            bytes,
        })
    }
}

// Only a successful reservation can construct a lease. It cannot be cloned;
// snapshots share the envelope that owns it instead.
struct Lease {
    budget: Arc<Budget>,
    bytes: usize,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut usage = lock(&self.budget.usage);
        usage.count -= 1;
        usage.bytes -= self.bytes;
    }
}

pub(super) struct Envelope<T> {
    payload: T,
    _lease: Lease,
}

impl<T> Envelope<T> {
    pub(super) fn payload(&self) -> &T {
        &self.payload
    }
}

/// A process-local opaque owner/binding generation. Its identity is compared
/// by allocation identity, not just by the locally repeated sequence number.
#[derive(Clone, Debug)]
pub(super) struct Scope {
    identity: Arc<()>,
    sequence: u64,
}

impl PartialEq for Scope {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence && Arc::ptr_eq(&self.identity, &other.identity)
    }
}

impl Eq for Scope {}

impl Hash for Scope {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.identity).hash(state);
        self.sequence.hash(state);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct EnvelopeId {
    scope: Scope,
    nonce: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Admission {
    Open,
    Sealed,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Stage {
    Queued,
    InFlight,
    NotAttempted,
    NotEnqueued,
    OfferAccepted,
    Uncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OfferReply {
    Accepted,
    NotEnqueued,
    Unknown,
}

struct Record<T> {
    scope: Scope,
    stage: Stage,
    envelope: Arc<Envelope<T>>,
}

struct State<T> {
    scope: Scope,
    admission: Admission,
    next_id: u64,
    records: HashMap<EnvelopeId, Record<T>>,
}

/// Process-local bookkeeping only. A separate owner must retain this object
/// until every record has been explicitly transferred or reconciled.
pub(super) struct Custody<T> {
    budget: Arc<Budget>,
    state: Mutex<State<T>>,
}

impl<T> Custody<T> {
    pub(super) fn new(max_count: usize, max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            budget: Arc::new(Budget {
                max_count,
                max_bytes,
                usage: Mutex::new(Usage::default()),
            }),
            state: Mutex::new(State {
                scope: Scope {
                    identity: Arc::new(()),
                    sequence: 0,
                },
                admission: Admission::Open,
                next_id: 0,
                records: HashMap::new(),
            }),
        })
    }

    pub(super) fn scope(&self) -> Scope {
        lock(&self.state).scope.clone()
    }

    /// The caller supplies a lifetime upper bound; the primitive cannot
    /// validate it. A refusal returns the original owned value untouched.
    pub(super) fn admit(&self, scope: &Scope, payload: T, bytes: usize) -> Result<EnvelopeId, T> {
        let mut state = lock(&self.state);
        if state.scope != *scope || state.admission != Admission::Open {
            return Err(payload);
        }
        let Some(next_id) = state.next_id.checked_add(1) else {
            return Err(payload);
        };
        let Some(lease) = self.budget.reserve(bytes) else {
            return Err(payload);
        };
        let id = EnvelopeId {
            scope: scope.clone(),
            nonce: next_id,
        };
        state.next_id = next_id;
        state.records.insert(
            id.clone(),
            Record {
                scope: scope.clone(),
                stage: Stage::Queued,
                envelope: Arc::new(Envelope {
                    payload,
                    _lease: lease,
                }),
            },
        );
        Ok(id)
    }

    pub(super) fn snapshot(&self, id: &EnvelopeId) -> Option<Arc<Envelope<T>>> {
        lock(&self.state)
            .records
            .get(id)
            .map(|record| Arc::clone(&record.envelope))
    }

    pub(super) fn stage(&self, id: &EnvelopeId) -> Option<Stage> {
        lock(&self.state).records.get(id).map(|record| record.stage)
    }

    /// Register the in-flight record synchronously, before an adapter can be
    /// awaited. The caller must separately retain the custody owner. Dropping
    /// the guard records unknown outcome; it never infers that a cancelled
    /// offer was refused.
    pub(super) fn begin_offer(
        self: &Arc<Self>,
        scope: &Scope,
        id: &EnvelopeId,
    ) -> Option<Offer<T>> {
        let mut state = lock(&self.state);
        if state.scope != *scope || state.admission != Admission::Open {
            return None;
        }
        let record = state.records.get_mut(id)?;
        if record.scope != *scope || record.stage != Stage::Queued {
            return None;
        }
        record.stage = Stage::InFlight;
        Some(Offer {
            owner: Arc::clone(self),
            scope: scope.clone(),
            id: id.clone(),
            envelope: Arc::clone(&record.envelope),
        })
    }

    fn retire_queued(state: &mut State<T>) {
        for record in state.records.values_mut() {
            if record.scope == state.scope && record.stage == Stage::Queued {
                record.stage = Stage::NotAttempted;
            }
        }
    }

    pub(super) fn seal(&self, scope: &Scope) -> bool {
        let mut state = lock(&self.state);
        if state.scope != *scope || state.admission == Admission::Closed {
            return false;
        }
        state.admission = Admission::Sealed;
        Self::retire_queued(&mut state);
        true
    }

    pub(super) fn close(&self, scope: &Scope) -> bool {
        let mut state = lock(&self.state);
        if state.scope != *scope {
            return false;
        }
        state.admission = Admission::Closed;
        Self::retire_queued(&mut state);
        true
    }

    /// Atomically fence the old generation and open the next one. Existing
    /// leases remain charged to the same budget, including old snapshots.
    pub(super) fn replace(&self, scope: &Scope) -> Option<Scope> {
        let mut state = lock(&self.state);
        if state.scope != *scope {
            return None;
        }
        let next = Scope {
            identity: Arc::new(()),
            sequence: scope.sequence.checked_add(1)?,
        };
        Self::retire_queued(&mut state);
        state.scope = next.clone();
        state.admission = Admission::Open;
        Some(next)
    }

    /// Transfer only a proven-not-enqueued record without releasing its lease.
    /// Accepted or uncertain offers stay owned here: neither proves terminal
    /// settlement. A later integration needs an independently verified terminal
    /// settlement or a typed retained owner before it can release those records.
    pub(super) fn take_not_enqueued(&self, id: &EnvelopeId) -> Option<(Arc<Envelope<T>>, Stage)> {
        let mut state = lock(&self.state);
        let stage = state.records.get(id)?.stage;
        if !matches!(stage, Stage::NotAttempted | Stage::NotEnqueued) {
            return None;
        }
        let record = state.records.remove(id)?;
        Some((record.envelope, stage))
    }
}

/// An attempt keeps both its owner and its charged envelope alive. The owner
/// must also be retained independently when an attempt may be cancelled.
pub(super) struct Offer<T> {
    owner: Arc<Custody<T>>,
    scope: Scope,
    id: EnvelopeId,
    envelope: Arc<Envelope<T>>,
}

impl<T> Offer<T> {
    pub(super) fn snapshot(&self) -> Arc<Envelope<T>> {
        Arc::clone(&self.envelope)
    }

    pub(super) fn resolve(self, reply: OfferReply) {
        let mut state = lock(&self.owner.state);
        let stale = state.scope != self.scope;
        if let Some(record) = state.records.get_mut(&self.id) {
            if record.scope == self.scope && record.stage == Stage::InFlight {
                record.stage = match reply {
                    OfferReply::Accepted if !stale => Stage::OfferAccepted,
                    OfferReply::NotEnqueued => Stage::NotEnqueued,
                    OfferReply::Accepted | OfferReply::Unknown => Stage::Uncertain,
                };
            }
        }
    }
}

impl<T> Drop for Offer<T> {
    fn drop(&mut self) {
        let mut state = lock(&self.owner.state);
        if let Some(record) = state.records.get_mut(&self.id) {
            if record.scope == self.scope && record.stage == Stage::InFlight {
                record.stage = Stage::Uncertain;
            }
        }
    }
}

#[cfg(test)]
mod tests;
