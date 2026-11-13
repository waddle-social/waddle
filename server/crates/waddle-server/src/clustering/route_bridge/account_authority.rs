//! The account authority across nodes: the sending side that reaches an
//! account's owning node, and the receiving side that serves it.

use std::sync::Arc;

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use waddle_xmpp::ownership::{BindingRef, NodeIdentity};
use waddle_xmpp::registry::{ForceDetachOrigin, ForceDetachRequest};

use super::{
    NodeId, OrderedRelayDeliveryBridge, RelayAskError, RelayHandle, RelaySendEffect,
    RelaySendFailure,
};
use crate::account_coordinator::{
    AccountAuthorityAnswer, AccountAuthorityPeers, AccountAuthorityRequest, AccountClaimError,
    AccountCoordinatorError, AccountOwner, ForwardFailure,
};
use crate::clustering::addressing::Ownership;
use crate::clustering::relay::{AccountBindingReplyWire, AccountBindingRequestWire};
use crate::clustering::ClusteringHandles;

/// The cluster as a node's routed account authority reaches it: the claim
/// store names an account's owner, and the relay carries requests to it.
pub struct ClusterAccountAuthorityPeers {
    handles: ClusteringHandles,
    bridge: Arc<OrderedRelayDeliveryBridge>,
}

impl ClusterAccountAuthorityPeers {
    /// `None` on a node without the claim store, a node identity or the
    /// relay: such a node cannot reach another one.
    pub fn new(handles: &ClusteringHandles) -> Option<Self> {
        let bridge = handles.ordered_relay_delivery_bridge.clone()?;
        handles.claim_store.as_ref()?;
        handles.node_identity.as_ref()?;
        Some(Self {
            handles: handles.clone(),
            bridge,
        })
    }

    async fn ask(
        &self,
        node: &NodeIdentity,
        request: AccountBindingRequestWire,
    ) -> Result<AccountBindingReplyWire, ForwardFailure> {
        let mut relay = RelayHandle::new(
            NodeId::new(node.node_id.clone()),
            self.bridge.stop_token.clone(),
        )
        .with_ask_timeouts(self.bridge.mailbox_timeout, self.bridge.reply_timeout);
        relay
            .account_binding(request)
            .await
            .map_err(forward_failure)
    }
}

/// What a failed ask establishes about the request.
fn forward_failure(error: RelayAskError) -> ForwardFailure {
    match error {
        RelayAskError::NotFound { .. } => ForwardFailure::NotDelivered,
        // The peer refused the message id before any handler ran: it
        // predates the message, and asking again will not change that.
        RelayAskError::Send {
            failure: RelaySendFailure::Codec,
            effect: RelaySendEffect::NoEffect,
            ..
        } => ForwardFailure::Unsupported,
        RelayAskError::Send {
            effect: RelaySendEffect::NoEffect,
            ..
        } => ForwardFailure::NotDelivered,
        RelayAskError::Send {
            effect: RelaySendEffect::MaybeCommitted,
            ..
        }
        | RelayAskError::Cancelled => ForwardFailure::Unknown,
    }
}

#[async_trait]
impl AccountAuthorityPeers for ClusterAccountAuthorityPeers {
    async fn owner(&self, account: &BareJid) -> Result<AccountOwner, AccountClaimError> {
        match self.handles.account_owner(account).await {
            Ok(Ownership::Remote(node)) => Ok(AccountOwner::Remote(node)),
            // Nobody live holds the account, or this node does: the local
            // coordinator takes the claim, or takes the account over.
            Ok(
                Ownership::NotClustered
                | Ownership::Unclaimed
                | Ownership::Local
                | Ownership::Stale,
            ) => Ok(AccountOwner::Local),
            Err(_) => Err(AccountClaimError::LookupFailed),
        }
    }

    async fn forward(
        &self,
        owner: &NodeIdentity,
        request: AccountAuthorityRequest,
    ) -> Result<Result<AccountAuthorityAnswer, AccountCoordinatorError>, ForwardFailure> {
        let wire = (&request)
            .try_into()
            .map_err(|_| ForwardFailure::Unsupported)?;
        match self
            .ask(owner, AccountBindingRequestWire::Authority(wire))
            .await?
        {
            AccountBindingReplyWire::Answered(answer) => answer
                .try_into()
                .map(Ok)
                // The owner decided, but its answer cannot be read here.
                .map_err(|_| ForwardFailure::Unknown),
            AccountBindingReplyWire::Refused(error) => Ok(Err(error.into())),
            AccountBindingReplyWire::Unavailable => Err(ForwardFailure::Unsupported),
            AccountBindingReplyWire::Displaced(_) => Err(ForwardFailure::Unknown),
        }
    }

    async fn displace(
        &self,
        host: &NodeIdentity,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
    ) -> Result<bool, ForwardFailure> {
        let request = AccountBindingRequestWire::Displace {
            full_jid: full_jid.clone(),
            binding_ref: binding_ref.clone(),
        };
        match self.ask(host, request).await? {
            AccountBindingReplyWire::Displaced(found) => Ok(found),
            AccountBindingReplyWire::Unavailable => Err(ForwardFailure::Unsupported),
            AccountBindingReplyWire::Answered(_) | AccountBindingReplyWire::Refused(_) => {
                Err(ForwardFailure::Unknown)
            }
        }
    }
}

impl OrderedRelayDeliveryBridge {
    /// Serve one `account_binding.v1` request on this node.
    ///
    /// An authority request is served by this node's own coordinators,
    /// never routed on: a node that does not hold the account's claim
    /// refuses it, and the sender looks the owner up again. A displacement
    /// closes the connection of this node that holds the binding, as a
    /// fresh bind's replacement; the binding's slot shows when it retired.
    pub(crate) async fn account_binding_local(
        &self,
        request: AccountBindingRequestWire,
    ) -> AccountBindingReplyWire {
        let Some(services) = self.services.get() else {
            return AccountBindingReplyWire::Unavailable;
        };
        match request {
            AccountBindingRequestWire::Authority(wire) => {
                let Some(state) = services.web_socket_state.upgrade() else {
                    return AccountBindingReplyWire::Unavailable;
                };
                let Some(coordinators) = state
                    .deps
                    .app_state
                    .account_authority()
                    .coordinators()
                    .cloned()
                else {
                    return AccountBindingReplyWire::Unavailable;
                };
                let Ok(request) = AccountAuthorityRequest::try_from(wire) else {
                    return AccountBindingReplyWire::Unavailable;
                };
                match coordinators.serve(request).await {
                    Ok(answer) => AccountBindingReplyWire::Answered((&answer).into()),
                    Err(error) => AccountBindingReplyWire::Refused((&error).into()),
                }
            }
            AccountBindingRequestWire::Displace {
                full_jid,
                binding_ref,
            } => {
                let Some(entry) = services
                    .connection_registry
                    .entry(&full_jid)
                    .filter(|entry| entry.binding_ref() == Some(&binding_ref))
                else {
                    return AccountBindingReplyWire::Displaced(false);
                };
                // The acknowledgement is not awaited: the slot at the
                // account's coordinator is the evidence of the retirement.
                let (ack, _ack_rx) = tokio::sync::oneshot::channel();
                let told = entry
                    .try_force_detach_and_interrupt(ForceDetachRequest {
                        origin: ForceDetachOrigin::FreshBindReplacement,
                        target_binding: Some(binding_ref),
                        requester_bare_jid: full_jid.to_bare(),
                        ack,
                    })
                    .is_ok();
                AccountBindingReplyWire::Displaced(told)
            }
        }
    }
}
