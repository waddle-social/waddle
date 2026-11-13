//! The account authority of a clustered node: each request is decided by
//! the coordinator on the node that holds the account's claim.
//!
//! A node runs coordinators only for the accounts whose claim it holds, so
//! a connection on any other node reaches its account's coordinator
//! through the cluster relay. [`RoutedAccountAuthority`] resolves the
//! account's owner for every request: this node serves it with its own
//! [`AccountCoordinators`], and another live node is sent the request
//! through [`AccountAuthorityPeers`]. An account nobody holds, or whose
//! holder lost its node lease, is served here: the local coordinator takes
//! the claim, or takes the account over, as it does on a single node.
//!
//! A request whose answer was lost is sent again under the same identity.
//! Every mutation carries its operation identity, and the owning node's
//! binding store records the outcome of each identity once
//! (`account_resource_operations`), so a repeat returns the recorded
//! outcome instead of applying the mutation again, whichever node serves
//! the repeat. A one-time presentation carries no identity and is not
//! repeated after an unknown outcome; the caller then does not present.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use waddle_xmpp::ownership::account_coordinator::Presentation;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindSuccessReceipt, AccountResourceOperation,
};
use waddle_xmpp::ownership::{BindingRef, NodeIdentity};

use super::authority::AccountBindingAuthority;
use super::claims::AccountClaimError;
use super::directory::AccountCoordinators;
use super::error::AccountCoordinatorError;
use super::messages::{
    AccountView, BindingReply, ReleaseSessionRequest, RepairEvidence, RepairOutcome,
    SessionRelease, SlotStatus, UnregisterOutcome, UnregisterRequest,
};
use crate::account_bindings::AccountBindingStore;

/// One request to an account's authority, as the owning node serves it.
#[derive(Debug, Clone, PartialEq)]
pub enum AccountAuthorityRequest {
    SlotStatus {
        full_jid: FullJid,
    },
    Execute {
        full_jid: FullJid,
        operation: Box<AccountResourceOperation>,
    },
    BindSuccess {
        full_jid: FullJid,
        binding_ref: BindingRef,
    },
    MarkPresented {
        full_jid: FullJid,
        binding_ref: BindingRef,
        presentation: Presentation,
    },
    Unregister {
        full_jid: FullJid,
        request: UnregisterRequest,
    },
    Repair {
        full_jid: FullJid,
        binding_ref: BindingRef,
        evidence: RepairEvidence,
    },
    ReleaseSession {
        full_jid: FullJid,
        request: ReleaseSessionRequest,
    },
    ExpiredBindings {
        account: BareJid,
    },
    View {
        account: BareJid,
    },
}

impl AccountAuthorityRequest {
    /// The account whose owner decides the request.
    pub fn account(&self) -> BareJid {
        match self {
            Self::SlotStatus { full_jid }
            | Self::Execute { full_jid, .. }
            | Self::BindSuccess { full_jid, .. }
            | Self::MarkPresented { full_jid, .. }
            | Self::Unregister { full_jid, .. }
            | Self::Repair { full_jid, .. }
            | Self::ReleaseSession { full_jid, .. } => full_jid.to_bare(),
            Self::ExpiredBindings { account } | Self::View { account } => account.clone(),
        }
    }

    /// Whether the request can be sent again after its outcome was lost.
    /// Reads can; every mutation names the identity its outcome is recorded
    /// under. A presentation is recorded without one, so a repeat could
    /// answer that it was already recorded when this caller recorded it.
    pub fn repeatable(&self) -> bool {
        !matches!(self, Self::MarkPresented { .. })
    }
}

/// The answer of an account's authority to one request.
#[derive(Debug, Clone, PartialEq)]
pub enum AccountAuthorityAnswer {
    SlotStatus(Box<SlotStatus>),
    Executed(BindingReply),
    BindSuccess(Option<Box<AccountResourceBindSuccessReceipt>>),
    Presented(bool),
    Unregistered(UnregisterOutcome),
    Repaired(RepairOutcome),
    SessionReleased(SessionRelease),
    ExpiredBindings(Vec<(FullJid, BindingRef)>),
    View(AccountView),
}

