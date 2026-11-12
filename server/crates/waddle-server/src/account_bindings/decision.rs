//! The transition a store commits, decided by the pure slot model.

use jid::FullJid;
use uuid::Uuid;
use waddle_xmpp::ownership::account_coordinator::{
    BindingFenceError, Presentation, PresentationFlags,
};
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceExecuteError, AccountResourceExecution, AccountResourceMutation,
    AccountResourceOperation, AccountResourceOperationOutcome, AccountResourceRevision,
    AccountResourceSlot, AccountResourceTransitionError,
};
use waddle_xmpp::ownership::{BindingRef, UserClaimIdentity};

use super::error::AccountBindingStoreError;
use super::row::StoredResource;
use super::store::PresentationMark;

/// What the store does with one operation against one stored row.
pub(super) enum Decision {
    /// The pure model refused the operation; nothing is written.
    Rejected(AccountResourceTransitionError),
    /// The operation committed; `stored` is the row to write.
    Committed {
        revision: AccountResourceRevision,
        stored: Box<StoredResource>,
    },
}

/// Apply `operation` to the slot rebuilt from `existing`. Without a row,
/// only a `Bind` proceeds, on a new slot under the claim the bind names.
pub(super) fn decide(
    existing: Option<StoredResource>,
    full_jid: &FullJid,
    operation: &AccountResourceOperation,
) -> Result<Decision, AccountBindingStoreError> {
    let (mut slot, presented) = match existing {
        Some(stored) => (
            AccountResourceSlot::restore(stored.image)?,
            stored.presented,
        ),
        None => {
            if !matches!(operation.mutation, AccountResourceMutation::Bind { .. }) {
                return Err(AccountBindingStoreError::UnknownResource);
            }
            let slot = AccountResourceSlot::new(
                full_jid.clone(),
                operation.mutation.claim_identity().clone(),
            )
            .map_err(|_| AccountBindingStoreError::AccountMismatch)?;
            (slot, PresentationFlags::default())
        }
    };
    if slot.full_jid() != full_jid {
        return Err(AccountBindingStoreError::CorruptRow { field: "full_jid" });
    }
    let execution = slot
        .execute(operation.clone())
        .map_err(|error| match error {
            AccountResourceExecuteError::OperationIdConflict => {
                AccountBindingStoreError::OperationIdConflict
            }
            AccountResourceExecuteError::SnapshotTooLarge => {
                AccountBindingStoreError::SnapshotTooLarge
            }
        })?;
    match execution {
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
            revision,
        }) => {
            // The flags belong to the binding's session or binding. A slot
            // whose retirement completed holds neither.
            let presented = if slot.binding_ref().is_some() {
                presented
            } else {
                PresentationFlags::default()
            };
            Ok(Decision::Committed {
                revision,
                stored: Box::new(StoredResource {
                    image: slot.image(),
                    presented,
                }),
            })
        }
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Rejected(error))
        | AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Rejected(error)) => {
            Ok(Decision::Rejected(error))
        }
        // A rebuilt slot has an empty journal, so it cannot replay a commit.
        AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
            ..
        }) => Err(AccountBindingStoreError::OperationIdConflict),
    }
}

/// The typed facts that identify a recorded operation. Repeating an
/// operation identity must repeat all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OperationFingerprint {
    pub(super) full_jid: FullJid,
    pub(super) kind: MutationKind,
    pub(super) binding_ref: BindingRef,
    pub(super) expected_revision: AccountResourceRevision,
}

impl OperationFingerprint {
    pub(super) fn of(full_jid: &FullJid, operation: &AccountResourceOperation) -> Self {
        Self {
            full_jid: full_jid.clone(),
            kind: MutationKind::of(&operation.mutation),
            binding_ref: operation.mutation.binding_ref().clone(),
            expected_revision: operation.mutation.expected_revision(),
        }
    }
}

/// A committed operation as the operations relation records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecordedOperation {
    pub(super) fingerprint: OperationFingerprint,
    pub(super) committed_revision: AccountResourceRevision,
}

impl RecordedOperation {
    /// The replayed outcome for a repeat of the recorded operation, or a
    /// conflict when the identity is reused for a different one.
    pub(super) fn replay(
        &self,
        fingerprint: &OperationFingerprint,
    ) -> Result<AccountResourceExecution, AccountBindingStoreError> {
        if &self.fingerprint != fingerprint {
            return Err(AccountBindingStoreError::OperationIdConflict);
        }
        Ok(AccountResourceExecution::Replayed(
            AccountResourceOperationOutcome::Committed {
                revision: self.committed_revision,
            },
        ))
    }
}

