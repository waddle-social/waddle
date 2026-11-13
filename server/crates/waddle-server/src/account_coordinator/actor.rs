//! The coordinator actor of one account.

use std::collections::BTreeMap;
use std::sync::Arc;

use jid::{BareJid, FullJid};
use kameo::message::{Context, Message};
use kameo::Actor;
use tokio::time::Instant;
use tracing::warn;
use waddle_xmpp::ownership::account_coordinator::{AccountResources, AccountResourcesError};
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindSuccessReceipt, AccountResourceBindingState, AccountResourceExecution,
    AccountResourceMutation, AccountResourceOperation, AccountResourceOperationId,
    AccountResourceOperationLookup, AccountResourceOperationOutcome,
    AccountResourceRetirementEvidence, AccountResourceRevision, AccountResourceSlot,
    AccountResourceTransitionError, ActorRegistrationReceipt, ActorUnregistrationEvidence,
    BindingEvidence, BindingRetirementDisposition, DirectedPresenceTeardownReceipt,
    EndpointRetirementEvidence, PresentationFlushReceipt,
};
use waddle_xmpp::ownership::{BindingRef, SmSessionKey, UserClaimIdentity};
use waddle_xmpp::telemetry::account_coordinator::{record_operation, record_slots_in_use};
use waddle_xmpp::telemetry::attributes::{AccountMutationKind, AccountOperationOutcome};

use super::claims::{
    AccountClaimError, AccountClaimSource, BindingLiveness, ClaimNeed, ClaimRelease,
    ConnectionLiveness,
};
use super::config::AccountCoordinatorConfig;
use super::error::AccountCoordinatorError;
use super::messages::{
    AccountView, Adopt, AdoptionReport, BindSuccess, BindingReply, Execute, ExpiredBindings,
    MarkPresented, OverdueRetirements, ReadView, ReclaimIfIdle, ReleaseClaimIfUnbound,
    ReleaseSession, Repair, RepairEvidence, RepairOutcome, SessionRelease, SettleExpired,
    SlotStatus, Status, Unregister, UnregisterOutcome, UnregisterRequest,
};
use super::publisher::RetireEvidence;
use crate::account_bindings::{
    AccountBindingStore, AccountBindingStoreError, PresentationMark, StoredResource,
};

type Reply<T> = Result<T, AccountCoordinatorError>;

/// A retirement that has started and awaits its endpoint's evidence.
struct PendingRetirement {
    full_jid: FullJid,
    binding_ref: BindingRef,
    deadline: Instant,
}

/// A bind that waits for the resource's current binding to retire. The
/// resource is reserved for `replacement` until `deadline`.
struct PendingReplacement {
    replacement: BindingRef,
    deadline: Instant,
}

/// The single authority for the resources of one bare JID under one claim
/// identity.
///
/// The actor's mailbox serializes every read and mutation. A mutation is
/// committed by the binding store first and applied to the in-memory slots
/// second. When the store's commit outcome is unknown the in-memory slots
/// are dropped and reloaded from the store before the next request, so
/// selection never reflects a mutation that did not commit.
///
/// The actor holds the account's claim from its creation until the
/// account's last binding is retired, when it releases the claim, or until
/// the store reports that the claim is no longer held. Without a claim it
/// keeps no slots. The next request that acts for a binding takes the
/// claim again and loads the rows under the new identity; a read is
/// answered only if this node holds a claim by then.
#[derive(Actor)]
pub(super) struct AccountCoordinatorActor {
    account: BareJid,
    /// `None` after the claim was released or lost.
    claim_identity: Option<UserClaimIdentity>,
    store: Arc<dyn AccountBindingStore>,
    claims: Arc<dyn AccountClaimSource>,
    liveness: Arc<dyn ConnectionLiveness>,
    config: AccountCoordinatorConfig,
    /// `None` until the account's rows are loaded, and again after a store
    /// failure that leaves a commit outcome unknown.
    resources: Option<AccountResources>,
    adoption: AdoptionReport,
    retirements: Vec<PendingRetirement>,
    replacements: BTreeMap<FullJid, PendingReplacement>,
    last_activity: Instant,
}

impl AccountCoordinatorActor {
    pub(super) fn new(
        claim_identity: UserClaimIdentity,
        store: Arc<dyn AccountBindingStore>,
        claims: Arc<dyn AccountClaimSource>,
        liveness: Arc<dyn ConnectionLiveness>,
        config: AccountCoordinatorConfig,
    ) -> Self {
        Self {
            account: claim_identity.bare_jid().clone(),
            claim_identity: Some(claim_identity),
            store,
            claims,
            liveness,
            config,
            resources: None,
            adoption: AdoptionReport::default(),
            retirements: Vec::new(),
            replacements: BTreeMap::new(),
            last_activity: Instant::now(),
        }
    }

    fn resources(&mut self) -> Reply<&mut AccountResources> {
        self.resources
            .as_mut()
            .ok_or(AccountCoordinatorError::Diverged)
    }

    /// The claim this coordinator holds. Callers establish it first, with
    /// [`Self::ensure_claim`] or [`Self::ensure_loaded`].
    fn claim(&self) -> Reply<&UserClaimIdentity> {
        self.claim_identity
            .as_ref()
            .ok_or(AccountCoordinatorError::Claim(AccountClaimError::Unclaimed))
    }

    /// Hold a claim on the account again after this coordinator released
    /// or lost its own. Nothing in memory describes the account under the
    /// new identity, so the slots are loaded afresh.
    async fn ensure_claim(&mut self, need: ClaimNeed) -> Reply<()> {
        if self.claim_identity.is_some() {
            return Ok(());
        }
        let claim = match need {
            ClaimNeed::Existing => self.claims.current_claim(&self.account).await?,
            ClaimNeed::Taken => self.claims.acquire_claim(&self.account).await?,
        };
        self.claim_identity = Some(claim);
        self.forget_account_state();
        Ok(())
    }

