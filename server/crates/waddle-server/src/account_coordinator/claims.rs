//! Where a coordinator's claim identity comes from, and which connections
//! it may treat as live.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use jid::BareJid;
use thiserror::Error;
use waddle_xmpp::ownership::{
    BindingRef, ClaimEpoch, ClaimError, ClaimStore, Entity, EntityType, NodeIdentity,
    NodeLeaseIdentity, SharedNodeIdentity, SmSessionKey, StalePredicate, UserClaimIdentity,
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

/// What became of a claim a coordinator gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimRelease {
    /// The claim is released. Any node may take the account.
    Released,
    /// The claim stays held: this source never releases, or another holder
    /// on this node still needs it.
    Retained,
}

/// Whether a request may take an account's claim for this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClaimNeed {
    /// The request reads or maintains what the account already holds. It
    /// is served only under a claim this node holds already.
    Existing,
    /// The request acts for a binding of the account, so this node takes
    /// the claim when nobody holds it.
    Taken,
}

/// The claim under which this node coordinates an account. Each call that
/// returns an identity mints the identity of a new coordinator incarnation,
/// so a coordinator asks once and keeps the answer until it gives the
/// claim up or loses it.
#[async_trait]
pub trait AccountClaimSource: Send + Sync + 'static {
    /// The claim this node holds on `account` now. Takes nothing.
    async fn current_claim(
        &self,
        account: &BareJid,
    ) -> Result<UserClaimIdentity, AccountClaimError>;

    /// The claim on `account`, taken for this node when the account has
    /// none or its holder's node lease is lost. A source whose accounts
    /// always belong to this node answers as [`Self::current_claim`] does.
    async fn acquire_claim(
        &self,
        account: &BareJid,
    ) -> Result<UserClaimIdentity, AccountClaimError> {
        self.current_claim(account).await
    }

    /// Whether [`Self::release_claim`] can release a claim. A coordinator
    /// of a source that cannot keeps its claim for as long as it runs.
    fn releases_claims(&self) -> bool {
        false
    }

    /// Give up `claim`, which the caller holds for an account without a
    /// binding.
    async fn release_claim(
        &self,
        _claim: &UserClaimIdentity,
    ) -> Result<ClaimRelease, AccountClaimError> {
        Ok(ClaimRelease::Retained)
    }
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

/// The account claims the `UserActor` mirror of this node holds.
///
/// A binding is also registered with the
/// account's `UserActor`, whose registry takes and releases the same
/// `UserActor` claim row through [`MirrorClaimStore`]. The claim must
/// outlive whichever of the two needs it longer, so each release consults
/// the other holder. The lock is held across the claim store call that
/// changes a hold, so a release and an acquisition of one claim cannot
/// interleave.
#[derive(Default)]
pub struct MirrorClaimHolds {
    pub(super) held: tokio::sync::Mutex<HashMap<BareJid, ClaimEpoch>>,
}

fn account_entity(account: &BareJid) -> Entity {
    Entity::new(EntityType::UserActor, account.to_string())
}

/// Claims kept in the cluster claim store. This node coordinates an
/// account only while the account's `UserActor` claim names this node's
/// current incarnation under a fresh lease. The claim is taken for the
/// first binding of an account and released once the account holds no
/// binding and the `UserActor` mirror no longer holds it either.
#[derive(Clone)]
pub struct ClaimStoreClaimSource {
    claim_store: Arc<dyn ClaimStore>,
    node_identity: SharedNodeIdentity,
    mirror: Arc<MirrorClaimHolds>,
}

impl ClaimStoreClaimSource {
    pub fn new(claim_store: Arc<dyn ClaimStore>, node_identity: SharedNodeIdentity) -> Self {
        Self::with_mirror(claim_store, node_identity, Arc::default())
    }

    /// A source that keeps a claim while `mirror` records a hold on it.
    pub fn with_mirror(
        claim_store: Arc<dyn ClaimStore>,
        node_identity: SharedNodeIdentity,
        mirror: Arc<MirrorClaimHolds>,
    ) -> Self {
        Self {
            claim_store,
            node_identity,
            mirror,
        }
    }
}

