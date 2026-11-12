//! The binding attempts of one physical connection.
//!
//! The connection mints a new [`BindingRef`] for each bind, for each resume
//! attempt, and for a fresh bind after a failed resume on the same socket. A
//! binding is prepared, installed when it is published, confirmed when its
//! result has been acknowledged to the client, and retired by its exact
//! reference. At most one attempt is current: preparing the next one
//! displaces the previous, which its owner then retires by reference, and a
//! step that names any other attempt is refused, so a replayed or late step
//! cannot touch a later binding on the same socket.
//!
//! The ledger only records. Publication itself goes through
//! [`BindingPublisher`], which the connection registry and the account
//! coordinator each implement.

use super::binding_messages::{ConnectionActorBindingAttempt, ConnectionActorOperationId};
use super::retirement::RetirementRefusal;
use crate::account_coordinator::RetireEvidence;
use waddle_xmpp::ownership::account_resource_transition::AccountResourceRevision;
use waddle_xmpp::ownership::joint_identity::ResourceLifetime;
use waddle_xmpp::ownership::{BindingRef, ConnectionRef};

/// How far the current attempt has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::routes::websocket) enum BindingStage {
    /// Minted. Nothing is published under it yet.
    Prepared,
    /// Published under this exact reference.
    Installed,
    /// The client was sent the bind result or `<resumed/>` for it.
    Confirmed,
}

/// Why a binding step was refused. The ledger is unchanged by a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(in crate::server::routes::websocket) enum BindingRefusal {
    #[error("the attempt belongs to a different physical connection")]
    WrongConnection,
    #[error("the operation is older than one this connection already admitted")]
    StaleOperation,
    #[error("the attempt is not this connection's current binding attempt")]
    NotCurrent,
    #[error("the attempt has not been installed")]
    NotInstalled,
    #[error("the process has no operation key left to mint")]
    OperationKeysExhausted,
    /// An installed binding is retired by retiring its connection, which
    /// detaches the session and reports through settlement. A message cannot
    /// retire it on its own until the account coordinator owns replacement.
    #[error("an installed binding is retired with its connection")]
    Installed,
    #[error("the connection has stopped")]
    ConnectionStopped,
    #[error(transparent)]
    Retirement(#[from] RetirementRefusal),
}

/// A freshly prepared attempt, and the binding it displaced as current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::server::routes::websocket) struct PreparedBinding {
    pub(in crate::server::routes::websocket) attempt: ConnectionActorBindingAttempt,
    /// The previous current binding, which this attempt replaced. Its owner
    /// retires whatever was published under it by this exact reference.
    pub(in crate::server::routes::websocket) displaced: Option<BindingRef>,
}

/// Whether an install changed the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server::routes::websocket) enum InstallOutcome {
    Installed,
    /// The same attempt was installed before; a retry reports that outcome.
    AlreadyInstalled,
}

#[derive(Debug)]
struct CurrentBinding {
    attempt: ConnectionActorBindingAttempt,
    stage: BindingStage,
}

/// The binding identity state of one physical connection.
#[derive(Debug)]
pub(in crate::server::routes::websocket) struct BindingLedger {
    connection_ref: ConnectionRef,
    // Counts prepared attempts; an attempt carries the value it was minted at.
    epoch: u64,
    // The highest operation admitted. Operations are admitted in strictly
    // increasing order, which rejects a replay without remembering each one.
    admitted: Option<ConnectionActorOperationId>,
    current: Option<CurrentBinding>,
}

impl BindingLedger {
    pub(in crate::server::routes::websocket) fn new(connection_ref: ConnectionRef) -> Self {
        Self {
            connection_ref,
            epoch: 0,
            admitted: None,
            current: None,
        }
    }

    /// Mint a new attempt for `operation_id`.
    ///
    /// Repeating the operation while its attempt is still only prepared
    /// returns that same attempt, so a caller whose request was cancelled can
    /// ask again. Any other operation that is not newer than the last one
    /// admitted is refused.
    pub(in crate::server::routes::websocket) fn prepare(
        &mut self,
        operation_id: ConnectionActorOperationId,
    ) -> Result<PreparedBinding, BindingRefusal> {
        if let Some(current) = &self.current {
            if current.attempt.operation_id == operation_id
                && current.stage == BindingStage::Prepared
            {
                return Ok(PreparedBinding {
                    attempt: current.attempt.clone(),
                    displaced: None,
                });
            }
        }
        if self
            .admitted
            .is_some_and(|admitted| operation_id <= admitted)
        {
            return Err(BindingRefusal::StaleOperation);
        }
        self.admitted = Some(operation_id);
        self.epoch = self.epoch.wrapping_add(1);
        let attempt = ConnectionActorBindingAttempt {
            connection_ref: self.connection_ref.clone(),
            binding_ref: BindingRef::fresh(self.connection_ref.clone()),
            operation_id,
            epoch: self.epoch,
        };
        let displaced = self
            .current
            .replace(CurrentBinding {
                attempt: attempt.clone(),
                stage: BindingStage::Prepared,
            })
            .map(|previous| previous.attempt.binding_ref);
        Ok(PreparedBinding { attempt, displaced })
    }

