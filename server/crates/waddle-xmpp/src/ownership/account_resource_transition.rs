//! Inactive, bounded model of one P3.2 account-resource slot.
//!
//! This fixture assumes the supplied [`UserClaimIdentity`] was authorized by
//! an external claim owner. It does not check claim authority, persist state,
//! coordinate concurrent processes, or participate in production routing.
//! Its synchronous `&mut self` transitions model a serialized compare-and-set
//! history for tests only. Claim replacement fences the old actor but does
//! not retire its binding; a successor must explicitly retire or continue the
//! exact binding. Resource selection, cleanup, and delivery remain owned by
//! their existing production paths.

use jid::FullJid;
use uuid::Uuid;

use super::{BindingRef, UserClaimIdentity};

/// Maximum number of operation identities retained by one fixture slot.
///
/// The journal never evicts entries. When full, new operation IDs are
/// rejected before the slot can change, while prior operations remain
/// replayable and queryable.
pub const ACCOUNT_RESOURCE_OPERATION_CAPACITY: usize = 64;

/// Monotonic revision of this single full-JID resource slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccountResourceRevision(u64);

impl AccountResourceRevision {
    pub const INITIAL: Self = Self(0);

    pub const fn value(self) -> u64 {
        self.0
    }

    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Idempotency key for one resource-slot mutation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccountResourceOperationId(Uuid);

impl AccountResourceOperationId {
    pub fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Compare-and-set request for the one resource slot.
///
/// Each mutation carries the exact actor identity and expected revision. The
/// update and retire forms also carry the exact binding attempt they target.
/// Every retire must include an explicit custody disposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountResourceMutation {
    Bind {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
    },
    Update {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
    },
    Retire {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
        custody_disposition: BindingRetirementDisposition,
    },
    ReplaceClaim {
        expected_claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        replacement_claim_identity: UserClaimIdentity,
    },
}

/// Caller-supplied assumed disposition of accepted work when retiring a
/// binding. The fixture cannot verify that work is absent or that a recovery
/// owner holds it. `Unknown` fails closed for original and successor actors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingRetirementDisposition {
    NoAcceptedWork,
    AcceptedWorkHasRecoveryOwner,
    Unknown,
}

/// One identified mutation and its replay key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountResourceOperation {
    pub id: AccountResourceOperationId,
    pub mutation: AccountResourceMutation,
}

impl AccountResourceOperation {
    pub fn new(id: AccountResourceOperationId, mutation: AccountResourceMutation) -> Self {
        Self { id, mutation }
    }
}

/// A terminal result retained in the bounded operation journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceOperationOutcome {
    Committed { revision: AccountResourceRevision },
    Rejected(AccountResourceTransitionError),
}

/// Whether an execution applied now or replayed an earlier terminal result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceExecution {
    Applied(AccountResourceOperationOutcome),
    Replayed(AccountResourceOperationOutcome),
}

/// Errors outside the state-machine transition, which are not journaled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceExecuteError {
    OperationIdConflict,
    JournalFull,
}

/// Terminal rejection recorded for an operation ID and payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceTransitionError {
    AccountMismatch,
    ClaimMismatch,
    ClaimIdentityReused,
    RevisionConflict,
    RevisionExhausted,
    AlreadyBound,
    NotBound,
    BindingMismatch,
    BindingOwnerMismatch,
    RetirementCustodyUnknown,
    BindingRefReused,
}

/// Result of looking up an operation after an unknown response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceOperationLookup<'a> {
    Unknown,
    PayloadMismatch,
    Recorded(&'a AccountResourceOperationOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JournalEntry {
    id: AccountResourceOperationId,
    mutation: AccountResourceMutation,
    outcome: AccountResourceOperationOutcome,
}

/// One serialized test fixture for a single exact full JID.
#[derive(Debug)]
pub struct AccountResourceSlot {
    full_jid: FullJid,
    claim_identity: UserClaimIdentity,
    revision: AccountResourceRevision,
    binding_ref: Option<BindingRef>,
    binding_claim_identity: Option<UserClaimIdentity>,
    retired_claim_identities: Vec<UserClaimIdentity>,
    seen_binding_refs: Vec<BindingRef>,
    journal: Vec<JournalEntry>,
}