    fn forget_account_state(&mut self) {
        self.resources = None;
        self.adoption = AdoptionReport::default();
        self.retirements.clear();
        self.replacements.clear();
    }

    /// Stop acting under a claim the store reports as no longer held: it
    /// was released, or another node took the account over.
    fn note_claim_loss<T>(&mut self, reply: &Reply<T>) {
        if matches!(
            reply,
            Err(AccountCoordinatorError::Store(
                AccountBindingStoreError::ClaimNotHeld
            ))
        ) {
            self.claim_identity = None;
            self.forget_account_state();
        }
    }

    /// Release the account's claim when the account holds no binding and
    /// nothing is pending for it, so another node can host the account's
    /// next binding. The free rows are deleted first, because only the
    /// claim's holder may delete them. Returns whether the claim was
    /// released.
    ///
    /// A claim the source keeps, because the `UserActor` mirror still
    /// holds it, is released when the mirror gives its hold up and asks
    /// again.
    async fn release_claim_if_unbound(&mut self) -> bool {
        self.try_release_claim().await == Some(ClaimRelease::Released)
    }

    /// [`Self::release_claim_if_unbound`], telling a claim that stays
    /// (`Retained`) from a release that failed (`None`).
    async fn try_release_claim(&mut self) -> Option<ClaimRelease> {
        if !self.claims.releases_claims() {
            return Some(ClaimRelease::Retained);
        }
        self.expire_replacements();
        let Some(claim) = self.claim_identity.clone() else {
            return Some(ClaimRelease::Retained);
        };
        let Some(resources) = self.resources.as_ref() else {
            return Some(ClaimRelease::Retained);
        };
        if resources.slots().any(|slot| slot.binding_ref().is_some())
            || !self.retirements.is_empty()
            || !self.replacements.is_empty()
        {
            return Some(ClaimRelease::Retained);
        }
        for slot in resources.slots() {
            if let Err(error) = self
                .store
                .forget(&claim, slot.full_jid(), slot.revision())
                .await
            {
                warn!(%error, "account coordinator could not delete a free resource row");
            }
        }
        // The rows of the free slots are gone, so the slots are read again
        // if the claim stays.
        self.resources = None;
        match self.claims.release_claim(&claim).await {
            Ok(ClaimRelease::Released) => {
                self.claim_identity = None;
                self.forget_account_state();
                Some(ClaimRelease::Released)
            }
            Ok(ClaimRelease::Retained) => Some(ClaimRelease::Retained),
            Err(error) => {
                warn!(%error, "account coordinator could not release its account claim");
                None
            }
        }
    }

    async fn ensure_loaded(&mut self) -> Reply<()> {
        self.ensure_claim(ClaimNeed::Taken).await?;
        if self.resources.is_none() {
            let loaded = self.load().await;
            self.note_claim_loss(&loaded);
            self.resources = Some(loaded?);
            // The rows may name connections that are gone. They are
            // settled before the slots answer anything, so a binding on a
            // lost connection is never served as a destination. The call
            // is boxed because settling runs the retirement path, which
            // itself requires loaded slots.
            let settled = Box::pin(self.settle_expired()).await;
            if settled.is_err() {
                self.resources = None;
            }
            self.note_claim_loss(&settled);
            settled?;
        }
        Ok(())
    }

    /// Build the account's slots from its stored rows. A row that holds no
    /// binding is deleted: it carries nothing a later binding needs. A row
    /// written under a previous claim identity is moved to this one first.
    /// A binding the rows show as retiring awaits its evidence again, with
    /// a deadline counted from now.
    async fn load(&mut self) -> Reply<AccountResources> {
        let store = Arc::clone(&self.store);
        let claim = self.claim()?.clone();
        let rows = store.load_account(&self.account).await?;
        let mut resources = AccountResources::new(claim.clone(), self.config.max_resources);
        let mut report = AdoptionReport::default();
        let mut retirements = Vec::new();
        for row in rows {
            if row.image.binding.is_none() {
                store
                    .forget(&claim, &row.image.full_jid, row.image.revision)
                    .await?;
                continue;
            }
            let stored = if row.image.claim_identity == claim {
                Some(row)
            } else {
                self.take_over(&claim, row, &mut report).await?
            };
            if let Some(stored) = stored {
                if let Some(binding) = stored
                    .image
                    .binding
                    .as_ref()
                    .filter(|binding| binding.state == AccountResourceBindingState::Retiring)
                {
                    retirements.push(PendingRetirement {
                        full_jid: stored.image.full_jid.clone(),
                        binding_ref: binding.binding_ref.clone(),
                        deadline: Instant::now() + self.config.retirement_deadline,
                    });
                }
                install(&mut resources, stored)?;
            }
        }
        self.retirements = retirements;
        self.adoption = report;
        Ok(resources)
    }

