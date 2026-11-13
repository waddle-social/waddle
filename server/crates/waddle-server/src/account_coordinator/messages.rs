//! The typed requests and replies of an account coordinator.

use jid::FullJid;
use waddle_xmpp::ownership::account_coordinator::{
    AccountResourcePresence, AccountResourceRouting, BindingFenceError, Presentation,
};
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindingState, AccountResourceExecution, AccountResourceOperation,
    AccountResourceOperationId, AccountResourceRevision, AccountResourceTransitionError,
    BindingRetirementDisposition, DirectedPresenceTeardownDisposition,
    PresentationFlushDisposition,
};
use waddle_xmpp::ownership::{BindingRef, SmSessionKey, UserClaimIdentity};

/// The reply to a binding operation.
#[derive(Debug, Clone, PartialEq)]
pub enum BindingReply {
    /// The resource's slot decided the operation: `Applied` or `Replayed`,
    /// with `Committed { revision }` or a typed rejection.
    Executed(AccountResourceExecution),
    /// A bind named a resource that already has a binding. The coordinator
    /// recorded the bind as the resource's pending replacement and did not
    /// wait for the displaced connection. The displaced binding retires on
    /// its own; the replacing connection then binds again with a new
    /// operation identity at the revision the retirement left.
    ReplacementPending {
        displaced: BindingRef,
        revision: AccountResourceRevision,
    },
}

/// What a connection needs to form its next mutation for a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotStatus {
    /// The claim identity mutations of this resource must name.
    pub claim_identity: UserClaimIdentity,
    /// The slot's revision; the initial revision when the account has no
    /// slot for the resource.
    pub revision: AccountResourceRevision,
    /// The resource's current binding and its lifecycle state.
    pub binding: Option<(BindingRef, AccountResourceBindingState)>,
}

/// One coherent read of an account's resources. Every list is ordered by
/// full JID and all are taken in one coordinator turn, so destination
/// selection and carbon policy agree.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AccountView {
    /// Every resource with an active binding, with no presence filter.
    pub resources: Vec<FullJid>,
    /// Every presence-available resource with its priority.
    pub available: Vec<(FullJid, i8)>,
    /// The destinations of a stanza addressed to the bare JID.
    pub routable: Vec<FullJid>,
    /// Availability, priority and carbons for every active resource.
    pub routing: Vec<AccountResourceRouting>,
    /// The announced presence of every presence-available resource.
    pub presence: Vec<AccountResourcePresence>,
    /// Active resources that want roster pushes.
    pub roster_interested: Vec<FullJid>,
    /// Active resources that want block-list pushes.
    pub blocklist_interested: Vec<FullJid>,
}

/// A request to start retiring one exact binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnregisterRequest {
    /// The identity the caller repeats the request under.
    pub operation: AccountResourceOperationId,
    pub binding_ref: BindingRef,
    /// What became of the work the binding had accepted.
    pub custody_disposition: BindingRetirementDisposition,
}

/// The result of an unregister.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnregisterOutcome {
    /// The retirement started, now or under an earlier use of the
    /// operation identity. The binding is no longer routable and awaits
    /// its endpoint's evidence.
    Retiring { revision: AccountResourceRevision },
    /// The binding named is not the resource's current binding. Nothing
    /// changed.
    Stale(BindingFenceError),
    /// The slot refused to start the retirement.
    Rejected(AccountResourceTransitionError),
}

/// What the repairer established about a retiring binding's teardown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairEvidence {
    /// The identity the caller repeats the repair under.
    pub operation: AccountResourceOperationId,
    pub directed_presence: DirectedPresenceTeardownDisposition,
    pub presentation_flush: PresentationFlushDisposition,
}