impl AccountResourceSlot {
    /// Start a slot with the externally authorized claim identity supplied by
    /// the fixture caller. The identity's account must match the full JID.
    pub fn new(
        full_jid: FullJid,
        claim_identity: UserClaimIdentity,
    ) -> Result<Self, AccountResourceTransitionError> {
        if claim_identity.bare_jid() != &full_jid.to_bare() {
            return Err(AccountResourceTransitionError::AccountMismatch);
        }

        Ok(Self {
            full_jid,
            claim_identity,
            revision: AccountResourceRevision::INITIAL,
            binding_ref: None,
            binding_claim_identity: None,
            retired_claim_identities: Vec::with_capacity(ACCOUNT_RESOURCE_OPERATION_CAPACITY),
            seen_binding_refs: Vec::with_capacity(ACCOUNT_RESOURCE_OPERATION_CAPACITY),
            journal: Vec::with_capacity(ACCOUNT_RESOURCE_OPERATION_CAPACITY),
        })
    }

    pub fn full_jid(&self) -> &FullJid {
        &self.full_jid
    }

    pub fn claim_identity(&self) -> &UserClaimIdentity {
        &self.claim_identity
    }

    pub fn revision(&self) -> AccountResourceRevision {
        self.revision
    }

    pub fn binding_ref(&self) -> Option<&BindingRef> {
        self.binding_ref.as_ref()
    }

    /// Apply one operation serially. Both successful and rejected transition
    /// results are journaled, so a repeated ID and payload returns the same
    /// terminal result. Conflicting reuse and capacity exhaustion are not
    /// journaled and cannot mutate the slot.
    pub fn execute(
        &mut self,
        operation: AccountResourceOperation,
    ) -> Result<AccountResourceExecution, AccountResourceExecuteError> {
        if let Some(previous) = self.journal.iter().find(|entry| entry.id == operation.id) {
            return if previous.mutation == operation.mutation {
                Ok(AccountResourceExecution::Replayed(previous.outcome))
            } else {
                Err(AccountResourceExecuteError::OperationIdConflict)
            };
        }

        if self.journal.len() == ACCOUNT_RESOURCE_OPERATION_CAPACITY {
            return Err(AccountResourceExecuteError::JournalFull);
        }

        let outcome = self.apply(&operation.mutation);
        self.journal.push(JournalEntry {
            id: operation.id,
            mutation: operation.mutation,
            outcome,
        });
        Ok(AccountResourceExecution::Applied(outcome))
    }