#[async_trait]
impl AccountClaimSource for ClaimStoreClaimSource {
    async fn current_claim(
        &self,
        account: &BareJid,
    ) -> Result<UserClaimIdentity, AccountClaimError> {
        let claim = self
            .claim_store
            .current_claim(&account_entity(account))
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

    /// Take the claim when the account has none, keep it when this node's
    /// current incarnation holds it, and take it over when its holder's
    /// node lease is lost. A claim another node holds under a fresh lease
    /// is left alone.
    async fn acquire_claim(
        &self,
        account: &BareJid,
    ) -> Result<UserClaimIdentity, AccountClaimError> {
        let entity = account_entity(account);
        let me = self.node_identity.current();
        let epoch = match self.claim_store.ensure_claimed(&entity, &me).await {
            Ok(epoch) => epoch,
            Err(ClaimError::AlreadyClaimed) => {
                let Some(held) = self
                    .claim_store
                    .current_claim(&entity)
                    .await
                    .map_err(|_| AccountClaimError::LookupFailed)?
                else {
                    // The holder released the claim between the two calls.
                    return match self.claim_store.ensure_claimed(&entity, &me).await {
                        Ok(epoch) => Ok(UserClaimIdentity::fresh(
                            account.clone(),
                            NodeLeaseIdentity::from_node_identity(&me),
                            epoch,
                        )),
                        Err(ClaimError::AlreadyClaimed) => Err(AccountClaimError::HeldElsewhere),
                        Err(_) => Err(AccountClaimError::LookupFailed),
                    };
                };
                if held.owner_lease_fresh {
                    return Err(AccountClaimError::HeldElsewhere);
                }
                // Losing this race means another node took the account
                // over, or the holder's lease turned out to be fresh.
                self.claim_store
                    .steal_stale(&entity, held.claim_epoch, StalePredicate::OwnerStale, &me)
                    .await
                    .map_err(|error| match error {
                        ClaimError::Conflict => AccountClaimError::HeldElsewhere,
                        _ => AccountClaimError::LookupFailed,
                    })?
            }
            Err(_) => return Err(AccountClaimError::LookupFailed),
        };
        Ok(UserClaimIdentity::fresh(
            account.clone(),
            NodeLeaseIdentity::from_node_identity(&me),
            epoch,
        ))
    }

    fn releases_claims(&self) -> bool {
        true
    }

    async fn release_claim(
        &self,
        claim: &UserClaimIdentity,
    ) -> Result<ClaimRelease, AccountClaimError> {
        let Some(owner) = claim.owner().clustered_node() else {
            return Ok(ClaimRelease::Retained);
        };
        let mut held = self.mirror.held.lock().await;
        match held.get(claim.bare_jid()).copied() {
            Some(epoch) if epoch == claim.claim_epoch() => return Ok(ClaimRelease::Retained),
            // A hold recorded for another epoch is a lease the registry
            // dropped without releasing it, when its claim was taken over.
            Some(_) => {
                held.remove(claim.bare_jid());
            }
            None => {}
        }
        self.claim_store
            .release(
                &account_entity(claim.bare_jid()),
                &owner,
                claim.claim_epoch(),
            )
            .await
            .map_err(|_| AccountClaimError::LookupFailed)?;
        Ok(ClaimRelease::Released)
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

/// Reads whether another node's lease is unexpired.
#[async_trait]
pub trait NodeLeaseRead: Send + Sync + 'static {
    /// `Some(true)` when the lease of this exact node incarnation is
    /// unexpired, `Some(false)` when it is expired or gone, and `None`
    /// when the read did not answer.
    async fn unexpired(&self, node: &NodeIdentity) -> Option<bool>;
}

/// Liveness on a clustered node, from the node leases. A connection under
/// this node's current lease is live. A connection another node hosts is
/// live while that node incarnation's lease is unexpired and expired once
/// the lease is expired or gone. A lease read that does not answer, and a
/// connection of a standalone process, are unknown.
#[derive(Clone)]
pub struct ClusterLeaseLiveness {
    lease: NodeLeaseIdentity,
    node_leases: Arc<dyn NodeLeaseRead>,
}

impl ClusterLeaseLiveness {
    pub fn new(lease: NodeLeaseIdentity, node_leases: Arc<dyn NodeLeaseRead>) -> Self {
        Self { lease, node_leases }
    }
}

#[async_trait]
impl ConnectionLiveness for ClusterLeaseLiveness {
    async fn liveness(
        &self,
        binding: &BindingRef,
        _session: Option<SmSessionKey>,
    ) -> BindingLiveness {
        let lease = binding.connection_ref().node_lease();
        if lease == &self.lease {
            return BindingLiveness::Live;
        }
        let Some(host) = lease.clustered_node() else {
            return BindingLiveness::Unknown;
        };
        match self.node_leases.unexpired(&host).await {
            Some(true) => BindingLiveness::Live,
            Some(false) => BindingLiveness::Expired,
            None => BindingLiveness::Unknown,
        }
    }
}
