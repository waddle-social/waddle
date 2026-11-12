//! Which account-resource authority a serving process reads.

use std::str::FromStr;
use std::sync::{Arc, Weak};

use thiserror::Error;
use waddle_xmpp::ownership::NodeLeaseIdentity;

use super::claims::{
    AccountClaimSource, ClaimStoreClaimSource, ConnectionLiveness, LocalNodeLiveness,
    ProcessClaimSource,
};
use super::config::AccountCoordinatorConfig;
use super::directory::AccountCoordinators;
use crate::account_bindings::{AccountBindingStoreError, DatabaseAccountBindingStore};
use crate::clustering::ClusteringHandles;
use crate::db::Database;

/// Environment variable naming the [`AccountAuthorityMode`].
pub const ACCOUNT_AUTHORITY_MODE_ENV: &str = "WADDLE_XMPP_ACCOUNT_AUTHORITY";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{ACCOUNT_AUTHORITY_MODE_ENV} must be `registry` or `coordinator`, not `{0}`")]
pub struct InvalidAccountAuthorityMode(String);

/// The configured account-resource authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountAuthorityMode {
    /// The connection registry and `UserActor` are written by the
    /// connection and read by routing.
    #[default]
    Registry,
    /// Routing reads the account coordinator in
    /// [`crate::account_coordinator`].
    Coordinator,
}

impl FromStr for AccountAuthorityMode {
    type Err = InvalidAccountAuthorityMode;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "registry" => Ok(Self::Registry),
            "coordinator" => Ok(Self::Coordinator),
            other => Err(InvalidAccountAuthorityMode(other.to_owned())),
        }
    }
}

impl AccountAuthorityMode {
    /// Read the mode from the environment. An unset variable is
    /// `Registry`; a value that is neither mode is an error, so a typo
    /// cannot silently select the other authority.
    pub fn from_env() -> Result<Self, InvalidAccountAuthorityMode> {
        match std::env::var(ACCOUNT_AUTHORITY_MODE_ENV) {
            Ok(value) => value.parse(),
            Err(_) => Ok(Self::Registry),
        }
    }
}

/// Why the coordinator could not be selected.
#[derive(Debug, Error)]
pub enum AccountAuthoritySelectionError {
    #[error("the account binding store could not be opened")]
    Store(#[from] AccountBindingStoreError),
    /// A binding's session reference points at the durable session
    /// relation, so a binding with a session of the legacy registry would
    /// not commit.
    #[error("the account coordinator requires the durable stream-management authority")]
    RequiresDurableStreamManagement,
    #[error("a clustered account coordinator requires the claim store and the node identity")]
    ClusterIdentityMissing,
}

/// The authority account reads consult. `Registry` carries no
/// coordinators, so a caller cannot reach them unless the mode selected
/// them.
#[derive(Clone, Default)]
pub enum AccountAuthoritySelection {
    #[default]
    Registry,
    Coordinator(Arc<AccountCoordinators>),
}

impl AccountAuthoritySelection {
    /// Build the selection for `mode` over the ingress database. The
    /// registry mode opens nothing. The coordinator mode opens the binding
    /// store, takes claims from the cluster claim store on a clustered node
    /// and from the process otherwise, and starts the task that reclaims
    /// idle coordinators.
    pub async fn open(
        mode: AccountAuthorityMode,
        db: &Database,
        clustering: &ClusteringHandles,
        clustered: bool,
        durable_stream_management: bool,
    ) -> Result<Self, AccountAuthoritySelectionError> {
        if mode == AccountAuthorityMode::Registry {
            return Ok(Self::Registry);
        }
        if !durable_stream_management {
            return Err(AccountAuthoritySelectionError::RequiresDurableStreamManagement);
        }
        let store = DatabaseAccountBindingStore::open(db.clone(), clustered).await?;
        let (claims, liveness): (Arc<dyn AccountClaimSource>, Arc<dyn ConnectionLiveness>) =
            if clustered {
                let (Some(claim_store), Some(node_identity)) = (
                    clustering.claim_store.clone(),
                    clustering.node_identity.clone(),
                ) else {
                    return Err(AccountAuthoritySelectionError::ClusterIdentityMissing);
                };
                let lease = NodeLeaseIdentity::from_node_identity(&node_identity.current());
                (
                    Arc::new(ClaimStoreClaimSource::new(claim_store, node_identity)),
                    Arc::new(LocalNodeLiveness::new(lease)),
                )
            } else {
                (
                    Arc::new(ProcessClaimSource),
                    Arc::new(LocalNodeLiveness::new(NodeLeaseIdentity::local_process())),
                )
            };
        let config = AccountCoordinatorConfig::default();
        let coordinators = Arc::new(AccountCoordinators::new(
            Arc::new(store),
            claims,
            liveness,
            config,
        ));
        tokio::spawn(reclaim_idle_coordinators(
            Arc::downgrade(&coordinators),
            config,
        ));
        Ok(Self::Coordinator(coordinators))
    }

    /// The coordinators, when the coordinator is the selected authority.
    pub fn coordinators(&self) -> Option<&Arc<AccountCoordinators>> {
        match self {
            Self::Registry => None,
            Self::Coordinator(coordinators) => Some(coordinators),
        }
    }
}

/// Reclaim idle coordinators for as long as the directory exists.
async fn reclaim_idle_coordinators(
    coordinators: Weak<AccountCoordinators>,
    config: AccountCoordinatorConfig,
) {
    // An interval's period must be positive.
    let period = config.idle_after.max(std::time::Duration::from_secs(1));
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let Some(coordinators) = coordinators.upgrade() else {
            return;
        };
        coordinators.reclaim_idle().await;
    }
}