    /// Resolve a possibly lost operation response from the retained journal.
    /// The complete operation must match. `Unknown` means this slot has no
    /// record for the ID; `PayloadMismatch` means the ID is already bound to
    /// another mutation. Neither result proves an unavailable external store
    /// rolled back.
    pub fn lookup(
        &self,
        operation: &AccountResourceOperation,
    ) -> AccountResourceOperationLookup<'_> {
        self.journal
            .iter()
            .find(|entry| entry.id == operation.id)
            .map_or(AccountResourceOperationLookup::Unknown, |entry| {
                if entry.mutation == operation.mutation {
                    AccountResourceOperationLookup::Recorded(&entry.outcome)
                } else {
                    AccountResourceOperationLookup::PayloadMismatch
                }
            })
    }

    fn apply(&mut self, mutation: &AccountResourceMutation) -> AccountResourceOperationOutcome {
        let result = match mutation {
            AccountResourceMutation::Bind {
                claim_identity,
                expected_revision,
                binding_ref,
            } => self.bind(claim_identity, *expected_revision, binding_ref),
            AccountResourceMutation::Update {
                claim_identity,
                expected_revision,
                binding_ref,
            } => self.update(claim_identity, *expected_revision, binding_ref),
            AccountResourceMutation::Retire {
                claim_identity,
                expected_revision,
                binding_ref,
                custody_disposition,
            } => self.retire(
                claim_identity,
                *expected_revision,
                binding_ref,
                *custody_disposition,
            ),
            AccountResourceMutation::ReplaceClaim {
                expected_claim_identity,
                expected_revision,
                replacement_claim_identity,
            } => self.replace_claim(
                expected_claim_identity,
                *expected_revision,
                replacement_claim_identity,
            ),
        };

        match result {
            Ok(revision) => AccountResourceOperationOutcome::Committed { revision },
            Err(error) => AccountResourceOperationOutcome::Rejected(error),
        }
    }

    fn bind(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        if self.binding_ref.is_some() {
            return Err(AccountResourceTransitionError::AlreadyBound);
        }
        if self.seen_binding_refs.contains(binding_ref) {
            return Err(AccountResourceTransitionError::BindingRefReused);
        }
        let revision = self.next_revision()?;

        self.binding_ref = Some(binding_ref.clone());
        self.binding_claim_identity = Some(claim_identity.clone());
        self.seen_binding_refs.push(binding_ref.clone());
        self.revision = revision;
        Ok(revision)
    }

    fn update(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        self.check_binding(binding_ref)?;
        if self.binding_claim_identity.as_ref() != Some(claim_identity) {
            return Err(AccountResourceTransitionError::BindingOwnerMismatch);
        }
        let revision = self.next_revision()?;

        // The fixture models the exact-CAS update boundary. Resource metadata
        // is deliberately omitted so this does not define production payload
        // semantics.
        self.revision = revision;
        Ok(revision)
    }

    fn retire(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
        custody_disposition: BindingRetirementDisposition,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        self.check_binding(binding_ref)?;
        if self.binding_claim_identity.is_none() {
            return Err(AccountResourceTransitionError::NotBound);
        }
        if custody_disposition == BindingRetirementDisposition::Unknown {
            return Err(AccountResourceTransitionError::RetirementCustodyUnknown);
        }
        let revision = self.next_revision()?;

        self.binding_ref = None;
        self.binding_claim_identity = None;
        self.revision = revision;
        Ok(revision)
    }

    fn replace_claim(
        &mut self,
        expected_claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        replacement_claim_identity: &UserClaimIdentity,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(expected_claim_identity)?;
        self.check_revision(expected_revision)?;
        if replacement_claim_identity.bare_jid() != &self.full_jid.to_bare() {
            return Err(AccountResourceTransitionError::AccountMismatch);
        }
        if replacement_claim_identity == &self.claim_identity
            || self
                .retired_claim_identities
                .contains(replacement_claim_identity)
        {
            return Err(AccountResourceTransitionError::ClaimIdentityReused);
        }
        let revision = self.next_revision()?;

        self.retired_claim_identities
            .push(self.claim_identity.clone());
        self.claim_identity = replacement_claim_identity.clone();
        // A claim change fences the old actor without silently discarding the
        // resource. A successor bind requires an explicit retirement of this
        // exact binding first.
        self.revision = revision;
        Ok(revision)
    }

    fn check_actor(
        &self,
        claim_identity: &UserClaimIdentity,
    ) -> Result<(), AccountResourceTransitionError> {
        if claim_identity != &self.claim_identity {
            return Err(AccountResourceTransitionError::ClaimMismatch);
        }
        Ok(())
    }

    fn check_revision(
        &self,
        expected_revision: AccountResourceRevision,
    ) -> Result<(), AccountResourceTransitionError> {
        if expected_revision != self.revision {
            return Err(AccountResourceTransitionError::RevisionConflict);
        }
        Ok(())
    }

    fn check_binding(
        &self,
        binding_ref: &BindingRef,
    ) -> Result<(), AccountResourceTransitionError> {
        match self.binding_ref.as_ref() {
            Some(current) if current == binding_ref => Ok(()),
            Some(_) => Err(AccountResourceTransitionError::BindingMismatch),
            None => Err(AccountResourceTransitionError::NotBound),
        }
    }

    fn next_revision(&self) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.revision
            .next()
            .ok_or(AccountResourceTransitionError::RevisionExhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::{ClaimEpoch, ConnectionRef, NodeIdentity, NodeLeaseIdentity};

    fn claim(node_id: &str, node_epoch: &str, claim_epoch: i64) -> UserClaimIdentity {
        UserClaimIdentity::fresh(
            "alice@example.test".parse().expect("valid bare JID"),
            NodeLeaseIdentity::from_node_identity(&NodeIdentity::new(node_id, node_epoch)),
            ClaimEpoch(claim_epoch),
        )
    }

    fn slot_with_claim(claim_identity: UserClaimIdentity) -> AccountResourceSlot {
        AccountResourceSlot::new(
            "alice@example.test/phone".parse().expect("valid full JID"),
            claim_identity,
        )
        .expect("claim belongs to full JID account")
    }

    fn slot() -> AccountResourceSlot {
        slot_with_claim(claim("node-a", "epoch-a", 1))
    }

    fn binding(connection_ref: ConnectionRef) -> BindingRef {
        BindingRef::fresh(connection_ref)
    }

    fn local_connection() -> ConnectionRef {
        ConnectionRef::fresh(NodeLeaseIdentity::local_process())
    }

    fn operation(mutation: AccountResourceMutation) -> AccountResourceOperation {
        AccountResourceOperation::new(AccountResourceOperationId::fresh(), mutation)
    }

    fn bind_operation(
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
    ) -> AccountResourceOperation {
        operation(AccountResourceMutation::Bind {
            claim_identity: claim_identity.clone(),
            expected_revision,
            binding_ref: binding_ref.clone(),
        })
    }

    fn committed_revision(execution: AccountResourceExecution) -> AccountResourceRevision {
        match execution {
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
                revision,
            })
            | AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
                revision,
            }) => revision,
            other => panic!("expected committed operation, got {other:?}"),
        }
    }

    #[test]
    fn concurrent_binds_linearize_as_one_serialized_slot_mutation() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let expected_revision = slot.revision();
        let first = binding(local_connection());
        let second = binding(local_connection());

        assert_eq!(
            committed_revision(
                slot.execute(bind_operation(&claim_identity, expected_revision, &first,))
                    .expect("first bind")
            ),
            AccountResourceRevision(1)
        );
        assert_eq!(
            slot.execute(bind_operation(&claim_identity, expected_revision, &second)),
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::RevisionConflict,
                ),
            ))
        );
        assert_eq!(slot.binding_ref(), Some(&first));
        assert_eq!(slot.revision(), AccountResourceRevision(1));
    }

    #[test]
    fn same_node_and_epoch_actor_replacement_fences_old_updates_and_retires() {
        let mut slot = slot();
        let old_claim = slot.claim_identity().clone();
        let old_binding = binding(local_connection());
        committed_revision(
            slot.execute(bind_operation(&old_claim, slot.revision(), &old_binding))
                .expect("initial bind"),
        );

        let replacement = claim("node-a", "epoch-a", 1);
        assert_ne!(old_claim, replacement);
        assert_eq!(old_claim.owner(), replacement.owner());
        assert_eq!(old_claim.claim_epoch(), replacement.claim_epoch());
        committed_revision(
            slot.execute(operation(AccountResourceMutation::ReplaceClaim {
                expected_claim_identity: old_claim.clone(),
                expected_revision: slot.revision(),
                replacement_claim_identity: replacement.clone(),
            }))
            .expect("replace actor claim"),
        );
        assert_eq!(slot.binding_ref(), Some(&old_binding));
        assert_eq!(
            slot.execute(operation(AccountResourceMutation::Update {
                claim_identity: replacement.clone(),
                expected_revision: slot.revision(),
                binding_ref: old_binding.clone(),
            })),
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::BindingOwnerMismatch,
                ),
            ))
        );

        committed_revision(
            slot.execute(operation(AccountResourceMutation::Retire {
                claim_identity: replacement.clone(),
                expected_revision: slot.revision(),
                binding_ref: old_binding.clone(),
                custody_disposition: BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
            }))
            .expect("successor explicitly retires predecessor binding"),
        );
        assert_eq!(slot.binding_ref(), None);

        let new_binding = binding(old_binding.connection_ref().clone());
        committed_revision(
            slot.execute(bind_operation(&replacement, slot.revision(), &new_binding))
                .expect("successor bind on same transport"),
        );
        for mutation in [
            AccountResourceMutation::Update {
                claim_identity: old_claim.clone(),
                expected_revision: slot.revision(),
                binding_ref: new_binding.clone(),
            },
            AccountResourceMutation::Retire {
                claim_identity: old_claim.clone(),
                expected_revision: slot.revision(),
                binding_ref: new_binding.clone(),
                custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
            },
        ] {
            assert_eq!(
                slot.execute(operation(mutation)),
                Ok(AccountResourceExecution::Applied(
                    AccountResourceOperationOutcome::Rejected(
                        AccountResourceTransitionError::ClaimMismatch,
                    ),
                ))
            );
        }
        assert_eq!(slot.binding_ref(), Some(&new_binding));

        assert_eq!(
            slot.execute(operation(AccountResourceMutation::ReplaceClaim {
                expected_claim_identity: replacement.clone(),
                expected_revision: slot.revision(),
                replacement_claim_identity: old_claim,
            })),
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::ClaimIdentityReused,
                ),
            ))
        );
        assert_eq!(slot.claim_identity(), &replacement);
    }

    #[test]
    fn owner_and_claim_epoch_replacements_fence_the_previous_actor() {
        for (node_id, node_epoch, claim_epoch) in [
            ("node-b", "epoch-a", 1),
            ("node-a", "epoch-b", 1),
            ("node-a", "epoch-a", 2),
        ] {
            let mut slot = slot();
            let old_claim = slot.claim_identity().clone();
            let old_binding = binding(local_connection());
            committed_revision(
                slot.execute(bind_operation(&old_claim, slot.revision(), &old_binding))
                    .expect("initial bind"),
            );
            let replacement = claim(node_id, node_epoch, claim_epoch);
            committed_revision(
                slot.execute(operation(AccountResourceMutation::ReplaceClaim {
                    expected_claim_identity: old_claim.clone(),
                    expected_revision: slot.revision(),
                    replacement_claim_identity: replacement.clone(),
                }))
                .expect("replace owner or epoch"),
            );
            assert_eq!(slot.binding_ref(), Some(&old_binding));
            assert_eq!(
                slot.execute(operation(AccountResourceMutation::Update {
                    claim_identity: replacement.clone(),
                    expected_revision: slot.revision(),
                    binding_ref: old_binding.clone(),
                })),
                Ok(AccountResourceExecution::Applied(
                    AccountResourceOperationOutcome::Rejected(
                        AccountResourceTransitionError::BindingOwnerMismatch,
                    ),
                ))
            );
            committed_revision(
                slot.execute(operation(AccountResourceMutation::Retire {
                    claim_identity: replacement.clone(),
                    expected_revision: slot.revision(),
                    binding_ref: old_binding,
                    custody_disposition: BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
                }))
                .expect("successor explicitly retires predecessor binding"),
            );
            let current_binding = binding(local_connection());
            committed_revision(
                slot.execute(bind_operation(
                    &replacement,
                    slot.revision(),
                    &current_binding,
                ))
                .expect("successor bind"),
            );

            assert_eq!(
                slot.execute(operation(AccountResourceMutation::Retire {
                    claim_identity: old_claim,
                    expected_revision: slot.revision(),
                    binding_ref: current_binding.clone(),
                    custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
                })),
                Ok(AccountResourceExecution::Applied(
                    AccountResourceOperationOutcome::Rejected(
                        AccountResourceTransitionError::ClaimMismatch,
                    ),
                ))
            );
            assert_eq!(slot.binding_ref(), Some(&current_binding));
        }
    }

    #[test]
    fn failed_resume_and_fresh_bind_on_one_transport_have_distinct_binding_refs() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let connection = local_connection();
        let failed_resume = binding(connection.clone());
        let fresh_bind = binding(connection.clone());
        assert_eq!(failed_resume.connection_ref(), fresh_bind.connection_ref());
        assert_ne!(failed_resume, fresh_bind);

        committed_revision(
            slot.execute(bind_operation(
                &claim_identity,
                slot.revision(),
                &fresh_bind,
            ))
            .expect("fresh bind after failed resume"),
        );
        for mutation in [
            AccountResourceMutation::Update {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: failed_resume.clone(),
            },
            AccountResourceMutation::Retire {
                claim_identity,
                expected_revision: slot.revision(),
                binding_ref: failed_resume,
                custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
            },
        ] {
            assert_eq!(
                slot.execute(operation(mutation)),
                Ok(AccountResourceExecution::Applied(
                    AccountResourceOperationOutcome::Rejected(
                        AccountResourceTransitionError::BindingMismatch,
                    ),
                ))
            );
        }
        assert_eq!(slot.binding_ref(), Some(&fresh_bind));
    }

    #[test]
    fn delayed_update_and_retire_cannot_change_a_successor_binding() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let old_binding = binding(local_connection());
        committed_revision(
            slot.execute(bind_operation(
                &claim_identity,
                slot.revision(),
                &old_binding,
            ))
            .expect("initial bind"),
        );
        let old_revision = slot.revision();
        committed_revision(
            slot.execute(operation(AccountResourceMutation::Retire {
                claim_identity: claim_identity.clone(),
                expected_revision: old_revision,
                binding_ref: old_binding.clone(),
                custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
            }))
            .expect("retire old resource"),
        );
        let next_binding = binding(local_connection());
        committed_revision(
            slot.execute(bind_operation(
                &claim_identity,
                slot.revision(),
                &next_binding,
            ))
            .expect("bind successor"),
        );

        assert_eq!(
            slot.execute(operation(AccountResourceMutation::Update {
                claim_identity: claim_identity.clone(),
                expected_revision: old_revision,
                binding_ref: old_binding.clone(),
            })),
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::RevisionConflict,
                ),
            ))
        );
        for mutation in [
            AccountResourceMutation::Update {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: old_binding.clone(),
            },
            AccountResourceMutation::Retire {
                claim_identity,
                expected_revision: slot.revision(),
                binding_ref: old_binding,
                custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
            },
        ] {
            assert_eq!(
                slot.execute(operation(mutation)),
                Ok(AccountResourceExecution::Applied(
                    AccountResourceOperationOutcome::Rejected(
                        AccountResourceTransitionError::BindingMismatch,
                    ),
                ))
            );
        }
        assert_eq!(slot.binding_ref(), Some(&next_binding));
    }

    #[test]
    fn operation_replay_returns_prior_result_and_conflicting_payload_is_rejected() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        let first_operation = bind_operation(&claim_identity, slot.revision(), &binding_ref);

        let first_result = slot
            .execute(first_operation.clone())
            .expect("apply first bind");
        let revision = committed_revision(first_result);
        assert_eq!(
            slot.execute(first_operation.clone()),
            Ok(AccountResourceExecution::Replayed(
                AccountResourceOperationOutcome::Committed { revision },
            ))
        );

        let conflicting_payload = AccountResourceOperation::new(
            first_operation.id,
            AccountResourceMutation::Bind {
                claim_identity,
                expected_revision: AccountResourceRevision::INITIAL,
                binding_ref: binding(local_connection()),
            },
        );
        assert_eq!(
            slot.execute(conflicting_payload.clone()),
            Err(AccountResourceExecuteError::OperationIdConflict)
        );
        // A lost conflict reply must resolve against B's complete payload,
        // not accidentally report A's committed result under the reused ID.
        assert_eq!(
            slot.lookup(&conflicting_payload),
            AccountResourceOperationLookup::PayloadMismatch
        );
        assert_eq!(
            slot.lookup(&first_operation),
            AccountResourceOperationLookup::Recorded(&AccountResourceOperationOutcome::Committed {
                revision
            })
        );
        assert_eq!(slot.revision(), revision);
        assert_eq!(slot.binding_ref(), Some(&binding_ref));
    }

    #[test]
    fn unknown_response_can_be_looked_up_and_missing_operation_stays_unknown() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let operation = bind_operation(
            &claim_identity,
            slot.revision(),
            &binding(local_connection()),
        );
        let unknown_operation = AccountResourceOperation::new(
            AccountResourceOperationId::fresh(),
            AccountResourceMutation::Bind {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding(local_connection()),
            },
        );
        assert_eq!(
            slot.lookup(&unknown_operation),
            AccountResourceOperationLookup::Unknown
        );

        let _lost_response = slot.execute(operation.clone()).expect("commit bind");
        assert_eq!(
            slot.lookup(&operation),
            AccountResourceOperationLookup::Recorded(&AccountResourceOperationOutcome::Committed {
                revision: AccountResourceRevision(1),
            },)
        );
        assert_eq!(slot.revision(), AccountResourceRevision(1));
    }

    #[test]
    fn retired_binding_and_claim_identities_cannot_be_revived() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let retired_binding = binding(local_connection());
        committed_revision(
            slot.execute(bind_operation(
                &claim_identity,
                slot.revision(),
                &retired_binding,
            ))
            .expect("bind resource"),
        );
        assert_eq!(
            slot.execute(operation(AccountResourceMutation::Retire {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: retired_binding.clone(),
                custody_disposition: BindingRetirementDisposition::Unknown,
            })),
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::RetirementCustodyUnknown,
                ),
            ))
        );
        assert_eq!(slot.binding_ref(), Some(&retired_binding));
        committed_revision(
            slot.execute(operation(AccountResourceMutation::Retire {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: retired_binding.clone(),
                custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
            }))
            .expect("retire resource"),
        );
        assert_eq!(
            slot.execute(bind_operation(
                &claim_identity,
                slot.revision(),
                &retired_binding,
            )),
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::BindingRefReused,
                ),
            ))
        );
        assert_eq!(slot.binding_ref(), None);
    }

    #[test]
    fn claim_replacement_alone_does_not_erase_an_active_binding() {
        let mut slot = slot();
        let previous_claim = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        committed_revision(
            slot.execute(bind_operation(
                &previous_claim,
                slot.revision(),
                &binding_ref,
            ))
            .expect("bind resource"),
        );
        let replacement = claim("node-b", "epoch-b", 2);

        committed_revision(
            slot.execute(operation(AccountResourceMutation::ReplaceClaim {
                expected_claim_identity: previous_claim,
                expected_revision: slot.revision(),
                replacement_claim_identity: replacement.clone(),
            }))
            .expect("replace claim"),
        );

        assert_eq!(slot.claim_identity(), &replacement);
        assert_eq!(slot.binding_ref(), Some(&binding_ref));
        assert_eq!(
            slot.execute(operation(AccountResourceMutation::Update {
                claim_identity: replacement.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
            })),
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::BindingOwnerMismatch,
                ),
            ))
        );
        assert_eq!(
            slot.execute(operation(AccountResourceMutation::Retire {
                claim_identity: replacement.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                custody_disposition: BindingRetirementDisposition::Unknown,
            })),
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::RetirementCustodyUnknown,
                ),
            ))
        );
        assert_eq!(
            slot.execute(bind_operation(
                &replacement,
                slot.revision(),
                &binding(local_connection()),
            )),
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::AlreadyBound,
                ),
            ))
        );
        assert_eq!(slot.binding_ref(), Some(&binding_ref));
    }

    #[test]
    fn journal_and_revision_exhaustion_fail_closed() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        let bind = bind_operation(&claim_identity, slot.revision(), &binding_ref);
        committed_revision(slot.execute(bind).expect("initial bind"));

        let mut last_operation = None;
        while slot.journal.len() < ACCOUNT_RESOURCE_OPERATION_CAPACITY {
            let operation = operation(AccountResourceMutation::Update {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
            });
            committed_revision(
                slot.execute(operation.clone())
                    .expect("available journal slot"),
            );
            last_operation = Some(operation);
        }
        let revision_at_capacity = slot.revision();
        let rejected_operation = AccountResourceOperation::new(
            AccountResourceOperationId::fresh(),
            AccountResourceMutation::Update {
                claim_identity: claim_identity.clone(),
                expected_revision: revision_at_capacity,
                binding_ref: binding_ref.clone(),
            },
        );
        assert_eq!(
            slot.execute(rejected_operation.clone()),
            Err(AccountResourceExecuteError::JournalFull)
        );
        assert_eq!(slot.revision(), revision_at_capacity);
        assert_eq!(
            slot.lookup(&rejected_operation),
            AccountResourceOperationLookup::Unknown
        );
        assert!(matches!(
            slot.execute(last_operation.expect("journal has operations")),
            Ok(AccountResourceExecution::Replayed(
                AccountResourceOperationOutcome::Committed { .. }
            ))
        ));

        let mut overflow_slot = slot_with_claim(claim("node-a", "epoch-a", 1));
        let overflow_claim = overflow_slot.claim_identity().clone();
        let overflow_binding = binding(local_connection());
        committed_revision(
            overflow_slot
                .execute(bind_operation(
                    &overflow_claim,
                    overflow_slot.revision(),
                    &overflow_binding,
                ))
                .expect("bind before revision boundary"),
        );
        overflow_slot.revision = AccountResourceRevision(u64::MAX);
        let overflow = operation(AccountResourceMutation::Update {
            claim_identity: overflow_claim,
            expected_revision: overflow_slot.revision(),
            binding_ref: overflow_binding.clone(),
        });
        assert_eq!(
            overflow_slot.execute(overflow),
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::RevisionExhausted,
                ),
            ))
        );
        assert_eq!(overflow_slot.revision(), AccountResourceRevision(u64::MAX));
        assert_eq!(overflow_slot.binding_ref(), Some(&overflow_binding));
    }
}