    /// Mint a new attempt under a fresh operation key, for the connection's
    /// own inline bind and resume handling.
    pub(in crate::server::routes::websocket) fn prepare_next(
        &mut self,
    ) -> Result<PreparedBinding, BindingRefusal> {
        let operation_id =
            ConnectionActorOperationId::fresh().ok_or(BindingRefusal::OperationKeysExhausted)?;
        self.prepare(operation_id)
    }

    /// The current attempt if it is exactly `attempt`.
    fn current_for(
        &mut self,
        attempt: &ConnectionActorBindingAttempt,
    ) -> Result<&mut CurrentBinding, BindingRefusal> {
        if attempt.connection_ref != self.connection_ref {
            return Err(BindingRefusal::WrongConnection);
        }
        match &mut self.current {
            Some(current) if &current.attempt == attempt => Ok(current),
            _ => Err(BindingRefusal::NotCurrent),
        }
    }

    /// The current attempt if `binding` is its exact reference.
    fn current_for_binding(
        &mut self,
        binding: &BindingRef,
    ) -> Result<&mut CurrentBinding, BindingRefusal> {
        if binding.connection_ref() != &self.connection_ref {
            return Err(BindingRefusal::WrongConnection);
        }
        match &mut self.current {
            Some(current) if &current.attempt.binding_ref == binding => Ok(current),
            _ => Err(BindingRefusal::NotCurrent),
        }
    }

    fn install_current(current: &mut CurrentBinding) -> InstallOutcome {
        match current.stage {
            BindingStage::Prepared => {
                current.stage = BindingStage::Installed;
                InstallOutcome::Installed
            }
            BindingStage::Installed | BindingStage::Confirmed => InstallOutcome::AlreadyInstalled,
        }
    }

    fn confirm_current(current: &mut CurrentBinding) -> Result<(), BindingRefusal> {
        match current.stage {
            BindingStage::Prepared => Err(BindingRefusal::NotInstalled),
            BindingStage::Installed | BindingStage::Confirmed => {
                current.stage = BindingStage::Confirmed;
                Ok(())
            }
        }
    }

    /// Record that `attempt` is published.
    pub(in crate::server::routes::websocket) fn install(
        &mut self,
        attempt: &ConnectionActorBindingAttempt,
    ) -> Result<InstallOutcome, BindingRefusal> {
        self.current_for(attempt).map(Self::install_current)
    }

    /// Record that the binding with this exact reference is published.
    pub(in crate::server::routes::websocket) fn install_binding(
        &mut self,
        binding: &BindingRef,
    ) -> Result<InstallOutcome, BindingRefusal> {
        self.current_for_binding(binding).map(Self::install_current)
    }

    /// Record that the client was sent the result of `attempt`.
    pub(in crate::server::routes::websocket) fn confirm(
        &mut self,
        attempt: &ConnectionActorBindingAttempt,
    ) -> Result<(), BindingRefusal> {
        self.current_for(attempt).and_then(Self::confirm_current)
    }

    /// Record that the client was sent the result of the binding with this
    /// exact reference.
    pub(in crate::server::routes::websocket) fn confirm_binding(
        &mut self,
        binding: &BindingRef,
    ) -> Result<(), BindingRefusal> {
        self.current_for_binding(binding)
            .and_then(Self::confirm_current)
    }

    /// Retire the attempt with this exact reference as compensation, which
    /// only an attempt that is not installed allows.
    pub(in crate::server::routes::websocket) fn retire_prepared(
        &mut self,
        binding: &BindingRef,
    ) -> Result<(), BindingRefusal> {
        match self.current_for_binding(binding)?.stage {
            BindingStage::Prepared => {
                self.current = None;
                Ok(())
            }
            BindingStage::Installed | BindingStage::Confirmed => Err(BindingRefusal::Installed),
        }
    }

    /// Retire the binding with this exact reference. Returns whether it was
    /// current; a reference to any other binding changes nothing.
    pub(in crate::server::routes::websocket) fn retire(&mut self, binding: &BindingRef) -> bool {
        if self
            .current
            .as_ref()
            .is_some_and(|current| &current.attempt.binding_ref == binding)
        {
            self.current = None;
            true
        } else {
            false
        }
    }

    /// The current binding and its stage.
    pub(in crate::server::routes::websocket) fn current(
        &self,
    ) -> Option<(&BindingRef, BindingStage)> {
        self.current
            .as_ref()
            .map(|current| (&current.attempt.binding_ref, current.stage))
    }
}