/// The result of repairing a retirement whose evidence is overdue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairOutcome {
    /// The retirement is complete and the resource is free.
    Completed { revision: AccountResourceRevision },
    /// The stored row holds a different binding, which is left untouched.
    HeldByNewerBinding(BindingRef),
    /// The stored row holds the binding in a state that is not retiring;
    /// repair does not start a retirement.
    NotRetiring(AccountResourceBindingState),
    /// The slot refused the completion. The binding stays non-routable.
    Rejected(AccountResourceTransitionError),
}

/// A request to release a resource row's reference to a stream-management
/// session that reached a terminal state, by retiring the binding that
/// holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseSessionRequest {
    /// The identity the retirement's start is repeated under.
    pub begin: AccountResourceOperationId,
    /// The identity the retirement's completion is repeated under.
    pub complete: AccountResourceOperationId,
    pub session: SmSessionKey,
    /// What became of the work the session had accepted.
    pub custody_disposition: BindingRetirementDisposition,
    pub directed_presence: DirectedPresenceTeardownDisposition,
    pub presentation_flush: PresentationFlushDisposition,
}

/// The result of releasing a session reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRelease {
    /// The binding is retired and the resource row no longer names the
    /// session.
    Released { revision: AccountResourceRevision },
    /// The resource's current binding does not reference the session.
    NotReferenced,
    /// The session is referenced by a binding that still has a connection.
    /// That connection retires it; the coordinator does not.
    HeldByConnection(BindingRef, AccountResourceBindingState),
    /// The slot refused the retirement. The reference stays.
    Rejected(AccountResourceTransitionError),
}

/// What a coordinator did with the rows it found under a previous claim
/// identity when it loaded the account.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdoptionReport {
    /// Bindings now held under this claim: those on live connections,
    /// detached ones, and active ones on a lost connection whose session
    /// can resume, which the coordinator detaches once the account is
    /// loaded.
    pub adopted: Vec<(FullJid, BindingRef)>,
    /// Bindings moved to this claim but not adopted: their connection's
    /// liveness is unknown. They are not routable and stay so until they
    /// are retired.
    pub unadopted: Vec<(FullJid, BindingRef)>,
    /// Bindings moved to this claim whose connection is gone: its node
    /// lost its lease or its process ended. They are not routable, and the
    /// coordinator retires them once the account is loaded.
    pub expired: Vec<(FullJid, BindingRef)>,
    /// Bindings whose row could not be moved to this claim, with the
    /// slot's reason. They remain under the previous claim.
    pub refused: Vec<(FullJid, BindingRef, AccountResourceTransitionError)>,
}

pub(super) struct Execute {
    pub(super) full_jid: FullJid,
    pub(super) operation: AccountResourceOperation,
}

pub(super) struct Status {
    pub(super) full_jid: FullJid,
}

pub(super) struct BindSuccess {
    pub(super) full_jid: FullJid,
    pub(super) binding_ref: BindingRef,
}

pub(super) struct MarkPresented {
    pub(super) full_jid: FullJid,
    pub(super) binding_ref: BindingRef,
    pub(super) presentation: Presentation,
}

pub(super) struct Unregister {
    pub(super) full_jid: FullJid,
    pub(super) request: UnregisterRequest,
}

pub(super) struct Repair {
    pub(super) full_jid: FullJid,
    pub(super) binding_ref: BindingRef,
    pub(super) evidence: RepairEvidence,
}

pub(super) struct ReleaseSession {
    pub(super) full_jid: FullJid,
    pub(super) request: ReleaseSessionRequest,
}

pub(super) struct ExpiredBindings;

pub(super) struct ReadView {
    /// Take the account over when its claim's holder lost its node lease,
    /// instead of failing the read.
    pub(super) take_over: bool,
}

/// Detach or retire the account's bindings whose connection is gone.
pub(super) struct SettleExpired;

pub(super) struct OverdueRetirements;

pub(super) struct Adopt;

/// Stop the coordinator if its account holds no binding and has been idle.
pub(super) struct ReclaimIfIdle;

/// Release the account's claim if the account holds no binding.
pub(super) struct ReleaseClaimIfUnbound;
