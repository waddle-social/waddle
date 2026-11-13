//! The contract a connection actor drives its binding through.

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use waddle_xmpp::ownership::account_coordinator::Presentation;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindSuccessReceipt, AccountResourceOperation,
};
use waddle_xmpp::ownership::BindingRef;

use super::error::AccountCoordinatorError;
use super::messages::{
    AccountView, BindingReply, ReleaseSessionRequest, RepairEvidence, RepairOutcome,
    SessionRelease, SlotStatus, UnregisterOutcome, UnregisterRequest,
};

/// The account authority as a connection sees it.
///
/// A connection states what happened to its own binding; the authority
/// decides. Every mutation names the claim identity and slot revision from
/// [`Self::slot_status`] or from the last committed reply, and the
/// connection's exact `BindingRef`. Each call returns after the decision is
/// durable, and none waits for another connection.
///
/// The binding messages map onto it as follows: preparing a binding is
/// `execute` with `Bind`; reporting the endpoint installed is `execute`
/// with `PublishEndpoint`; confirming the durable commit is `execute` with
/// `RegisterActor` followed by [`Self::bind_success`] immediately before
/// the client is answered; a snapshot change is `execute` with
/// `UpdateSnapshot`; losing the transport with a resumable session is
/// `execute` with `Detach`; a resume is `execute` with `SuccessfulResume`
/// and then the publish and register steps for the new binding; retiring is
/// [`Self::unregister`] or `execute` with `BeginRetirement`, then `execute`
/// with `CompleteRetirement` once the pump has stopped.
///
/// An `Err` is never a decision about the binding. After a store failure
/// the commit outcome is unknown; the connection repeats the same operation
/// identity and proceeds on the recorded outcome.
///
/// The authority of an account is the coordinator on the node that holds
/// the account's claim. [`super::AccountCoordinators`] is the coordinators
/// of this node only; [`super::RoutedAccountAuthority`] sends each request
/// to the node that owns the account.
#[async_trait]
pub trait AccountBindingAuthority: Send + Sync + 'static {
    /// The claim identity, revision and current binding of a resource.
    async fn slot_status(&self, full_jid: &FullJid) -> Result<SlotStatus, AccountCoordinatorError>;

    /// Apply one identified mutation to the resource's slot.
    async fn execute(
        &self,
        full_jid: &FullJid,
        operation: AccountResourceOperation,
    ) -> Result<BindingReply, AccountCoordinatorError>;

    /// The current authorization to answer the client's bind, or `None`
    /// when the binding is not the resource's active binding any more.
    async fn bind_success(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
    ) -> Result<Option<AccountResourceBindSuccessReceipt>, AccountCoordinatorError>;

    /// Record a one-time presentation. `true` means this call recorded it
    /// and the connection presents; `false` means it must not.
    async fn mark_presented(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        presentation: Presentation,
    ) -> Result<bool, AccountCoordinatorError>;

    /// Start retiring one exact binding. A binding that is not the
    /// resource's current one is a no-op with a typed result.
    async fn unregister(
        &self,
        full_jid: &FullJid,
        request: UnregisterRequest,
    ) -> Result<UnregisterOutcome, AccountCoordinatorError>;

    /// Complete a retirement whose endpoint evidence is overdue.
    async fn repair(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        evidence: RepairEvidence,
    ) -> Result<RepairOutcome, AccountCoordinatorError>;

    /// Release a resource's reference to a terminal stream-management
    /// session by retiring the detached binding that holds it.
    async fn release_session(
        &self,
        full_jid: &FullJid,
        request: ReleaseSessionRequest,
    ) -> Result<SessionRelease, AccountCoordinatorError>;

    /// The account's bindings whose connection is gone.
    async fn expired_bindings(
        &self,
        account: &BareJid,
    ) -> Result<Vec<(FullJid, BindingRef)>, AccountCoordinatorError>;

    /// One coherent read of the account's resources.
    async fn view(&self, account: &BareJid) -> Result<AccountView, AccountCoordinatorError>;
}