/// Reply to [`super::binding_messages::PrepareBinding`]. A refusal is a
/// value, never an error result, so a refused step cannot fail the actor
/// however the message was sent.
#[derive(Debug, Clone, PartialEq, Eq, kameo::Reply)]
pub(in crate::server::routes::websocket) enum PrepareBindingReply {
    // Boxed so a refusal does not carry the size of an attempt.
    Prepared(Box<PreparedBinding>),
    Refused(BindingRefusal),
}

/// Reply to an install or confirm step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub(in crate::server::routes::websocket) enum BindingStepReply {
    Applied,
    /// The step had already been applied to this exact attempt.
    AlreadyApplied,
    Refused(BindingRefusal),
}

/// The result of publishing a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BindingPublication<P, R> {
    /// The binding is recorded and published, not yet confirmed.
    Published(P),
    /// The resource holds another binding. The resource is reserved for
    /// this connection, which has `displaced` retired and then publishes a
    /// new attempt.
    ReplacementPending {
        displaced: BindingRef,
        revision: AccountResourceRevision,
    },
    /// Nothing was published.
    Refused(R),
}

/// The result of confirming a published binding with the account's owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BindingConfirmation<C, R> {
    /// The owner holds the binding as active. The receipt is what allows
    /// the client to be answered.
    Confirmed(C),
    /// The binding is no longer the resource's current one: a later binding
    /// of the same full JID owns the resource and confirms itself.
    NotCurrent,
    /// The owner did not accept the binding.
    Refused(R),
}

/// The result of retiring a binding with the account's owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BindingRetirement<R> {
    /// The owner no longer holds the binding.
    Retired,
    /// The binding was not the resource's current one. `current` names the
    /// binding that is, when the owner reported it; it was left alone.
    NotCurrent { current: Option<BindingRef> },
    /// The owner refused the retirement and still holds the binding, which
    /// stays non-routable.
    Refused(R),
}

/// Where a connection publishes its bindings.
///
/// The connection decides when a binding is published, confirmed with the
/// account's owner and retired; this trait is the one place those decisions
/// reach the routing state. Two publishers implement it: the connection
/// registry with its mirrors, and the account coordinator.
///
/// A publisher failure (`Error`) is never a decision about the binding: the
/// outcome of the step is unknown and the caller repeats or compensates.
pub(crate) trait BindingPublisher {
    /// What identifies a published binding to the publisher.
    type Published: Clone + Send + Sync;
    /// What the publisher needs to know about a committed resume in order
    /// to move the resource's slot to the resuming binding.
    type Resume: Send;
    /// What a confirmation hands back as the authorization to answer the
    /// client.
    type Receipt;
    /// Why the publisher refused a step.
    type Refusal;
    /// What the routing index held for a binding that was withdrawn.
    type Withdrawn;
    /// What retiring a binding at the node that owns its account reports.
    type RemoteRetirement;
    /// A failure that leaves the outcome of a step unknown.
    type Error;

    /// Record `binding` as the binding of `jid` for a resource of the given
    /// lifetime and publish it.
    fn publish(
        &self,
        jid: &jid::FullJid,
        binding: &BindingRef,
        lifetime: ResourceLifetime,
    ) -> impl std::future::Future<
        Output = Result<BindingPublication<Self::Published, Self::Refusal>, Self::Error>,
    > + Send;

    /// Record a committed resume: `binding` takes the place of the
    /// session's previous binding in the same resource slot, and is
    /// published. It is confirmed like any other binding.
    fn publish_resumed(
        &self,
        jid: &jid::FullJid,
        binding: &BindingRef,
        resume: Self::Resume,
    ) -> impl std::future::Future<
        Output = Result<BindingPublication<Self::Published, Self::Refusal>, Self::Error>,
    > + Send;

    /// Confirm the published binding with the owner of the account.
    fn confirm(
        &self,
        jid: &jid::FullJid,
        published: &Self::Published,
    ) -> impl std::future::Future<
        Output = Result<BindingConfirmation<Self::Receipt, Self::Refusal>, Self::Error>,
    > + Send;

    /// Take exactly the published binding out of the routing index, so
    /// nothing more is enqueued for it. A later binding of the same full
    /// JID is left alone and `None` is returned.
    fn withdraw(&self, jid: &jid::FullJid, published: &Self::Published) -> Option<Self::Withdrawn>;

    /// Retire exactly the published binding with the owner of the account,
    /// stating what became of the work it accepted and of its teardown. A
    /// later binding of the same full JID is left alone.
    fn retire(
        &self,
        jid: &jid::FullJid,
        published: &Self::Published,
        evidence: RetireEvidence,
    ) -> impl std::future::Future<Output = Result<BindingRetirement<Self::Refusal>, Self::Error>> + Send;

    /// Retire exactly the published binding where the account is owned by
    /// another node, which holds a proxy for it.
    fn retire_remote(
        &self,
        jid: &jid::FullJid,
        published: &Self::Published,
        evidence: RetireEvidence,
    ) -> impl std::future::Future<Output = Result<Self::RemoteRetirement, Self::Error>> + Send;
}
