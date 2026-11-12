//! Publication of a connection's bindings through the account coordinator.
//!
//! [`CoordinatorBindingPublisher`] maps the steps of a connection's
//! binding publisher (publish, publish a resumed binding, confirm, retire,
//! retire a binding whose socket another node hosts) onto
//! [`AccountBindingAuthority`]. Its implementation of the websocket
//! `BindingPublisher` trait is next to that trait, in
//! `server/routes/websocket/connection/coordinator_publisher.rs`.
//!
//! Every step is safe to repeat. A step first reads the slot and skips
//! what the binding's lifecycle state shows as done, so a repeat after a
//! lost reply neither reuses an operation identity for a different payload
//! nor applies a transition twice.

use std::sync::Arc;

use jid::FullJid;
use waddle_xmpp::ownership::account_coordinator::BindingFenceError;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindSuccessReceipt, AccountResourceBindingState, AccountResourceExecution,
    AccountResourceMutation, AccountResourceOperation, AccountResourceOperationId,
    AccountResourceOperationOutcome, AccountResourceRevision, AccountResourceTransitionError,
    ActorRegistrationReceipt, BindingEvidence, BindingRetirementDisposition,
    DirectedPresenceTeardownDisposition, EndpointPublicationReceipt, PresentationFlushDisposition,
    SuccessfulResumeReceipt,
};
use waddle_xmpp::ownership::joint_identity::ResourceLifetime;
use waddle_xmpp::ownership::{BindingRef, UserClaimIdentity};

use super::authority::AccountBindingAuthority;
use super::directory::AccountCoordinators;
use super::error::AccountCoordinatorError;
use super::messages::{
    BindingReply, RepairEvidence, RepairOutcome, UnregisterOutcome, UnregisterRequest,
};

/// A binding the coordinator holds as published, and what the next step
/// for it must name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedBinding {
    pub binding_ref: BindingRef,
    pub claim_identity: UserClaimIdentity,
    pub revision: AccountResourceRevision,
}

/// The result of publishing a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The binding is recorded and published non-routable.
    Published(PublishedBinding),
    /// The resource has another binding. The coordinator reserved the
    /// resource for this one; the connection retires `displaced` and
    /// publishes again.
    ReplacementPending {
        displaced: BindingRef,
        revision: AccountResourceRevision,
    },
    /// The slot refused the binding. Nothing is published.
    Refused(AccountResourceTransitionError),
}

/// The result of confirming a published binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmOutcome {
    /// The binding is active. The receipt is the authorization to answer
    /// the client, read after the activation committed.
    Confirmed(Box<AccountResourceBindSuccessReceipt>),
    /// The binding is no longer the resource's current one, or was retired
    /// before it became active. The client must not be answered for it.
    NotCurrent,
    Refused(AccountResourceTransitionError),
}

/// What the connection established about the binding it retires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetireEvidence {
    /// The identity the retirement's start is repeated under.
    pub begin: AccountResourceOperationId,
    /// The identity the retirement's completion is repeated under.
    pub complete: AccountResourceOperationId,
    pub custody_disposition: BindingRetirementDisposition,
    pub directed_presence: DirectedPresenceTeardownDisposition,
    pub presentation_flush: PresentationFlushDisposition,
}

impl RetireEvidence {
    /// What a retirement states when the binding's own teardown evidence is
    /// not available: the work it accepted is owned by the stream-management
    /// and offline recovery paths, which also retract its directed presence
    /// and own any presentation it had pending.
    pub fn recovery_owned() -> Self {
        Self {
            begin: AccountResourceOperationId::fresh(),
            complete: AccountResourceOperationId::fresh(),
            custody_disposition: BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
            directed_presence:
                DirectedPresenceTeardownDisposition::RecipientsRetractedAndUnavailableRecoveryOwned,
            presentation_flush: PresentationFlushDisposition::RecoveryOwner,
        }
    }
}

/// The result of retiring a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetireOutcome {
    /// The binding is retired and the resource is free.
    Retired {
        revision: AccountResourceRevision,
    },
    /// The binding is not the resource's current one. A later binding of
    /// the same full JID was left alone.
    Stale(BindingFenceError),
    /// The resource holds a newer binding, which was left alone.
    Superseded(BindingRef),
    Refused(AccountResourceTransitionError),
}

/// Publishes, confirms and retires bindings through the account
/// coordinators of this node.
#[derive(Clone)]
pub struct CoordinatorBindingPublisher {
    coordinators: Arc<AccountCoordinators>,
}

fn fresh(mutation: AccountResourceMutation) -> AccountResourceOperation {
    AccountResourceOperation::new(AccountResourceOperationId::fresh(), mutation)
}

