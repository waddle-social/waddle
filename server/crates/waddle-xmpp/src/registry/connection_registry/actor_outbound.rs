//! Process-local custody for a dormant actor-owned outbound endpoint.
//!
//! Queue and actor messages carry IDs. The full envelope remains here until an
//! explicit terminal disposition; a local socket write is not durable custody.

use super::{LocalConnectionBindingId, OutboundStanza};
use crate::ownership::{BindingRef, ConnectionRef};
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActorOutboundId {
    connection: ConnectionRef,
    binding: BindingRef,
    owner: LocalConnectionBindingId,
    nonce: u64,
}

impl ActorOutboundId {
    pub fn connection(&self) -> &ConnectionRef {
        &self.connection
    }

    pub fn binding(&self) -> &BindingRef {
        &self.binding
    }

    pub fn owner(&self) -> LocalConnectionBindingId {
        self.owner
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorOutboundStage {
    Queued,
    Attempting,
    NotAttempted,
    AttemptedUncertain,
    Flushed,
}

struct ActorOutboundRecord {
    outbound: Mutex<Option<Arc<ActorOutboundLease>>>,
    stage: Mutex<ActorOutboundStage>,
}

#[derive(Default)]
struct OutboundLeaseBudgetState {
    count: usize,
    payload_bytes: usize,
}

#[derive(Default)]
struct OutboundLeaseBudget {
    state: Mutex<OutboundLeaseBudgetState>,
}

/// An opaque shared handle for an accepted outbound envelope. The original
/// serialized-byte charge remains live until the last handle is dropped.
/// Consumers may inspect the envelope by reference but cannot move its fields
/// out from under the charge.
pub struct ActorOutboundLease {
    outbound: OutboundStanza,
    _charge: OutboundLeaseCharge,
}

impl ActorOutboundLease {
    pub fn outbound(&self) -> &OutboundStanza {
        &self.outbound
    }
}

struct OutboundLeaseCharge {
    budget: Arc<OutboundLeaseBudget>,
    payload_bytes: usize,
}

impl Drop for OutboundLeaseCharge {
    fn drop(&mut self) {
        let mut state = self
            .budget
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.count -= 1;
        state.payload_bytes -= self.payload_bytes;
    }
}

struct AttemptGuard(Arc<ActorOutboundRecord>);

impl Drop for AttemptGuard {
    fn drop(&mut self) {
        let mut stage = self
            .0
            .stage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *stage == ActorOutboundStage::Attempting {
            *stage = ActorOutboundStage::AttemptedUncertain;
        }
    }
}

struct CustodyState {
    next_nonce: u64,
    records: HashMap<ActorOutboundId, Arc<ActorOutboundRecord>>,
    sealed: bool,
}

/// Bound for the original envelopes, including work dequeued by the actor or
/// writer and work retained after local termination. A separate writer budget
/// bounds a simultaneously serialized copy of each frame.
pub struct ActorOutboundCustody {
    connection: ConnectionRef,
    max_count: usize,
    max_payload_bytes: usize,
    state: Mutex<CustodyState>,
    lease_budget: Arc<OutboundLeaseBudget>,
}

struct CappedByteCount {
    count: usize,
    limit: usize,
}

impl Write for CappedByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .count
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("actor stanza exceeds byte bound"))?;
        if next > self.limit {
            return Err(io::Error::other("actor stanza exceeds byte bound"));
        }
        self.count = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl std::fmt::Debug for ActorOutboundCustody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActorOutboundCustody")
            .field("connection", &self.connection)
            .finish_non_exhaustive()
    }
}

impl ActorOutboundCustody {
    pub fn new(
        connection: ConnectionRef,
        max_count: usize,
        max_payload_bytes: usize,
    ) -> Option<Arc<Self>> {
        if max_count == 0 || max_payload_bytes == 0 {
            return None;
        }
        Some(Arc::new(Self {
            connection,
            max_count,
            max_payload_bytes,
            state: Mutex::new(CustodyState {
                next_nonce: 0,
                records: HashMap::new(),
                sealed: false,
            }),
            lease_budget: Arc::new(OutboundLeaseBudget::default()),
        }))
    }

