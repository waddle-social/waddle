//! Pure state of the account coordinator: the slots of one bare JID and
//! selection over them.
//!
//! [`AccountResources`] holds one [`AccountResourceSlot`] per full JID of an
//! account, all under one [`UserClaimIdentity`]. Each slot is the state
//! machine of that resource's current binding; this type adds what only the
//! account as a whole can answer: which resources exist, which are available,
//! which receive a stanza addressed to the bare JID, whether a command still
//! speaks for a resource's current binding, and whether the resource's
//! one-time presentations have happened.
//!
//! The type performs no I/O and holds no handles. The account coordinator
//! service owns one instance per bare JID and serializes access to it.
//! Selection here follows the rules `UserActor` implements, so the two
//! agree on the same resources.
//!
//! Only a binding in [`AccountResourceBindingState::Active`] that is held
//! under the account's claim identity is a destination. A prepared or
//! published binding is recorded but not routable, a detached one is
//! resumable but has no endpoint, and a retiring or retired one is on its
//! way out. A binding the account's current claim has not adopted belongs
//! to a previous owner's connection and is not routable either.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use jid::FullJid;

use super::account_resource_transition::{
    AccountResourceBindingState, AccountResourceExecuteError, AccountResourceExecution,
    AccountResourceMutation, AccountResourceOperation, AccountResourceOperationOutcome,
    AccountResourceSlot, AccountResourceSnapshot, AccountResourceTransitionError,
};
use super::joint_identity::SmSessionKey;
use super::{BindingRef, UserClaimIdentity};

/// Why the account refused an operation before any slot saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccountResourcesError {
    #[error("the full JID belongs to a different account")]
    AccountMismatch,
    #[error("the account already holds its maximum number of resources")]
    ResourceLimit,
    #[error("the account has no slot for this full JID")]
    UnknownResource,
    #[error("the resource slot refused the operation: {0:?}")]
    Slot(AccountResourceExecuteError),
}

/// Why a command does not speak for a resource's current binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BindingFenceError {
    #[error("the account has no slot for this full JID")]
    UnknownResource,
    #[error("the resource has no current binding")]
    NotBound,
    #[error("the resource's current binding is a different one")]
    Superseded,
}

/// One resource's routing policy, read together with every other resource's
/// so destination selection and carbon policy agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountResourceRouting {
    pub jid: FullJid,
    pub available: bool,
    pub priority: i8,
    pub carbons_enabled: bool,
}

/// What a resource's one-time presentations are keyed by. A resume mints a
/// new binding for the same stream-management session, so a resource with a
/// session is keyed by the session and is not presented again on resume. A
/// resource without one cannot resume and is keyed by its binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PresentationKey {
    Session(SmSessionKey),
    Binding(BindingRef),
}

/// The one-time presentations a resource receives after it becomes
/// available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presentation {
    /// Stored offline messages were flushed to the resource.
    OfflineMessages,
    /// Pending subscription requests were re-sent to the resource.
    PendingSubscriptions,
}

/// Which one-time presentations a resource has received.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PresentationFlags {
    pub offline_messages: bool,
    pub pending_subscriptions: bool,
}

/// The resources of one account under one claim identity.
#[derive(Debug)]
pub struct AccountResources {
    claim_identity: UserClaimIdentity,
    max_resources: NonZeroUsize,
    slots: BTreeMap<FullJid, AccountResourceSlot>,
    presentations: Vec<(PresentationKey, PresentationFlags)>,
}

impl AccountResources {
    /// Start an account with no resources. `max_resources` bounds the number
    /// of slots; a slot is kept after its binding retires, because it carries
    /// the resource's operation journal and install generation.
    pub fn new(claim_identity: UserClaimIdentity, max_resources: NonZeroUsize) -> Self {
        Self {
            claim_identity,
            max_resources,
            slots: BTreeMap::new(),
            presentations: Vec::new(),
        }
    }

    pub fn claim_identity(&self) -> &UserClaimIdentity {
        &self.claim_identity
    }

    /// Install a slot rebuilt from its durable facts, with the one-time
    /// presentations already recorded for its current binding. An existing
    /// slot for the same full JID is replaced, which is how the owner of
    /// this state adopts what a store committed.
    pub fn restore_slot(
        &mut self,
        slot: AccountResourceSlot,
        presented: PresentationFlags,
    ) -> Result<(), AccountResourcesError> {
        let full_jid = slot.full_jid().clone();
        if &full_jid.to_bare() != self.claim_identity.bare_jid() {
            return Err(AccountResourcesError::AccountMismatch);
        }
        if !self.slots.contains_key(&full_jid) && self.slots.len() >= self.max_resources.get() {
            return Err(AccountResourcesError::ResourceLimit);
        }
        if let Some(key) = self.presentation_key(&full_jid) {
            self.presentations.retain(|(held, _)| held != &key);
        }
        self.slots.insert(full_jid.clone(), slot);
        if presented != PresentationFlags::default() {
            if let Some(key) = self.presentation_key(&full_jid) {
                self.presentations.push((key, presented));
            }
        }
        Ok(())
    }