/// The committed revision of a reply, or why there is none.
enum Step {
    Committed(AccountResourceRevision),
    Rejected(AccountResourceTransitionError),
    Pending {
        displaced: BindingRef,
        revision: AccountResourceRevision,
    },
}

fn step(reply: BindingReply) -> Step {
    match reply {
        BindingReply::Executed(
            AccountResourceExecution::Applied(outcome)
            | AccountResourceExecution::Replayed(outcome),
        ) => match outcome {
            AccountResourceOperationOutcome::Committed { revision } => Step::Committed(revision),
            AccountResourceOperationOutcome::Rejected(error) => Step::Rejected(error),
        },
        BindingReply::ReplacementPending {
            displaced,
            revision,
        } => Step::Pending {
            displaced,
            revision,
        },
    }
}

impl CoordinatorBindingPublisher {
    pub fn new(coordinators: Arc<AccountCoordinators>) -> Self {
        Self { coordinators }
    }

    /// Record `binding_ref` as the binding of `jid` and publish it
    /// non-routable: `Bind`, then `PublishEndpoint`.
    pub async fn publish(
        &self,
        jid: &FullJid,
        binding_ref: &BindingRef,
        lifetime: ResourceLifetime,
    ) -> Result<PublishOutcome, AccountCoordinatorError> {
        let status = self.coordinators.slot_status(jid).await?;
        let claim_identity = status.claim_identity;
        let prepared = match status.binding {
            Some((current, state)) if &current == binding_ref => match state {
                AccountResourceBindingState::Prepared => status.revision,
                AccountResourceBindingState::Published | AccountResourceBindingState::Active => {
                    return Ok(PublishOutcome::Published(PublishedBinding {
                        binding_ref: current,
                        claim_identity,
                        revision: status.revision,
                    }));
                }
                AccountResourceBindingState::Detached
                | AccountResourceBindingState::Retiring
                | AccountResourceBindingState::Retired => {
                    return Ok(PublishOutcome::Refused(
                        AccountResourceTransitionError::LifecycleConflict,
                    ));
                }
            },
            _ => {
                let bind = fresh(AccountResourceMutation::Bind {
                    claim_identity: claim_identity.clone(),
                    expected_revision: status.revision,
                    binding_ref: binding_ref.clone(),
                    lifetime,
                });
                match step(self.coordinators.execute(jid, bind).await?) {
                    Step::Committed(revision) => revision,
                    Step::Rejected(error) => return Ok(PublishOutcome::Refused(error)),
                    Step::Pending {
                        displaced,
                        revision,
                    } => {
                        return Ok(PublishOutcome::ReplacementPending {
                            displaced,
                            revision,
                        })
                    }
                }
            }
        };
        self.publish_endpoint(jid, binding_ref, claim_identity, prepared)
            .await
    }

    /// Record a committed resume as the new binding of `jid` and publish
    /// it non-routable: `SuccessfulResume`, then `PublishEndpoint`.
    pub async fn publish_resumed(
        &self,
        jid: &FullJid,
        previous: &BindingRef,
        binding_ref: &BindingRef,
        receipt: SuccessfulResumeReceipt,
    ) -> Result<PublishOutcome, AccountCoordinatorError> {
        let status = self.coordinators.slot_status(jid).await?;
        let claim_identity = status.claim_identity;
        let prepared = match status.binding {
            Some((current, AccountResourceBindingState::Prepared)) if &current == binding_ref => {
                status.revision
            }
            Some((current, _)) if &current == binding_ref => {
                return Ok(PublishOutcome::Published(PublishedBinding {
                    binding_ref: current,
                    claim_identity,
                    revision: status.revision,
                }));
            }
            _ => {
                let resume = fresh(AccountResourceMutation::SuccessfulResume {
                    claim_identity: claim_identity.clone(),
                    expected_revision: status.revision,
                    previous_binding_ref: previous.clone(),
                    binding_ref: binding_ref.clone(),
                    receipt,
                });
                match step(self.coordinators.execute(jid, resume).await?) {
                    Step::Committed(revision) => revision,
                    Step::Rejected(error) => return Ok(PublishOutcome::Refused(error)),
                    Step::Pending { .. } => return Err(AccountCoordinatorError::Diverged),
                }
            }
        };
        self.publish_endpoint(jid, binding_ref, claim_identity, prepared)
            .await
    }