    pub fn connection(&self) -> &ConnectionRef {
        &self.connection
    }

    fn lock_state(&self) -> MutexGuard<'_, CustodyState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Called only after the queue slot was reserved and while endpoint
    /// admission is held. Capacity and overflow fail before moving the frame.
    #[expect(
        clippy::result_large_err,
        reason = "pre-acceptance refusal returns the exact owned outbound envelope"
    )]
    pub(super) fn insert(
        &self,
        owner: LocalConnectionBindingId,
        binding: &BindingRef,
        outbound: OutboundStanza,
    ) -> Result<ActorOutboundId, OutboundStanza> {
        if binding.connection_ref() != &self.connection {
            return Err(outbound);
        }
        let mut serialized = CappedByteCount {
            count: 0,
            limit: self.max_payload_bytes,
        };
        if outbound
            .stanza
            .to_element()
            .write_to(&mut serialized)
            .is_err()
        {
            return Err(outbound);
        }
        let Some(payload_bytes) = envelope_budget_bytes(&outbound, serialized.count) else {
            return Err(outbound);
        };
        let mut state = self.lock_state();
        let mut budget = self
            .lease_budget
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(next_bytes) = budget.payload_bytes.checked_add(payload_bytes) else {
            return Err(outbound);
        };
        let Some(next_lease_count) = budget.count.checked_add(1) else {
            return Err(outbound);
        };
        let Some(next_nonce) = state.next_nonce.checked_add(1) else {
            return Err(outbound);
        };
        if state.sealed
            || state.records.len() >= self.max_count
            || next_bytes > self.max_payload_bytes
            || next_lease_count > self.max_count
        {
            return Err(outbound);
        }
        let id = ActorOutboundId {
            connection: self.connection.clone(),
            binding: binding.clone(),
            owner,
            nonce: next_nonce,
        };
        state.next_nonce = next_nonce;
        budget.count = next_lease_count;
        budget.payload_bytes = next_bytes;
        // The checked nonce is unique within this custody, so this insert
        // cannot replace an earlier record and drop its charge under the
        // budget lock.
        state.records.insert(
            id.clone(),
            Arc::new(ActorOutboundRecord {
                outbound: Mutex::new(Some(Arc::new(ActorOutboundLease {
                    outbound,
                    _charge: OutboundLeaseCharge {
                        budget: Arc::clone(&self.lease_budget),
                        payload_bytes,
                    },
                }))),
                stage: Mutex::new(ActorOutboundStage::Queued),
            }),
        );
        Ok(id)
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[expect(
        clippy::result_large_err,
        reason = "test admission preserves the exact owned refusal envelope"
    )]
    pub fn insert_for_test(
        &self,
        owner: LocalConnectionBindingId,
        binding: &BindingRef,
        outbound: OutboundStanza,
    ) -> Result<ActorOutboundId, OutboundStanza> {
        self.insert(owner, binding, outbound)
    }

    /// The actor may derive a typed output without moving callback or durable
    /// metadata out of the custody record.
    pub fn snapshot(&self, id: &ActorOutboundId) -> Option<Arc<ActorOutboundLease>> {
        let record = self.lock_state().records.get(id)?.clone();
        let outbound = record
            .outbound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        outbound
    }

    /// Decide Queued→Attempting atomically with seal, then call the sink with
    /// no custody lock. A winner is uncertain even if seal occurs before its
    /// OS call. The drop guard also classifies an unwind as uncertain.
    pub fn attempt_with<T>(
        &self,
        id: &ActorOutboundId,
        start_send: impl FnOnce() -> T,
    ) -> Option<T> {
        let record = {
            let state = self.lock_state();
            if state.sealed {
                return None;
            }
            let record = state.records.get(id)?.clone();
            let mut stage = record
                .stage
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *stage != ActorOutboundStage::Queued {
                return None;
            }
            *stage = ActorOutboundStage::Attempting;
            drop(stage);
            record
        };
        let _attempt = AttemptGuard(record);
        let result = start_send();
        Some(result)
    }

    pub fn mark_flushed(&self, id: &ActorOutboundId) {
        let record = self.lock_state().records.get(id).cloned();
        if let Some(record) = record {
            let mut stage = record
                .stage
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *stage == ActorOutboundStage::AttemptedUncertain {
                *stage = ActorOutboundStage::Flushed;
            }
        }
    }

    /// Seal and classify queued items atomically with attempt reservation.
    /// An in-progress attempt stays Attempting until its guard drops or the
    /// physical pump has been joined; terminal retrieval refuses it meanwhile.
    pub fn seal(&self) {
        let mut state = self.lock_state();
        state.sealed = true;
        for record in state.records.values() {
            let mut stage = record
                .stage
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *stage = match *stage {
                ActorOutboundStage::Queued => ActorOutboundStage::NotAttempted,
                other => other,
            };
        }
    }

    pub fn stage(&self, id: &ActorOutboundId) -> Option<ActorOutboundStage> {
        let record = self.lock_state().records.get(id)?.clone();
        let stage = *record
            .stage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(stage)
    }

    /// Explicit local compensation/settlement. Only the exact ID can remove
    /// a record; callers must choose retry vs retention by its typed stage.
    pub fn take_terminal(
        &self,
        id: &ActorOutboundId,
    ) -> Option<(Arc<ActorOutboundLease>, ActorOutboundStage)> {
        let mut state = self.lock_state();
        let record = state.records.get(id)?.clone();
        let stage = *record
            .stage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if stage == ActorOutboundStage::Attempting
            || (!state.sealed && stage != ActorOutboundStage::Flushed)
        {
            return None;
        }
        state.records.remove(id)?;
        let outbound = record
            .outbound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()?;
        Some((outbound, stage))
    }

    pub fn retained_count(&self) -> usize {
        self.lock_state().records.len()
    }

    /// Whether this sealed generation has no records or outstanding leased
    /// envelopes and can be replaced by a new binding channel.
    pub fn ready_for_replacement(&self) -> bool {
        let state = self.lock_state();
        if !state.sealed || !state.records.is_empty() {
            return false;
        }
        let budget = self
            .lease_budget
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        budget.count == 0 && budget.payload_bytes == 0
    }

    /// Number of original envelopes whose serialized-byte charge is still
    /// live, including terminal envelopes held by external snapshot handles.
    pub fn outstanding_lease_count(&self) -> usize {
        self.lease_budget
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .count
    }

    /// The independent cleanup owner can recover even IDs lost from a
    /// cancelled actor stack or dropped mailbox after sealing this table.
    pub fn terminal_ids(&self) -> Option<Vec<ActorOutboundId>> {
        let state = self.lock_state();
        state
            .sealed
            .then(|| state.records.keys().cloned().collect())
    }

    pub fn is_sealed(&self) -> bool {
        self.lock_state().sealed
    }
}

