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
//! [`BindingPublisher`], which the connection registry implements until the
//! account coordinator exists.

use super::binding_messages::{ConnectionActorBindingAttempt, ConnectionActorOperationId};
use super::retirement::RetirementRefusal;
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

/// Where a connection publishes its bindings.
///
/// The connection decides when a binding is published, confirmed with the
/// account's owner and retired; this trait is the one place those decisions
/// reach the routing state. The connection registry and its mirrors implement
/// it today. The account coordinator takes its place by implementing it.
pub(in crate::server::routes::websocket) trait BindingPublisher {
    /// What identifies a published binding to the publisher.
    type Published: Copy;
    /// The outcome of confirming a published binding with the account owner.
    type Confirmation;

    /// Publish `entry`, which carries its exact `BindingRef`, as the binding
    /// of `jid`. `None` means nothing was published.
    fn publish(
        &self,
        jid: &jid::FullJid,
        entry: waddle_xmpp::registry::ConnectionEntry,
    ) -> Option<Self::Published>;

    /// Confirm the published binding with the owner of the account.
    fn confirm(
        &self,
        jid: &jid::FullJid,
        published: Self::Published,
        resumed: bool,
    ) -> impl std::future::Future<Output = Option<Self::Confirmation>> + Send;

    /// Retire exactly the published binding, as compensation for a
    /// publication that will not be acknowledged. A later binding of the
    /// same full JID is left alone.
    fn retire(
        &self,
        jid: &jid::FullJid,
        published: Self::Published,
    ) -> impl std::future::Future<Output = ()> + Send;

    /// Retire exactly the published binding where the account is owned by
    /// another node, which holds a proxy for it.
    fn retire_remote(
        &self,
        jid: &jid::FullJid,
        published: Self::Published,
    ) -> impl std::future::Future<Output = ()> + Send;
}