    /// Move a row written under a previous claim identity to this one, and
    /// adopt its binding when it is detached, when its connection is live,
    /// or when it is active on a lost connection and names a
    /// stream-management session: right after the load that one is
    /// detached when its session can resume, so the session keeps its
    /// resume right, and retired when it has ended. A binding that is not
    /// adopted keeps its previous binding claim, which keeps it out of
    /// selection.
    async fn take_over(
        &self,
        claim: &UserClaimIdentity,
        row: StoredResource,
        report: &mut AdoptionReport,
    ) -> Reply<Option<StoredResource>> {
        let Some(binding) = row.image.binding.clone() else {
            return Ok(Some(row));
        };
        let full_jid = row.image.full_jid.clone();
        let replace = fresh_operation(AccountResourceMutation::ReplaceClaim {
            claim_identity: row.image.claim_identity.clone(),
            expected_revision: row.image.revision,
            binding_ref: binding.binding_ref.clone(),
            replacement_claim_identity: claim.clone(),
        });
        let revision = match self
            .store
            .execute(claim, &full_jid, &replace)
            .await?
            .execution
        {
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
                revision,
            })
            | AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
                revision,
            }) => revision,
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Rejected(error))
            | AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Rejected(
                error,
            )) => {
                report
                    .refused
                    .push((full_jid, binding.binding_ref.clone(), error));
                return Ok(Some(row));
            }
        };
        let session = binding
            .snapshot
            .stream_management_ref
            .map(|reference| reference.session);
        // A detached binding has no connection whose liveness could matter.
        let liveness = if binding.state == AccountResourceBindingState::Detached {
            BindingLiveness::Unknown
        } else {
            self.liveness.liveness(&binding.binding_ref, session).await
        };
        let adoptable = match binding.state {
            AccountResourceBindingState::Detached => true,
            AccountResourceBindingState::Prepared | AccountResourceBindingState::Published => {
                liveness == BindingLiveness::Live
            }
            AccountResourceBindingState::Active => {
                liveness == BindingLiveness::Live
                    || (liveness == BindingLiveness::Expired && session.is_some())
            }
            AccountResourceBindingState::Retiring | AccountResourceBindingState::Retired => false,
        };
        if !adoptable {
            let entry = (full_jid.clone(), binding.binding_ref.clone());
            if liveness == BindingLiveness::Expired {
                report.expired.push(entry);
            } else {
                report.unadopted.push(entry);
            }
            return Ok(self.store.load(&full_jid).await?);
        }
        let adopt = fresh_operation(AccountResourceMutation::AdoptBinding {
            claim_identity: claim.clone(),
            expected_revision: revision,
            binding_ref: binding.binding_ref.clone(),
            expected_binding_claim_identity: binding.binding_claim_identity.clone(),
            actor_registration: binding.registered.then(|| {
                ActorRegistrationReceipt::observed(
                    claim.clone(),
                    BindingEvidence::for_binding(&binding.binding_ref),
                )
            }),
        });
        match self
            .store
            .execute(claim, &full_jid, &adopt)
            .await?
            .execution
        {
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
                ..
            })
            | AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
                ..
            }) => report
                .adopted
                .push((full_jid.clone(), binding.binding_ref.clone())),
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Rejected(error))
            | AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Rejected(
                error,
            )) => report
                .refused
                .push((full_jid.clone(), binding.binding_ref.clone(), error)),
        }
        Ok(self.store.load(&full_jid).await?)
    }

    /// Settle every binding that can no longer serve its resource. An
    /// active binding on a lost connection that this claim holds and whose
    /// stream-management session can still resume is detached, and the
    /// slot keeps the snapshot a resume inherits. A detached binding whose
    /// session has ended has nothing left to resume. Each of those that is
    /// not detached is retired with the recovery paths as the owner of what
    /// it accepted. A detached binding whose session can resume is left to
    /// that session; a binding whose liveness is unknown is neither
    /// detached nor retired. Returns how many bindings were retired.
    async fn settle_expired(&mut self) -> Reply<usize> {
        let claim = self.claim()?.clone();
        let candidates: Vec<_> = self
            .resources()?
            .slots()
            .filter_map(|slot| {
                let binding = slot.binding_receipt()?;
                Some((
                    slot.full_jid().clone(),
                    binding.binding_ref().clone(),
                    binding.state(),
                    binding
                        .snapshot()
                        .stream_management_ref
                        .map(|reference| reference.session),
                ))
            })
            .collect();
        let mut retired = 0;
        for (full_jid, binding_ref, state, session) in candidates {
            if state == AccountResourceBindingState::Detached {
                let Some(session) = session else {
                    continue;
                };
                if self.session_resumable(session).await {
                    continue;
                }
            } else {
                if self.liveness.liveness(&binding_ref, session).await != BindingLiveness::Expired {
                    continue;
                }
                let resumable = match session {
                    Some(session) => self.session_resumable(session).await,
                    None => false,
                };
                if resumable && self.detach_expired(&claim, &full_jid, &binding_ref).await? {
                    continue;
                }
            }
            let evidence = RetireEvidence::recovery_owned();
            let started = self
                .unregister(Unregister {
                    full_jid: full_jid.clone(),
                    request: UnregisterRequest {
                        operation: evidence.begin,
                        binding_ref: binding_ref.clone(),
                        custody_disposition: evidence.custody_disposition,
                    },
                })
                .await?;
            if !matches!(started, UnregisterOutcome::Retiring { .. }) {
                continue;
            }
            let completed = self
                .repair(Repair {
                    full_jid,
                    binding_ref,
                    evidence: RepairEvidence {
                        operation: evidence.complete,
                        directed_presence: evidence.directed_presence,
                        presentation_flush: evidence.presentation_flush,
                    },
                })
                .await?;
            if matches!(completed, RepairOutcome::Completed { .. }) {
                retired += 1;
            }
        }
        Ok(retired)
    }

    /// Whether the stream-management session a binding names can still
    /// resume. A read that fails counts as resumable: a detached binding
    /// loses nothing, and the session's authority releases it once the
    /// session ends.
    async fn session_resumable(&self, session: SmSessionKey) -> bool {
        match self.store.session_resumable(session).await {
            Ok(resumable) => resumable,
            Err(error) => {
                tracing::debug!(%error, "a binding's session was not read; it is kept resumable");
                true
            }
        }
    }

    /// Detach an active binding on a lost connection when it names a
    /// stream-management session and this claim holds it. Returns whether
    /// the slot now holds it as detached.
    async fn detach_expired(
        &mut self,
        claim: &UserClaimIdentity,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
    ) -> Reply<bool> {
        let Some(slot) = self.resources()?.slot(full_jid) else {
            return Ok(false);
        };
        let Some(binding) = slot.binding_receipt().filter(|binding| {
            binding.binding_ref() == binding_ref
                && binding.state() == AccountResourceBindingState::Active
                && binding.binding_claim_identity() == claim
                && binding.snapshot().stream_management_ref.is_some()
        }) else {
            return Ok(false);
        };
        let Some(publication) = binding.publication().cloned() else {
            return Ok(false);
        };
        let detach = fresh_operation(AccountResourceMutation::Detach {
            claim_identity: slot.claim_identity().clone(),
            expected_revision: slot.revision(),
            binding_ref: binding_ref.clone(),
            endpoint: EndpointRetirementEvidence::Unpublished {
                publication,
                removed: BindingEvidence::for_binding(binding_ref),
            },
            custody_disposition: BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        });
        Ok(matches!(
            self.execute(full_jid.clone(), detach).await?,
            BindingReply::Executed(
                AccountResourceExecution::Applied(
                    AccountResourceOperationOutcome::Committed { .. }
                ) | AccountResourceExecution::Replayed(
                    AccountResourceOperationOutcome::Committed { .. }
                )
            )
        ))
    }

    /// Replace one in-memory slot with the stored row. When the store has
    /// no row for a resource the account still holds, every slot is
    /// reloaded before the next request.
    async fn reload_slot(&mut self, full_jid: &FullJid) -> Reply<()> {
        let store = Arc::clone(&self.store);
        match store.load(full_jid).await? {
            Some(stored) => install(self.resources()?, stored),
            None => {
                if !self.resources()?.forget_free_slot(full_jid)
                    && self.resources()?.slot(full_jid).is_some()
                {
                    self.resources = None;
                }
                Ok(())
            }
        }
    }

    /// Delete free slots, durably and in memory, until the account can
    /// take another resource.
    async fn make_room(&mut self) -> Reply<()> {
        let store = Arc::clone(&self.store);
        let claim = self.claim()?.clone();
        let limit = self.config.max_resources.get();
        if self.resources()?.slot_count() < limit {
            return Ok(());
        }
        for full_jid in self.resources()?.free_slots() {
            let revision = self.resources()?.slot(&full_jid).map_or(
                AccountResourceRevision::INITIAL,
                AccountResourceSlot::revision,
            );
            if store.forget(&claim, &full_jid, revision).await?
                || store.load(&full_jid).await?.is_none()
            {
                self.resources()?.forget_free_slot(&full_jid);
            }
            if self.resources()?.slot_count() < limit {
                return Ok(());
            }
        }
        Err(AccountCoordinatorError::Resources(
            AccountResourcesError::ResourceLimit,
        ))
    }

    fn expire_replacements(&mut self) {
        let now = Instant::now();
        self.replacements
            .retain(|_, pending| pending.deadline > now);
    }

    async fn execute(
        &mut self,
        full_jid: FullJid,
        operation: AccountResourceOperation,
    ) -> Reply<BindingReply> {
        self.ensure_loaded().await?;
        let claim = self.claim()?.clone();
        if full_jid.to_bare() != self.account {
            return Err(AccountCoordinatorError::Resources(
                AccountResourcesError::AccountMismatch,
            ));
        }
        let (known, current, revision) = match self.resources()?.slot(&full_jid) {
            Some(slot) => {
                match slot.lookup(&operation) {
                    AccountResourceOperationLookup::Recorded(outcome) => {
                        return Ok(BindingReply::Executed(AccountResourceExecution::Replayed(
                            *outcome,
                        )));
                    }
                    AccountResourceOperationLookup::PayloadMismatch => {
                        return Err(AccountCoordinatorError::OperationIdConflict);
                    }
                    AccountResourceOperationLookup::Unknown => {}
                }
                (true, slot.binding_ref().cloned(), slot.revision())
            }
            None => (false, None, AccountResourceRevision::INITIAL),
        };
        match &operation.mutation {
            AccountResourceMutation::Bind {
                claim_identity,
                binding_ref,
                ..
            } => {
                self.expire_replacements();
                if self
                    .replacements
                    .get(&full_jid)
                    .is_some_and(|pending| &pending.replacement != binding_ref)
                {
                    return Err(AccountCoordinatorError::ReplacementAlreadyPending);
                }
                if let Some(displaced) = current.filter(|current| current != binding_ref) {
                    self.replacements.insert(
                        full_jid,
                        PendingReplacement {
                            replacement: binding_ref.clone(),
                            deadline: Instant::now() + self.config.replacement_deadline,
                        },
                    );
                    return Ok(BindingReply::ReplacementPending {
                        displaced,
                        revision,
                    });
                }
                if !known {
                    // A new slot is created under this coordinator's claim,
                    // so a bind under any other claim cannot create one.
                    if claim_identity != &claim {
                        return Ok(BindingReply::Executed(AccountResourceExecution::Applied(
                            AccountResourceOperationOutcome::Rejected(
                                AccountResourceTransitionError::ClaimMismatch,
                            ),
                        )));
                    }
                    self.make_room().await?;
                }
            }
            AccountResourceMutation::BeginRetirement { .. }
                if self.retirements.len() >= self.config.max_pending_retirements =>
            {
                return Err(AccountCoordinatorError::RetirementBacklogFull);
            }
            _ if !known => {
                return Err(AccountCoordinatorError::Resources(
                    AccountResourcesError::UnknownResource,
                ));
            }
            _ => {}
        }

        let store = Arc::clone(&self.store);
        let stored = match store.execute(&claim, &full_jid, &operation).await {
            Ok(stored) => stored,
            Err(AccountBindingStoreError::OperationIdConflict) => {
                return Err(AccountCoordinatorError::OperationIdConflict);
            }
            Err(error) => {
                if matches!(
                    error,
                    AccountBindingStoreError::Database(_) | AccountBindingStoreError::Unavailable
                ) {
                    // The commit may have happened. Selection must not keep
                    // serving slots that may be behind the stored rows.
                    self.resources = None;
                }
                return Err(error.into());
            }
        };
        match stored.execution {
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
                revision,
            }) => {
                let row = stored.stored.ok_or(AccountCoordinatorError::Diverged)?;
                let agreed = matches!(
                    self.resources()?.execute(&full_jid, operation.clone()),
                    Ok(AccountResourceExecution::Applied(
                        AccountResourceOperationOutcome::Committed { revision: applied }
                    )) if applied == revision
                );
                if !agreed {
                    install(self.resources()?, row)?;
                }
                self.after_commit(&full_jid, &operation.mutation);
            }
            AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
                ..
            }) => {
                // The store committed this operation before these slots
                // were loaded or reloaded; they may not reflect it.
                self.reload_slot(&full_jid).await?;
            }
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Rejected(_))
            | AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Rejected(_)) => {
                if known {
                    let agreed = matches!(
                        self.resources()?.execute(&full_jid, operation.clone()),
                        Ok(AccountResourceExecution::Applied(
                            AccountResourceOperationOutcome::Rejected(_)
                        ))
                    );
                    if !agreed {
                        self.reload_slot(&full_jid).await?;
                    }
                }
            }
        }
        Ok(BindingReply::Executed(stored.execution))
    }

    fn after_commit(&mut self, full_jid: &FullJid, mutation: &AccountResourceMutation) {
        match mutation {
            AccountResourceMutation::Bind { binding_ref, .. } => {
                if self
                    .replacements
                    .get(full_jid)
                    .is_some_and(|pending| &pending.replacement == binding_ref)
                {
                    self.replacements.remove(full_jid);
                }
            }
            AccountResourceMutation::BeginRetirement { binding_ref, .. } => {
                self.retirements.push(PendingRetirement {
                    full_jid: full_jid.clone(),
                    binding_ref: binding_ref.clone(),
                    deadline: Instant::now() + self.config.retirement_deadline,
                });
            }
            AccountResourceMutation::CompleteRetirement { binding_ref, .. } => {
                self.retirements.retain(|pending| {
                    &pending.full_jid != full_jid || &pending.binding_ref != binding_ref
                });
            }
            _ => {}
        }
        if let Some(resources) = self.resources.as_ref() {
            record_slots_in_use(resources.slot_count());
        }
    }

    async fn mark_presented(&mut self, message: MarkPresented) -> Reply<bool> {
        self.ensure_claim(ClaimNeed::Existing).await?;
        self.ensure_loaded().await?;
        let MarkPresented {
            full_jid,
            binding_ref,
            presentation,
        } = message;
        if self
            .resources()?
            .presented(&full_jid, &binding_ref, presentation)
            .map_err(AccountCoordinatorError::Fenced)?
        {
            return Ok(false);
        }
        let store = Arc::clone(&self.store);
        let claim = self.claim()?.clone();
        let mark = store
            .mark_presented(&claim, &full_jid, &binding_ref, presentation)
            .await?;
        match mark {
            PresentationMark::First | PresentationMark::AlreadyPresented => {
                self.resources()?
                    .mark_presented(&full_jid, &binding_ref, presentation)
                    .map_err(AccountCoordinatorError::Fenced)?;
                Ok(mark == PresentationMark::First)
            }
            PresentationMark::Fenced(error) => {
                self.reload_slot(&full_jid).await?;
                Err(AccountCoordinatorError::Fenced(error))
            }
        }
    }

    async fn unregister(&mut self, message: Unregister) -> Reply<UnregisterOutcome> {
        self.ensure_loaded().await?;
        let Unregister { full_jid, request } = message;
        if let Err(error) = self
            .resources()?
            .check_fence(&full_jid, &request.binding_ref)
        {
            return Ok(UnregisterOutcome::Stale(error));
        }
        let Some(slot) = self.resources()?.slot(&full_jid) else {
            return Err(AccountCoordinatorError::Diverged);
        };
        let revision = slot.revision();
        if slot
            .binding_receipt()
            .is_some_and(|binding| binding.state() == AccountResourceBindingState::Retiring)
        {
            return Ok(UnregisterOutcome::Retiring { revision });
        }
        let operation = AccountResourceOperation::new(
            request.operation,
            AccountResourceMutation::BeginRetirement {
                claim_identity: slot.claim_identity().clone(),
                expected_revision: revision,
                binding_ref: request.binding_ref,
                custody_disposition: request.custody_disposition,
            },
        );
        match self.execute(full_jid, operation).await? {
            BindingReply::Executed(execution) => Ok(match outcome_of(execution) {
                AccountResourceOperationOutcome::Committed { revision } => {
                    UnregisterOutcome::Retiring { revision }
                }
                AccountResourceOperationOutcome::Rejected(error) => {
                    UnregisterOutcome::Rejected(error)
                }
            }),
            BindingReply::ReplacementPending { .. } => Err(AccountCoordinatorError::Diverged),
        }
    }

    /// Re-read the stored row of a retiring binding and complete the
    /// retirement with the caller's teardown evidence. A row that holds
    /// another binding is reported and left untouched; a retired binding is
    /// never made current again.
    async fn repair(&mut self, message: Repair) -> Reply<RepairOutcome> {
        self.ensure_loaded().await?;
        let Repair {
            full_jid,
            binding_ref,
            evidence,
        } = message;
        self.reload_slot(&full_jid).await?;
        self.ensure_loaded().await?;
        let Some(slot) = self.resources()?.slot(&full_jid) else {
            return Ok(RepairOutcome::Completed {
                revision: AccountResourceRevision::INITIAL,
            });
        };
        let revision = slot.revision();
        let Some(binding) = slot.binding_receipt() else {
            self.forget_retirement(&full_jid, &binding_ref);
            return Ok(RepairOutcome::Completed { revision });
        };
        if binding.binding_ref() != &binding_ref {
            let current = binding.binding_ref().clone();
            self.forget_retirement(&full_jid, &binding_ref);
            return Ok(RepairOutcome::HeldByNewerBinding(current));
        }
        let Some(start) = binding
            .retirement_start()
            .filter(|_| binding.state() == AccountResourceBindingState::Retiring)
        else {
            return Ok(RepairOutcome::NotRetiring(binding.state()));
        };
        let source = BindingEvidence::for_binding(&binding_ref);
        let retirement = AccountResourceRetirementEvidence {
            endpoint: match binding.publication() {
                Some(publication) => EndpointRetirementEvidence::Unpublished {
                    publication: publication.clone(),
                    removed: source.clone(),
                },
                None => EndpointRetirementEvidence::NeverPublished {
                    source: source.clone(),
                },
            },
            actor: match binding.registration() {
                Some(registration) => ActorUnregistrationEvidence::Unregistered {
                    registration: registration.clone(),
                    removed: source.clone(),
                },
                None => ActorUnregistrationEvidence::NeverRegistered {
                    claim_identity: binding.binding_claim_identity().clone(),
                    source: source.clone(),
                },
            },
            directed_presence: DirectedPresenceTeardownReceipt::observed(
                source.clone(),
                start.snapshot_revision(),
                evidence.directed_presence,
            ),
            presentation_flush: PresentationFlushReceipt::observed(
                source,
                start.snapshot_revision(),
                evidence.presentation_flush,
            ),
            custody_disposition: start.custody_disposition(),
        };
        let operation = AccountResourceOperation::new(
            evidence.operation,
            AccountResourceMutation::CompleteRetirement {
                claim_identity: slot.claim_identity().clone(),
                expected_revision: revision,
                binding_ref,
                evidence: Box::new(retirement),
            },
        );
        match self.execute(full_jid, operation).await? {
            BindingReply::Executed(execution) => Ok(match outcome_of(execution) {
                AccountResourceOperationOutcome::Committed { revision } => {
                    RepairOutcome::Completed { revision }
                }
                AccountResourceOperationOutcome::Rejected(error) => RepairOutcome::Rejected(error),
            }),
            BindingReply::ReplacementPending { .. } => Err(AccountCoordinatorError::Diverged),
        }
    }

    /// Retire the detached or retiring binding that references a terminal
    /// stream-management session, which clears the row's session reference
    /// in the same commit. A binding that still has a connection is left
    /// to that connection.
    async fn release_session(&mut self, message: ReleaseSession) -> Reply<SessionRelease> {
        self.ensure_loaded().await?;
        let ReleaseSession { full_jid, request } = message;
        let Some(binding) = self
            .resources()?
            .slot(&full_jid)
            .and_then(AccountResourceSlot::binding_receipt)
            .filter(|binding| {
                binding
                    .snapshot()
                    .stream_management_ref
                    .is_some_and(|reference| reference.session == request.session)
            })
        else {
            return Ok(SessionRelease::NotReferenced);
        };
        let binding_ref = binding.binding_ref().clone();
        let state = binding.state();
        if !matches!(
            state,
            AccountResourceBindingState::Detached | AccountResourceBindingState::Retiring
        ) {
            return Ok(SessionRelease::HeldByConnection(binding_ref, state));
        }
        let started = self
            .unregister(Unregister {
                full_jid: full_jid.clone(),
                request: UnregisterRequest {
                    operation: request.begin,
                    binding_ref: binding_ref.clone(),
                    custody_disposition: request.custody_disposition,
                },
            })
            .await?;
        match started {
            UnregisterOutcome::Retiring { .. } => {}
            UnregisterOutcome::Stale(_) => return Ok(SessionRelease::NotReferenced),
            UnregisterOutcome::Rejected(error) => return Ok(SessionRelease::Rejected(error)),
        }
        let completed = self
            .repair(Repair {
                full_jid,
                binding_ref,
                evidence: RepairEvidence {
                    operation: request.complete,
                    directed_presence: request.directed_presence,
                    presentation_flush: request.presentation_flush,
                },
            })
            .await?;
        Ok(match completed {
            RepairOutcome::Completed { revision } => SessionRelease::Released { revision },
            RepairOutcome::Rejected(error) => SessionRelease::Rejected(error),
            RepairOutcome::HeldByNewerBinding(_) | RepairOutcome::NotRetiring(_) => {
                SessionRelease::NotReferenced
            }
        })
    }

    fn forget_retirement(&mut self, full_jid: &FullJid, binding_ref: &BindingRef) {
        self.retirements
            .retain(|pending| &pending.full_jid != full_jid || &pending.binding_ref != binding_ref);
    }

    fn view(&mut self) -> Reply<AccountView> {
        let resources = self.resources()?;
        Ok(AccountView {
            resources: resources.resources(),
            available: resources.available_resources(),
            routable: resources.routable_resources(),
            routing: resources.routing_snapshot(),
            presence: resources.presence_states(),
            roster_interested: resources.roster_interested_resources(),
            blocklist_interested: resources.blocklist_interested_resources(),
            bindings: resources.active_bindings(),
        })
    }

    /// Whether the account holds no binding, has nothing pending and has
    /// been idle for the configured time.
    fn is_idle(&mut self) -> bool {
        self.expire_replacements();
        let unbound = self
            .resources
            .as_ref()
            .is_none_or(|resources| resources.slots().all(|slot| slot.binding_ref().is_none()));
        unbound
            && self.retirements.is_empty()
            && self.replacements.is_empty()
            && self.last_activity.elapsed() >= self.config.idle_after
    }
}