/// Charge the serialized stanza and every variable-size metadata field that
/// travels with the original envelope. The writer has an independent encoded
/// frame budget while the original remains retained here.
fn envelope_budget_bytes(outbound: &OutboundStanza, stanza_bytes: usize) -> Option<usize> {
    let mut bytes = std::mem::size_of::<OutboundStanza>().checked_add(stanza_bytes)?;
    if let Some(row) = &outbound.pending_row_id {
        bytes = bytes.checked_add(row.as_str().len())?;
    }
    if let Some(obligation) = &outbound.ingress_append {
        bytes = bytes.checked_add(obligation.archive_positions.capacity().checked_mul(
            std::mem::size_of::<crate::stream_management::ArchiveDispatchPosition>(),
        )?)?;
        bytes = bytes.checked_add(obligation.key.resource.to_string().len())?;
        bytes = bytes.checked_add(obligation.sender_bare.to_string().len())?;
        for position in &obligation.archive_positions {
            bytes = bytes.checked_add(position.archive.to_string().len())?;
        }
        if let Some(stream) = &obligation.dispatch_stream {
            bytes = bytes.checked_add(stream.as_str().len())?;
        }
    }
    Some(bytes)
}

pub struct ActorOutboundReceiver {
    receiver: mpsc::Receiver<ActorOutboundId>,
    custody: Arc<ActorOutboundCustody>,
}