impl AccountCoordinators {
    /// Serve one authority request with this node's coordinators. A node
    /// that does not hold the account's claim answers
    /// [`AccountClaimError::HeldElsewhere`] and changes nothing.
    pub async fn serve(
        &self,
        request: AccountAuthorityRequest,
    ) -> Result<AccountAuthorityAnswer, AccountCoordinatorError> {
        Ok(match request {
            AccountAuthorityRequest::SlotStatus { full_jid } => {
                AccountAuthorityAnswer::SlotStatus(Box::new(self.slot_status(&full_jid).await?))
            }
            AccountAuthorityRequest::Execute {
                full_jid,
                operation,
            } => AccountAuthorityAnswer::Executed(self.execute(&full_jid, *operation).await?),
            AccountAuthorityRequest::BindSuccess {
                full_jid,
                binding_ref,
            } => AccountAuthorityAnswer::BindSuccess(
                self.bind_success(&full_jid, &binding_ref)
                    .await?
                    .map(Box::new),
            ),
            AccountAuthorityRequest::MarkPresented {
                full_jid,
                binding_ref,
                presentation,
            } => AccountAuthorityAnswer::Presented(
                self.mark_presented(&full_jid, &binding_ref, presentation)
                    .await?,
            ),
            AccountAuthorityRequest::Unregister { full_jid, request } => {
                AccountAuthorityAnswer::Unregistered(self.unregister(&full_jid, request).await?)
            }
            AccountAuthorityRequest::Repair {
                full_jid,
                binding_ref,
                evidence,
            } => AccountAuthorityAnswer::Repaired(
                AccountCoordinators::repair(self, &full_jid, &binding_ref, evidence).await?,
            ),
            AccountAuthorityRequest::ReleaseSession { full_jid, request } => {
                AccountAuthorityAnswer::SessionReleased(
                    AccountCoordinators::release_session(self, &full_jid, request).await?,
                )
            }
            AccountAuthorityRequest::ExpiredBindings { account } => {
                AccountAuthorityAnswer::ExpiredBindings(
                    AccountCoordinators::expired_bindings(self, &account).await?,
                )
            }
            AccountAuthorityRequest::View { account } => {
                AccountAuthorityAnswer::View(AccountCoordinators::view(self, &account).await?)
            }
        })
    }
}

/// Which node decides an account's requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountOwner {
    /// This node: it holds the claim, or nobody live does and this node
    /// takes it.
    Local,
    /// Another node holds the claim under a fresh lease.
    Remote(NodeIdentity),
}

/// Why a request sent to another node has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardFailure {
    /// The request did not reach the other node's handler; nothing was
    /// decided.
    NotDelivered,
    /// The request may have been handled; its outcome is unknown.
    Unknown,
    /// The other node does not serve the request: it does not run the
    /// account coordinator, or it predates the message.
    Unsupported,
}

/// The cluster as the routed authority reaches it.
#[async_trait]
pub trait AccountAuthorityPeers: Send + Sync + 'static {
    /// The node that decides `account`'s requests now. The answer is a read
    /// of the claim and can be stale by the time the request arrives; the
    /// node that receives a request it does not own refuses it.
    async fn owner(&self, account: &BareJid) -> Result<AccountOwner, AccountClaimError>;

    /// Have `owner` serve `request` with its coordinators and return its
    /// answer, or its error, as [`AccountCoordinators::serve`] returns it.
    async fn forward(
        &self,
        owner: &NodeIdentity,
        request: AccountAuthorityRequest,
    ) -> Result<Result<AccountAuthorityAnswer, AccountCoordinatorError>, ForwardFailure>;

    /// Ask `host`, the node whose connection holds `binding_ref`, to close
    /// that connection as replaced by a fresh bind. `Ok(true)` means the
    /// connection was found and told; `Ok(false)` that `host` holds no
    /// connection with that binding.
    async fn displace(
        &self,
        host: &NodeIdentity,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
    ) -> Result<bool, ForwardFailure>;
}

/// How long a routed request keeps trying to reach its account's owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountRoutingPolicy {
    /// The whole budget of one request, across owner reads and attempts.
    /// It stays under the budget a bind has to wait for a displaced
    /// binding, so a bind that cannot reach the owner fails before the
    /// client's bind times out.
    pub deadline: Duration,
    /// The wait before the second attempt; each later wait doubles, up to
    /// `max_backoff`.
    pub first_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for AccountRoutingPolicy {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(8),
            first_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_secs(1),
        }
    }
}