fn fresh_operation(mutation: AccountResourceMutation) -> AccountResourceOperation {
    AccountResourceOperation::new(AccountResourceOperationId::fresh(), mutation)
}

fn outcome_of(execution: AccountResourceExecution) -> AccountResourceOperationOutcome {
    match execution {
        AccountResourceExecution::Applied(outcome)
        | AccountResourceExecution::Replayed(outcome) => outcome,
    }
}

/// Install a stored row as the in-memory slot of its resource.
fn install(resources: &mut AccountResources, stored: StoredResource) -> Reply<()> {
    let slot = AccountResourceSlot::restore(stored.image)?;
    resources
        .restore_slot(slot, stored.presented)
        .map_err(AccountCoordinatorError::Resources)
}

fn mutation_kind(mutation: &AccountResourceMutation) -> AccountMutationKind {
    match mutation {
        AccountResourceMutation::Bind { .. } => AccountMutationKind::Bind,
        AccountResourceMutation::PublishEndpoint { .. } => AccountMutationKind::PublishEndpoint,
        AccountResourceMutation::RegisterActor { .. } => AccountMutationKind::RegisterActor,
        AccountResourceMutation::UpdateSnapshot { .. } => AccountMutationKind::UpdateSnapshot,
        AccountResourceMutation::Detach { .. } => AccountMutationKind::Detach,
        AccountResourceMutation::SuccessfulResume { .. } => AccountMutationKind::SuccessfulResume,
        AccountResourceMutation::BeginRetirement { .. } => AccountMutationKind::BeginRetirement,
        AccountResourceMutation::CompleteRetirement { .. } => {
            AccountMutationKind::CompleteRetirement
        }
        AccountResourceMutation::ReplaceClaim { .. } => AccountMutationKind::ReplaceClaim,
        AccountResourceMutation::AdoptBinding { .. } => AccountMutationKind::AdoptBinding,
    }
}