    async fn publish_endpoint(
        &self,
        jid: &FullJid,
        binding_ref: &BindingRef,
        claim_identity: UserClaimIdentity,
        revision: AccountResourceRevision,
    ) -> Result<PublishOutcome, AccountCoordinatorError> {
        let publish = fresh(AccountResourceMutation::PublishEndpoint {
            claim_identity: claim_identity.clone(),
            expected_revision: revision,
            binding_ref: binding_ref.clone(),
            publication: EndpointPublicationReceipt::observed(BindingEvidence::for_binding(
                binding_ref,
            )),
        });
        Ok(match step(self.coordinators.execute(jid, publish).await?) {
            Step::Committed(revision) => PublishOutcome::Published(PublishedBinding {
                binding_ref: binding_ref.clone(),
                claim_identity,
                revision,
            }),
            Step::Rejected(error) => PublishOutcome::Refused(error),
            Step::Pending { .. } => return Err(AccountCoordinatorError::Diverged),
        })
    }

    /// Activate the published binding and read the authorization to answer
    /// the client: `RegisterActor`, then the bind-success receipt. A
    /// resumed binding is confirmed the same way.
    pub async fn confirm(
        &self,
        jid: &FullJid,
        published: &PublishedBinding,
    ) -> Result<ConfirmOutcome, AccountCoordinatorError> {
        let status = self.coordinators.slot_status(jid).await?;
        match status.binding {
            Some((current, AccountResourceBindingState::Published))
                if current == published.binding_ref =>
            {
                let register = fresh(AccountResourceMutation::RegisterActor {
                    claim_identity: status.claim_identity.clone(),
                    expected_revision: status.revision,
                    binding_ref: current.clone(),
                    registration: ActorRegistrationReceipt::observed(
                        status.claim_identity,
                        BindingEvidence::for_binding(&current),
                    ),
                });
                match step(self.coordinators.execute(jid, register).await?) {
                    Step::Committed(_) => {}
                    Step::Rejected(error) => return Ok(ConfirmOutcome::Refused(error)),
                    Step::Pending { .. } => return Err(AccountCoordinatorError::Diverged),
                }
            }
            Some((current, AccountResourceBindingState::Active))
                if current == published.binding_ref => {}
            _ => return Ok(ConfirmOutcome::NotCurrent),
        }
        Ok(
            match self
                .coordinators
                .bind_success(jid, &published.binding_ref)
                .await?
            {
                Some(receipt) => ConfirmOutcome::Confirmed(Box::new(receipt)),
                None => ConfirmOutcome::NotCurrent,
            },
        )
    }

    /// Retire exactly the published binding: start its retirement and
    /// complete it with the connection's teardown evidence. A later
    /// binding of the same full JID is left alone.
    pub async fn retire(
        &self,
        jid: &FullJid,
        published: &PublishedBinding,
        evidence: RetireEvidence,
    ) -> Result<RetireOutcome, AccountCoordinatorError> {
        self.retire_binding(jid, &published.binding_ref, evidence)
            .await
    }

    /// Retire the binding with this exact reference, whoever published it.
    /// This is how a binding whose connection is gone is retired.
    pub async fn retire_binding(
        &self,
        jid: &FullJid,
        binding_ref: &BindingRef,
        evidence: RetireEvidence,
    ) -> Result<RetireOutcome, AccountCoordinatorError> {
        let started = self
            .coordinators
            .unregister(
                jid,
                UnregisterRequest {
                    operation: evidence.begin,
                    binding_ref: binding_ref.clone(),
                    custody_disposition: evidence.custody_disposition,
                },
            )
            .await?;
        match started {
            UnregisterOutcome::Retiring { .. } => {}
            UnregisterOutcome::Stale(error) => return Ok(RetireOutcome::Stale(error)),
            UnregisterOutcome::Rejected(error) => return Ok(RetireOutcome::Refused(error)),
        }
        let completed = self
            .coordinators
            .repair(
                jid,
                binding_ref,
                RepairEvidence {
                    operation: evidence.complete,
                    directed_presence: evidence.directed_presence,
                    presentation_flush: evidence.presentation_flush,
                },
            )
            .await?;
        Ok(match completed {
            RepairOutcome::Completed { revision } => RetireOutcome::Retired { revision },
            RepairOutcome::HeldByNewerBinding(current) => RetireOutcome::Superseded(current),
            RepairOutcome::NotRetiring(_) => {
                RetireOutcome::Refused(AccountResourceTransitionError::LifecycleConflict)
            }
            RepairOutcome::Rejected(error) => RetireOutcome::Refused(error),
        })
    }

    /// Retire exactly the published binding of a socket that another node
    /// hosts. The coordinator is on the node that owns the account, not on
    /// the node that hosts the socket, so this is the same retirement. On
    /// a node that does not own the account the call fails with the claim
    /// error, and the caller addresses the owner instead.
    pub async fn retire_remote(
        &self,
        jid: &FullJid,
        published: &PublishedBinding,
        evidence: RetireEvidence,
    ) -> Result<RetireOutcome, AccountCoordinatorError> {
        self.retire(jid, published, evidence).await
    }
}
