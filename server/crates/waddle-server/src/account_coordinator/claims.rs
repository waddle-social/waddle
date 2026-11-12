//! Where a coordinator's claim identity comes from, and which connections
//! it may treat as live.

use std::sync::Arc;

use async_trait::async_trait;
use jid::BareJid;
use thiserror::Error;
use waddle_xmpp::ownership::{
    BindingRef, ClaimEpoch, ClaimStore, Entity, EntityType, NodeLeaseIdentity, SharedNodeIdentity,
    SmSessionKey, UserClaimIdentity,
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

/// What is known about the connection a stored binding names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingLiveness {
    /// The connection's node holds a fresh lease.
    Live,
    /// The connection's node lost its lease, or its process ended. The
    /// connection is gone and the binding can be retired without it.
    Expired,
    /// Nothing proves either. The binding is not adopted, not routed to
    /// and not retired on this evidence.
    Unknown,
}

/// Whether the connection a stored binding names still exists. A
/// coordinator that takes over an account adopts only bindings on live
/// connections.
#[async_trait]
pub trait ConnectionLiveness: Send + Sync + 'static {
    /// The liveness of `binding`'s connection. `session` is the binding's
    /// stream-management session, when it has one.
    async fn liveness(
        &self,
        binding: &BindingRef,
        session: Option<SmSessionKey>,
    ) -> BindingLiveness;
}

/// Liveness as one node knows it without asking the cluster. A connection
/// accepted under this node's current lease is live. On a single node, a
/// connection of a previous process is expired, because that process has
/// ended. A connection hosted by another node is unknown.
#[derive(Debug, Clone)]
pub struct LocalNodeLiveness {
    lease: NodeLeaseIdentity,
}

impl LocalNodeLiveness {
    pub fn new(lease: NodeLeaseIdentity) -> Self {
        Self { lease }
    }
}

#[async_trait]
impl ConnectionLiveness for LocalNodeLiveness {
    async fn liveness(
        &self,
        binding: &BindingRef,
        _session: Option<SmSessionKey>,
    ) -> BindingLiveness {
        let lease = binding.connection_ref().node_lease();
        if lease == &self.lease {
            BindingLiveness::Live
        } else if lease.clustered_node().is_none() && self.lease.clustered_node().is_none() {
            BindingLiveness::Expired
        } else {
            BindingLiveness::Unknown
        }
    }
}

/// Liveness on a clustered node, from the claim store's read of the node
/// lease. The only lease read the claim store exposes is the freshness of
/// the owner of a claim, so a binding hosted by another node is judged
/// through its stream-management session's claim: when that claim is held
/// by the node the binding's connection names, the node's lease decides.
/// A binding without a session, or whose session claim is absent or held
/// by a different node, is unknown.
#[derive(Clone)]
pub struct ClusterLeaseLiveness {
    lease: NodeLeaseIdentity,
    claim_store: Arc<dyn ClaimStore>,
}

impl ClusterLeaseLiveness {
    pub fn new(lease: NodeLeaseIdentity, claim_store: Arc<dyn ClaimStore>) -> Self {
        Self { lease, claim_store }
    }
}

#[async_trait]
impl ConnectionLiveness for ClusterLeaseLiveness {
    async fn liveness(
        &self,
        binding: &BindingRef,
        session: Option<SmSessionKey>,
    ) -> BindingLiveness {
        let lease = binding.connection_ref().node_lease();
        if lease == &self.lease {
            return BindingLiveness::Live;
        }
        let (Some(host), Some(session)) = (lease.clustered_node(), session) else {
            return BindingLiveness::Unknown;
        };
        let entity = Entity::new(EntityType::SmSession, session.as_uuid().to_string());
        match self.claim_store.current_claim(&entity).await {
            Ok(Some(claim)) if claim.owner == host => {
                if claim.owner_lease_fresh {
                    BindingLiveness::Live
                } else {
                    BindingLiveness::Expired
                }
            }
            Ok(_) | Err(_) => BindingLiveness::Unknown,
        }
    }
}