fn binding_outcome(reply: &Reply<BindingReply>) -> AccountOperationOutcome {
    match reply {
        Ok(BindingReply::Executed(AccountResourceExecution::Applied(
            AccountResourceOperationOutcome::Committed { .. },
        ))) => AccountOperationOutcome::Committed,
        Ok(BindingReply::Executed(AccountResourceExecution::Replayed(_))) => {
            AccountOperationOutcome::Replayed
        }
        Ok(BindingReply::Executed(AccountResourceExecution::Applied(
            AccountResourceOperationOutcome::Rejected(_),
        )))
        | Ok(BindingReply::ReplacementPending { .. }) => AccountOperationOutcome::Rejected,
        Err(_) => AccountOperationOutcome::Failed,
    }
}

impl Message<Execute> for AccountCoordinatorActor {
    type Reply = Reply<BindingReply>;

    async fn handle(
        &mut self,
        message: Execute,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.last_activity = Instant::now();
        let kind = mutation_kind(&message.operation.mutation);
        let reply = self.execute(message.full_jid, message.operation).await;
        self.note_claim_loss(&reply);
        record_operation(kind, binding_outcome(&reply));
        if kind == AccountMutationKind::CompleteRetirement && reply.is_ok() {
            self.release_claim_if_unbound().await;
        }
        reply
    }
}

