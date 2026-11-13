//! An in-memory account binding store for tests.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use uuid::Uuid;
use waddle_xmpp::ownership::account_coordinator::Presentation;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceExecution, AccountResourceOperation, AccountResourceOperationOutcome,
    AccountResourceRevision, ACCOUNT_RESOURCE_OPERATION_CAPACITY,
};
use waddle_xmpp::ownership::{BindingRef, SmSessionKey, UserClaimIdentity};

use super::decision::{
    check_account, decide, decide_presentation, operation_key, Decision, OperationFingerprint,
    RecordedOperation,
};
use super::error::AccountBindingStoreError;
use super::row::StoredResource;
use super::store::{AccountBindingStore, PresentationMark, StoredExecution};

#[derive(Default)]
struct State {
    resources: BTreeMap<FullJid, (StoredResource, u64)>,
    operations: HashMap<Uuid, (RecordedOperation, u64)>,
}

/// Account resource rows held in process memory, with the fences and the
/// operation record of the database store. Presence payloads survive here,
/// unlike in a database row, and a session reference is not checked,
/// because this store keeps no session relation. `set_unavailable` makes every call fail before
/// it reads or writes, to exercise a caller's handling of a store failure.
#[derive(Default)]
pub struct InMemoryAccountBindingStore {
    state: Mutex<State>,
    unavailable: AtomicBool,
}

impl InMemoryAccountBindingStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, AccountBindingStoreError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(AccountBindingStoreError::Unavailable);
        }
        self.state
            .lock()
            .map_err(|_| AccountBindingStoreError::Unavailable)
    }
}

#[async_trait]
impl AccountBindingStore for InMemoryAccountBindingStore {
    async fn execute(
        &self,
        authority: &UserClaimIdentity,
        full_jid: &FullJid,
        operation: &AccountResourceOperation,
    ) -> Result<StoredExecution, AccountBindingStoreError> {
        check_account(authority, full_jid)?;
        let fingerprint = OperationFingerprint::of(full_jid, operation);
        let mut state = self.state()?;
        if let Some((recorded, _)) = state.operations.get(&operation_key(operation)) {
            return Ok(StoredExecution {
                execution: recorded.replay(&fingerprint)?,
                stored: None,
            });
        }
        let existing = state.resources.get(full_jid).cloned();
        let previous_seq = existing.as_ref().map_or(0, |(_, seq)| *seq);
        let (revision, stored) =
            match decide(existing.map(|(stored, _)| stored), full_jid, operation)? {
                Decision::Rejected(error) => {
                    return Ok(StoredExecution {
                        execution: AccountResourceExecution::Applied(
                            AccountResourceOperationOutcome::Rejected(error),
                        ),
                        stored: None,
                    })
                }
                Decision::Committed { revision, stored } => (revision, stored),
            };
        let operation_seq = previous_seq
            .checked_add(1)
            .ok_or(AccountBindingStoreError::CounterOutOfRange)?;
        state
            .resources
            .insert(full_jid.clone(), ((*stored).clone(), operation_seq));
        state.operations.insert(
            operation_key(operation),
            (
                RecordedOperation {
                    fingerprint,
                    committed_revision: revision,
                },
                operation_seq,
            ),
        );
        let capacity = ACCOUNT_RESOURCE_OPERATION_CAPACITY as u64;
        if operation_seq > capacity {
            let oldest_kept = operation_seq - capacity;
            state.operations.retain(|_, (recorded, seq)| {
                &recorded.fingerprint.full_jid != full_jid || *seq > oldest_kept
            });
        }
        Ok(StoredExecution {
            execution: AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Committed { revision },
            ),
            stored: Some(*stored),
        })
    }

    async fn mark_presented(
        &self,
        authority: &UserClaimIdentity,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        presentation: Presentation,
    ) -> Result<PresentationMark, AccountBindingStoreError> {
        check_account(authority, full_jid)?;
        let mut state = self.state()?;
        let existing = state.resources.get(full_jid).cloned();
        let operation_seq = existing.as_ref().map_or(0, |(_, seq)| *seq);
        let (mark, write) = decide_presentation(
            existing.map(|(stored, _)| stored),
            binding_ref,
            presentation,
        );
        if let Some(stored) = write {
            state
                .resources
                .insert(full_jid.clone(), (stored, operation_seq));
        }
        Ok(mark)
    }

    async fn forget(
        &self,
        authority: &UserClaimIdentity,
        full_jid: &FullJid,
        expected_revision: AccountResourceRevision,
    ) -> Result<bool, AccountBindingStoreError> {
        check_account(authority, full_jid)?;
        let mut state = self.state()?;
        let free = state.resources.get(full_jid).is_some_and(|(stored, _)| {
            stored.image.binding.is_none() && stored.image.revision == expected_revision
        });
        if free {
            state.resources.remove(full_jid);
            state
                .operations
                .retain(|_, (recorded, _)| &recorded.fingerprint.full_jid != full_jid);
        }
        Ok(free)
    }

    async fn load(
        &self,
        full_jid: &FullJid,
    ) -> Result<Option<StoredResource>, AccountBindingStoreError> {
        Ok(self
            .state()?
            .resources
            .get(full_jid)
            .map(|(stored, _)| stored.clone()))
    }

    async fn resources_for_session(
        &self,
        session: SmSessionKey,
    ) -> Result<Vec<FullJid>, AccountBindingStoreError> {
        Ok(self
            .state()?
            .resources
            .iter()
            .filter(|(_, (stored, _))| {
                stored
                    .image
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.snapshot.stream_management_ref)
                    .is_some_and(|reference| reference.session == session)
            })
            .map(|(jid, _)| jid.clone())
            .collect())
    }

    /// This store keeps no session relation, so every session a binding
    /// names is taken as resumable and its session authority decides.
    async fn session_resumable(
        &self,
        _session: SmSessionKey,
    ) -> Result<bool, AccountBindingStoreError> {
        drop(self.state()?);
        Ok(true)
    }

    async fn bound_accounts(
        &self,
        after: Option<&BareJid>,
        limit: u32,
    ) -> Result<Vec<BareJid>, AccountBindingStoreError> {
        let accounts: std::collections::BTreeSet<BareJid> = self
            .state()?
            .resources
            .iter()
            .filter(|(_, (stored, _))| stored.image.binding.is_some())
            .map(|(jid, _)| jid.to_bare())
            .filter(|account| after.is_none_or(|after| account > after))
            .collect();
        Ok(accounts
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .collect())
    }

    async fn load_account(
        &self,
        account: &BareJid,
    ) -> Result<Vec<StoredResource>, AccountBindingStoreError> {
        Ok(self
            .state()?
            .resources
            .iter()
            .filter(|(jid, _)| &jid.to_bare() == account)
            .map(|(_, (stored, _))| stored.clone())
            .collect())
    }
}