pub(super) fn operation_key(operation: &AccountResourceOperation) -> Uuid {
    operation.id.as_uuid()
}

/// Refuse a write whose authority is not a claim on the resource's account.
pub(super) fn check_account(
    authority: &UserClaimIdentity,
    full_jid: &FullJid,
) -> Result<(), AccountBindingStoreError> {
    if authority.bare_jid() == &full_jid.to_bare() {
        Ok(())
    } else {
        Err(AccountBindingStoreError::AccountMismatch)
    }
}

/// Which of the ten mutations an operation is, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MutationKind {
    Bind,
    PublishEndpoint,
    RegisterActor,
    UpdateSnapshot,
    Detach,
    SuccessfulResume,
    BeginRetirement,
    CompleteRetirement,
    ReplaceClaim,
    AdoptBinding,
}

impl MutationKind {
    fn of(mutation: &AccountResourceMutation) -> Self {
        match mutation {
            AccountResourceMutation::Bind { .. } => Self::Bind,
            AccountResourceMutation::PublishEndpoint { .. } => Self::PublishEndpoint,
            AccountResourceMutation::RegisterActor { .. } => Self::RegisterActor,
            AccountResourceMutation::UpdateSnapshot { .. } => Self::UpdateSnapshot,
            AccountResourceMutation::Detach { .. } => Self::Detach,
            AccountResourceMutation::SuccessfulResume { .. } => Self::SuccessfulResume,
            AccountResourceMutation::BeginRetirement { .. } => Self::BeginRetirement,
            AccountResourceMutation::CompleteRetirement { .. } => Self::CompleteRetirement,
            AccountResourceMutation::ReplaceClaim { .. } => Self::ReplaceClaim,
            AccountResourceMutation::AdoptBinding { .. } => Self::AdoptBinding,
        }
    }

    pub(super) const fn to_storage(self) -> i64 {
        match self {
            Self::Bind => 1,
            Self::PublishEndpoint => 2,
            Self::RegisterActor => 3,
            Self::UpdateSnapshot => 4,
            Self::Detach => 5,
            Self::SuccessfulResume => 6,
            Self::BeginRetirement => 7,
            Self::CompleteRetirement => 8,
            Self::ReplaceClaim => 9,
            Self::AdoptBinding => 10,
        }
    }

    pub(super) fn from_storage(value: i64) -> Result<Self, AccountBindingStoreError> {
        Ok(match value {
            1 => Self::Bind,
            2 => Self::PublishEndpoint,
            3 => Self::RegisterActor,
            4 => Self::UpdateSnapshot,
            5 => Self::Detach,
            6 => Self::SuccessfulResume,
            7 => Self::BeginRetirement,
            8 => Self::CompleteRetirement,
            9 => Self::ReplaceClaim,
            10 => Self::AdoptBinding,
            _ => {
                return Err(AccountBindingStoreError::CorruptRow {
                    field: "operation kind",
                })
            }
        })
    }
}

/// Decide a presentation mark against a stored row. Returns the mark and,
/// for the first mark, the row to write.
pub(super) fn decide_presentation(
    existing: Option<StoredResource>,
    binding_ref: &BindingRef,
    presentation: Presentation,
) -> (PresentationMark, Option<StoredResource>) {
    let Some(mut stored) = existing else {
        return (
            PresentationMark::Fenced(BindingFenceError::UnknownResource),
            None,
        );
    };
    match stored.image.binding.as_ref() {
        None => return (PresentationMark::Fenced(BindingFenceError::NotBound), None),
        Some(binding) if &binding.binding_ref != binding_ref => {
            return (
                PresentationMark::Fenced(BindingFenceError::Superseded),
                None,
            )
        }
        Some(_) => {}
    }
    let flag = match presentation {
        Presentation::OfflineMessages => &mut stored.presented.offline_messages,
        Presentation::PendingSubscriptions => &mut stored.presented.pending_subscriptions,
    };
    if *flag {
        return (PresentationMark::AlreadyPresented, None);
    }
    *flag = true;
    (PresentationMark::First, Some(stored))
}