impl ActorOutboundReceiver {
    pub(super) fn new(
        receiver: mpsc::Receiver<ActorOutboundId>,
        custody: Arc<ActorOutboundCustody>,
    ) -> Self {
        Self { receiver, custody }
    }

    pub async fn recv(&mut self) -> Option<ActorOutboundId> {
        self.receiver.recv().await
    }

    pub fn try_recv(&mut self) -> Result<ActorOutboundId, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn custody(&self) -> &Arc<ActorOutboundCustody> {
        &self.custody
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pending_delivery::PendingRowId;
    use crate::Stanza;
    use xmpp_parsers::message::{Lang, Message};

    fn prepared() -> (Arc<ActorOutboundCustody>, ActorOutboundId) {
        let connection = ConnectionRef::fresh(crate::ownership::NodeLeaseIdentity::local_process());
        let binding = BindingRef::fresh(connection.clone());
        let custody = ActorOutboundCustody::new(connection, 2, 4096).expect("valid bound");
        let id = custody
            .insert(
                LocalConnectionBindingId::fresh(),
                &binding,
                OutboundStanza::new(Stanza::Message(Message::new(None))),
            )
            .expect("first frame fits");
        (custody, id)
    }

    #[test]
    fn seal_during_attempt_never_labels_it_not_attempted() {
        let (custody, id) = prepared();
        let snapshot = custody.snapshot(&id).expect("external snapshot");
        let (entered, seen) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let writer = std::thread::spawn({
            let custody = custody.clone();
            let id = id.clone();
            move || {
                custody.attempt_with(&id, || {
                    entered.send(()).expect("signal attempt");
                    resume.recv().expect("release attempt");
                })
            }
        });
        seen.recv().expect("attempt entered");
        custody.seal();
        assert_eq!(custody.stage(&id), Some(ActorOutboundStage::Attempting));
        assert!(custody.take_terminal(&id).is_none());
        let drop_snapshot = std::thread::spawn(move || drop(snapshot));
        release.send(()).expect("release writer");
        assert_eq!(writer.join().expect("writer joined"), Some(()));
        drop_snapshot.join().expect("snapshot drop joined");
        assert_eq!(
            custody.stage(&id),
            Some(ActorOutboundStage::AttemptedUncertain)
        );
        let (terminal, stage) = custody.take_terminal(&id).expect("terminal custody");
        assert_eq!(stage, ActorOutboundStage::AttemptedUncertain);
        assert_eq!(custody.outstanding_lease_count(), 1);
        drop(terminal);
        assert_eq!(custody.outstanding_lease_count(), 0);
    }

    #[test]
    fn seal_before_attempt_retains_proven_not_attempted_envelope() {
        let (custody, id) = prepared();
        custody.seal();
        assert!(custody
            .attempt_with(&id, || panic!("sealed attempt ran"))
            .is_none());
        let (outbound, stage) = custody.take_terminal(&id).expect("terminal custody");
        assert_eq!(stage, ActorOutboundStage::NotAttempted);
        assert!(outbound.outbound().pending_row_id.is_none());
    }

    #[test]
    fn attempt_panic_keeps_uncertain_terminal_custody() {
        let (custody, id) = prepared();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            custody.attempt_with(&id, || panic!("synthetic sink panic"));
        }));
        assert!(result.is_err());
        custody.seal();
        assert_eq!(
            custody.stage(&id),
            Some(ActorOutboundStage::AttemptedUncertain)
        );
        assert!(custody.take_terminal(&id).is_some());
    }

    #[test]
    fn byte_budget_charges_metadata_and_rejects_oversized_xml_with_exact_envelope() {
        let connection = ConnectionRef::fresh(crate::ownership::NodeLeaseIdentity::local_process());
        let binding = BindingRef::fresh(connection.clone());
        let baseline = OutboundStanza::new(Stanza::Message(Message::new(None)));
        let mut sized = CappedByteCount {
            count: 0,
            limit: usize::MAX,
        };
        baseline
            .stanza
            .to_element()
            .write_to(&mut sized)
            .expect("baseline XML size");
        let baseline_bytes = envelope_budget_bytes(&baseline, sized.count).expect("baseline cost");
        let custody = ActorOutboundCustody::new(connection.clone(), 2, baseline_bytes * 2 + 8)
            .expect("two bounded slots");
        custody
            .insert(LocalConnectionBindingId::fresh(), &binding, baseline)
            .expect("first envelope fits");
        let (acceptance, mut accepted) = crate::registry::OutboundWriteAcceptance::new();
        let mut with_metadata =
            OutboundStanza::with_write_acceptance(Stanza::Message(Message::new(None)), acceptance);
        let row_id = "r".repeat(64);
        with_metadata.pending_row_id = Some(PendingRowId::new(row_id.clone()));
        let refused = custody
            .insert(LocalConnectionBindingId::fresh(), &binding, with_metadata)
            .expect_err("metadata must exceed remaining byte budget");
        assert_eq!(
            refused.pending_row_id.as_ref().map(PendingRowId::as_str),
            Some(row_id.as_str())
        );
        assert!(refused.write_acceptance.is_some());
        assert!(matches!(
            accepted.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(custody.retained_count(), 1);

        let oversized =
            Stanza::Message(Message::new(None).with_body(Lang(String::new()), "x".repeat(4096)));
        let expected = oversized.to_element();
        let small = ActorOutboundCustody::new(connection, 2, 512).expect("small byte bound");
        let refused = small
            .insert(
                LocalConnectionBindingId::fresh(),
                &binding,
                OutboundStanza::new(oversized),
            )
            .expect_err("XML exceeds the capped serialization bound");
        assert_eq!(refused.stanza.to_element(), expected);
        assert_eq!(small.retained_count(), 0);
    }

    fn flushed_one(custody: &ActorOutboundCustody, id: &ActorOutboundId) {
        assert_eq!(custody.attempt_with(id, || ()), Some(()));
        custody.mark_flushed(id);
    }

    fn one_message_cost() -> (OutboundStanza, usize) {
        let outbound = OutboundStanza::new(Stanza::Message(Message::new(None)));
        let mut serialized = CappedByteCount {
            count: 0,
            limit: usize::MAX,
        };
        outbound
            .stanza
            .to_element()
            .write_to(&mut serialized)
            .expect("message XML size");
        let charge = envelope_budget_bytes(&outbound, serialized.count).expect("message cost");
        (outbound, charge)
    }

    #[test]
    fn terminal_charge_blocks_replacement_until_last_snapshot_is_dropped() {
        let connection = ConnectionRef::fresh(crate::ownership::NodeLeaseIdentity::local_process());
        let binding = BindingRef::fresh(connection.clone());
        let custody = ActorOutboundCustody::new(connection, 1, 4096).expect("valid bound");
        let (outbound, charge) = one_message_cost();
        let id = custody
            .insert(LocalConnectionBindingId::fresh(), &binding, outbound)
            .expect("one frame fits");
        let snapshot = custody.snapshot(&id).expect("shared snapshot");
        flushed_one(&custody, &id);
        custody.seal();
        let (terminal, stage) = custody.take_terminal(&id).expect("terminal transfer");

        assert_eq!(stage, ActorOutboundStage::Flushed);
        assert_eq!(custody.retained_count(), 0);
        assert_eq!(custody.outstanding_lease_count(), 1);
        assert_eq!(
            custody
                .lease_budget
                .state
                .lock()
                .expect("budget")
                .payload_bytes,
            charge
        );
        assert!(!custody.ready_for_replacement());

        drop(terminal);
        assert_eq!(custody.outstanding_lease_count(), 1);
        assert!(!custody.ready_for_replacement());

        drop(snapshot);
        assert_eq!(custody.outstanding_lease_count(), 0);
        assert_eq!(
            custody
                .lease_budget
                .state
                .lock()
                .expect("budget")
                .payload_bytes,
            0
        );
        assert!(custody.ready_for_replacement());
    }

    #[test]
    fn outstanding_count_limit_is_held_by_terminal_snapshot() {
        let connection = ConnectionRef::fresh(crate::ownership::NodeLeaseIdentity::local_process());
        let binding = BindingRef::fresh(connection.clone());
        let custody = ActorOutboundCustody::new(connection, 1, 4096).expect("one slot");
        let (first, _) = one_message_cost();
        let (second, _) = one_message_cost();
        let expected = second.stanza.to_element();
        let id = custody
            .insert(LocalConnectionBindingId::fresh(), &binding, first)
            .expect("first fits");
        let snapshot = custody.snapshot(&id).expect("snapshot");
        flushed_one(&custody, &id);
        let (terminal, _) = custody.take_terminal(&id).expect("terminal transfer");

        let refused = custody
            .insert(LocalConnectionBindingId::fresh(), &binding, second)
            .expect_err("outstanding lease still occupies the count budget");
        assert_eq!(refused.stanza.to_element(), expected);
        assert_eq!(custody.retained_count(), 0);
        assert_eq!(custody.outstanding_lease_count(), 1);

        drop(terminal);
        assert_eq!(custody.outstanding_lease_count(), 1);
        drop(snapshot);
        let (next, _) = one_message_cost();
        assert!(custody
            .insert(LocalConnectionBindingId::fresh(), &binding, next)
            .is_ok());
    }

    #[test]
    fn outstanding_byte_limit_is_held_by_terminal_snapshot() {
        let connection = ConnectionRef::fresh(crate::ownership::NodeLeaseIdentity::local_process());
        let binding = BindingRef::fresh(connection.clone());
        let (first, charge) = one_message_cost();
        let (second, _) = one_message_cost();
        let expected = second.stanza.to_element();
        let custody = ActorOutboundCustody::new(connection, 2, charge).expect("byte slot");
        let id = custody
            .insert(LocalConnectionBindingId::fresh(), &binding, first)
            .expect("first fits exactly");
        let snapshot = custody.snapshot(&id).expect("snapshot");
        flushed_one(&custody, &id);
        let (terminal, _) = custody.take_terminal(&id).expect("terminal transfer");

        let refused = custody
            .insert(LocalConnectionBindingId::fresh(), &binding, second)
            .expect_err("outstanding lease still occupies the byte budget");
        assert_eq!(refused.stanza.to_element(), expected);
        assert_eq!(custody.retained_count(), 0);
        assert_eq!(custody.outstanding_lease_count(), 1);
        assert_eq!(
            custody
                .lease_budget
                .state
                .lock()
                .expect("budget")
                .payload_bytes,
            charge
        );

        drop(terminal);
        assert_eq!(custody.outstanding_lease_count(), 1);
        drop(snapshot);
        let (next, _) = one_message_cost();
        assert!(custody
            .insert(LocalConnectionBindingId::fresh(), &binding, next)
            .is_ok());
    }

    #[test]
    fn lease_budget_outlives_custody_until_the_last_handle_drops() {
        let connection = ConnectionRef::fresh(crate::ownership::NodeLeaseIdentity::local_process());
        let binding = BindingRef::fresh(connection.clone());
        let custody = ActorOutboundCustody::new(connection, 1, 4096).expect("valid bound");
        let budget = Arc::clone(&custody.lease_budget);
        let (outbound, charge) = one_message_cost();
        let id = custody
            .insert(LocalConnectionBindingId::fresh(), &binding, outbound)
            .expect("frame fits");
        let snapshot = custody.snapshot(&id).expect("snapshot");
        flushed_one(&custody, &id);
        custody.seal();
        let (terminal, _) = custody.take_terminal(&id).expect("terminal transfer");

        drop(custody);
        assert_eq!(budget.state.lock().expect("budget").count, 1);
        assert_eq!(budget.state.lock().expect("budget").payload_bytes, charge);
        drop(terminal);
        assert_eq!(budget.state.lock().expect("budget").count, 1);
        drop(snapshot);

        let state = budget.state.lock().expect("budget");
        assert_eq!(state.count, 0);
        assert_eq!(state.payload_bytes, 0);
    }
}