    /// Drop the slot of a resource that has no current binding, so the
    /// account can take another resource in its place. Returns whether a
    /// slot was dropped; a slot that holds a binding is kept.
    pub fn forget_free_slot(&mut self, full_jid: &FullJid) -> bool {
        match self.slots.get(full_jid) {
            Some(slot) if slot.binding_ref().is_none() => {
                self.slots.remove(full_jid);
                true
            }
            _ => false,
        }
    }

    /// The resources whose slot holds no current binding.
    pub fn free_slots(&self) -> Vec<FullJid> {
        self.slots
            .iter()
            .filter(|(_, slot)| slot.binding_ref().is_none())
            .map(|(jid, _)| jid.clone())
            .collect()
    }

    /// The number of slots the account holds, bound or free.
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Every slot of the account, ordered by full JID.
    pub fn slots(&self) -> impl Iterator<Item = &AccountResourceSlot> {
        self.slots.values()
    }

    /// The one-time presentations recorded for the resource's current
    /// binding or session.
    pub fn presentation_flags(&self, full_jid: &FullJid) -> PresentationFlags {
        self.presentation_key(full_jid)
            .and_then(|key| {
                self.presentations
                    .iter()
                    .find(|(held, _)| held == &key)
                    .map(|(_, flags)| *flags)
            })
            .unwrap_or_default()
    }

    /// The slot of one resource, when the account has ever bound it.
    pub fn slot(&self, full_jid: &FullJid) -> Option<&AccountResourceSlot> {
        self.slots.get(full_jid)
    }

