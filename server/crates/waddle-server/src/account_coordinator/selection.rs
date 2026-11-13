//! Which account-resource authority a serving process reads.

use std::str::FromStr;
use std::sync::{Arc, Weak};

use thiserror::Error;
use tracing::Instrument;
use waddle_xmpp::ownership::NodeLeaseIdentity;
use waddle_xmpp::telemetry::attributes::{Janitor, MetricAttribute, SweepOutcome};
use waddle_xmpp::telemetry::reliability::record_janitor_sweep;

use super::claims::{
    AccountClaimSource, ClaimStoreClaimSource, ClusterLeaseLiveness, ConnectionLiveness,
    LocalNodeLiveness, MirrorClaimHolds, NodeLeaseRead, ProcessClaimSource,
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
    #[error(
        "a clustered account coordinator requires the claim store, the node identity and the node leases"
    )]
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
        let mirror_holds = Arc::new(MirrorClaimHolds::default());
        let (claims, liveness): (Arc<dyn AccountClaimSource>, Arc<dyn ConnectionLiveness>) =
            if clustered {
                let (Some(claim_store), Some(node_identity)) = (
                    clustering.claim_store.clone(),
                    clustering.node_identity.clone(),
                ) else {
                    return Err(AccountAuthoritySelectionError::ClusterIdentityMissing);
                };
                let Some(node_leases) = cluster_node_leases(clustering) else {
                    return Err(AccountAuthoritySelectionError::ClusterIdentityMissing);
                };
                let lease = NodeLeaseIdentity::from_node_identity(&node_identity.current());
                (
                    Arc::new(ClaimStoreClaimSource::with_mirror(
                        Arc::clone(&claim_store),
                        node_identity,
                        Arc::clone(&mirror_holds),
                    )),
                    Arc::new(ClusterLeaseLiveness::new(lease, node_leases)),
                )
            } else {
                (
                    Arc::new(ProcessClaimSource),
                    Arc::new(LocalNodeLiveness::new(NodeLeaseIdentity::local_process())),
                )
            };
        let config = AccountCoordinatorConfig::default();
        let coordinators = Arc::new(
            AccountCoordinators::new(Arc::new(store), claims, liveness, config)
                .with_mirror_holds(mirror_holds),
        );
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

/// The cluster's node leases as the coordinators read them.
#[cfg(feature = "clustering")]
struct ClusterNodeLeases(Arc<dyn crate::clustering::claims::NodeLeaseStore>);

#[cfg(feature = "clustering")]
#[async_trait::async_trait]
impl NodeLeaseRead for ClusterNodeLeases {
    async fn unexpired(&self, node: &waddle_xmpp::ownership::NodeIdentity) -> Option<bool> {
        self.0.is_unexpired(node).await.ok()
    }
}

#[cfg(feature = "clustering")]
fn cluster_node_leases(clustering: &ClusteringHandles) -> Option<Arc<dyn NodeLeaseRead>> {
    let store = clustering.node_lease.clone()?;
    Some(Arc::new(ClusterNodeLeases(store)))
}

/// A build without clustering has no node leases to read.
#[cfg(not(feature = "clustering"))]
fn cluster_node_leases(_clustering: &ClusteringHandles) -> Option<Arc<dyn NodeLeaseRead>> {
    None
}

/// How many accounts one sweep examines for bindings without a connection.
const EXPIRED_BINDING_SWEEP_ACCOUNTS: u32 = 256;

/// Settle the bindings whose connection is gone: its node lost its lease,
/// or its process ended. Such a binding has no connection to retire it, so
/// its resource would stay reserved and its account's claim held. Each
/// account's coordinator detaches the ones whose session can still resume
/// and retires the rest, stating that the recovery paths own the work they
/// accepted. A binding whose liveness is unknown is left alone.
///
/// The sweep takes over an account whose claim nobody holds or whose
/// holder's node lease is lost, because nobody else acts for the rows such
/// an account still has. An account another node holds is skipped.
///
/// One sweep examines a bounded page of the accounts that hold a binding
/// and resumes after it on the next; `cursor` restarts at the beginning
/// once the last page was read. Returns how many bindings were retired.
async fn retire_expired_bindings(
    coordinators: &Arc<AccountCoordinators>,
    cursor: &mut Option<jid::BareJid>,
) -> usize {
    let accounts = match coordinators
        .store()
        .bound_accounts(cursor.as_ref(), EXPIRED_BINDING_SWEEP_ACCOUNTS)
        .await
    {
        Ok(accounts) => accounts,
        Err(error) => {
            tracing::warn!(%error, "account coordinator reaper: bound accounts could not be read");
            return 0;
        }
    };
    *cursor = accounts.last().cloned();
    let mut retired = 0;
    for account in accounts {
        match coordinators.settle_expired(&account).await {
            Ok(settled) => retired += settled,
            Err(error) => {
                tracing::debug!(%error, "account coordinator reaper: account not settled");
            }
        }
    }
    retired
}

/// Reclaim idle coordinators for as long as the directory exists. Each
/// sweep runs in a `janitor.sweep` span and is counted as a janitor sweep,
/// so a stalled reaper shows as a missing heartbeat like any other janitor.
async fn reclaim_idle_coordinators(
    coordinators: Weak<AccountCoordinators>,
    config: AccountCoordinatorConfig,
) {
    // An interval's period must be positive.
    let period = config.idle_after.max(std::time::Duration::from_secs(1));
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut cursor = None;
    loop {
        ticks.tick().await;
        let Some(coordinators) = coordinators.upgrade() else {
            return;
        };
        async {
            let retired = retire_expired_bindings(&coordinators, &mut cursor).await;
            let reclaimed = coordinators.reclaim_idle().await;
            tracing::debug!(
                reclaimed,
                retired,
                "account coordinator reaper: sweep finished"
            );
            record_janitor_sweep(Janitor::AccountCoordinatorReaper, SweepOutcome::Completed);
        }
        .instrument(tracing::info_span!(
            parent: None,
            "janitor.sweep",
            janitor = Janitor::AccountCoordinatorReaper.value()
        ))
        .await;
    }
}