impl Message<Status> for AccountCoordinatorActor {
    type Reply = Reply<SlotStatus>;

    async fn handle(
        &mut self,
        message: Status,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.last_activity = Instant::now();
        self.ensure_loaded().await?;
        let claim_identity = self.claim()?.clone();
        Ok(match self.resources()?.slot(&message.full_jid) {
            Some(slot) => SlotStatus {
                claim_identity: slot.claim_identity().clone(),
                revision: slot.revision(),
                binding: slot
                    .binding_receipt()
                    .map(|binding| (binding.binding_ref().clone(), binding.state())),
            },
            None => SlotStatus {
                claim_identity,
                revision: AccountResourceRevision::INITIAL,
                binding: None,
            },
        })
    }
}

impl Message<BindSuccess> for AccountCoordinatorActor {
    type Reply = Reply<Option<AccountResourceBindSuccessReceipt>>;

    async fn handle(
        &mut self,
        message: BindSuccess,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.last_activity = Instant::now();
        self.ensure_claim(ClaimNeed::Existing).await?;
        self.ensure_loaded().await?;
        let claim_identity = self.claim()?.clone();
        Ok(self.resources()?.slot(&message.full_jid).and_then(|slot| {
            slot.current_bind_success_receipt(&claim_identity, &message.binding_ref)
        }))
    }
}