    /// Apply one operation to the slot of `full_jid`.
    ///
    /// Only a `Bind` creates a slot; every other mutation of an unknown
    /// resource is refused without creating one. The slot journals the
    /// operation, so repeating it with the same identity returns the
    /// recorded outcome. A resource whose retirement completes loses its
    /// presentation record, so its next binding is presented afresh. A
    /// snapshot update that gives the binding a stream-management session
    /// moves the record to the session, so enabling stream management after
    /// a presentation does not present again.
    pub fn execute(
        &mut self,
        full_jid: &FullJid,
        operation: AccountResourceOperation,
    ) -> Result<AccountResourceExecution, AccountResourcesError> {
        if &full_jid.to_bare() != self.claim_identity.bare_jid() {
            return Err(AccountResourcesError::AccountMismatch);
        }
        if !self.slots.contains_key(full_jid) {
            if !matches!(operation.mutation, AccountResourceMutation::Bind { .. }) {
                return Err(AccountResourcesError::UnknownResource);
            }
            if self.slots.len() >= self.max_resources.get() {
                return Err(AccountResourcesError::ResourceLimit);
            }
            let slot = AccountResourceSlot::new(full_jid.clone(), self.claim_identity.clone())
                .map_err(|error| match error {
                    AccountResourceTransitionError::AccountMismatch => {
                        AccountResourcesError::AccountMismatch
                    }
                    _ => AccountResourcesError::UnknownResource,
                })?;
            self.slots.insert(full_jid.clone(), slot);
        }
        let completes_retirement = matches!(
            operation.mutation,
            AccountResourceMutation::CompleteRetirement { .. }
        );
        let updates_snapshot = matches!(
            operation.mutation,
            AccountResourceMutation::UpdateSnapshot { .. }
        );
        let key_before = self.presentation_key(full_jid);
        let slot = self
            .slots
            .get_mut(full_jid)
            .ok_or(AccountResourcesError::UnknownResource)?;
        let execution = slot
            .execute(operation)
            .map_err(AccountResourcesError::Slot)?;
        let committed = matches!(
            execution,
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed { .. })
        );
        if completes_retirement && self.binding(full_jid).is_none() {
            if let Some(key) = key_before {
                self.presentations.retain(|(held, _)| held != &key);
            }
        } else if committed && updates_snapshot {
            let key_after = self.presentation_key(full_jid);
            if let (Some(before), Some(after)) = (key_before, key_after) {
                if before != after {
                    for (held, _) in &mut self.presentations {
                        if held == &before {
                            *held = after.clone();
                        }
                    }
                }
            }
        }
        Ok(execution)
    }

    /// The exact binding a resource currently has, in any lifecycle state.
    pub fn binding(&self, full_jid: &FullJid) -> Option<&BindingRef> {
        self.slots
            .get(full_jid)
            .and_then(AccountResourceSlot::binding_ref)
    }

    /// Check that `binding_ref` is the resource's current binding. A command
    /// that carries only a full JID, or an older binding, must not act on a
    /// replacement.
    pub fn check_fence(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
    ) -> Result<(), BindingFenceError> {
        let slot = self
            .slots
            .get(full_jid)
            .ok_or(BindingFenceError::UnknownResource)?;
        match slot.binding_ref() {
            None => Err(BindingFenceError::NotBound),
            Some(current) if current == binding_ref => Ok(()),
            Some(_) => Err(BindingFenceError::Superseded),
        }
    }

    /// Every resource with an active binding, with no presence filter.
    pub fn resources(&self) -> Vec<FullJid> {
        self.active().map(|(jid, _)| jid.clone()).collect()
    }

    /// Every presence-available resource with its priority. Presence
    /// side-effect fan-out uses this shape: RFC 6121 presence goes to all
    /// available resources, not only the highest-priority ones.
    pub fn available_resources(&self) -> Vec<(FullJid, i8)> {
        self.active()
            .filter(|(_, snapshot)| snapshot.presence_available)
            .map(|(jid, snapshot)| (jid.clone(), snapshot.presence_priority))
            .collect()
    }

    /// The destinations of a stanza addressed to the bare JID (RFC 6121
    /// section 8.5.2.1.1): among available resources with a non-negative
    /// priority, all those whose priority equals the highest. An empty
    /// result means the caller applies its offline handling.
    pub fn routable_resources(&self) -> Vec<FullJid> {
        let candidates: Vec<(&FullJid, i8)> = self
            .active()
            .filter(|(_, snapshot)| snapshot.presence_available)
            .map(|(jid, snapshot)| (jid, snapshot.presence_priority))
            .filter(|(_, priority)| *priority >= 0)
            .collect();
        let Some(highest) = candidates.iter().map(|(_, priority)| *priority).max() else {
            return Vec::new();
        };
        candidates
            .into_iter()
            .filter(|(_, priority)| *priority == highest)
            .map(|(jid, _)| jid.clone())
            .collect()
    }

    /// Availability, priority and carbons for every resource with an active
    /// binding, ordered by full JID.
    pub fn routing_snapshot(&self) -> Vec<AccountResourceRouting> {
        self.active()
            .map(|(jid, snapshot)| AccountResourceRouting {
                jid: jid.clone(),
                available: snapshot.presence_available,
                priority: snapshot.presence_priority,
                carbons_enabled: snapshot.carbons_enabled,
            })
            .collect()
    }

    /// Every resource with an active binding that wants roster pushes.
    pub fn roster_interested_resources(&self) -> Vec<FullJid> {
        self.active()
            .filter(|(_, snapshot)| snapshot.roster_interested)
            .map(|(jid, _)| jid.clone())
            .collect()
    }

    /// Every resource with an active binding that wants block-list pushes.
    pub fn blocklist_interested_resources(&self) -> Vec<FullJid> {
        self.active()
            .filter(|(_, snapshot)| snapshot.blocklist_interested)
            .map(|(jid, _)| jid.clone())
            .collect()
    }

    /// Whether `presentation` has already happened for the resource's
    /// current session or binding.
    pub fn presented(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        presentation: Presentation,
    ) -> Result<bool, BindingFenceError> {
        self.check_fence(full_jid, binding_ref)?;
        let Some(key) = self.presentation_key(full_jid) else {
            return Err(BindingFenceError::NotBound);
        };
        Ok(self
            .presentations
            .iter()
            .find(|(held, _)| held == &key)
            .is_some_and(|(_, flags)| flags.get(presentation)))
    }

    /// Record that `presentation` happened. Returns whether this call was
    /// the first to record it; a caller that gets `false` must not present
    /// again.
    pub fn mark_presented(
        &mut self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        presentation: Presentation,
    ) -> Result<bool, BindingFenceError> {
        self.check_fence(full_jid, binding_ref)?;
        let Some(key) = self.presentation_key(full_jid) else {
            return Err(BindingFenceError::NotBound);
        };
        let position = match self.presentations.iter().position(|(held, _)| held == &key) {
            Some(position) => position,
            None => {
                self.presentations.push((key, PresentationFlags::default()));
                self.presentations.len() - 1
            }
        };
        let flags = &mut self.presentations[position].1;
        let first = !flags.get(presentation);
        flags.set(presentation);
        Ok(first)
    }

    fn presentation_key(&self, full_jid: &FullJid) -> Option<PresentationKey> {
        let binding = self.slots.get(full_jid)?.binding_receipt()?;
        Some(match binding.snapshot().stream_management_ref {
            Some(sm) => PresentationKey::Session(sm.session),
            None => PresentationKey::Binding(binding.binding_ref().clone()),
        })
    }

    fn active(&self) -> impl Iterator<Item = (&FullJid, &AccountResourceSnapshot)> {
        self.slots.iter().filter_map(|(jid, slot)| {
            let binding = slot.binding_receipt()?;
            (binding.state() == AccountResourceBindingState::Active
                && binding.binding_claim_identity() == &self.claim_identity)
                .then(|| (jid, binding.snapshot()))
        })
    }
}

impl PresentationFlags {
    fn get(self, presentation: Presentation) -> bool {
        match presentation {
            Presentation::OfflineMessages => self.offline_messages,
            Presentation::PendingSubscriptions => self.pending_subscriptions,
        }
    }

    fn set(&mut self, presentation: Presentation) {
        match presentation {
            Presentation::OfflineMessages => self.offline_messages = true,
            Presentation::PendingSubscriptions => self.pending_subscriptions = true,
        }
    }
}

#[cfg(test)]
mod tests;
