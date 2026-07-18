use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::debug;

use crate::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};

use super::persistence_codec::{
    detached_to_persisted, detached_to_persisted_snapshot, detached_to_terminal_generation,
    parse_xml_to_persisted_unacked, persisted_terminal_to_detached, persisted_to_detached,
};
use super::{DetachedSession, SmRegistryError, DEFAULT_MAX_SESSIONS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum PendingClaimAcquisitionDisposition {
    ReleaseRejectedEnable,
    RetainDetachedSession(super::SmSessionGenerationId),
}

/// Whether one exact locally-retained claim may be released yet.
///
/// A durable-recovery handoff keeps the backend claim discoverable until the
/// durable inventory is proven empty. Once that proof succeeds the state may
/// advance to `ReleaseMayComplete`; it must never move back, because an exact
/// release may already have committed even when its result was not observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingClaimReleaseDisposition {
    RetainedForDurableRecovery,
    ReleaseMayComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DetachClaimFenceReservation {
    Owned,
    BorrowedRejectedEnable,
}

impl DetachClaimFenceReservation {
    pub(super) fn cancel_if_owned(self, registry: &InMemorySmSessionRegistry, stream_id: &str) {
        if self == Self::Owned {
            registry.cancel_claim_fence_reservation(stream_id);
        }
    }

    fn preserves_rejected_enable(self) -> bool {
        self == Self::BorrowedRejectedEnable
    }
}

const STREAM_LOCK_SHARDS: usize = 256;

/// Generation-scoped promotion authorities for one opaque SM stream id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingPromotionDurability {
    DurableRow,
    TerminalDurable,
    /// The atomic publication may or may not have committed, and the exact
    /// marker read also failed.  This generation remains durable recovery
    /// inventory but is deliberately not leasable until a later
    /// reconciliation or restart establishes which row shape won.
    PublicationUnknown,
    DefinitelyNeverPublished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalPromotionRetention {
    Inserted,
    Upgraded,
    Unchanged,
}

#[derive(Debug, Clone)]
struct PendingPromotionGeneration {
    current: bool,
    failures: u32,
    claim_fence: Option<super::super::persistence::SmClaimFence>,
    durability: PendingPromotionDurability,
}

#[derive(Debug, Default)]
pub(super) struct PendingPromotions {
    by_stream: HashMap<String, HashMap<super::SmSessionGenerationId, PendingPromotionGeneration>>,
    active_leases: HashMap<(String, super::SmSessionGenerationId), super::SmPromotionLeaseNonce>,
    active_current: HashMap<String, (super::SmSessionGenerationId, super::SmPromotionLeaseNonce)>,
}

impl PendingPromotions {
    pub(super) fn insert_current(&mut self, session: &DetachedSession) -> bool {
        if self.current_reservation_active(&session.stream_id) {
            return false;
        }
        let generations = self.by_stream.entry(session.stream_id.clone()).or_default();
        for state in generations.values_mut() {
            state.current = false;
        }
        generations.insert(
            session.generation_id,
            PendingPromotionGeneration {
                current: true,
                failures: 0,
                claim_fence: None,
                durability: PendingPromotionDurability::DurableRow,
            },
        );
        true
    }

    /// Retain a detach payload that could not be published as resumable.
    /// It may own terminal cleanup only when no older generation still owns
    /// the stream; otherwise it is a payload-only obsolete carrier.
    pub(super) fn insert_terminal_carrier(
        &mut self,
        session: &DetachedSession,
        force_obsolete: bool,
    ) -> bool {
        let generations = self.by_stream.entry(session.stream_id.clone()).or_default();
        if generations.contains_key(&session.generation_id) {
            return false;
        }
        let current = !force_obsolete
            && !self.active_current.contains_key(&session.stream_id)
            && !generations.values().any(|state| state.current);
        generations.insert(
            session.generation_id,
            PendingPromotionGeneration {
                current,
                failures: 0,
                claim_fence: None,
                durability: PendingPromotionDurability::DefinitelyNeverPublished,
            },
        );
        true
    }

    /// Retain the full payload of a snapshot that may exist durably but for
    /// which this node definitively failed to acquire ownership. Unlike a
    /// `DefinitelyNeverPublished` carrier, this generation must not claim
    /// authority to delete the durable row: a foreign owner may already be
    /// using it. It therefore enters Q6 as payload-only from the outset.
    pub(super) fn insert_unowned_durable_carrier(&mut self, session: &DetachedSession) -> bool {
        let generations = self.by_stream.entry(session.stream_id.clone()).or_default();
        if generations.contains_key(&session.generation_id) {
            return false;
        }
        generations.insert(
            session.generation_id,
            PendingPromotionGeneration {
                current: false,
                failures: 0,
                claim_fence: None,
                durability: PendingPromotionDurability::DurableRow,
            },
        );
        true
    }

    /// Mark one exact archived generation as durably terminal.
    ///
    /// The live same-id replacement path already has the predecessor in the
    /// inventory, while startup/reclaimed hydration does not.  Distinguish
    /// those cases so only hydration injects a retry payload; the live caller
    /// continues to own and promote the carrier it already received.
    pub(super) fn retain_terminal_durable(
        &mut self,
        session: &DetachedSession,
        promotion_attempts: u32,
        claim_fence: super::super::persistence::SmClaimFence,
    ) -> TerminalPromotionRetention {
        let generations = self.by_stream.entry(session.stream_id.clone()).or_default();
        if let Some(state) = generations.get_mut(&session.generation_id) {
            if state.current
                || self
                    .active_leases
                    .contains_key(&(session.stream_id.clone(), session.generation_id))
                || state.durability != PendingPromotionDurability::DurableRow
            {
                return TerminalPromotionRetention::Unchanged;
            }
            state.failures = state.failures.max(promotion_attempts);
            state.claim_fence = Some(claim_fence);
            state.durability = PendingPromotionDurability::TerminalDurable;
            return TerminalPromotionRetention::Upgraded;
        }
        generations.insert(
            session.generation_id,
            PendingPromotionGeneration {
                current: false,
                failures: promotion_attempts,
                claim_fence: Some(claim_fence),
                durability: PendingPromotionDurability::TerminalDurable,
            },
        );
        TerminalPromotionRetention::Inserted
    }

    /// Park an exact payload whose atomic publication result cannot be
    /// classified.  Publication-unknown generations are non-leasable: Q6
    /// must not guess whether to mutate the bare row or a terminal archive.
    pub(super) fn park_publication_unknown(
        &mut self,
        session: &DetachedSession,
        claim_fence: Option<super::super::persistence::SmClaimFence>,
    ) {
        let generations = self.by_stream.entry(session.stream_id.clone()).or_default();
        match generations.get_mut(&session.generation_id) {
            Some(state) => {
                state.current = false;
                if state.claim_fence.is_none() {
                    state.claim_fence = claim_fence;
                }
                state.durability = PendingPromotionDurability::PublicationUnknown;
            }
            None => {
                generations.insert(
                    session.generation_id,
                    PendingPromotionGeneration {
                        current: false,
                        failures: 0,
                        claim_fence,
                        durability: PendingPromotionDurability::PublicationUnknown,
                    },
                );
            }
        }
        if self
            .active_current
            .get(&session.stream_id)
            .is_some_and(|(generation_id, _)| *generation_id == session.generation_id)
        {
            self.active_current.remove(&session.stream_id);
        }
    }

    pub(super) fn demote_for_successor(&mut self, stream_id: &str) -> bool {
        if self.current_reservation_active(stream_id) {
            return false;
        }
        if let Some(generations) = self.by_stream.get_mut(stream_id) {
            for state in generations.values_mut() {
                state.current = false;
            }
        }
        true
    }

    /// External self-fencing invalidates durable mutation authority even
    /// when a Q6 lease is already active. Bare-row generations remain as
    /// payload-only retries, but terminal generations are retired: they have
    /// already been durably archived and must never fall back to an unfenced
    /// ordinary pending insertion. `exact_fence` limits terminal retirement
    /// to one lifecycle; `None` denotes broad whole-stream demotion.
    pub(super) fn demote_for_external_claim_loss(
        &mut self,
        stream_id: &str,
        exact_fence: Option<&super::super::persistence::SmClaimFence>,
    ) -> Vec<super::SmSessionGenerationId> {
        let mut terminal_generations = Vec::new();
        if let Some(generations) = self.by_stream.get_mut(stream_id) {
            for (generation_id, state) in generations.iter_mut() {
                if state.durability == PendingPromotionDurability::TerminalDurable {
                    if exact_fence.is_none_or(|fence| state.claim_fence.as_ref() == Some(fence)) {
                        terminal_generations.push(*generation_id);
                    }
                    continue;
                }
                state.current = false;
            }
        }
        self.active_current.remove(stream_id);
        terminal_generations
            .retain(|generation_id| self.purge_terminal_generation(stream_id, *generation_id));
        terminal_generations
    }

    /// Retire terminal carriers whose retained typed fence belongs to one
    /// node incarnation without touching a same-stream lifecycle owned by a
    /// different incarnation. This selector remains valid after the active
    /// fence has already moved into exact pending-release inventory.
    pub(super) fn purge_terminal_generations_owned_by(
        &mut self,
        stream_id: &str,
        owner: &NodeIdentity,
    ) -> Vec<super::SmSessionGenerationId> {
        let mut terminal_generations = self
            .by_stream
            .get(stream_id)
            .into_iter()
            .flat_map(|generations| generations.iter())
            .filter_map(|(generation_id, state)| {
                (state.durability == PendingPromotionDurability::TerminalDurable
                    && state
                        .claim_fence
                        .as_ref()
                        .is_some_and(|fence| fence.owner() == owner))
                .then_some(*generation_id)
            })
            .collect::<Vec<_>>();
        terminal_generations
            .retain(|generation_id| self.purge_terminal_generation(stream_id, *generation_id));
        terminal_generations
    }

    /// Revoke bare-row authority for one exact ambiguous detach generation
    /// without disturbing a same-stream successor that may have published
    /// while the older acquisition awaited reconciliation.
    pub(super) fn demote_generation_for_external_claim_loss(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> Option<super::SmSessionGenerationId> {
        if self.purge_terminal_generation(stream_id, generation_id) {
            return Some(generation_id);
        }
        if let Some(state) = self
            .by_stream
            .get_mut(stream_id)
            .and_then(|generations| generations.get_mut(&generation_id))
        {
            state.current = false;
        }
        if self
            .active_current
            .get(stream_id)
            .is_some_and(|(active_generation, _)| *active_generation == generation_id)
        {
            self.active_current.remove(stream_id);
        }
        None
    }

    /// Revoke only promotion authority derived from one stale exact claim.
    ///
    /// Nonterminal payload generations remain available for Q6 payload-only
    /// retry, but a durably archived terminal generation is retired instead
    /// of being converted into an unfenced ordinary retry. Generations
    /// carrying a different fence belong to a newer lifecycle and are left
    /// untouched.
    pub(super) fn relinquish_exact_claim_fence(
        &mut self,
        stream_id: &str,
        fence: &super::super::persistence::SmClaimFence,
    ) -> Vec<super::SmSessionGenerationId> {
        let matching_generations = self
            .by_stream
            .get(stream_id)
            .into_iter()
            .flat_map(|generations| generations.iter())
            .filter_map(|(generation_id, state)| {
                (state.claim_fence.as_ref() == Some(fence)).then_some(*generation_id)
            })
            .collect::<Vec<_>>();

        let mut purged_terminal_generations = Vec::new();
        for generation_id in matching_generations {
            if self.purge_terminal_generation(stream_id, generation_id) {
                purged_terminal_generations.push(generation_id);
                continue;
            }
            if let Some(state) = self
                .by_stream
                .get_mut(stream_id)
                .and_then(|generations| generations.get_mut(&generation_id))
            {
                state.claim_fence = None;
                state.current = false;
            }
            self.active_leases
                .remove(&(stream_id.to_string(), generation_id));
            if self
                .active_current
                .get(stream_id)
                .is_some_and(|(active_generation, _)| *active_generation == generation_id)
            {
                self.active_current.remove(stream_id);
            }
        }
        purged_terminal_generations
    }

    /// Remove one exact durably archived generation and revoke every local
    /// lease that could still carry its terminal mutation authority.
    fn purge_terminal_generation(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> bool {
        let Some(generations) = self.by_stream.get_mut(stream_id) else {
            return false;
        };
        if !generations
            .get(&generation_id)
            .is_some_and(|state| state.durability == PendingPromotionDurability::TerminalDurable)
        {
            return false;
        }
        generations.remove(&generation_id);
        if generations.is_empty() {
            self.by_stream.remove(stream_id);
        }
        self.active_leases
            .remove(&(stream_id.to_string(), generation_id));
        if self
            .active_current
            .get(stream_id)
            .is_some_and(|(active_generation, _)| *active_generation == generation_id)
        {
            self.active_current.remove(stream_id);
        }
        true
    }

    /// Replace every promotion authority derived from an older same-stream
    /// fence after a newer fence is verified as the backend's current shared
    /// stream claim. Current-row and terminal-row generations rebind to the
    /// new shared authority; payload-only generations relinquish exact
    /// authority. Any affected lease is revoked because its value captured
    /// the superseded fence and must be reacquired before another mutation.
    pub(super) fn publish_verified_claim_fence(
        &mut self,
        stream_id: &str,
        fence: &super::super::persistence::SmClaimFence,
    ) -> Vec<super::super::persistence::SmClaimFence> {
        let mut superseded = Vec::new();
        let mut affected_generations = Vec::new();
        let Some(generations) = self.by_stream.get_mut(stream_id) else {
            return superseded;
        };

        for (generation_id, state) in generations.iter_mut() {
            let rebind = state.durability == PendingPromotionDurability::TerminalDurable
                || (state.durability == PendingPromotionDurability::DurableRow && state.current);
            let next_fence = rebind.then(|| fence.clone());
            let demote = !rebind && state.current;
            if state.claim_fence == next_fence && !demote {
                continue;
            }
            if let Some(old) = state
                .claim_fence
                .as_ref()
                .filter(|old| *old != fence)
                .cloned()
            {
                superseded.push(old);
            }
            state.claim_fence = next_fence;
            if !rebind {
                state.current = false;
            }
            affected_generations.push(*generation_id);
        }

        for generation_id in affected_generations {
            self.active_leases
                .remove(&(stream_id.to_string(), generation_id));
            if self
                .active_current
                .get(stream_id)
                .is_some_and(|(active_generation, _)| *active_generation == generation_id)
            {
                self.active_current.remove(stream_id);
            }
        }
        superseded
    }

    pub(super) fn restore_current_generation(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> bool {
        if self.current_reservation_active(stream_id) {
            return false;
        }
        let Some(generations) = self.by_stream.get_mut(stream_id) else {
            return false;
        };
        if !generations.get(&generation_id).is_some_and(|state| {
            state.durability != PendingPromotionDurability::TerminalDurable
                && state.durability != PendingPromotionDurability::PublicationUnknown
        }) {
            return false;
        }
        for state in generations.values_mut() {
            state.current = false;
        }
        if let Some(state) = generations.get_mut(&generation_id) {
            state.current = true;
        }
        true
    }

    pub(super) fn reserve_generation(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        nonce: super::SmPromotionLeaseNonce,
    ) -> bool {
        let Some(authority) = self.authority(stream_id, generation_id) else {
            return false;
        };
        let current = authority == super::SmSessionPromotionAuthority::CurrentDurable;
        let key = (stream_id.to_string(), generation_id);
        if self.active_leases.contains_key(&key)
            || (current && self.active_current.contains_key(stream_id))
        {
            return false;
        }
        self.active_leases.insert(key, nonce);
        if current {
            self.active_current
                .insert(stream_id.to_string(), (generation_id, nonce));
        }
        true
    }

    pub(super) fn reservation_matches(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        nonce: super::SmPromotionLeaseNonce,
    ) -> bool {
        self.active_leases
            .get(&(stream_id.to_string(), generation_id))
            == Some(&nonce)
    }

    /// Whether the exact lease still owns current-generation mutation
    /// authority. `active_current` is intentionally part of the predicate:
    /// external demotion removes it while retaining `active_leases`, so an
    /// older lease cannot regain authority if a generation is later marked
    /// current again.
    pub(super) fn current_reservation_matches(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        nonce: super::SmPromotionLeaseNonce,
    ) -> bool {
        self.is_current(stream_id, generation_id) == Some(true)
            && self.reservation_matches(stream_id, generation_id, nonce)
            && self.active_current.get(stream_id) == Some(&(generation_id, nonce))
    }

    pub(super) fn terminal_reservation_matches(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        nonce: super::SmPromotionLeaseNonce,
    ) -> bool {
        self.authority(stream_id, generation_id)
            == Some(super::SmSessionPromotionAuthority::TerminalDurable)
            && self.reservation_matches(stream_id, generation_id, nonce)
    }

    pub(super) fn current_reservation_active(&self, stream_id: &str) -> bool {
        self.active_current.contains_key(stream_id)
    }

    pub(super) fn generation_reservation_active(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> bool {
        self.active_leases
            .contains_key(&(stream_id.to_string(), generation_id))
    }

    pub(super) fn release_reservation(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        nonce: super::SmPromotionLeaseNonce,
    ) -> bool {
        if !self.reservation_matches(stream_id, generation_id, nonce) {
            return false;
        }
        self.active_leases
            .remove(&(stream_id.to_string(), generation_id));
        if self.active_current.get(stream_id) == Some(&(generation_id, nonce)) {
            self.active_current.remove(stream_id);
        }
        true
    }

    /// Remove one exact generation only while no promotion lease can be
    /// using it. Used when an ambiguous atomic replacement is later proven
    /// definitely uncommitted and the predecessor becomes resumable again.
    pub(super) fn remove_unreserved_generation(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> bool {
        if self.generation_reservation_active(stream_id, generation_id) {
            return false;
        }
        let Some(generations) = self.by_stream.get_mut(stream_id) else {
            return false;
        };
        if generations.remove(&generation_id).is_none() {
            return false;
        }
        if generations.is_empty() {
            self.by_stream.remove(stream_id);
        }
        if self
            .active_current
            .get(stream_id)
            .is_some_and(|(active_generation, _)| *active_generation == generation_id)
        {
            self.active_current.remove(stream_id);
        }
        true
    }

    pub(super) fn contains(&self, stream_id: &str) -> bool {
        self.active_current.contains_key(stream_id)
            || self
                .by_stream
                .get(stream_id)
                .is_some_and(|generations| !generations.is_empty())
    }

    pub(super) fn contains_generation(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> bool {
        self.by_stream
            .get(stream_id)
            .is_some_and(|generations| generations.contains_key(&generation_id))
    }

    pub(super) fn contains_other_generation(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> bool {
        self.by_stream.get(stream_id).is_some_and(|generations| {
            generations
                .keys()
                .any(|candidate| *candidate != generation_id)
        })
    }

    pub(super) fn is_current(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> Option<bool> {
        self.by_stream
            .get(stream_id)?
            .get(&generation_id)
            .map(|state| state.current)
    }

    /// Mutation authority for one exact generation. `None` is distinct from
    /// absence: publication-unknown inventory intentionally cannot be leased.
    pub(super) fn authority(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> Option<super::SmSessionPromotionAuthority> {
        let state = self.by_stream.get(stream_id)?.get(&generation_id)?;
        match state.durability {
            PendingPromotionDurability::PublicationUnknown => None,
            PendingPromotionDurability::TerminalDurable => {
                Some(super::SmSessionPromotionAuthority::TerminalDurable)
            }
            PendingPromotionDurability::DurableRow
            | PendingPromotionDurability::DefinitelyNeverPublished => Some(if state.current {
                super::SmSessionPromotionAuthority::CurrentDurable
            } else {
                super::SmSessionPromotionAuthority::ObsoleteGeneration
            }),
        }
    }

    pub(super) fn current_durable_generation(
        &self,
        stream_id: &str,
    ) -> Option<super::SmSessionGenerationId> {
        self.by_stream
            .get(stream_id)?
            .iter()
            .find_map(|(generation_id, state)| {
                (state.current && state.durability == PendingPromotionDurability::DurableRow)
                    .then_some(*generation_id)
            })
    }

    pub(super) fn retain_claim_fence(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        fence: super::super::persistence::SmClaimFence,
    ) -> Option<super::super::persistence::SmClaimFence> {
        let state = self.by_stream.get_mut(stream_id)?.get_mut(&generation_id)?;
        if state.claim_fence.is_none() {
            state.claim_fence = Some(fence);
        }
        state.claim_fence.clone()
    }

    pub(super) fn claim_fence(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> Option<super::super::persistence::SmClaimFence> {
        self.by_stream
            .get(stream_id)?
            .get(&generation_id)?
            .claim_fence
            .clone()
    }

    pub(super) fn retained_claim_fences(
        &self,
    ) -> impl Iterator<Item = (&String, &super::super::persistence::SmClaimFence)> {
        self.by_stream.iter().flat_map(|(stream_id, generations)| {
            generations
                .values()
                .filter_map(|state| state.claim_fence.as_ref())
                .map(move |fence| (stream_id, fence))
        })
    }

    /// Exact claim responsibility that is safe to report as abandoned at a
    /// shutdown deadline. A publication-unknown payload may retain a copied
    /// fence only as conservative recovery context; that carrier is not proof
    /// that this process still owns the claim.
    pub(super) fn shutdown_claim_fences(
        &self,
    ) -> impl Iterator<Item = (&String, &super::super::persistence::SmClaimFence)> {
        self.by_stream.iter().flat_map(|(stream_id, generations)| {
            generations
                .values()
                .filter(|state| state.durability != PendingPromotionDurability::PublicationUnknown)
                .filter_map(|state| state.claim_fence.as_ref())
                .map(move |fence| (stream_id, fence))
        })
    }

    fn retains_claim_fence(
        &self,
        stream_id: &str,
        fence: &super::super::persistence::SmClaimFence,
    ) -> bool {
        self.by_stream.get(stream_id).is_some_and(|generations| {
            generations
                .values()
                .any(|state| state.claim_fence.as_ref() == Some(fence))
        })
    }

    pub(super) fn is_definitely_never_published(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> bool {
        self.by_stream
            .get(stream_id)
            .and_then(|generations| generations.get(&generation_id))
            .is_some_and(|state| {
                state.durability == PendingPromotionDurability::DefinitelyNeverPublished
            })
    }

    /// Give up durable-row authority without retiring the payload token.
    /// This is valid for a carrier whose snapshot was proven never to have
    /// committed, or a normal generation already unlinked by external local
    /// demotion. Its next Q6 attempt is payload-only while retaining the
    /// exact stanza generation for retry.
    pub(super) fn demote_for_payload_retry_under_reservation(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        nonce: super::SmPromotionLeaseNonce,
    ) -> bool {
        if !self.reservation_matches(stream_id, generation_id, nonce) {
            return false;
        }
        let Some(state) = self
            .by_stream
            .get_mut(stream_id)
            .and_then(|generations| generations.get_mut(&generation_id))
        else {
            return false;
        };
        if state.current && state.durability != PendingPromotionDurability::DefinitelyNeverPublished
        {
            return false;
        }
        state.current = false;
        self.active_leases
            .remove(&(stream_id.to_string(), generation_id));
        if self.active_current.get(stream_id) == Some(&(generation_id, nonce)) {
            self.active_current.remove(stream_id);
        }
        true
    }

    pub(super) fn record_failure_under_reservation(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        nonce: super::SmPromotionLeaseNonce,
    ) -> Option<u32> {
        if !self.reservation_matches(stream_id, generation_id, nonce) {
            return None;
        }
        let state = self.by_stream.get_mut(stream_id)?.get_mut(&generation_id)?;
        state.failures = state.failures.saturating_add(1);
        Some(state.failures)
    }

    pub(super) fn retire_under_reservation(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
        nonce: super::SmPromotionLeaseNonce,
    ) -> Option<bool> {
        if !self.reservation_matches(stream_id, generation_id, nonce) {
            return None;
        }
        let generations = self.by_stream.get_mut(stream_id)?;
        let current = generations.remove(&generation_id)?.current;
        if generations.is_empty() {
            self.by_stream.remove(stream_id);
        }
        self.active_leases
            .remove(&(stream_id.to_string(), generation_id));
        if self.active_current.get(stream_id) == Some(&(generation_id, nonce)) {
            self.active_current.remove(stream_id);
        }
        Some(current)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &String> {
        self.by_stream
            .keys()
            .chain(self.active_current.keys())
            .chain(self.active_leases.keys().map(|(stream_id, _)| stream_id))
    }
}

/// Retry payloads keyed by the exact promotion generation. A bare stream id
/// can temporarily have both an obsolete carrier and a current durable
/// successor, so cancellation must not overwrite either payload.
#[derive(Debug, Default)]
pub(super) struct PendingPromotionRetries {
    by_stream: HashMap<String, HashMap<super::SmSessionGenerationId, DetachedSession>>,
}

impl PendingPromotionRetries {
    pub(super) fn insert(&mut self, session: DetachedSession) -> Option<DetachedSession> {
        self.by_stream
            .entry(session.stream_id.clone())
            .or_default()
            .insert(session.generation_id, session)
    }

    pub(super) fn get_generation(
        &self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> Option<&DetachedSession> {
        self.by_stream.get(stream_id)?.get(&generation_id)
    }

    pub(super) fn get_generation_mut(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> Option<&mut DetachedSession> {
        self.by_stream.get_mut(stream_id)?.get_mut(&generation_id)
    }

    #[cfg(test)]
    pub(super) fn contains_key(&self, stream_id: &str) -> bool {
        self.by_stream
            .get(stream_id)
            .is_some_and(|generations| !generations.is_empty())
    }

    pub(super) fn remove_generation(
        &mut self,
        stream_id: &str,
        generation_id: super::SmSessionGenerationId,
    ) -> Option<DetachedSession> {
        let sessions = self.by_stream.get_mut(stream_id)?;
        let removed = sessions.remove(&generation_id);
        if sessions.is_empty() {
            self.by_stream.remove(stream_id);
        }
        removed
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&String, &DetachedSession)> {
        self.by_stream.iter().flat_map(|(stream_id, sessions)| {
            sessions.values().map(move |session| (stream_id, session))
        })
    }

    pub(super) fn generation_keys(
        &self,
    ) -> impl Iterator<Item = (&String, super::SmSessionGenerationId)> {
        self.by_stream.iter().flat_map(|(stream_id, sessions)| {
            sessions
                .keys()
                .copied()
                .map(move |generation_id| (stream_id, generation_id))
        })
    }

    pub(super) fn len(&self) -> usize {
        self.by_stream.values().map(HashMap::len).sum()
    }
}

/// Operation-owned capacity marker for a reclaimed SM claim mutation.
/// Only the operation holding this token may consume, cancel, or defer its
/// reservation, so an older same-stream lifecycle cannot erase a newer
/// ownership CAS's ambiguity marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReclaimedClaimReservation(u64);

impl ReclaimedClaimReservation {
    /// Construct a deterministic token for adapters that model reservation
    /// ownership in tests. Production tokens are issued by the registry.
    #[doc(hidden)]
    pub const fn from_generation(generation: u64) -> Self {
        Self(generation)
    }
}

/// Count distinct bounded ownership responsibilities across the three exact
/// fence inventories. A reservation paired with an active, non-terminal
/// fence is only an ambiguity marker for that same responsibility; counting
/// both would reject otherwise usable session capacity. A reservation can
/// likewise reuse a retained durable-recovery handoff, but remains distinct
/// from `ReleaseMayComplete` because that release may have committed and a
/// subsequent acquisition may mint a new generation.
fn occupied_claim_fence_capacity(
    promotions: &PendingPromotions,
    reservations: &HashSet<String>,
    reclaimed_reservations: &HashMap<String, ReclaimedClaimReservation>,
    pending: &HashMap<
        (String, super::super::persistence::SmClaimFence),
        PendingClaimReleaseDisposition,
    >,
    fences: &HashMap<String, super::super::persistence::SmClaimFence>,
) -> usize {
    let current_not_pending = fences
        .iter()
        .filter(|(id, fence)| !pending.contains_key(&(id.to_string(), (*fence).clone())))
        .count();
    let unrepresented_reservations = reservations
        .iter()
        .chain(reclaimed_reservations.keys())
        .filter(|id| claim_reservation_requires_independent_capacity(pending, fences, id))
        .count();
    let retained_unrepresented = promotions
        .retained_claim_fences()
        .filter(|(stream_id, fence)| {
            fences.get(*stream_id) != Some(*fence)
                && !pending.contains_key(&((*stream_id).clone(), (*fence).clone()))
        })
        .map(|(stream_id, fence)| (stream_id.clone(), fence.clone()))
        .collect::<HashSet<_>>()
        .len();

    pending
        .len()
        .saturating_add(current_not_pending)
        .saturating_add(retained_unrepresented)
        .saturating_add(unrepresented_reservations)
}

/// Whether a fence-less reservation represents an independently generational
/// shutdown/capacity responsibility rather than the acquisition side of an
/// already-counted active or retained exact fence.
pub(super) fn claim_reservation_requires_independent_capacity(
    pending: &HashMap<
        (String, super::super::persistence::SmClaimFence),
        PendingClaimReleaseDisposition,
    >,
    fences: &HashMap<String, super::super::persistence::SmClaimFence>,
    stream_id: &str,
) -> bool {
    let represented_by_active_fence = fences
        .get(stream_id)
        .is_some_and(|fence| !pending.contains_key(&(stream_id.to_string(), fence.clone())));
    !represented_by_active_fence && !pending_claim_reservation_reuses_capacity(pending, stream_id)
}

/// A same-stream ownership mutation can reuse a retained handoff's capacity
/// only while every pending exact fence is still known not to have entered a
/// release attempt. Any `ReleaseMayComplete` fence may already be absent in
/// the backend, so a subsequent acquisition can mint another generation and
/// needs independently-counted capacity.
fn pending_claim_reservation_reuses_capacity(
    pending: &HashMap<
        (String, super::super::persistence::SmClaimFence),
        PendingClaimReleaseDisposition,
    >,
    stream_id: &str,
) -> bool {
    let mut retained = false;
    for ((pending_stream_id, _), disposition) in pending {
        if pending_stream_id != stream_id {
            continue;
        }
        match disposition {
            PendingClaimReleaseDisposition::RetainedForDurableRecovery => retained = true,
            PendingClaimReleaseDisposition::ReleaseMayComplete => return false,
        }
    }
    retained
}

/// Bound on any `ClaimStore` acquire/`ensure_claimed` call made while this
/// registry holds one of its [`STREAM_LOCK_SHARDS`] stream-shard locks (FIX
/// 5, council-adjudicated ADR-0017 Phase 3 Slice 5 corrigenda:
/// `claim_session`, `claims.rs::acquire_claim_store_entry_for_detach`, and
/// [`InMemorySmSessionRegistry::hydrate_reclaimed`] below).
///
/// **Shard-fan-in rationale**: `stream_lock` hashes a stream id down to one
/// of a fixed, small number of shard mutexes — many unrelated stream ids
/// share the same shard. A hung `ClaimStore` call while holding one shard's
/// lock therefore does not just stall the one stream id it was issued for;
/// it stalls every OTHER live stream id that happens to hash to the same
/// shard too (store/take/claim/release, all of which take the same shard
/// lock before touching `sessions`/`claimed_sessions`). This is a strictly
/// wider blast radius than a genuinely per-entity lock would have, which is
/// why every `ClaimStore` call issued under a shard lock is bounded here —
/// mirrors `self_fence.rs::expire_bounded`'s bounded/best-effort/logged
/// pattern one level down (a per-entity claim call instead of a per-node
/// lease call).
pub(super) const CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// In-memory implementation of the SM session registry, optionally
/// backed by a [`SmPersistenceStorage`] so detached sessions survive
/// process restarts (issue #209 slice (d) phase 3, locked Q8 = B).
///
/// When `persistence` is `Some`, every `store_session` /
/// `take_session` / `cleanup_expired` mutation also writes to the
/// durable backend; on startup, [`Self::restore_from_persistence`]
/// rebuilds the in-memory view so an XEP-0198 `<resume previd='…'/>`
/// finds sessions that detached before the most recent restart.
///
/// Custom Debug skips the persistence handle (the
/// [`SmPersistenceStorage`] trait does not require `Debug`) and the
/// claim store (`dyn ClaimStore` does not require `Debug` either).
pub struct InMemorySmSessionRegistry {
    pub(super) sessions: RwLock<HashMap<String, DetachedSession>>,
    pub(super) claimed_sessions: RwLock<HashMap<String, DetachedSession>>,
    pub(super) stream_locks: Vec<Arc<tokio::sync::Mutex<()>>>,
    pub(super) max_sessions: usize,
    /// Recently applied XEP-0424/0425 tombstones, kept for the
    /// promotion-time re-check (round-2 review R2). Bounded by
    /// [`super::tombstones::RECENT_TOMBSTONE_TTL`] +
    /// [`super::tombstones::MAX_RECENT_TOMBSTONES`].
    pub(super) recent_tombstones: RwLock<Vec<super::tombstones::RecentTombstone>>,
    /// Optional durable backing store. When `None` the registry is
    /// strictly in-memory (legacy behaviour); production wiring sets
    /// this via [`Self::with_persistence`] before Arc-wrapping.
    pub(super) persistence:
        Option<std::sync::Arc<dyn super::super::persistence::SmPersistenceStorage>>,
    /// The entity-ownership authority for this registry's SM-session claims
    /// (ADR-0017 Phase 3 Slice 1, Q2 "retrofit, not wrap"). Defaults to
    /// [`InProcessClaimStore`] — correct for every build today, since no
    /// caller yet constructs this registry with `clustering.enabled`; a
    /// later slice injects a Postgres-backed store via
    /// [`Self::with_claim_store`] once `SmPersistenceStorage` itself
    /// becomes claim-scoped (Slice 4+).
    ///
    /// This is the **authority** on whether a claim is granted
    /// (`claims.rs`'s `claim_session` gates its own outcome on
    /// [`ClaimStore::acquire`]'s result) and on when a claim ends
    /// (`release_claim`, every terminal branch of `complete_claim`/
    /// `complete_claim_if_resumable`, and `invalidate_sessions_for_jid`'s
    /// removal of a claimed session all call back into it). `stream_locks`/
    /// `sessions`/`claimed_sessions` remain exactly the in-process
    /// contention optimization and session-*state* holders the ADR names
    /// for `StreamLockMap` (element 4) — never a second source of
    /// ownership truth alongside this store, which is precisely the
    /// *wrap* design Q2 rejected.
    pub(super) claim_store: Arc<dyn ClaimStore>,
    /// This node's identity, as presented to `claim_store`. Single-node
    /// deployments use a [`SharedNodeIdentity`] wrapping
    /// [`NodeIdentity::local`]; [`Self::with_claim_store`] (ADR-0017 Phase 3
    /// Slice 5) instead wires in the SAME live, updatable handle
    /// `self_fence::run_node_lease` refreshes on every re-registration
    /// (mirroring `PostgresFencedSmPersistence`'s identical Slice 4
    /// follow-up plumbing fix). New acquisitions read `.current()` once;
    /// owned work then carries that immutable owner together with its epoch,
    /// so a later self-fence cannot silently rebind an old claim to the new
    /// node incarnation.
    pub(super) node_identity: SharedNodeIdentity,
    /// Tracks the immutable owner+epoch fence this registry last observed for each currently
    /// claimed SM-session entity, so `release_claim`/`complete_claim` can
    /// hand the right epoch back to `claim_store.release`. Purely local
    /// bookkeeping — the `ClaimStore` implementation itself is the
    /// authority on what epoch is actually current.
    pub(super) claim_fences: RwLock<HashMap<String, super::super::persistence::SmClaimFence>>,
    /// Exact claims handed out of live authority, together with whether their
    /// durable recovery work has been proven complete. Separate from
    /// `claim_fences`: a session drained for promotion is absent from the
    /// active map but its claim remains retained until durable inventory is
    /// empty; an attempted release is monotonic `ReleaseMayComplete` because
    /// its backend mutation may already have committed.
    pub(super) pending_claim_releases: RwLock<
        HashMap<(String, super::super::persistence::SmClaimFence), PendingClaimReleaseDisposition>,
    >,
    /// Acquisitions whose timeout made commit status ambiguous. The typed
    /// disposition distinguishes rejected enable admission (recover then
    /// release) from detach after durable snapshot publication (recover and
    /// retain ownership).
    pub(super) pending_claim_acquisitions:
        RwLock<HashSet<(String, NodeIdentity, PendingClaimAcquisitionDisposition)>>,
    /// Sessions removed from the resumable maps and handed to the XEP-0198
    /// promote-then-confirm lifecycle. Their exact claim must remain held
    /// across displacement, expiry, shutdown, invalidation, retry
    /// reinsertion, and caller cancellation until durable deletion is
    /// confirmed.
    pub(super) pending_promotions: Arc<RwLock<PendingPromotions>>,
    /// Full payloads handed back by cancellation guards. They remain outside
    /// the resumable map until `drain_expired` reconciles them against the
    /// durable row, preventing stale pre-tombstone queues from being
    /// republished directly from `Drop`.
    pub(super) pending_promotion_retries: RwLock<PendingPromotionRetries>,
    /// Claimed ISR sessions whose follow-up epoch lookup failed before the
    /// route could prove that the recorded exact fence still owns the backend
    /// row. Kept out of `sessions` until a read-only reconciliation proves
    /// the same owner+epoch or terminalizes the stale local lifecycle.
    pub(super) pending_epoch_failure_reconciliations: RwLock<HashSet<String>>,
    /// Exact reclaimed-session hydration work that has not yet reached a
    /// terminal outcome. This registry-owned inventory is the common safety
    /// net for both the supervised orphan reaper and the one-shot inline
    /// self-fence path: once a node wins a claim, a transient durable read or
    /// an identity rotation must not leave that live-owned claim invisible to
    /// every future orphan scan.
    pub(super) pending_reclaimed_hydrations: RwLock<
        HashMap<
            (
                String,
                super::super::persistence::SmClaimFence,
                ReclaimedClaimReservation,
            ),
            Entity,
        >,
    >,
    /// Ownership-changing calls whose timeout made the committed result
    /// unknown before an epoch could be returned. The attempted owner is
    /// enough to reconcile them without replaying a one-shot CAS: a later
    /// `current_claim` either supplies the exact epoch now owned by that
    /// incarnation or proves that this attempt did not remain authoritative.
    pub(super) pending_reclaimed_claim_lookups:
        RwLock<HashMap<(String, NodeIdentity, ReclaimedClaimReservation), Entity>>,
    /// Capacity reserved before an acquisition whose exact epoch is not yet
    /// known. A reservation survives an ambiguous timeout and is consumed
    /// only when reconciliation either records the resulting fence or proves
    /// that this node did not acquire the claim.
    pub(super) claim_fence_reservations: RwLock<HashSet<String>>,
    pub(super) reclaimed_claim_reservations: RwLock<HashMap<String, ReclaimedClaimReservation>>,
    next_reclaimed_claim_reservation: AtomicU64,
    /// ADR-0017 Phase 3 Slice 6: the cross-node "ask the live owner to
    /// detach" bridge for the XEP-0198 resume path's live-handshake branch.
    /// `None` for single-node/non-clustering deployments (the cross-node
    /// resume fallback then never has anything to ask — see
    /// `cross_node_resume::attempt_cross_node_resume`'s doc comment).
    /// Production wiring injects a `waddle-server`-side adapter over
    /// `RelayHandle` via [`Self::with_remote_resume_asker`].
    pub(super) remote_resume: Option<Arc<dyn super::cross_node_resume::RemoteResumeAsker>>,
}

impl Default for InMemorySmSessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for InMemorySmSessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemorySmSessionRegistry")
            .field("max_sessions", &self.max_sessions)
            .field(
                "session_count",
                &self.sessions.read().map(|s| s.len()).unwrap_or(0),
            )
            .field(
                "claimed_count",
                &self.claimed_sessions.read().map(|s| s.len()).unwrap_or(0),
            )
            .field("stream_lock_shards", &self.stream_locks.len())
            .field("persistence_attached", &self.persistence.is_some())
            .field("node_identity", &self.node_identity.current())
            .finish()
    }
}

/// Result of persisting a same-id replacement whose terminal predecessor is
/// also the transaction's durable commit marker.
pub(super) enum PersistDetachedReplacementOutcome {
    Committed,
    PublicationUnknown(super::super::persistence::SmPersistenceError),
}

impl InMemorySmSessionRegistry {
    /// Reserve bounded local responsibility before an external caller
    /// performs an ownership-changing CAS for a reclaimed SM session.
    /// The reservation is consumed when the exact returned fence is
    /// published, or cancelled when the CAS is known not to have won.
    pub fn reserve_reclaimed_claim_capacity(
        &self,
        entity: &Entity,
    ) -> Option<ReclaimedClaimReservation> {
        if entity.entity_type != EntityType::SmSession {
            return None;
        }
        self.reserve_reclaimed_claim_fence_capacity(&entity.id)
    }

    pub fn cancel_reclaimed_claim_capacity(
        &self,
        entity: &Entity,
        reservation: ReclaimedClaimReservation,
    ) {
        self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
    }

    /// Retain a timed-out ownership mutation without replaying it. The
    /// registry later uses a read-only `current_claim` to discover the exact
    /// epoch iff this attempted owner actually won.
    pub fn defer_uncertain_reclaimed_claim(
        &self,
        entity: &Entity,
        owner: &NodeIdentity,
        reservation: ReclaimedClaimReservation,
    ) {
        if !self.has_reclaimed_claim_fence_reservation(&entity.id, reservation) {
            return;
        }
        if let Ok(mut pending) = self.pending_reclaimed_claim_lookups.write() {
            pending.insert(
                (entity.id.clone(), owner.clone(), reservation),
                entity.clone(),
            );
        }
    }

    /// Convert matching reclaimed/active responsibility into terminal exact
    /// release for the cross-node repair path. The caller must hold this
    /// stream's shard through this conversion and local-lifecycle removal.
    pub(super) fn transfer_reclaimed_claim_to_exact_release(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<bool, SmRegistryError> {
        let key = (entity.id.clone(), fence.clone());
        let (
            Ok(sessions),
            Ok(claimed),
            Ok(promotions),
            Ok(reservations),
            Ok(mut reclaimed),
            Ok(mut pending),
            Ok(mut fences),
        ) = (
            self.sessions.read(),
            self.claimed_sessions.read(),
            self.pending_promotions.read(),
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.write(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
        )
        else {
            return Err(SmRegistryError::Internal(
                "cross-node repair could not inspect exact-release bookkeeping".to_string(),
            ));
        };
        let promotion_pending = promotions.contains(&entity.id);
        let stream_live = sessions.contains_key(&entity.id)
            || claimed.contains_key(&entity.id)
            || promotion_pending;
        let matching_reservation = reclaimed.get(&entity.id) == Some(&reservation);
        let matching_active_fence = fences.get(&entity.id) == Some(fence);
        let matching_pending_release = pending.contains_key(&key);
        let conflicting_generic_reservation = reservations.contains(&entity.id);
        let conflicting_reservation = reclaimed
            .get(&entity.id)
            .is_some_and(|current| current != &reservation);
        let conflicting_active_fence = fences
            .get(&entity.id)
            .is_some_and(|current| current != fence);
        let pending_only =
            matching_pending_release && !matching_reservation && !matching_active_fence;
        if promotion_pending
            || conflicting_generic_reservation
            || conflicting_reservation
            || conflicting_active_fence
            || (pending_only && stream_live)
            || (!matching_reservation && !matching_active_fence && !matching_pending_release)
        {
            return Ok(false);
        }
        if matching_reservation {
            reclaimed.remove(&entity.id);
        }
        if matching_active_fence {
            fences.remove(&entity.id);
        }
        pending
            .entry(key)
            .or_insert(PendingClaimReleaseDisposition::RetainedForDurableRecovery);
        drop(fences);
        drop(pending);
        drop(reclaimed);
        self.clear_pending_reclaimed_hydration(entity, fence, reservation);
        if let Ok(mut pending) = self.pending_reclaimed_claim_lookups.write() {
            pending.remove(&(entity.id.clone(), fence.owner().clone(), reservation));
        }
        Ok(true)
    }

    /// Advance one exact retained handoff to terminal release eligibility.
    ///
    /// This also records a release issued directly from an active exact fence.
    /// A same-stream acquisition reservation blocks the first transition: an
    /// issued release could otherwise complete after that acquisition
    /// republishes the same fence. Repeating an already-issued transition is
    /// idempotent, and no caller can downgrade it afterward.
    pub(super) fn mark_claim_release_may_complete(
        &self,
        stream_id: &str,
        fence: &super::super::persistence::SmClaimFence,
    ) -> bool {
        let (Ok(reservations), Ok(reclaimed), Ok(mut pending)) = (
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.write(),
        ) else {
            return false;
        };
        let key = (stream_id.to_string(), fence.clone());
        if pending.get(&key) == Some(&PendingClaimReleaseDisposition::ReleaseMayComplete) {
            return true;
        }
        if reservations.contains(stream_id) || reclaimed.contains_key(stream_id) {
            return false;
        }
        pending.insert(key, PendingClaimReleaseDisposition::ReleaseMayComplete);
        true
    }

    pub(super) fn reserve_claim_fence_capacity(&self, stream_id: &str) -> bool {
        self.reserve_claim_fence_capacity_up_to(stream_id, self.max_sessions)
    }

    fn reserve_reclaimed_claim_fence_capacity(
        &self,
        stream_id: &str,
    ) -> Option<ReclaimedClaimReservation> {
        let (Ok(promotions), Ok(reservations), Ok(mut reclaimed), Ok(pending), Ok(fences)) = (
            self.pending_promotions.read(),
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.write(),
            self.pending_claim_releases.read(),
            self.claim_fences.read(),
        ) else {
            return None;
        };
        if reservations.contains(stream_id) || reclaimed.contains_key(stream_id) {
            return None;
        }
        let occupied = occupied_claim_fence_capacity(
            &promotions,
            &reservations,
            &reclaimed,
            &pending,
            &fences,
        );
        let active_nonterminal = fences
            .get(stream_id)
            .is_some_and(|fence| !pending.contains_key(&(stream_id.to_string(), fence.clone())));
        let reuses_retained_handoff =
            pending_claim_reservation_reuses_capacity(&pending, stream_id);
        if !active_nonterminal && !reuses_retained_handoff && occupied >= self.max_sessions {
            return None;
        }
        let token = ReclaimedClaimReservation(
            self.next_reclaimed_claim_reservation
                .fetch_add(1, Ordering::Relaxed),
        );
        reclaimed.insert(stream_id.to_string(), token);
        Some(token)
    }

    fn cancel_reclaimed_claim_fence_reservation(
        &self,
        stream_id: &str,
        reservation: ReclaimedClaimReservation,
    ) {
        if let Ok(mut reservations) = self.reclaimed_claim_reservations.write() {
            if reservations.get(stream_id) == Some(&reservation) {
                reservations.remove(stream_id);
            }
        }
    }

    fn has_reclaimed_claim_fence_reservation(
        &self,
        stream_id: &str,
        reservation: ReclaimedClaimReservation,
    ) -> bool {
        self.reclaimed_claim_reservations
            .read()
            .is_ok_and(|reservations| reservations.get(stream_id) == Some(&reservation))
    }

    /// Reserve the exact-fence slot needed by a live detach. Capacity
    /// eviction briefly needs both the displaced session's fence (until its
    /// caller confirms promotion) and the replacement session's fence. Keep
    /// one explicitly bounded turnover slot for that transition; subsequent
    /// detaches reject until the displaced responsibility is drained.
    pub(super) fn reserve_detach_claim_fence_capacity(
        &self,
        stream_id: &str,
    ) -> Option<DetachClaimFenceReservation> {
        // A detach can intentionally supersede this stream's timed-out,
        // rejected-enable acquisition. Both paths are serialized by the
        // stream shard before the detach reaches this point, so transferring
        // that already-counted marker is not the unsafe concurrent sharing
        // rejected by the general reservation API.
        let rejected_enable_handoff = self
            .claim_fence_reservations
            .read()
            .is_ok_and(|reservations| reservations.contains(stream_id))
            && self.pending_claim_acquisitions.read().is_ok_and(|pending| {
                let has_rejected_enable = pending.iter().any(|(id, _, disposition)| {
                    id == stream_id
                        && *disposition == PendingClaimAcquisitionDisposition::ReleaseRejectedEnable
                });
                let has_uncertain_detach = pending.iter().any(|(id, _, disposition)| {
                    id == stream_id
                        && matches!(
                            disposition,
                            PendingClaimAcquisitionDisposition::RetainDetachedSession(_)
                        )
                });
                has_rejected_enable && !has_uncertain_detach
            });
        if rejected_enable_handoff {
            return Some(DetachClaimFenceReservation::BorrowedRejectedEnable);
        }
        self.reserve_claim_fence_capacity_up_to(stream_id, self.max_sessions.saturating_add(1))
            .then_some(DetachClaimFenceReservation::Owned)
    }

    fn reserve_claim_fence_capacity_up_to(&self, stream_id: &str, capacity: usize) -> bool {
        // Reclaimed hydration and ambiguous-lookup inventories are already
        // represented here: every reclaim reserves before its ownership CAS,
        // then either retains that reservation while the epoch is unknown or
        // consumes it into `claim_fences` once the exact fence is known.
        // `try_record_verified_reclaimed_fence` removes the reservation and
        // inserts that fence while holding all three inventory write locks in
        // one non-awaiting critical section; no transient/cancellation window
        // can leave only `pending_reclaimed_hydrations` behind.
        // Counting those retry maps separately would double-charge the same
        // ownership responsibility and reject usable capacity.
        let (Ok(promotions), Ok(mut reservations), Ok(reclaimed), Ok(pending), Ok(fences)) = (
            self.pending_promotions.read(),
            self.claim_fence_reservations.write(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.read(),
            self.claim_fences.read(),
        ) else {
            return false;
        };
        // A reservation is an operation-owned ambiguity marker, not an
        // idempotent shared lease. Admitting another same-stream mutation
        // onto it would let the loser cancel the winner's only capacity
        // representation after an external CAS committed ambiguously.
        if reservations.contains(stream_id) || reclaimed.contains_key(stream_id) {
            return false;
        }
        if let Some(fence) = fences.get(stream_id) {
            // A confirmed-current fence makes ensure_claimed idempotent and
            // cannot create another generation. A fence whose terminal
            // release timed out is different: the release may have committed,
            // so the next ensure can mint a new generation and must reserve a
            // second exact-fence slot before touching the backend.
            if !pending.contains_key(&(stream_id.to_string(), fence.clone())) {
                // Even an idempotent self-ensure can be cancelled before its
                // outcome is observed. Publish an in-flight marker paired
                // with this already-counted fence so demotion can transfer
                // the ambiguity into reservation-backed retry responsibility
                // before removing the confirmed fence.
                reservations.insert(stream_id.to_string());
                return true;
            }
        }
        let occupied = occupied_claim_fence_capacity(
            &promotions,
            &reservations,
            &reclaimed,
            &pending,
            &fences,
        );
        let reuses_retained_handoff =
            pending_claim_reservation_reuses_capacity(&pending, stream_id);
        if !reuses_retained_handoff && occupied >= capacity {
            return false;
        }
        reservations.insert(stream_id.to_string());
        true
    }

    pub(super) fn cancel_claim_fence_reservation(&self, stream_id: &str) {
        if let Ok(mut reservations) = self.claim_fence_reservations.write() {
            reservations.remove(stream_id);
        }
    }

    #[cfg(test)]
    pub(super) fn has_claim_fence_reservation(&self, stream_id: &str) -> bool {
        self.claim_fence_reservations
            .read()
            .is_ok_and(|reservations| reservations.contains(stream_id))
    }

    #[cfg(test)]
    pub(super) fn claim_fence_capacity_used(&self) -> usize {
        let (Ok(promotions), Ok(reservations), Ok(reclaimed), Ok(pending), Ok(fences)) = (
            self.pending_promotions.read(),
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.read(),
            self.claim_fences.read(),
        ) else {
            return self.max_sessions;
        };
        occupied_claim_fence_capacity(&promotions, &reservations, &reclaimed, &pending, &fences)
    }

    pub(super) fn try_record_claim_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        let mut superseded = Vec::new();
        let (
            Ok(mut promotions),
            Ok(mut reservations),
            Ok(reclaimed),
            Ok(mut pending),
            Ok(mut fences),
            Ok(mut hydrations),
        ) = (
            self.pending_promotions.write(),
            self.claim_fence_reservations.write(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
            self.pending_reclaimed_hydrations.write(),
        )
        else {
            return false;
        };
        let key = (stream_id.to_string(), fence.clone());
        if pending.get(&key) == Some(&PendingClaimReleaseDisposition::ReleaseMayComplete) {
            return false;
        }
        if fences.get(stream_id) == Some(&fence) {
            reservations.remove(stream_id);
            pending.retain(|(id, old), _| {
                if id == stream_id {
                    if old != &fence {
                        superseded.push(old.clone());
                    }
                    false
                } else {
                    true
                }
            });
            superseded.extend(promotions.publish_verified_claim_fence(stream_id, &fence));
        } else {
            let represented_by_promotion = promotions.retains_claim_fence(stream_id, &fence);
            let reserved = reservations.remove(stream_id);
            let occupied = occupied_claim_fence_capacity(
                &promotions,
                &reservations,
                &reclaimed,
                &pending,
                &fences,
            );
            if !reserved && !represented_by_promotion && occupied >= self.max_sessions {
                return false;
            }
            if let Some(previous) = fences.remove(stream_id) {
                if previous != fence {
                    superseded.push(previous);
                }
            }
            pending.retain(|(id, old), _| {
                if id == stream_id {
                    if old != &fence {
                        superseded.push(old.clone());
                    }
                    false
                } else {
                    true
                }
            });
            superseded.extend(promotions.publish_verified_claim_fence(stream_id, &fence));
            fences.insert(stream_id.to_string(), fence.clone());
        }
        hydrations.retain(|(id, old, _), _| {
            if id == stream_id && old != &fence {
                superseded.push(old.clone());
                false
            } else {
                true
            }
        });
        drop(hydrations);
        drop(fences);
        drop(pending);
        drop(reclaimed);
        drop(reservations);
        drop(promotions);
        if let Some(storage) = &self.persistence {
            let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
            superseded.sort_by(|left, right| {
                left.owner()
                    .node_id
                    .cmp(&right.owner().node_id)
                    .then_with(|| left.owner().node_epoch.cmp(&right.owner().node_epoch))
                    .then_with(|| left.epoch().cmp(&right.epoch()))
            });
            superseded.dedup();
            for old in superseded {
                storage.evict_claim_cache(&session_id, &old);
            }
        }
        true
    }

    /// Convert a reserved acquisition slot into terminal exact-release
    /// responsibility without publishing the supplied fence as authority for
    /// live-session persistence. This is used when a claim belongs to an old
    /// node incarnation while a newer, claimless lifecycle occupies the same
    /// stream id.
    pub(super) fn try_record_terminal_claim_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        self.try_record_terminal_claim_fence_with_reservation_policy(stream_id, fence, false)
    }

    /// Retire a stale detach fence without consuming a rejected-enable
    /// ambiguity marker borrowed by that detach. The marker represents a
    /// different acquisition whose CAS outcome is still unknown, so the
    /// exact old-fence release and the generic acquisition reservation must
    /// remain independently capacity-counted after an authority rejection.
    pub(super) fn try_record_terminal_claim_fence_for_detach(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
        reservation: DetachClaimFenceReservation,
    ) -> bool {
        self.try_record_terminal_claim_fence_with_reservation_policy(
            stream_id,
            fence,
            reservation.preserves_rejected_enable(),
        )
    }

    pub(super) fn try_record_terminal_claim_fence_preserving_reservation(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        self.try_record_terminal_claim_fence_with_reservation_policy(stream_id, fence, true)
    }

    /// Atomically convert an active claim or its acquisition reservation into
    /// an exact durable-recovery handoff. Callers that still need a distinct
    /// same-stream acquisition reservation must use the preserving variant
    /// explicitly.
    pub(super) fn try_record_durable_claim_handoff(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        self.try_record_terminal_claim_fence_with_reservation_policy(stream_id, fence, false)
    }

    fn try_record_terminal_claim_fence_with_reservation_policy(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
        preserve_reservation: bool,
    ) -> bool {
        let (Ok(promotions), Ok(mut reservations), Ok(reclaimed), Ok(mut pending), Ok(mut fences)) = (
            self.pending_promotions.read(),
            self.claim_fence_reservations.write(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
        ) else {
            return false;
        };
        if pending.contains_key(&(stream_id.to_string(), fence.clone())) {
            if !preserve_reservation {
                reservations.remove(stream_id);
            }
            if fences.get(stream_id) == Some(&fence) {
                fences.remove(stream_id);
            }
            return true;
        }
        let represented_by_reservation = if preserve_reservation {
            reservations.contains(stream_id)
        } else {
            reservations.remove(stream_id)
        };
        let converts_active = fences.get(stream_id) == Some(&fence);
        let converts_retained = promotions.retains_claim_fence(stream_id, &fence);
        let occupied = occupied_claim_fence_capacity(
            &promotions,
            &reservations,
            &reclaimed,
            &pending,
            &fences,
        );
        if !represented_by_reservation
            && !converts_active
            && !converts_retained
            && occupied >= self.max_sessions
        {
            return false;
        }
        if converts_active {
            fences.remove(stream_id);
        }
        pending
            .entry((stream_id.to_string(), fence))
            .or_insert(PendingClaimReleaseDisposition::RetainedForDurableRecovery);
        true
    }

    /// Publish a later verified acquisition as the active fence while
    /// directionally retiring every older same-stream generation. Used when
    /// an in-flight displacement still owns the promote/confirm lifecycle.
    pub(super) fn try_record_verified_claim_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        self.try_record_verified_acquisition_fence(stream_id, fence)
    }

    fn try_record_verified_acquisition_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        let mut superseded = Vec::new();
        {
            let (
                Ok(mut promotions),
                Ok(mut reservations),
                Ok(reclaimed),
                Ok(mut pending),
                Ok(mut fences),
                Ok(mut hydrations),
            ) = (
                self.pending_promotions.write(),
                self.claim_fence_reservations.write(),
                self.reclaimed_claim_reservations.read(),
                self.pending_claim_releases.write(),
                self.claim_fences.write(),
                self.pending_reclaimed_hydrations.write(),
            )
            else {
                return false;
            };
            let key = (stream_id.to_string(), fence.clone());
            if pending.get(&key) == Some(&PendingClaimReleaseDisposition::ReleaseMayComplete) {
                return false;
            }
            if reclaimed.contains_key(stream_id) || !reservations.remove(stream_id) {
                return false;
            }
            if let Some(old) = fences.remove(stream_id) {
                if old != fence {
                    superseded.push(old);
                }
            }
            pending.retain(|(id, old), _| {
                if id == stream_id {
                    if old != &fence {
                        superseded.push(old.clone());
                    }
                    false
                } else {
                    true
                }
            });
            superseded.extend(promotions.publish_verified_claim_fence(stream_id, &fence));
            hydrations.retain(|(id, old, _), _| {
                if id == stream_id && old != &fence {
                    superseded.push(old.clone());
                    false
                } else {
                    true
                }
            });
            fences.insert(stream_id.to_string(), fence.clone());
        }
        if let Some(storage) = &self.persistence {
            let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
            superseded.sort_by(|left, right| {
                left.owner()
                    .node_id
                    .cmp(&right.owner().node_id)
                    .then_with(|| left.owner().node_epoch.cmp(&right.owner().node_epoch))
                    .then_with(|| left.epoch().cmp(&right.epoch()))
            });
            superseded.dedup();
            for old in superseded {
                storage.evict_claim_cache(&session_id, &old);
            }
        }
        true
    }

    fn try_record_terminal_reclaimed_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> bool {
        let (Ok(_reservations), Ok(mut reclaimed), Ok(mut pending), Ok(mut fences)) = (
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.write(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
        ) else {
            return false;
        };
        if reclaimed.get(stream_id) != Some(&reservation) {
            return false;
        }
        reclaimed.remove(stream_id);
        if fences.get(stream_id) == Some(&fence) {
            fences.remove(stream_id);
        }
        pending
            .entry((stream_id.to_string(), fence))
            .or_insert(PendingClaimReleaseDisposition::RetainedForDurableRecovery);
        true
    }

    /// Retain exact cleanup while local liveness cannot be read. Unlike a
    /// terminal conversion, this keeps a matching active fence in place: a
    /// poisoned session-map lock is not evidence that the lifecycle ended.
    /// Adding a fence already represented by the active map consumes no new
    /// capacity; an externally reclaimed fence must fit the normal bound.
    pub(super) fn try_record_uncertain_release_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        let (Ok(promotions), Ok(reservations), Ok(reclaimed), Ok(mut pending), Ok(fences)) = (
            self.pending_promotions.read(),
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.write(),
            self.claim_fences.read(),
        ) else {
            return false;
        };
        let key = (stream_id.to_string(), fence.clone());
        if pending.contains_key(&key) {
            return true;
        }
        let represented_exact = fences.get(stream_id) == Some(&fence)
            || promotions.retains_claim_fence(stream_id, &fence);
        let represented_other = fences.contains_key(stream_id)
            || pending.keys().any(|(id, _)| id == stream_id)
            || reservations.contains(stream_id)
            || reclaimed.contains_key(stream_id)
            || promotions
                .retained_claim_fences()
                .any(|(id, _)| id == stream_id);
        if represented_other && !represented_exact {
            // Direction cannot be inferred from numeric epochs across node
            // incarnations. Only the verified-hydration path may replace a
            // same-stream generation.
            return false;
        }
        let occupied = occupied_claim_fence_capacity(
            &promotions,
            &reservations,
            &reclaimed,
            &pending,
            &fences,
        );
        if !represented_exact && occupied >= self.max_sessions {
            return false;
        }
        pending
            .entry(key)
            .or_insert(PendingClaimReleaseDisposition::RetainedForDurableRecovery);
        true
    }

    /// Publish a reclaimed fence only after `ensure_claimed` proved that it
    /// is the backend's current owner+epoch. That proof makes replacement of
    /// every older same-stream generation directional and lets the new fence
    /// consume an existing reservation without growing bounded inventory.
    pub(super) fn try_record_verified_reclaimed_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> bool {
        let mut superseded = Vec::new();
        let recorded = {
            let (
                Ok(mut promotions),
                Ok(reservations),
                Ok(mut reclaimed),
                Ok(mut pending),
                Ok(mut fences),
                Ok(mut hydrations),
            ) = (
                self.pending_promotions.write(),
                self.claim_fence_reservations.read(),
                self.reclaimed_claim_reservations.write(),
                self.pending_claim_releases.write(),
                self.claim_fences.write(),
                self.pending_reclaimed_hydrations.write(),
            )
            else {
                return false;
            };
            let key = (stream_id.to_string(), fence.clone());
            if pending.get(&key) == Some(&PendingClaimReleaseDisposition::ReleaseMayComplete) {
                return false;
            }
            let reserved = reclaimed.get(stream_id) == Some(&reservation);
            if reserved {
                reclaimed.remove(stream_id);
            }
            let represented = reserved
                || fences.contains_key(stream_id)
                || pending.keys().any(|(id, _)| id == stream_id);
            let occupied = occupied_claim_fence_capacity(
                &promotions,
                &reservations,
                &reclaimed,
                &pending,
                &fences,
            );
            if !represented && occupied >= self.max_sessions {
                return false;
            }
            if let Some(old) = fences.remove(stream_id) {
                if old != fence {
                    superseded.push(old);
                }
            }
            pending.retain(|(id, old), _| {
                if id == stream_id {
                    if old != &fence {
                        superseded.push(old.clone());
                    }
                    false
                } else {
                    true
                }
            });
            superseded.extend(promotions.publish_verified_claim_fence(stream_id, &fence));
            hydrations.retain(|(id, old, _), _| {
                if id == stream_id && old != &fence {
                    superseded.push(old.clone());
                    false
                } else {
                    true
                }
            });
            fences.insert(stream_id.to_string(), fence.clone());
            true
        };
        if recorded {
            if let Ok(mut acquisitions) = self.pending_claim_acquisitions.write() {
                acquisitions.retain(|(id, _, _)| id != stream_id);
            }
            if let Some(storage) = &self.persistence {
                let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
                superseded.sort_by(|left, right| {
                    left.owner()
                        .node_id
                        .cmp(&right.owner().node_id)
                        .then_with(|| left.owner().node_epoch.cmp(&right.owner().node_epoch))
                        .then_with(|| left.epoch().cmp(&right.epoch()))
                });
                superseded.dedup();
                for old in superseded {
                    storage.evict_claim_cache(&session_id, &old);
                }
            }
        }
        recorded
    }

    /// Create a new in-memory registry with default settings.
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            claimed_sessions: RwLock::new(HashMap::new()),
            stream_locks: new_stream_locks(),
            max_sessions: DEFAULT_MAX_SESSIONS,
            recent_tombstones: RwLock::new(Vec::new()),
            persistence: None,
            claim_store: Arc::new(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            claim_fences: RwLock::new(HashMap::new()),
            pending_claim_releases: RwLock::new(HashMap::new()),
            pending_claim_acquisitions: RwLock::new(HashSet::new()),
            pending_promotions: Arc::new(RwLock::new(PendingPromotions::default())),
            pending_promotion_retries: RwLock::new(PendingPromotionRetries::default()),
            pending_epoch_failure_reconciliations: RwLock::new(HashSet::new()),
            pending_reclaimed_hydrations: RwLock::new(HashMap::new()),
            pending_reclaimed_claim_lookups: RwLock::new(HashMap::new()),
            claim_fence_reservations: RwLock::new(HashSet::new()),
            reclaimed_claim_reservations: RwLock::new(HashMap::new()),
            next_reclaimed_claim_reservation: AtomicU64::new(1),
            remote_resume: None,
        }
    }

    /// Create a registry with custom settings.
    pub fn with_capacity(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::with_capacity(max_sessions.min(10000))),
            claimed_sessions: RwLock::new(HashMap::new()),
            stream_locks: new_stream_locks(),
            max_sessions,
            recent_tombstones: RwLock::new(Vec::new()),
            persistence: None,
            claim_store: Arc::new(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            claim_fences: RwLock::new(HashMap::new()),
            pending_claim_releases: RwLock::new(HashMap::new()),
            pending_claim_acquisitions: RwLock::new(HashSet::new()),
            pending_promotions: Arc::new(RwLock::new(PendingPromotions::default())),
            pending_promotion_retries: RwLock::new(PendingPromotionRetries::default()),
            pending_epoch_failure_reconciliations: RwLock::new(HashSet::new()),
            pending_reclaimed_hydrations: RwLock::new(HashMap::new()),
            pending_reclaimed_claim_lookups: RwLock::new(HashMap::new()),
            claim_fence_reservations: RwLock::new(HashSet::new()),
            reclaimed_claim_reservations: RwLock::new(HashMap::new()),
            next_reclaimed_claim_reservation: AtomicU64::new(1),
            remote_resume: None,
        }
    }

    /// Attach a durable backing store. Must be called once at
    /// construction time before the registry is wrapped in `Arc`.
    /// Subsequent mutating writes are mirrored into `storage`; reads
    /// stay in-memory for hot-path latency.
    pub fn with_persistence(
        mut self,
        storage: std::sync::Arc<dyn super::super::persistence::SmPersistenceStorage>,
    ) -> Self {
        self.persistence = Some(storage);
        self
    }

    /// Inject a `ClaimStore`/live-identity pair other than the single-node
    /// [`InProcessClaimStore`] default (ADR-0017 Phase 3, Q2). Must be
    /// called once at construction time before the registry is wrapped in
    /// `Arc`. ADR-0017 Phase 3 Slice 5 wires this in production
    /// (`server/http.rs::create_sm_session_registry`) with
    /// `ClusteringHandles::claim_pair()`'s pair — the *same* `SharedNodeIdentity`
    /// `self_fence::run_node_lease` updates on every re-registration, not a
    /// one-time snapshot, so this registry's claim calls always bind
    /// whatever identity is currently in force.
    pub fn with_claim_store(
        mut self,
        claim_store: Arc<dyn ClaimStore>,
        me: SharedNodeIdentity,
    ) -> Self {
        self.claim_store = claim_store;
        self.node_identity = me;
        self
    }

    /// Inject the cross-node "ask the live owner to detach" bridge
    /// (ADR-0017 Phase 3 Slice 6). Must be called once at construction time
    /// before the registry is wrapped in `Arc`, exactly like
    /// [`Self::with_claim_store`]. Production wiring
    /// (`server/http.rs::create_sm_session_registry`) sets this alongside
    /// the claim store whenever clustering is enabled; single-node builds
    /// leave it `None`, so `cross_node_resume::attempt_cross_node_resume`'s
    /// live-handshake branch never has anything to ask (byte-identical
    /// single-node behavior).
    pub fn with_remote_resume_asker(
        mut self,
        asker: Arc<dyn super::cross_node_resume::RemoteResumeAsker>,
    ) -> Self {
        self.remote_resume = Some(asker);
        self
    }

    /// Rebuild the in-memory view from the attached durable store.
    /// Called on server startup before any traffic is accepted, so
    /// an XEP-0198 `<resume previd='…'/>` for a session that
    /// detached before restart still succeeds.
    ///
    /// **Startup-time operation only (FIX 2, council-adjudicated ADR-0017
    /// Phase 3 Slice 5 corrigenda)**: this method's unfenced, unscoped
    /// `list_all_sessions_with_unacked` table scan is safe only because
    /// nothing else can plausibly be racing it for a stream id it has not
    /// yet reached — this runs once, before any traffic is accepted. It
    /// MUST NOT be re-run against a live, already-serving registry (the
    /// orphan reaper previously re-ran it after every successful steal,
    /// which re-scans every row this node already holds on every sweep and
    /// — worse — can observe a row a live session concurrently
    /// completes/re-claims mid-scan). [`Self::hydrate_reclaimed`] is the
    /// live-safe alternative for exactly that case: given the specific
    /// entities a caller just proved ownership of (via `steal_stale` or an
    /// equivalent CAS), it hydrates only those, under each one's own
    /// stream-shard lock, with a fresh in-memory absence re-check — never a
    /// table scan, never a blind insert.
    ///
    /// **ADR-0017 Phase 3 Slice 5 — acquire-then-hydrate** (element 9,
    /// quoted verbatim: *"hydrates only sessions whose claim this node
    /// holds or can acquire at startup ... it never performs unscoped
    /// full-table hydration"*): the read below (`list_all_sessions_with_unacked`)
    /// is still a full, unfenced table scan — it has to be, there is no
    /// other way to discover which stream ids exist — but every row is now
    /// gated on a per-entity [`ClaimStore::ensure_claimed`] call before it
    /// is allowed into `self.sessions`. A row this node successfully claims
    /// (a fresh claim on a single-node/first-ever-restore deployment, or a
    /// self-reacquire of this exact node's own pre-restart claim once
    /// `ensure_claimed`'s self-match fires under the *same* `node_id` — see
    /// that method's doc comment) is hydrated; a row genuinely claimed by
    /// a different, still-live node is skipped — that node already has it
    /// in memory (or will, on its own restore pass), and this node MUST NOT
    /// also hydrate a copy (the exact double-ownership hazard this slice
    /// closes). A row whose owner has died is left unclaimed here (a
    /// concurrent restore/steal never matches this node's identity, so it
    /// stays `AlreadyClaimed` against the dead owner until that owner's
    /// `clustering_nodes` row is provably stale) — the **orphan reaper**
    /// (`server::session_janitors::spawn_orphan_reaper_janitor`) is the
    /// mechanism that reclaims those, not this startup pass, since a
    /// dead-owner determination requires the owner-stale predicate this
    /// unfenced per-row read does not evaluate.
    ///
    /// **Restart-time expired-row deletion (element 9/element 4)**: this
    /// slice does *not* add an unscoped delete-on-restore step. Code
    /// research for this slice found no existing unscoped delete to
    /// claim-scope here — issue #1098 deliberately *hydrates* expired
    /// sessions rather than deleting them at restore time, specifically so
    /// their unacked queues still run the Q6 promote → confirm chain
    /// instead of being silently discarded. Deleting a claimed session
    /// eagerly here, before that chain runs, would re-introduce exactly
    /// the data-loss bug #1098 fixed. Once a row is hydrated under this
    /// node's claim, the (now itself claim-scoped, see
    /// `server::session_janitors::spawn_sm_expiry_janitor`) SM-expiry
    /// janitor's `drain_expired`/promote/`confirm_drained` chain is the
    /// sole deletion path, and its writes already run under the row-locked
    /// fenced epoch via `PostgresFencedSmPersistence`. Recorded as
    /// deviation 28 (plan doc; corrected from an earlier "deviation 27"
    /// citation — see the plan's Slice 5 "Design addition (major fix 6)"
    /// paragraph, amended in place to point at 28) — the plan's
    /// major-fix-6 premise of an existing unscoped restore-time delete
    /// does not match this codebase's actual state.
    ///
    /// **Per-row stream-shard-lock discipline (FIX 2)**: each row's
    /// eventual in-memory insert takes that row's own stream-shard lock —
    /// the same lock every other registry mutator (`store_session`,
    /// `take_session`, `claim_session`, …) takes before touching
    /// `sessions`/`claimed_sessions` — and re-checks the stream id is
    /// absent from BOTH maps immediately before inserting. This is cheap
    /// safety for this method's startup-time role (see above): at true
    /// cold start nothing else can have raced ahead, but the same
    /// discipline the live-only [`Self::hydrate_reclaimed`] needs is applied
    /// here too rather than special-cased away, so a row this node's own
    /// Slice-4 lazy first-fenced-write path (or a live detach) already
    /// raced into memory ahead of this scan reaching the same row is
    /// skipped rather than overwritten with a stale durable read.
    ///
    /// Returns the total number of newly hydrated durable generations:
    /// resumable heads plus non-resumable terminal generations. Terminal
    /// rows enter only the pending promotion/retry inventory, so
    /// [`Self::session_count`] remains a resumable-head count. No-op when no
    /// persistence is attached.
    pub async fn restore_from_persistence(&self) -> Result<usize, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(0);
        };
        let now = chrono::Utc::now();
        // The unfenced global read is discovery-only. Reading typed stream
        // ids directly keeps a corrupt current row discoverable; the old
        // joined snapshot API could best-effort omit the entire group when
        // its queue was poison. Every value and queue is read exactly only
        // after the per-stream claim below.
        let current_stream_ids = storage
            .list_session_ids()
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
        // Terminal generations are a separate, generation-keyed outbox.
        // Startup is the only lifecycle path allowed to scan it globally;
        // reclaimed live traffic uses the targeted per-stream query below.
        let terminal_generations = storage
            .list_terminal_generations()
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
        let mut hydrated = 0usize;
        let mut terminal_hydrated = 0usize;
        let mut expired = 0usize;
        let mut foreign_claims = 0usize;
        let mut already_present = 0usize;
        // Both whole-table scans are discovery hints only. Their values were
        // read before ownership was established, so hydrating either snapshot
        // after `ensure_claimed` would resurrect work a prior owner deleted in
        // the scan-to-claim window. Group the hinted stream ids, claim each
        // shared entity once, then re-read its current row and authoritative
        // terminal inventory under that fence and its stream shard.
        let mut hinted_streams =
            Vec::with_capacity(current_stream_ids.len() + terminal_generations.len());
        let mut seen_streams = HashSet::new();
        for current_stream_id in current_stream_ids {
            let stream_id = current_stream_id.as_str().to_string();
            if seen_streams.insert(stream_id.clone()) {
                hinted_streams.push(stream_id);
            }
        }
        for terminal in terminal_generations {
            let stream_id = terminal.stream_id().as_str().to_string();
            if seen_streams.insert(stream_id.clone()) {
                hinted_streams.push(stream_id);
            }
        }

        for stream_id in hinted_streams {
            // Read once per stream, immediately before `ensure_claimed`.
            // Every current head and terminal generation for this stream
            // shares the resulting immutable owner+epoch fence.
            let identity = self.node_identity.current();
            let entity = Entity::new(EntityType::SmSession, stream_id.clone());
            let epoch = match self.claim_store.ensure_claimed(&entity, &identity).await {
                Ok(epoch) => epoch,
                Err(
                    crate::ownership::ClaimError::AlreadyClaimed
                    | crate::ownership::ClaimError::Conflict,
                ) => {
                    foreign_claims += 1;
                    continue;
                }
                Err(error @ crate::ownership::ClaimError::Draining) => {
                    debug!(
                        stream_id,
                        %error,
                        "restore_from_persistence: node definitively refused a new claim; \
                         skipping every generation for this stream in this pass"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::error!(
                        stream_id,
                        %error,
                        "restore_from_persistence: ClaimStore ensure_claimed outcome may be \
                         ambiguous; aborting startup before serving with untracked ownership"
                    );
                    return Err(SmRegistryError::Internal(format!(
                        "restore_from_persistence: claim acquisition for {stream_id} was not \
                         proven unsuccessful: {error}"
                    )));
                }
            };
            let fence = super::super::persistence::SmClaimFence::new(identity, epoch);
            self.claim_fences
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .insert(stream_id.clone(), fence.clone());
            if self.node_identity.current() != *fence.owner() {
                return Err(SmRegistryError::Internal(format!(
                    "restore_from_persistence: node identity changed after claiming stream \
                     {stream_id}; exact fence retained"
                )));
            }

            let stream_lock = self.stream_lock(&stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            let typed_stream_id = crate::pending_delivery::SmSessionId::new(stream_id.clone());

            // The global current-row value is stale by construction. Re-read
            // both the head and its queue only after ownership, and stage the
            // decoded session until every exact terminal key has also been
            // validated. A group-level failure therefore cannot publish a
            // healthy prefix while a later sibling remains invisible.
            let exact_current = match storage.get_session(&typed_stream_id).await {
                Ok(Some(persisted)) if persisted.stream_id != typed_stream_id => {
                    return Err(SmRegistryError::Internal(format!(
                        "restore_from_persistence: exact current read for {typed_stream_id} \
                         returned foreign stream {}; exact fence retained",
                        persisted.stream_id
                    )));
                }
                Ok(Some(persisted)) => match storage.list_unacked(&typed_stream_id).await {
                    Ok(unacked) => Some((persisted, unacked)),
                    Err(super::super::persistence::SmPersistenceError::Corrupt {
                        stream_id: corrupt_stream_id,
                        detail,
                    }) => {
                        if corrupt_stream_id != typed_stream_id {
                            return Err(SmRegistryError::Internal(format!(
                                "restore_from_persistence: exact current queue read for \
                                 {typed_stream_id} reported foreign corrupt stream \
                                 {corrupt_stream_id}; exact fence retained"
                            )));
                        }
                        if let Err(error) =
                            storage.quarantine_session(&typed_stream_id, &fence).await
                        {
                            tracing::error!(
                                stream_id,
                                %detail,
                                %error,
                                "restore_from_persistence: corrupt current queue quarantine \
                                 failed; retaining the shared fence and aborting startup recovery"
                            );
                            return Err(SmRegistryError::Persistence(error));
                        }
                        debug!(
                            stream_id,
                            %detail,
                            "restore_from_persistence: quarantined corrupt current queue"
                        );
                        None
                    }
                    Err(error) => {
                        tracing::error!(
                            stream_id,
                            %error,
                            "restore_from_persistence: claimed current queue read failed; \
                             retaining the shared fence and aborting startup recovery"
                        );
                        return Err(SmRegistryError::Persistence(error));
                    }
                },
                Ok(None) => None,
                Err(super::super::persistence::SmPersistenceError::Corrupt {
                    stream_id: corrupt_stream_id,
                    detail,
                }) => {
                    if corrupt_stream_id != typed_stream_id {
                        return Err(SmRegistryError::Internal(format!(
                            "restore_from_persistence: exact current read for {typed_stream_id} \
                             reported foreign corrupt stream {corrupt_stream_id}; exact fence \
                             retained"
                        )));
                    }
                    if let Err(error) = storage.quarantine_session(&typed_stream_id, &fence).await {
                        tracing::error!(
                            stream_id,
                            %detail,
                            %error,
                            "restore_from_persistence: corrupt current generation quarantine \
                             failed; retaining the shared fence and aborting startup recovery"
                        );
                        return Err(SmRegistryError::Persistence(error));
                    }
                    debug!(
                        stream_id,
                        %detail,
                        "restore_from_persistence: quarantined corrupt current generation"
                    );
                    None
                }
                Err(error) => {
                    tracing::error!(
                        stream_id,
                        %error,
                        "restore_from_persistence: claimed current generation read failed; \
                         retaining the shared fence and aborting startup recovery"
                    );
                    return Err(SmRegistryError::Persistence(error));
                }
            };
            let staged_current = match exact_current {
                Some((persisted, unacked)) => {
                    let expires_at = persisted.detached_at
                        + chrono::Duration::from_std(persisted.max_resume_duration)
                            .unwrap_or(chrono::Duration::seconds(0));
                    if expires_at <= now {
                        expired += 1;
                    }
                    match persisted_to_detached(&persisted, &unacked) {
                        Ok(session) => Some(session),
                        Err(error) => {
                            tracing::error!(
                                stream_id,
                                %error,
                                "restore_from_persistence: claimed current generation decode \
                                 failed; retaining the shared fence and aborting startup \
                                 recovery"
                            );
                            return Err(SmRegistryError::Internal(format!(
                                "restore_from_persistence: current stream {stream_id} decode \
                                     failed after claiming it: {error}"
                            )));
                        }
                    }
                }
                None => None,
            };

            // Re-scan this one stream after the claim. This captures siblings
            // inserted between the global discovery scan and ownership. The
            // targeted values are still treated as keys only: exact reads
            // distinguish a current healthy generation, an already-deleted
            // generation, and a structurally identifiable poison row.
            let terminal_entries = match storage
                .list_terminal_generations_for_stream(&typed_stream_id)
                .await
            {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::error!(
                        stream_id,
                        %error,
                        "restore_from_persistence: claimed terminal inventory read failed; \
                         retaining the shared fence and aborting startup recovery"
                    );
                    return Err(SmRegistryError::Persistence(error));
                }
            };
            let mut seen_terminal_keys = HashSet::new();
            let mut staged_terminals = Vec::with_capacity(terminal_entries.len());
            for terminal_entry in terminal_entries {
                let terminal_key = terminal_entry.key().clone();
                if terminal_key.stream_id() != &typed_stream_id {
                    return Err(SmRegistryError::Internal(format!(
                        "restore_from_persistence: targeted terminal scan for {stream_id} \
                         returned foreign key {terminal_key}; exact fence retained"
                    )));
                }
                if !seen_terminal_keys.insert(terminal_key.clone()) {
                    continue;
                }
                let terminal = match storage.get_terminal_generation(&terminal_key).await {
                    Ok(Some(terminal)) => terminal,
                    Ok(None) => continue,
                    Err(super::super::persistence::SmPersistenceError::CorruptTerminal {
                        key,
                        detail,
                    }) => {
                        if key != terminal_key {
                            return Err(SmRegistryError::Internal(format!(
                                "restore_from_persistence: exact terminal read for \
                                 {terminal_key} reported foreign corrupt key {key}; exact fence \
                                 retained"
                            )));
                        }
                        if let Err(error) = storage
                            .quarantine_terminal_generation(&terminal_key, &fence)
                            .await
                        {
                            tracing::error!(
                                terminal_generation = %terminal_key,
                                %detail,
                                %error,
                                "restore_from_persistence: exact terminal poison quarantine \
                                 failed; retaining the shared fence and aborting startup recovery"
                            );
                            return Err(SmRegistryError::Persistence(error));
                        }
                        debug!(
                            terminal_generation = %terminal_key,
                            %detail,
                            "restore_from_persistence: quarantined one corrupt exact terminal \
                             generation"
                        );
                        continue;
                    }
                    Err(error) => {
                        tracing::error!(
                            terminal_generation = %terminal_key,
                            %error,
                            "restore_from_persistence: exact terminal generation read failed; \
                             retaining the shared fence and aborting startup recovery"
                        );
                        return Err(SmRegistryError::Persistence(error));
                    }
                };
                if terminal.key() != &terminal_key {
                    return Err(SmRegistryError::Internal(format!(
                        "restore_from_persistence: exact terminal read for {terminal_key} \
                         returned {}; exact fence retained",
                        terminal.key()
                    )));
                }
                let session = match persisted_terminal_to_detached(&terminal) {
                    Ok(session) => session,
                    Err(error) => {
                        tracing::error!(
                            stream_id,
                            generation_id = %terminal_key.generation_id(),
                            %error,
                            "restore_from_persistence: claimed terminal generation decode \
                             failed; retaining the shared fence and aborting startup recovery"
                        );
                        return Err(SmRegistryError::Internal(format!(
                            "restore_from_persistence: terminal generation {terminal_key} \
                             decode failed after claiming its stream: {error}"
                        )));
                    }
                };
                staged_terminals.push((session, terminal.promotion_attempts()));
            }

            match tokio::time::timeout(
                CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                self.claim_store
                    .fence(&entity, fence.owner(), fence.epoch()),
            )
            .await
            {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => {
                    if let (Ok(mut promotions), Ok(mut retries)) = (
                        self.pending_promotions.write(),
                        self.pending_promotion_retries.write(),
                    ) {
                        for generation_id in
                            promotions.demote_for_external_claim_loss(&stream_id, Some(&fence))
                        {
                            retries.remove_generation(&stream_id, generation_id);
                        }
                    }
                    self.forget_claim_locally_locked(&stream_id, None);
                    foreign_claims += 1;
                    continue;
                }
                Ok(Err(error)) => {
                    return Err(SmRegistryError::Internal(format!(
                        "restore_from_persistence: final exact fence check for {stream_id} \
                         failed; exact cleanup retained: {error}"
                    )));
                }
                Err(_) => {
                    return Err(SmRegistryError::Internal(format!(
                        "restore_from_persistence: final exact fence check for {stream_id} \
                         timed out after {CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT:?}; exact cleanup \
                         retained"
                    )));
                }
            }
            let Some(publication_guard) = self.node_identity.guard_if_current(fence.owner()).await
            else {
                return Err(SmRegistryError::Internal(format!(
                    "restore_from_persistence: node identity changed between its final fence \
                     check and publication for stream {stream_id}; exact cleanup retained"
                )));
            };

            if let Some(session) = staged_current {
                let present = {
                    let sessions = self
                        .sessions
                        .read()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                    let claimed = self
                        .claimed_sessions
                        .read()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                    sessions.contains_key(&stream_id) || claimed.contains_key(&stream_id)
                };
                if present {
                    already_present += 1;
                } else {
                    self.sessions
                        .write()
                        .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                        .insert(stream_id.clone(), session);
                    hydrated += 1;
                }
            }
            for (session, promotion_attempts) in staged_terminals {
                if self.retain_terminal_durable_promotion(
                    session,
                    promotion_attempts,
                    fence.clone(),
                )? {
                    terminal_hydrated += 1;
                    hydrated += 1;
                } else {
                    already_present += 1;
                }
            }
            drop(publication_guard);

            // A corrupt-only or stale discovery group may leave no work at
            // all. Release at most once, after every authoritative key was
            // revalidated and only when both local state and a final durable
            // probe prove the shared entity empty. Probe failures retain the
            // exact fence for a later recovery pass.
            if !self.any_durable_work_may_remain(&stream_id).await {
                self.release_claim_store_entry_under(&stream_id, fence)
                    .await;
            }
        }
        debug!(
            hydrated,
            terminal_hydrated,
            expired,
            foreign_claims,
            already_present,
            "restored resumable heads and terminal SM generations from persistence"
        );
        Ok(hydrated)
    }

    /// Targeted hydration for freshly-reclaimed SM-session claims (FIX 2,
    /// council-adjudicated ADR-0017 Phase 3 Slice 5 corrigenda) — the
    /// live-safe counterpart to [`Self::restore_from_persistence`] (a
    /// startup-time-only, whole-table operation; see its doc comment).
    /// Callers: the orphan reaper janitor, after a successful
    /// `steal_stale(OwnerStale)` for one or more entities
    /// (`server::session_janitors::run_orphan_reaper_sweep`), and the
    /// inline post-fence reclaim in `self_fence::run_node_lease` (FIX 4),
    /// after this node's own just-superseded identity's claims are stolen
    /// back under the freshly re-registered identity. Neither caller may
    /// re-run `restore_from_persistence` — the server is already serving
    /// live traffic, and an unscoped table scan racing a live session that
    /// completes/re-claims mid-scan is exactly the **live restore hazard**
    /// this method exists to close.
    ///
    /// Per entity, under that entity's own stream-shard lock (never a
    /// table scan, never a blind insert):
    /// 1. Entities whose type is not `SmSession` are skipped (logged) —
    ///    this registry only ever hydrates SM-session claims.
    /// 2. Re-checks the stream id is absent from BOTH `sessions` and
    ///    `claimed_sessions` — if either already holds it (a live session
    ///    completed, another concurrent hydration already landed it, or
    ///    this entity was reclaimed more than once across overlapping
    ///    sweeps), skip: never overwrite a live in-memory copy with a
    ///    stale durable read.
    /// 3. Re-confirms this node still holds the claim via a bounded
    ///    `ClaimStore::ensure_claimed` self-reacquire (FIX 5 — bounded
    ///    because this call runs under the stream-shard lock; see
    ///    [`CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT`]'s doc comment for the
    ///    shard-fan-in rationale) — a defensive re-check rather than
    ///    trusting the caller-supplied epoch blindly, since the caller's
    ///    `steal_stale` may have committed some time before this call
    ///    actually reaches this entity's turn in a batch.
    /// 4. Loads the durable row (`get_session` + `list_unacked`); a
    ///    missing row (already promoted/deleted by a concurrent sweep) is
    ///    a no-op, not an error.
    /// 5. Inserts into `sessions`, recording the epoch `ensure_claimed`
    ///    confirmed in step 3.
    ///
    /// Returns the number of entities actually hydrated — entities skipped
    /// by steps 1-4 are not counted and do not produce an `Err`, mirroring
    /// `restore_from_persistence`'s best-effort, skip-and-continue
    /// semantics for individual rows.
    pub async fn hydrate_reclaimed_typed(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ReclaimedHydrationOutcome, SmRegistryError> {
        if entity.entity_type != EntityType::SmSession {
            return Ok(ReclaimedHydrationOutcome::LostClaim);
        }
        if !self.try_record_pending_reclaimed_hydration(entity, caller_fence, reservation)? {
            self.clear_pending_reclaimed_hydration(entity, caller_fence, reservation);
            return Ok(ReclaimedHydrationOutcome::LostClaim);
        }
        let stream_lock = self.stream_lock(&entity.id)?;
        let _stream_guard = stream_lock.lock().await;
        self.hydrate_reclaimed_typed_locked(entity, caller_fence, reservation)
            .await
    }

    fn try_record_pending_reclaimed_hydration(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<bool, SmRegistryError> {
        let (reservations, releases, fences, mut hydrations) = (
            self.reclaimed_claim_reservations
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.pending_claim_releases
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.claim_fences
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.pending_reclaimed_hydrations
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
        );
        let represented = reservations.get(&entity.id) == Some(&reservation)
            || fences.get(&entity.id) == Some(caller_fence)
            || releases.contains_key(&(entity.id.clone(), caller_fence.clone()));
        if represented {
            hydrations.insert(
                (entity.id.clone(), caller_fence.clone(), reservation),
                entity.clone(),
            );
        }
        Ok(represented)
    }

    async fn hydrate_reclaimed_typed_locked(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ReclaimedHydrationOutcome, SmRegistryError> {
        let pending = self
            .pending_reclaimed_hydrations
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .contains_key(&(entity.id.clone(), caller_fence.clone(), reservation));
        let represented = self
            .reclaimed_claim_reservations
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get(&entity.id)
            == Some(&reservation)
            || self
                .claim_fences
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .get(&entity.id)
                == Some(caller_fence)
            || self
                .pending_claim_releases
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .contains_key(&(entity.id.clone(), caller_fence.clone()));
        if !pending {
            self.clear_pending_reclaimed_hydration(entity, caller_fence, reservation);
            return Ok(ReclaimedHydrationOutcome::LostClaim);
        }
        if !represented {
            return Ok(
                if self.complete_lost_reclaimed_claim(entity, caller_fence, reservation) {
                    ReclaimedHydrationOutcome::LostClaim
                } else {
                    ReclaimedHydrationOutcome::TransientFailure
                },
            );
        }

        let outcome = self
            .hydrate_reclaimed_once(entity, caller_fence, reservation)
            .await?;
        if outcome == ReclaimedHydrationOutcome::StaleIdentity
            && !self.abandon_stale_reclaimed_hydration(entity, caller_fence, reservation)
        {
            // `StaleIdentity` is a completed-handoff contract for every
            // caller. If local bookkeeping could not be relinquished, keep
            // the work retryable instead of falsely reporting completion.
            return Ok(ReclaimedHydrationOutcome::TransientFailure);
        }
        if outcome == ReclaimedHydrationOutcome::LostClaim
            && !self.complete_lost_reclaimed_claim(entity, caller_fence, reservation)
        {
            // A lost backend fence is terminal only after its exact local
            // authority and retry inventory have been relinquished together.
            return Ok(ReclaimedHydrationOutcome::TransientFailure);
        }
        if matches!(
            outcome,
            ReclaimedHydrationOutcome::Hydrated | ReclaimedHydrationOutcome::AlreadyPresent
        ) {
            self.clear_pending_reclaimed_hydration(entity, caller_fence, reservation);
        }
        Ok(outcome)
    }

    fn clear_pending_reclaimed_hydration(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) {
        if let Ok(mut pending) = self.pending_reclaimed_hydrations.write() {
            pending.remove(&(entity.id.clone(), fence.clone(), reservation));
        }
    }

    /// Hand stale-incarnation hydration back to global orphan discovery.
    ///
    /// Identity rotation makes the local work item permanently unable to
    /// publish under `fence`. Remove only that exact hydration, its matching
    /// operation reservation, its matching active fence, and any promotion
    /// authority retained under that exact fence in one local critical
    /// section. Nonterminal promotion payloads remain as unowned work, while
    /// durably archived terminal retry carriers retire with their exact
    /// fence. The backend claim is deliberately untouched and no pending
    /// release is created: a fresh owner-scoped orphan pass must be able to
    /// observe and steal the durable old-incarnation claim.
    pub fn abandon_stale_reclaimed_hydration(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> bool {
        let key = (entity.id.clone(), fence.clone(), reservation);
        let (
            Ok(mut promotions),
            Ok(mut retries),
            Ok(mut reservations),
            Ok(mut fences),
            Ok(mut hydrations),
        ) = (
            self.pending_promotions.write(),
            self.pending_promotion_retries.write(),
            self.reclaimed_claim_reservations.write(),
            self.claim_fences.write(),
            self.pending_reclaimed_hydrations.write(),
        )
        else {
            return false;
        };
        if !hydrations.contains_key(&key) {
            return false;
        }
        hydrations.remove(&key);
        if reservations.get(&entity.id) == Some(&reservation) {
            reservations.remove(&entity.id);
        }
        if fences.get(&entity.id) == Some(fence) {
            fences.remove(&entity.id);
        }
        for generation_id in promotions.relinquish_exact_claim_fence(&entity.id, fence) {
            retries.remove_generation(&entity.id, generation_id);
        }
        drop(hydrations);
        drop(fences);
        drop(reservations);
        drop(retries);
        drop(promotions);
        if let Some(storage) = &self.persistence {
            let stream_id = crate::pending_delivery::SmSessionId::new(entity.id.clone());
            storage.evict_claim_cache(&stream_id, fence);
        }
        true
    }

    /// Complete a reclaimed hydration whose exact backend fence was lost.
    ///
    /// The caller holds the stream shard. Remove only bookkeeping tied to the
    /// exact `(stream, fence, reservation)` operation: its hydration work,
    /// matching reservation, active or pending exact fence, promotion
    /// authority, and persistence cache. The backend is deliberately
    /// untouched because the observed fence is no longer authoritative.
    pub fn complete_lost_reclaimed_claim(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> bool {
        let key = (entity.id.clone(), fence.clone(), reservation);
        let (
            Ok(mut promotions),
            Ok(mut retries),
            Ok(mut reservations),
            Ok(mut releases),
            Ok(mut fences),
            Ok(mut hydrations),
        ) = (
            self.pending_promotions.write(),
            self.pending_promotion_retries.write(),
            self.reclaimed_claim_reservations.write(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
            self.pending_reclaimed_hydrations.write(),
        )
        else {
            return false;
        };
        if !hydrations.contains_key(&key) {
            return false;
        }
        hydrations.remove(&key);
        if reservations.get(&entity.id) == Some(&reservation) {
            reservations.remove(&entity.id);
        }
        if fences.get(&entity.id) == Some(fence) {
            fences.remove(&entity.id);
        }
        releases.remove(&(entity.id.clone(), fence.clone()));
        for generation_id in promotions.relinquish_exact_claim_fence(&entity.id, fence) {
            retries.remove_generation(&entity.id, generation_id);
        }
        drop(hydrations);
        drop(fences);
        drop(releases);
        drop(reservations);
        drop(retries);
        drop(promotions);
        if let Some(storage) = &self.persistence {
            let stream_id = crate::pending_delivery::SmSessionId::new(entity.id.clone());
            storage.evict_claim_cache(&stream_id, fence);
        }
        true
    }

    async fn hydrate_reclaimed_once(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ReclaimedHydrationOutcome, SmRegistryError> {
        if entity.entity_type != EntityType::SmSession {
            return Ok(ReclaimedHydrationOutcome::LostClaim);
        }
        if self.node_identity.current() != *caller_fence.owner() {
            return Ok(ReclaimedHydrationOutcome::StaleIdentity);
        }
        let stream_id = entity.id.clone();
        let (current_present, any_present) = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let promotions = self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let current_present =
                sessions.contains_key(&stream_id) || claimed.contains_key(&stream_id);
            (
                current_present,
                current_present || promotions.contains(&stream_id),
            )
        };
        // The caller already won this exact owner+epoch before entering the
        // registry. Revalidate that immutable fence; never call a fresh-
        // acquiring primitive here. If the won claim disappeared,
        // `ensure_claimed` could mint a new epoch and leave it untracked when
        // the caller fence no longer matched (or when its response was lost).
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store
                .fence(entity, caller_fence.owner(), caller_fence.epoch()),
        )
        .await
        {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => return Ok(ReclaimedHydrationOutcome::LostClaim),
            Ok(Err(error)) => {
                debug!(
                    stream_id = %stream_id,
                    %error,
                    "hydrate_reclaimed: exact ClaimStore fence check failed"
                );
                return Ok(ReclaimedHydrationOutcome::TransientFailure);
            }
            Err(_) => return Ok(ReclaimedHydrationOutcome::TransientFailure),
        }
        let Some(initial_publication_guard) = self
            .node_identity
            .guard_if_current(caller_fence.owner())
            .await
        else {
            return Ok(ReclaimedHydrationOutcome::StaleIdentity);
        };
        if !self.try_record_verified_reclaimed_fence(&stream_id, caller_fence.clone(), reservation)
        {
            return Ok(ReclaimedHydrationOutcome::TransientFailure);
        }
        drop(initial_publication_guard);
        let Some(storage) = &self.persistence else {
            return Ok(if any_present {
                ReclaimedHydrationOutcome::AlreadyPresent
            } else {
                ReclaimedHydrationOutcome::MissingDurable
            });
        };

        let session_id = crate::pending_delivery::SmSessionId::new(stream_id.clone());
        // Targeted reclaim must never run the startup-global terminal scan.
        // One claimed stream can own a current resumable head, terminal-only
        // work, or both, so read the exact generation-keyed outbox even when
        // the bare current row is absent.
        let terminal_rows = match storage
            .list_terminal_generations_for_stream(&session_id)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                debug!(
                    stream_id = %stream_id,
                    %error,
                    "hydrate_reclaimed: targeted terminal-generation read failed"
                );
                return Ok(ReclaimedHydrationOutcome::TransientFailure);
            }
        };
        let mut terminal_sessions = Vec::with_capacity(terminal_rows.len());
        let mut terminal_poison_released = false;
        for terminal_entry in terminal_rows {
            match terminal_entry {
                super::super::persistence::TerminalGenerationScanEntry::Persisted(terminal) => {
                    match persisted_terminal_to_detached(&terminal) {
                        Ok(session) => {
                            terminal_sessions.push((session, terminal.promotion_attempts()));
                        }
                        Err(error) => {
                            tracing::error!(
                                stream_id = %stream_id,
                                generation_id = %terminal.key().generation_id(),
                                %error,
                                "hydrate_reclaimed: typed terminal generation decode failed; \
                                 retaining the shared claim for retry"
                            );
                            return Ok(ReclaimedHydrationOutcome::TransientFailure);
                        }
                    }
                }
                super::super::persistence::TerminalGenerationScanEntry::Corrupt { key, detail } => {
                    match storage
                        .quarantine_terminal_generation(&key, caller_fence)
                        .await
                    {
                        Ok(()) => {
                            terminal_poison_released = true;
                            debug!(
                                terminal_generation = %key,
                                %detail,
                                "hydrate_reclaimed: quarantined one corrupt exact terminal generation"
                            );
                        }
                        Err(super::super::persistence::SmPersistenceError::NotOwner { .. }) => {
                            return Ok(ReclaimedHydrationOutcome::LostClaim);
                        }
                        Err(error) => {
                            debug!(
                                terminal_generation = %key,
                                %detail,
                                %error,
                                "hydrate_reclaimed: exact terminal poison quarantine failed"
                            );
                            return Ok(ReclaimedHydrationOutcome::TransientFailure);
                        }
                    }
                }
            }
        }

        let mut current_session = None;
        let mut current_poison_released = false;
        if !current_present {
            match storage.get_session(&session_id).await {
                Ok(Some(persisted)) => {
                    let unacked = match storage.list_unacked(&session_id).await {
                        Ok(rows) => rows,
                        Err(super::super::persistence::SmPersistenceError::Corrupt {
                            stream_id: corrupt_stream,
                            detail,
                        }) if corrupt_stream == session_id => {
                            debug!(stream_id = %stream_id, %detail, "hydrate_reclaimed: corrupt durable unacked row");
                            match self
                                .quarantine_reclaimed_poison(
                                    storage.as_ref(),
                                    entity,
                                    caller_fence,
                                    &session_id,
                                )
                                .await?
                            {
                                ReclaimedHydrationOutcome::PoisonReleased => {
                                    current_poison_released = true;
                                    Vec::new()
                                }
                                outcome => return Ok(outcome),
                            }
                        }
                        Err(error) => {
                            debug!(stream_id = %stream_id, %error, "hydrate_reclaimed: list_unacked failed");
                            return Ok(ReclaimedHydrationOutcome::TransientFailure);
                        }
                    };
                    if !current_poison_released {
                        current_session = match persisted_to_detached(&persisted, &unacked) {
                            Ok(session) => Some(session),
                            Err(error) => {
                                debug!(stream_id = %stream_id, %error, "hydrate_reclaimed: current row decode failed");
                                match self
                                    .quarantine_reclaimed_poison(
                                        storage.as_ref(),
                                        entity,
                                        caller_fence,
                                        &session_id,
                                    )
                                    .await?
                                {
                                    ReclaimedHydrationOutcome::PoisonReleased => {
                                        current_poison_released = true;
                                        None
                                    }
                                    outcome => return Ok(outcome),
                                }
                            }
                        };
                    }
                }
                Ok(None) => {}
                Err(super::super::persistence::SmPersistenceError::Corrupt {
                    stream_id: corrupt_stream,
                    detail,
                }) if corrupt_stream == session_id => {
                    debug!(stream_id = %stream_id, %detail, "hydrate_reclaimed: corrupt durable session row");
                    match self
                        .quarantine_reclaimed_poison(
                            storage.as_ref(),
                            entity,
                            caller_fence,
                            &session_id,
                        )
                        .await?
                    {
                        ReclaimedHydrationOutcome::PoisonReleased => {
                            current_poison_released = true;
                        }
                        outcome => return Ok(outcome),
                    }
                }
                Err(error) => {
                    debug!(stream_id = %stream_id, %error, "hydrate_reclaimed: get_session failed");
                    return Ok(ReclaimedHydrationOutcome::TransientFailure);
                }
            }
        }
        if !any_present && current_session.is_none() && terminal_sessions.is_empty() {
            return Ok(if current_poison_released || terminal_poison_released {
                ReclaimedHydrationOutcome::PoisonReleased
            } else {
                ReclaimedHydrationOutcome::MissingDurable
            });
        }
        // Every persistence read above is an await point. Re-prove both the
        // node incarnation and exact claim epoch immediately before the
        // synchronous in-memory publication.
        if self.node_identity.current() != *caller_fence.owner() {
            return Ok(ReclaimedHydrationOutcome::StaleIdentity);
        }
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store
                .fence(entity, caller_fence.owner(), caller_fence.epoch()),
        )
        .await
        {
            Ok(Ok(true)) if self.node_identity.current() == *caller_fence.owner() => {}
            Ok(Ok(true)) => return Ok(ReclaimedHydrationOutcome::StaleIdentity),
            Ok(Ok(_)) => return Ok(ReclaimedHydrationOutcome::LostClaim),
            Ok(Err(error)) => {
                debug!(stream_id = %stream_id, %error, "hydrate_reclaimed: final exact fence failed");
                return Ok(ReclaimedHydrationOutcome::TransientFailure);
            }
            Err(_) => return Ok(ReclaimedHydrationOutcome::TransientFailure),
        }
        let Some(_identity_guard) = self
            .node_identity
            .guard_if_current(caller_fence.owner())
            .await
        else {
            return Ok(ReclaimedHydrationOutcome::StaleIdentity);
        };
        let mut hydrated = false;
        if let Some(session) = current_session {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.insert(stream_id.clone(), session);
            hydrated = true;
        }
        for (session, promotion_attempts) in terminal_sessions {
            hydrated |= self.retain_terminal_durable_promotion(
                session,
                promotion_attempts,
                caller_fence.clone(),
            )?;
        }
        if hydrated {
            Ok(ReclaimedHydrationOutcome::Hydrated)
        } else if any_present {
            Ok(ReclaimedHydrationOutcome::AlreadyPresent)
        } else if current_poison_released || terminal_poison_released {
            Ok(ReclaimedHydrationOutcome::PoisonReleased)
        } else {
            Ok(ReclaimedHydrationOutcome::MissingDurable)
        }
    }

    async fn quarantine_reclaimed_poison(
        &self,
        storage: &dyn super::super::persistence::SmPersistenceStorage,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        session_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<ReclaimedHydrationOutcome, SmRegistryError> {
        if self.node_identity.current() != *caller_fence.owner() {
            return Ok(ReclaimedHydrationOutcome::StaleIdentity);
        }
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store
                .fence(entity, caller_fence.owner(), caller_fence.epoch()),
        )
        .await
        {
            Ok(Ok(true)) if self.node_identity.current() == *caller_fence.owner() => {}
            Ok(Ok(true)) => return Ok(ReclaimedHydrationOutcome::StaleIdentity),
            Ok(Ok(_)) => return Ok(ReclaimedHydrationOutcome::LostClaim),
            Ok(Err(_)) | Err(_) => return Ok(ReclaimedHydrationOutcome::TransientFailure),
        }
        // The clustered implementation binds `caller_fence` into the same
        // transaction that removes both durable tables. Thus stale work can
        // neither quarantine a newer epoch nor report terminal success
        // before the poison state is actually gone.
        match storage.quarantine_session(session_id, caller_fence).await {
            Ok(()) => Ok(ReclaimedHydrationOutcome::PoisonReleased),
            Err(super::super::persistence::SmPersistenceError::NotOwner { .. }) => {
                Ok(ReclaimedHydrationOutcome::LostClaim)
            }
            Err(error) => {
                debug!(
                    stream_id = %session_id,
                    %error,
                    "hydrate_reclaimed: poison quarantine failed; retaining exact claim for retry"
                );
                Ok(ReclaimedHydrationOutcome::TransientFailure)
            }
        }
    }

    pub async fn hydrate_reclaimed(
        &self,
        entities: &[(
            Entity,
            super::super::persistence::SmClaimFence,
            ReclaimedClaimReservation,
        )],
    ) -> Result<usize, SmRegistryError> {
        let mut hydrated = 0usize;
        for (entity, fence, reservation) in entities {
            if self
                .hydrate_reclaimed_typed(entity, fence, *reservation)
                .await?
                == ReclaimedHydrationOutcome::Hydrated
            {
                hydrated += 1;
            }
        }
        Ok(hydrated)
    }

    /// Retry bounded reclaimed-session work retained by
    /// [`Self::hydrate_reclaimed_typed`]. A live node's won claim is no
    /// longer discoverable by the orphan scan, so this inventory — not a
    /// future scan — owns retry until hydration succeeds, ownership is
    /// disproved, or terminal cleanup completes.
    pub async fn retry_pending_reclaimed_hydrations(&self, limit: usize) -> usize {
        self.retry_pending_reclaimed_hydrations_observing(limit, |_| {})
            .await
    }

    /// Retry reclaimed hydration while synchronously reporting each observed
    /// exact backend release reached by terminal hydration cleanup. A
    /// transfer back to a live lifecycle and backend `NotOwned` remain
    /// metric-neutral. The observer runs immediately after the release future
    /// completes, before this pass can reach another cancellation point.
    pub async fn retry_pending_reclaimed_hydrations_observing<F>(
        &self,
        limit: usize,
        mut observe: F,
    ) -> usize
    where
        F: FnMut(super::SmClaimReleaseRetryOutcome),
    {
        let lookups = self
            .pending_reclaimed_claim_lookups
            .read()
            .map(|pending| {
                pending
                    .iter()
                    .take(limit)
                    .map(|((_, owner, reservation), entity)| {
                        (entity.clone(), owner.clone(), *reservation)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut attempted = 0;
        for (entity, owner, reservation) in lookups {
            attempted += 1;
            let Ok(stream_lock) = self.stream_lock(&entity.id) else {
                continue;
            };
            let stream_guard = stream_lock.lock().await;
            let lookup_key = (entity.id.clone(), owner.clone(), reservation);
            let still_pending = self
                .pending_reclaimed_claim_lookups
                .read()
                .map(|pending| pending.contains_key(&lookup_key))
                .unwrap_or(false);
            if !still_pending {
                continue;
            }
            let snapshot = match tokio::time::timeout(
                CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                self.claim_store.current_claim_after_pending_writes(&entity),
            )
            .await
            {
                Ok(Ok(snapshot)) => snapshot,
                Ok(Err(_)) | Err(_) => continue,
            };
            if let Some(snapshot) = snapshot.filter(|snapshot| snapshot.owner == owner) {
                if let Ok(mut pending) = self.pending_reclaimed_claim_lookups.write() {
                    pending.remove(&lookup_key);
                }
                let fence =
                    super::super::persistence::SmClaimFence::new(owner, snapshot.claim_epoch);
                let outcome = if self
                    .try_record_pending_reclaimed_hydration(&entity, &fence, reservation)
                    .unwrap_or(false)
                {
                    self.hydrate_reclaimed_typed_locked(&entity, &fence, reservation)
                        .await
                } else {
                    Ok(ReclaimedHydrationOutcome::LostClaim)
                };
                let terminal = outcome
                    .as_ref()
                    .is_ok_and(|outcome| outcome.is_release_terminal());
                drop(stream_guard);
                if terminal {
                    let _ = self
                        .release_reclaimed_claim_observing(
                            &entity,
                            &fence,
                            reservation,
                            &mut observe,
                        )
                        .await;
                }
            } else {
                if let Ok(mut pending) = self.pending_reclaimed_claim_lookups.write() {
                    pending.remove(&lookup_key);
                }
                self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
            }
        }
        let remaining = limit.saturating_sub(attempted);
        let pending = self
            .pending_reclaimed_hydrations
            .read()
            .map(|pending| {
                pending
                    .iter()
                    .take(remaining)
                    .map(|((_, fence, reservation), entity)| {
                        (entity.clone(), fence.clone(), *reservation)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (entity, fence, reservation) in pending {
            attempted += 1;
            if let Ok(outcome) = self
                .hydrate_reclaimed_typed(&entity, &fence, reservation)
                .await
            {
                if outcome.is_release_terminal() {
                    let _ = self
                        .release_reclaimed_claim_observing(
                            &entity,
                            &fence,
                            reservation,
                            &mut observe,
                        )
                        .await;
                }
            }
        }
        attempted
    }

    #[cfg(test)]
    pub(super) fn pending_reclaimed_hydration_count(&self) -> usize {
        self.pending_reclaimed_hydrations
            .read()
            .map_or(0, |pending| pending.len())
    }

    pub async fn release_reclaimed_claim(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<crate::ownership::ExactReleaseOutcome, SmRegistryError> {
        self.release_reclaimed_claim_observing(entity, fence, reservation, |_| {})
            .await
    }

    /// Release one reclaimed exact fence while reporting only a result
    /// returned by the backend `release_exact` call. A concurrent transfer
    /// back to a live lifecycle returns `NotOwned` to the legacy caller but
    /// deliberately emits no observer outcome because no backend release or
    /// ownership proof occurred.
    pub async fn release_reclaimed_claim_observing<F>(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
        mut observe: F,
    ) -> Result<crate::ownership::ExactReleaseOutcome, SmRegistryError>
    where
        F: FnMut(super::SmClaimReleaseRetryOutcome),
    {
        let stream_lock = self.stream_lock(&entity.id)?;
        let _stream_guard = stream_lock.lock().await;
        match self.stream_liveness(&entity.id) {
            Some(true) => {
                // Responsibility transferred back to the live local session.
                // Never let terminal cleanup release its claim.
                self.clear_pending_reclaimed_hydration(entity, fence, reservation);
                self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
                return Ok(crate::ownership::ExactReleaseOutcome::NotOwned);
            }
            None => {
                if !self.try_record_terminal_reclaimed_fence(&entity.id, fence.clone(), reservation)
                    && !self.try_record_uncertain_release_fence(&entity.id, fence.clone())
                {
                    return Err(SmRegistryError::Internal(
                        "release_reclaimed_claim: local liveness is uncertain and exact retry capacity is exhausted".to_string(),
                    ));
                }
                // Retain the exact fence locally as well as reporting a
                // retryable failure. This covers both the supervised worker
                // and one-shot self-fence callers.
                return Err(SmRegistryError::Internal(
                    "release_reclaimed_claim: local session liveness is uncertain; exact cleanup retained".to_string(),
                ));
            }
            Some(false) => {}
        }
        if !self.try_record_terminal_reclaimed_fence(&entity.id, fence.clone(), reservation) {
            if !self.reserve_claim_fence_capacity(&entity.id) {
                return Err(SmRegistryError::Internal(
                    "release_reclaimed_claim: exact-release retry capacity exhausted".to_string(),
                ));
            }
            if !self.try_record_claim_fence(&entity.id, fence.clone()) {
                self.cancel_claim_fence_reservation(&entity.id);
                return Err(SmRegistryError::Internal(
                    "release_reclaimed_claim: failed to retain exact claim fence".to_string(),
                ));
            }
        }
        if !self.mark_claim_release_may_complete(&entity.id, fence) {
            return Err(SmRegistryError::Internal(
                "release_reclaimed_claim: failed to issue-mark the exact release".to_string(),
            ));
        }
        let outcome = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store
                .release_exact(entity, fence.owner(), fence.epoch()),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => {
                return Err(SmRegistryError::Internal(
                    "release_reclaimed_claim: exact release failed and was retained for retry"
                        .to_string(),
                ))
            }
        };
        if let Ok(mut fences) = self.claim_fences.write() {
            if fences.get(&entity.id) == Some(fence) {
                fences.remove(&entity.id);
            }
        }
        if let Ok(mut pending) = self.pending_claim_releases.write() {
            pending.remove(&(entity.id.clone(), fence.clone()));
        }
        if let Some(storage) = &self.persistence {
            let session_id = crate::pending_delivery::SmSessionId::new(entity.id.clone());
            storage.evict_claim_cache(&session_id, fence);
        }
        self.clear_pending_reclaimed_hydration(entity, fence, reservation);
        observe(match outcome {
            crate::ownership::ExactReleaseOutcome::Released => {
                super::SmClaimReleaseRetryOutcome::Released
            }
            crate::ownership::ExactReleaseOutcome::NotOwned => {
                super::SmClaimReleaseRetryOutcome::Disproved
            }
        });
        Ok(outcome)
    }

    /// Resolve an ownership CAS whose result was dropped before its claimed
    /// durable work was classified.
    ///
    /// A read-only claim lookup discovers the exact epoch won by
    /// `attempted_owner`, then normal exact-fence hydration decides whether
    /// durable recovery work remains. Only an empty or poison-quarantined
    /// durable inventory is released. Hydrated/live work keeps the claim, and
    /// every lookup or hydration failure retains the local capacity
    /// responsibility so the caller remains self-fenced.
    pub async fn retire_uncertain_reclaimed_claim(
        &self,
        entity: &Entity,
        attempted_owner: &NodeIdentity,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ReclaimedHydrationOutcome, SmRegistryError> {
        if entity.entity_type != EntityType::SmSession {
            self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
            return Ok(ReclaimedHydrationOutcome::LostClaim);
        }
        let snapshot = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.current_claim(entity),
        )
        .await
        {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(error)) => {
                return Err(SmRegistryError::Internal(format!(
                    "retire_uncertain_reclaimed_claim: exact owner lookup failed: {error}"
                )));
            }
            Err(_) => {
                return Err(SmRegistryError::Internal(
                    "retire_uncertain_reclaimed_claim: exact owner lookup timed out".to_string(),
                ));
            }
        };
        let Some(snapshot) = snapshot.filter(|snapshot| snapshot.owner == *attempted_owner) else {
            return Err(SmRegistryError::Internal(
                "retire_uncertain_reclaimed_claim: attempted owner not yet observable; \
                 reservation retained because the ownership CAS may still commit"
                    .to_string(),
            ));
        };
        let fence = super::super::persistence::SmClaimFence::new(
            attempted_owner.clone(),
            snapshot.claim_epoch,
        );
        let outcome = self
            .hydrate_reclaimed_typed(entity, &fence, reservation)
            .await?;
        match outcome {
            ReclaimedHydrationOutcome::MissingDurable
            | ReclaimedHydrationOutcome::PoisonReleased => {
                self.release_reclaimed_claim(entity, &fence, reservation)
                    .await?;
                Ok(outcome)
            }
            ReclaimedHydrationOutcome::Hydrated
            | ReclaimedHydrationOutcome::AlreadyPresent
            | ReclaimedHydrationOutcome::LostClaim
            | ReclaimedHydrationOutcome::StaleIdentity => Ok(outcome),
            ReclaimedHydrationOutcome::TransientFailure => Err(SmRegistryError::Internal(
                "retire_uncertain_reclaimed_claim: exact hydration remains transient".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimedHydrationOutcome {
    Hydrated,
    AlreadyPresent,
    MissingDurable,
    LostClaim,
    StaleIdentity,
    TransientFailure,
    PoisonReleased,
}

impl ReclaimedHydrationOutcome {
    /// Only outcomes that prove every durable carrier absent or quarantined
    /// authorize releasing the shared claim. A transient failure retains
    /// local retry ownership. `StaleIdentity` instead means exact local
    /// authority was already relinquished so global orphan discovery can
    /// reclaim the untouched backend claim under the fresh identity.
    pub fn is_release_terminal(self) -> bool {
        matches!(self, Self::MissingDurable | Self::PoisonReleased)
    }
}

impl InMemorySmSessionRegistry {
    /// Helper: delete every durable row for `stream_id` (session +
    /// unacked queue). Returns the underlying error so callers can
    /// adopt a "persist-first" ordering — refuse to mutate the
    /// in-memory map when the durable delete failed, so a transient
    /// storage hiccup doesn't leave an orphaned `sm_sessions` row
    /// that `restore_from_persistence` would resurrect on restart.
    /// (Codex P1 + Copilot + Qodo on PR #344: best-effort silent
    /// swallow allowed durable orphans whenever the in-memory state
    /// had already moved on.)
    pub(super) async fn persist_delete_session(
        &self,
        stream_id: &str,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        storage
            .delete_session(&crate::pending_delivery::SmSessionId::new(
                stream_id.to_string(),
            ))
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    pub(super) async fn persist_delete_session_with_authority(
        &self,
        stream_id: &str,
        authority: &crate::ownership::CurrentNodeIdentityGuard,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        storage
            .delete_session_with_authority(
                &crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
                authority,
            )
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    pub(super) async fn persist_delete_session_under_fence(
        &self,
        stream_id: &str,
        expected_fence: &super::super::persistence::SmClaimFence,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        storage
            .delete_session_under_fence(
                &crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
                expected_fence,
            )
            .await
            .map_err(SmRegistryError::Persistence)
    }

    pub(super) async fn persist_detached_session_snapshot(
        &self,
        session: &DetachedSession,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        let definitely_not_committed = |error: SmRegistryError| {
            SmRegistryError::Persistence(
                super::super::persistence::SmPersistenceError::SnapshotDefinitelyNotCommitted(
                    error.to_string(),
                ),
            )
        };
        let persisted = detached_to_persisted(session).map_err(&definitely_not_committed)?;
        let mut unacked_rows = Vec::with_capacity(session.unacked_stanzas.len());
        for entry in &session.unacked_stanzas {
            unacked_rows.push(
                parse_xml_to_persisted_unacked(
                    &session.stream_id,
                    entry.sequence,
                    &entry.stanza_xml,
                    entry.original_receipt_at,
                    entry.purpose,
                )
                .map_err(&definitely_not_committed)?,
            );
        }
        storage
            .store_session_atomic(persisted, unacked_rows)
            .await
            .map_err(SmRegistryError::Persistence)
    }

    /// Atomically publish a resumable successor and archive its exact same-id
    /// predecessor as terminal work. The terminal key is also the commit
    /// marker used to reconcile an acknowledgement failure without guessing
    /// from the bare `sm_sessions` row.
    pub(super) async fn persist_detached_session_replacement(
        &self,
        successor: &DetachedSession,
        predecessor: &DetachedSession,
    ) -> Result<PersistDetachedReplacementOutcome, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(PersistDetachedReplacementOutcome::Committed);
        };
        let definitely_not_committed = |error: SmRegistryError| {
            SmRegistryError::Persistence(
                super::super::persistence::SmPersistenceError::SnapshotDefinitelyNotCommitted(
                    error.to_string(),
                ),
            )
        };
        let successor =
            detached_to_persisted_snapshot(successor).map_err(&definitely_not_committed)?;
        let predecessor =
            detached_to_terminal_generation(predecessor).map_err(&definitely_not_committed)?;
        let predecessor_key = predecessor.key().clone();

        match storage
            .replace_resumable_session_atomic(successor, Some(predecessor))
            .await
        {
            Ok(()) => Ok(PersistDetachedReplacementOutcome::Committed),
            Err(
                error @ (super::super::persistence::SmPersistenceError::SnapshotDefinitelyNotCommitted(
                    _,
                ) | super::super::persistence::SmPersistenceError::NotOwner { .. }),
            ) => Err(SmRegistryError::Persistence(error)),
            Err(write_error) => match storage.get_terminal_generation(&predecessor_key).await {
                Ok(Some(_)) => {
                    tracing::warn!(
                        terminal_key = %predecessor_key,
                        persistence_error = %write_error,
                        "same-id SM replacement commit was confirmed by its exact terminal marker"
                    );
                    Ok(PersistDetachedReplacementOutcome::Committed)
                }
                Err(super::super::persistence::SmPersistenceError::CorruptTerminal {
                    key,
                    detail,
                }) if key == predecessor_key => {
                    tracing::warn!(
                        terminal_key = %predecessor_key,
                        %detail,
                        persistence_error = %write_error,
                        "same-id SM replacement committed with a corrupt exact terminal marker; \
                         retaining the valid in-memory predecessor payload for fenced promotion"
                    );
                    Ok(PersistDetachedReplacementOutcome::Committed)
                }
                Ok(None) => {
                    tracing::warn!(
                        terminal_key = %predecessor_key,
                        persistence_error = %write_error,
                        "same-id SM replacement marker is not yet visible after an ambiguous \
                         write; parking both generations because a late commit may still publish"
                    );
                    Ok(PersistDetachedReplacementOutcome::PublicationUnknown(
                        write_error,
                    ))
                }
                Err(read_error) => {
                    tracing::warn!(
                        terminal_key = %predecessor_key,
                        persistence_error = %write_error,
                        reconciliation_error = %read_error,
                        "same-id SM replacement publication remains unknown; parking both \
                         generations until durable reconciliation or restart"
                    );
                    Ok(PersistDetachedReplacementOutcome::PublicationUnknown(
                        write_error,
                    ))
                }
            },
        }
    }

    /// Durably delete the named unacked rows for a stream — exact
    /// `(stream_id, sequence)` matches, idempotent for absent rows.
    ///
    /// Used by the Q6 promotion retry path (round-2 review R4): after
    /// a PARTIAL promotion failure, the successfully promoted stanzas'
    /// `pending_delivery` rows are already committed, so their
    /// `sm_unacked` rows must be erased before the session is
    /// re-inserted for retry — otherwise every janitor tick re-promotes
    /// the whole queue and duplicates the already-queued stanzas.
    /// Ordering is crash-safe: the pending row commits BEFORE its
    /// `sm_unacked` row is deleted here, preserving at-least-once.
    ///
    /// Takes the stream lock so the delete serializes with
    /// detached-append full snapshots that could otherwise resurrect
    /// the rows. No in-memory mutation happens here — the caller owns
    /// the drained session and drops the entries from its local copy.
    #[cfg(test)]
    pub async fn delete_unacked_sequences(
        &self,
        stream_id: &str,
        sequences: &[u32],
    ) -> Result<u64, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(0);
        };
        if sequences.is_empty() {
            return Ok(0);
        }
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        storage
            .delete_unacked(
                &crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
                sequences,
            )
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    /// Delete promoted durable rows only under the exact current generation.
    pub async fn delete_unacked_sequences_under(
        &self,
        lease: &super::SmSessionPromotionLease,
        sequences: &[u32],
    ) -> Result<u64, SmRegistryError> {
        let expected_shard = self.stream_lock(lease.stream_id.as_str())?;
        let _stream_guard = expected_shard.lock().await;
        let authorized = self
            .pending_promotions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .current_reservation_matches(
                lease.stream_id.as_str(),
                lease.generation_id,
                lease.nonce,
            );
        if lease.authority != super::SmSessionPromotionAuthority::CurrentDurable || !authorized {
            return Err(SmRegistryError::Internal(
                "unacked-row delete lacks exact current-generation authority".to_string(),
            ));
        }
        let Some(storage) = &self.persistence else {
            return Ok(0);
        };
        if sequences.is_empty() {
            return Ok(0);
        }
        let session_id = lease.stream_id.clone();
        match lease.claim_fence.as_ref() {
            Some(fence) => storage
                .delete_unacked_under_fence(&session_id, sequences, fence)
                .await
                .map_err(SmRegistryError::Persistence),
            None if !storage.requires_exact_claim_fence() => storage
                .delete_unacked(&session_id, sequences)
                .await
                .map_err(SmRegistryError::Persistence),
            None => Err(SmRegistryError::Internal(
                "unacked-row delete lacks an exact captured claim fence".to_string(),
            )),
        }
    }

    pub(super) fn stream_lock(
        &self,
        stream_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, SmRegistryError> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stream_id.hash(&mut hasher);
        let shard = (hasher.finish() as usize) % self.stream_locks.len();
        Ok(Arc::clone(&self.stream_locks[shard]))
    }

    pub async fn lock_session_operation(
        &self,
        stream_id: &str,
    ) -> Result<super::SmSessionOperationGuard, SmRegistryError> {
        let shard = self.stream_lock(stream_id)?;
        let guard = shard.clone().lock_owned().await;
        Ok(super::SmSessionOperationGuard {
            stream_id: stream_id.to_string(),
            shard,
            _guard: guard,
        })
    }

    pub(super) fn find_session_id_matching(
        &self,
        predicate: impl Fn(&DetachedSession) -> bool,
    ) -> Result<Option<String>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some((stream_id, _)) = sessions.iter().find(|(_, session)| predicate(session)) {
            return Ok(Some(stream_id.clone()));
        }
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Ok(claimed
            .iter()
            .find(|(_, session)| predicate(session))
            .map(|(stream_id, _)| stream_id.clone()))
    }

    pub(super) async fn update_detached_session_snapshot(
        &self,
        stream_id: &str,
        predicate: impl Fn(&DetachedSession) -> bool,
        mutate: impl FnOnce(&mut DetachedSession) -> Result<(), SmRegistryError>,
    ) -> Result<bool, SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;

        let current = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions
                .get(stream_id)
                .filter(|session| predicate(session))
                .cloned()
        };
        let current = if current.is_some() {
            current
        } else {
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            claimed
                .get(stream_id)
                .filter(|session| predicate(session))
                .cloned()
        };

        let Some(mut updated) = current else {
            return Ok(false);
        };
        mutate(&mut updated)?;

        // Durable snapshot first, then publish the same typed state in memory.
        // The stream lock serializes this full-snapshot write with other appends
        // and with claim completion/deletion so an older clone cannot overwrite
        // a newer replay window.
        self.persist_detached_session_snapshot(&updated).await?;

        let updated = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            if sessions.contains_key(stream_id) {
                sessions.insert(stream_id.to_string(), updated);
                return Ok(true);
            }
            updated
        };

        let found_claimed = {
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            if claimed.contains_key(stream_id) {
                claimed.insert(stream_id.to_string(), updated);
                true
            } else {
                false
            }
        };
        if found_claimed {
            return Ok(true);
        }

        // The session vanished from both maps between the stream-lock
        // read and this recheck. The only remover that does NOT take
        // this stream's lock is displacement by `store_session` (jid
        // collision / max_sessions eviction, which holds only the NEW
        // stream's shard lock) — and displaced sessions follow the
        // persist-until-confirmed contract (traits.rs): their durable
        // rows must survive until the promote → confirm_drained chain
        // erases them. The previous fail-closed `persist_delete_session`
        // here (PR #486, guarding against hypothetical lock-free
        // removers resurrecting an already-consumed stream) deleted a
        // displaced session's rows mid-promotion, losing the queue on a
        // crash. Every consuming path (take_session, complete_claim,
        // confirm_drained) takes
        // this stream lock, so the consumed-stream-resurrection concern
        // cannot arise here; deletion stays owned by
        // confirm_drained / the janitor. Worst case is an orphan
        // snapshot row that restore_from_persistence rehydrates and the
        // janitor later promotes — at-least-once, never data loss.
        Ok(false)
    }
}

fn new_stream_locks() -> Vec<Arc<tokio::sync::Mutex<()>>> {
    (0..STREAM_LOCK_SHARDS)
        .map(|_| Arc::new(tokio::sync::Mutex::new(())))
        .collect()
}