/// The account authority of this node, which sends each request to the
/// node that owns the account. Without peers every request is served by
/// this node's coordinators.
#[derive(Clone)]
pub struct RoutedAccountAuthority {
    local: Arc<AccountCoordinators>,
    peers: Option<Arc<dyn AccountAuthorityPeers>>,
    policy: AccountRoutingPolicy,
}

/// What one attempt at a request established.
enum Attempt {
    Decided(Result<AccountAuthorityAnswer, AccountCoordinatorError>),
    /// Nothing was decided, or a repeat is safe: try again.
    Retry,
}

impl RoutedAccountAuthority {
    /// An authority whose every request is served by `local`.
    pub fn local_only(local: Arc<AccountCoordinators>) -> Self {
        Self {
            local,
            peers: None,
            policy: AccountRoutingPolicy::default(),
        }
    }

    /// An authority that sends a request for an account another node owns
    /// to that node through `peers`.
    pub fn new(local: Arc<AccountCoordinators>, peers: Arc<dyn AccountAuthorityPeers>) -> Self {
        Self {
            local,
            peers: Some(peers),
            policy: AccountRoutingPolicy::default(),
        }
    }

    pub fn with_policy(mut self, policy: AccountRoutingPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// This node's own coordinators.
    pub fn local(&self) -> &Arc<AccountCoordinators> {
        &self.local
    }

    /// The durable rows the coordinators commit to. Every node reads the
    /// same rows; reads of them are not decisions.
    pub fn store(&self) -> &Arc<dyn AccountBindingStore> {
        self.local.store()
    }

    /// Ask the node whose connection holds `binding_ref` to close that
    /// connection as replaced by a fresh bind. Returns whether that node
    /// was reached and told; a binding on this node, or on a node that is
    /// not clustered, is not asked about.
    pub async fn displace_remote(&self, full_jid: &FullJid, binding_ref: &BindingRef) -> bool {
        let Some(peers) = self.peers.as_ref() else {
            return false;
        };
        let Some(host) = binding_ref.connection_ref().node_lease().clustered_node() else {
            return false;
        };
        match peers.displace(&host, full_jid, binding_ref).await {
            Ok(found) => found,
            Err(failure) => {
                tracing::debug!(%full_jid, ?failure, "the node hosting a displaced binding was not reached");
                false
            }
        }
    }

    async fn route(
        &self,
        request: AccountAuthorityRequest,
    ) -> Result<AccountAuthorityAnswer, AccountCoordinatorError> {
        let Some(peers) = self.peers.as_ref() else {
            return self.local.serve(request).await;
        };
        let account = request.account();
        let deadline = tokio::time::Instant::now() + self.policy.deadline;
        let mut backoff = self.policy.first_backoff;
        loop {
            let attempt = match peers.owner(&account).await {
                Ok(AccountOwner::Local) => {
                    Attempt::Decided(self.local.serve(request.clone()).await)
                }
                Ok(AccountOwner::Remote(owner)) => {
                    match peers.forward(&owner, request.clone()).await {
                        Ok(decided) => Attempt::Decided(decided),
                        Err(ForwardFailure::NotDelivered) => Attempt::Retry,
                        Err(ForwardFailure::Unknown) if request.repeatable() => Attempt::Retry,
                        Err(ForwardFailure::Unknown | ForwardFailure::Unsupported) => {
                            return Err(AccountCoordinatorError::OwnerUndecided)
                        }
                    }
                }
                // The claim store did not answer: nothing was sent.
                Err(_) => Attempt::Retry,
            };
            match attempt {
                // The claim moved between the owner read and the request;
                // the node that refused it changed nothing.
                Attempt::Decided(Err(AccountCoordinatorError::Claim(
                    AccountClaimError::HeldElsewhere,
                )))
                | Attempt::Retry => {}
                Attempt::Decided(decided) => return decided,
            }
            if tokio::time::Instant::now() + backoff > deadline {
                return Err(AccountCoordinatorError::OwnerUndecided);
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(self.policy.max_backoff);
        }
    }
}

/// An answer of the wrong kind: the owner and this node disagree about the
/// request.
fn diverged<T>() -> Result<T, AccountCoordinatorError> {
    Err(AccountCoordinatorError::Diverged)
}

#[async_trait]
impl AccountBindingAuthority for RoutedAccountAuthority {
    async fn slot_status(&self, full_jid: &FullJid) -> Result<SlotStatus, AccountCoordinatorError> {
        let request = AccountAuthorityRequest::SlotStatus {
            full_jid: full_jid.clone(),
        };
        match self.route(request).await? {
            AccountAuthorityAnswer::SlotStatus(status) => Ok(*status),
            _ => diverged(),
        }
    }

    async fn execute(
        &self,
        full_jid: &FullJid,
        operation: AccountResourceOperation,
    ) -> Result<BindingReply, AccountCoordinatorError> {
        let request = AccountAuthorityRequest::Execute {
            full_jid: full_jid.clone(),
            operation: Box::new(operation),
        };
        match self.route(request).await? {
            AccountAuthorityAnswer::Executed(reply) => Ok(reply),
            _ => diverged(),
        }
    }

    async fn bind_success(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
    ) -> Result<Option<AccountResourceBindSuccessReceipt>, AccountCoordinatorError> {
        let request = AccountAuthorityRequest::BindSuccess {
            full_jid: full_jid.clone(),
            binding_ref: binding_ref.clone(),
        };
        match self.route(request).await? {
            AccountAuthorityAnswer::BindSuccess(receipt) => Ok(receipt.map(|receipt| *receipt)),
            _ => diverged(),
        }
    }

    async fn mark_presented(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        presentation: Presentation,
    ) -> Result<bool, AccountCoordinatorError> {
        let request = AccountAuthorityRequest::MarkPresented {
            full_jid: full_jid.clone(),
            binding_ref: binding_ref.clone(),
            presentation,
        };
        match self.route(request).await? {
            AccountAuthorityAnswer::Presented(first) => Ok(first),
            _ => diverged(),
        }
    }

    async fn unregister(
        &self,
        full_jid: &FullJid,
        request: UnregisterRequest,
    ) -> Result<UnregisterOutcome, AccountCoordinatorError> {
        let request = AccountAuthorityRequest::Unregister {
            full_jid: full_jid.clone(),
            request,
        };
        match self.route(request).await? {
            AccountAuthorityAnswer::Unregistered(outcome) => Ok(outcome),
            _ => diverged(),
        }
    }

    async fn repair(
        &self,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        evidence: RepairEvidence,
    ) -> Result<RepairOutcome, AccountCoordinatorError> {
        let request = AccountAuthorityRequest::Repair {
            full_jid: full_jid.clone(),
            binding_ref: binding_ref.clone(),
            evidence,
        };
        match self.route(request).await? {
            AccountAuthorityAnswer::Repaired(outcome) => Ok(outcome),
            _ => diverged(),
        }
    }

    async fn release_session(
        &self,
        full_jid: &FullJid,
        request: ReleaseSessionRequest,
    ) -> Result<SessionRelease, AccountCoordinatorError> {
        let request = AccountAuthorityRequest::ReleaseSession {
            full_jid: full_jid.clone(),
            request,
        };
        match self.route(request).await? {
            AccountAuthorityAnswer::SessionReleased(release) => Ok(release),
            _ => diverged(),
        }
    }

    async fn expired_bindings(
        &self,
        account: &BareJid,
    ) -> Result<Vec<(FullJid, BindingRef)>, AccountCoordinatorError> {
        let request = AccountAuthorityRequest::ExpiredBindings {
            account: account.clone(),
        };
        match self.route(request).await? {
            AccountAuthorityAnswer::ExpiredBindings(bindings) => Ok(bindings),
            _ => diverged(),
        }
    }

    async fn view(&self, account: &BareJid) -> Result<AccountView, AccountCoordinatorError> {
        let request = AccountAuthorityRequest::View {
            account: account.clone(),
        };
        match self.route(request).await? {
            AccountAuthorityAnswer::View(view) => Ok(view),
            _ => diverged(),
        }
    }
}
