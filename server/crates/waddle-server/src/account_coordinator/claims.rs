//! Where a coordinator's claim identity comes from, and which connections
//! it may treat as live.

use std::sync::Arc;

use async_trait::async_trait;
use jid::BareJid;
use thiserror::Error;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimStore, ConnectionRef, Entity, EntityType, NodeLeaseIdentity,
    SharedNodeIdentity, UserClaimIdentity,
};

/// Why this node cannot act as an account's coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AccountClaimError {
    #[error("the account has no claim")]
    Unclaimed,
    #[error("the account's claim is held by another node")]
    HeldElsewhere,
    #[error("the account's claim is held under an expired node lease")]
    Stale,
    #[error("the claim store did not answer")]
    LookupFailed,
}

/// The claim under which this node coordinates an account. Each call mints
/// the identity of a new coordinator incarnation, so the directory asks
/// once per coordinator it creates.
#[async_trait]
pub trait AccountClaimSource: Send + Sync + 'static {
    async fn current_claim(
        &self,
        account: &BareJid,
    ) -> Result<UserClaimIdentity, AccountClaimError>;
}

/// Claims of a single-node process: every account belongs to this process
/// for as long as it runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessClaimSource;

#[async_trait]
impl AccountClaimSource for ProcessClaimSource {
    async fn current_claim(
        &self,
        account: &BareJid,
    ) -> Result<UserClaimIdentity, AccountClaimError> {
        Ok(UserClaimIdentity::fresh(
            account.clone(),
            NodeLeaseIdentity::local_process(),
            ClaimEpoch(0),
        ))
    }
}

/// Claims read from the cluster claim store. This node coordinates an
/// account only while the account's `UserActor` claim names this node's
/// current incarnation under a fresh lease.
#[derive(Clone)]
pub struct ClaimStoreClaimSource {
    claim_store: Arc<dyn ClaimStore>,
    node_identity: SharedNodeIdentity,
}

impl ClaimStoreClaimSource {
    pub fn new(claim_store: Arc<dyn ClaimStore>, node_identity: SharedNodeIdentity) -> Self {
        Self {
            claim_store,
            node_identity,
        }
    }
}

#[async_trait]
impl AccountClaimSource for ClaimStoreClaimSource {
    async fn current_claim(
        &self,
        account: &BareJid,
    ) -> Result<UserClaimIdentity, AccountClaimError> {
        let entity = Entity::new(EntityType::UserActor, account.to_string());
        let claim = self
            .claim_store
            .current_claim(&entity)
            .await
            .map_err(|_| AccountClaimError::LookupFailed)?
            .ok_or(AccountClaimError::Unclaimed)?;
        if !claim.owner_lease_fresh {
            return Err(AccountClaimError::Stale);
        }
        if self.node_identity.current() != claim.owner {
            return Err(AccountClaimError::HeldElsewhere);
        }
        Ok(UserClaimIdentity::fresh(
            account.clone(),
            NodeLeaseIdentity::from_node_identity(&claim.owner),
            claim.claim_epoch,
        ))
    }
}

/// Whether the connection a stored binding names still exists. A
/// coordinator that takes over an account adopts only bindings on live
/// connections; the rest stay non-routable until they are retired.
pub trait ConnectionLiveness: Send + Sync + 'static {
    fn is_live(&self, connection: &ConnectionRef) -> bool;
}

/// Liveness as one node knows it without asking the cluster: a connection
/// is live when it was accepted under this node's current lease. A
/// connection hosted by another node is not known to be live here.
#[derive(Debug, Clone)]
pub struct LocalNodeLiveness {
    lease: NodeLeaseIdentity,
}

impl LocalNodeLiveness {
    pub fn new(lease: NodeLeaseIdentity) -> Self {
        Self { lease }
    }
}

impl ConnectionLiveness for LocalNodeLiveness {
    fn is_live(&self, connection: &ConnectionRef) -> bool {
        connection.node_lease() == &self.lease
    }
}