impl Message<MarkPresented> for AccountCoordinatorActor {
    type Reply = Reply<bool>;

    async fn handle(
        &mut self,
        message: MarkPresented,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.last_activity = Instant::now();
        let reply = self.mark_presented(message).await;
        self.note_claim_loss(&reply);
        record_operation(
            AccountMutationKind::MarkPresented,
            match &reply {
                Ok(true) => AccountOperationOutcome::Committed,
                Ok(false) => AccountOperationOutcome::Replayed,
                Err(AccountCoordinatorError::Fenced(_)) => AccountOperationOutcome::Rejected,
                Err(_) => AccountOperationOutcome::Failed,
            },
        );
        reply
    }
}

impl Message<Unregister> for AccountCoordinatorActor {
    type Reply = Reply<UnregisterOutcome>;

    async fn handle(
        &mut self,
        message: Unregister,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.last_activity = Instant::now();
        let reply = self.unregister(message).await;
        self.note_claim_loss(&reply);
        if reply.is_ok() {
            self.release_claim_if_unbound().await;
        }
        record_operation(
            AccountMutationKind::BeginRetirement,
            match &reply {
                Ok(UnregisterOutcome::Retiring { .. }) => AccountOperationOutcome::Committed,
                Ok(UnregisterOutcome::Stale(_) | UnregisterOutcome::Rejected(_)) => {
                    AccountOperationOutcome::Rejected
                }
                Err(_) => AccountOperationOutcome::Failed,
            },
        );
        reply
    }
}

impl Message<Repair> for AccountCoordinatorActor {
    type Reply = Reply<RepairOutcome>;

    async fn handle(
        &mut self,
        message: Repair,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.last_activity = Instant::now();
        let reply = self.repair(message).await;
        self.note_claim_loss(&reply);
        if reply.is_ok() {
            self.release_claim_if_unbound().await;
        }
        record_operation(
            AccountMutationKind::Repair,
            match &reply {
                Ok(RepairOutcome::Completed { .. }) => AccountOperationOutcome::Committed,
                Ok(_) => AccountOperationOutcome::Rejected,
                Err(_) => AccountOperationOutcome::Failed,
            },
        );
        reply
    }
}

impl Message<ReleaseSession> for AccountCoordinatorActor {
    type Reply = Reply<SessionRelease>;

    async fn handle(
        &mut self,
        message: ReleaseSession,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.last_activity = Instant::now();
        let reply = self.release_session(message).await;
        self.note_claim_loss(&reply);
        if reply.is_ok() {
            self.release_claim_if_unbound().await;
        }
        record_operation(
            AccountMutationKind::CompleteRetirement,
            match &reply {
                Ok(SessionRelease::Released { .. }) => AccountOperationOutcome::Committed,
                Ok(_) => AccountOperationOutcome::Rejected,
                Err(_) => AccountOperationOutcome::Failed,
            },
        );
        reply
    }
}

impl Message<ExpiredBindings> for AccountCoordinatorActor {
    type Reply = Reply<Vec<(FullJid, BindingRef)>>;

    /// The bindings whose connection is gone, judged now. A binding whose
    /// liveness is unknown is not listed.
    async fn handle(
        &mut self,
        _message: ExpiredBindings,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.ensure_claim(ClaimNeed::Existing).await?;
        self.ensure_loaded().await?;
        let candidates: Vec<_> = self
            .resources()?
            .slots()
            .filter_map(|slot| {
                let binding = slot.binding_receipt()?;
                Some((
                    slot.full_jid().clone(),
                    binding.binding_ref().clone(),
                    binding
                        .snapshot()
                        .stream_management_ref
                        .map(|reference| reference.session),
                ))
            })
            .collect();
        let mut expired = Vec::new();
        for (full_jid, binding_ref, session) in candidates {
            if self.liveness.liveness(&binding_ref, session).await == BindingLiveness::Expired {
                expired.push((full_jid, binding_ref));
            }
        }
        Ok(expired)
    }
}

impl Message<ReadView> for AccountCoordinatorActor {
    type Reply = Reply<AccountView>;

    async fn handle(
        &mut self,
        message: ReadView,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.last_activity = Instant::now();
        self.ensure_claim(if message.take_over {
            ClaimNeed::Taken
        } else {
            ClaimNeed::Existing
        })
        .await?;
        self.ensure_loaded().await?;
        self.view()
    }
}

impl Message<OverdueRetirements> for AccountCoordinatorActor {
    type Reply = Reply<Vec<(FullJid, BindingRef)>>;

    async fn handle(
        &mut self,
        _message: OverdueRetirements,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.ensure_claim(ClaimNeed::Existing).await?;
        self.ensure_loaded().await?;
        let now = Instant::now();
        Ok(self
            .retirements
            .iter()
            .filter(|pending| pending.deadline <= now)
            .map(|pending| (pending.full_jid.clone(), pending.binding_ref.clone()))
            .collect())
    }
}

impl Message<Adopt> for AccountCoordinatorActor {
    type Reply = Reply<AdoptionReport>;

    async fn handle(
        &mut self,
        _message: Adopt,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.last_activity = Instant::now();
        self.ensure_claim(ClaimNeed::Existing).await?;
        self.ensure_loaded().await?;
        Ok(self.adoption.clone())
    }
}

impl Message<ReclaimIfIdle> for AccountCoordinatorActor {
    type Reply = Reply<bool>;

    async fn handle(
        &mut self,
        _message: ReclaimIfIdle,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.is_idle() {
            return Ok(false);
        }
        if self.claims.releases_claims() {
            // The claim goes with the coordinator, and only for an account
            // whose rows show no binding. Slots that were dropped after an
            // unknown commit outcome are read again to know that.
            if self.claim_identity.is_some() {
                // A coordinator that cannot read its rows or release its
                // claim stays, so the next sweep tries again: stopping
                // would leave the claim held with nobody to release it.
                if let Err(error) = self.ensure_loaded().await {
                    warn!(%error, "account coordinator could not read its rows to release its claim");
                    return Ok(false);
                }
                if !self.is_idle() || self.try_release_claim().await.is_none() {
                    return Ok(false);
                }
            }
        } else if let (Some(claim), Some(resources)) =
            (self.claim_identity.as_ref(), self.resources.as_ref())
        {
            // Free rows carry nothing a later coordinator needs. A failure
            // here leaves them for the next load, which deletes them.
            for slot in resources.slots() {
                if let Err(error) = self
                    .store
                    .forget(claim, slot.full_jid(), slot.revision())
                    .await
                {
                    warn!(%error, "account coordinator could not delete a free resource row");
                }
            }
        }
        ctx.stop();
        Ok(true)
    }
}

impl Message<SettleExpired> for AccountCoordinatorActor {
    type Reply = Reply<usize>;

    async fn handle(
        &mut self,
        _message: SettleExpired,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.ensure_loaded().await?;
        let settled = self.settle_expired().await;
        self.note_claim_loss(&settled);
        if settled.is_ok() {
            self.release_claim_if_unbound().await;
        }
        settled
    }
}

impl Message<ReleaseClaimIfUnbound> for AccountCoordinatorActor {
    type Reply = Reply<bool>;

    async fn handle(
        &mut self,
        _message: ReleaseClaimIfUnbound,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.ensure_claim(ClaimNeed::Existing).await?;
        self.ensure_loaded().await?;
        Ok(self.release_claim_if_unbound().await)
    }
}
