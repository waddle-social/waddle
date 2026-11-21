//! Which account-resource authority a serving process reads.

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
use super::routed::{AccountAuthorityPeers, RoutedAccountAuthority};
use crate::account_bindings::{AccountBindingStoreError, DatabaseAccountBindingStore};
use crate::clustering::ClusteringHandles;
use crate::db::Database;

/// Environment variable that once chose between the connection registry and
/// the account coordinator. The coordinator is the only account-resource
/// authority, so the variable selects nothing; startup logs a warning when
/// it is set so a deployment still carrying it can drop it.
pub const REMOVED_ACCOUNT_AUTHORITY_ENV: &str = "WADDLE_XMPP_ACCOUNT_AUTHORITY";

/// Warn when the removed selector variable is still set. Any value is
/// ignored: `coordinator` is what startup selects anyway, and `registry`
/// no longer exists. Rolling back to the registry is a restart on a release
/// that still has it.
pub fn warn_if_removed_selector_set() {
    if let Ok(value) = std::env::var(REMOVED_ACCOUNT_AUTHORITY_ENV) {
        tracing::warn!(
            variable = REMOVED_ACCOUNT_AUTHORITY_ENV,
            value = %value,
            "the account-authority selector was removed and is ignored; the account coordinator is the only account-resource authority"
        );
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
        "a clustered account coordinator requires the claim store, the node identity, the node leases and the cluster relay"
    )]
    ClusterIdentityMissing,
}

/// The account-resource authority account reads consult.
///
/// `Coordinator` carries this node's own coordinators and the routed
/// authority over them. Connections and routing go through the routed
/// authority, which reaches the coordinator of the node that owns each
/// account; the coordinators themselves serve only what this node owns.
/// Startup always selects it.
///
/// `Unselected` is the state of an application whose startup never ran the
/// selection, such as a unit-test fixture that publishes no coordinator. It
/// serves no account: every read fails as
/// [`super::AccountCoordinatorError::Unselected`] and a connection records no
/// binding.
#[derive(Clone, Default)]
pub enum AccountAuthoritySelection {
    #[default]
    Unselected,
    Coordinator {
        coordinators: Arc<AccountCoordinators>,
        authority: Arc<RoutedAccountAuthority>,
        expired_binding_sweep_wake: Arc<tokio::sync::Notify>,
    },
}

impl AccountAuthoritySelection {
    /// Select the account coordinator over the ingress database: open the
    /// binding store, take claims from the cluster claim store on a
    /// clustered node and from the process otherwise, and start the task
    /// that sweeps expired bindings and reclaims idle coordinators.
    pub async fn open(
        db: &Database,
        clustering: &ClusteringHandles,
        clustered: bool,
        durable_stream_management: bool,
    ) -> Result<Self, AccountAuthoritySelectionError> {
        if !durable_stream_management {
            return Err(AccountAuthoritySelectionError::RequiresDurableStreamManagement);
        }
        let store = DatabaseAccountBindingStore::open(db.clone(), clustered).await?;
        let mirror_holds = Arc::new(MirrorClaimHolds::default());
        let mut peers = None;
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
                let Some(cluster_peers) = cluster_peers(clustering) else {
                    return Err(AccountAuthoritySelectionError::ClusterIdentityMissing);
                };
                peers = Some(cluster_peers);
                (
                    Arc::new(ClaimStoreClaimSource::with_mirror(
                        Arc::clone(&claim_store),
                        node_identity,
                        Arc::clone(&mirror_holds),
                    )),
                    Arc::new(ClusterLeaseLiveness::new(node_leases)),
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
        let expired_binding_sweep_wake = Arc::new(tokio::sync::Notify::new());
        tokio::spawn(reclaim_idle_coordinators(
            Arc::downgrade(&coordinators),
            config,
            Arc::clone(&expired_binding_sweep_wake),
        ));
        let authority = match peers {
            Some(peers) => RoutedAccountAuthority::new(Arc::clone(&coordinators), peers),
            None => RoutedAccountAuthority::local_only(Arc::clone(&coordinators)),
        };
        Ok(Self::Coordinator {
            coordinators,
            authority: Arc::new(authority),
            expired_binding_sweep_wake,
        })
    }

    /// This node's own coordinators, once startup selected them. They serve only the accounts this node owns; a request
    /// on behalf of a connection goes through [`Self::authority`].
    pub fn coordinators(&self) -> Option<&Arc<AccountCoordinators>> {
        match self {
            Self::Unselected => None,
            Self::Coordinator { coordinators, .. } => Some(coordinators),
        }
    }

    /// The authority that decides each account's requests on the node that
    /// owns it, once startup selected it.
    pub fn authority(&self) -> Option<&Arc<RoutedAccountAuthority>> {
        match self {
            Self::Unselected => None,
            Self::Coordinator { authority, .. } => Some(authority),
        }
    }

    /// Wake the expired-binding reaper immediately (for example after the
    /// stale-node watchdog commits `expired = true` for a departed peer).
    pub fn wake_expired_binding_sweep(&self) {
        if let Self::Coordinator {
            expired_binding_sweep_wake,
            ..
        } = self
        {
            expired_binding_sweep_wake.notify_one();
        }
    }
}

/// The cluster as the routed account authority reaches it.
#[cfg(feature = "clustering")]
fn cluster_peers(clustering: &ClusteringHandles) -> Option<Arc<dyn AccountAuthorityPeers>> {
    crate::clustering::route_bridge::ClusterAccountAuthorityPeers::new(clustering)
        .map(|peers| Arc::new(peers) as Arc<dyn AccountAuthorityPeers>)
}

/// A build without clustering has no other node to reach.
#[cfg(not(feature = "clustering"))]
fn cluster_peers(_clustering: &ClusteringHandles) -> Option<Arc<dyn AccountAuthorityPeers>> {
    None
}

/// The cluster's node leases as the coordinators read them.
#[cfg(feature = "clustering")]
struct ClusterNodeLeases(Arc<dyn crate::clustering::claims::NodeLeaseStore>);

#[cfg(feature = "clustering")]
#[async_trait::async_trait]
impl NodeLeaseRead for ClusterNodeLeases {
    async fn lease_state(
        &self,
        node: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Option<waddle_xmpp::ownership::NodeLeaseState> {
        self.0.lease_state(node).await.ok()
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

/// How many accounts one sweep page examines for bindings without a connection.
pub const EXPIRED_BINDING_SWEEP_ACCOUNTS: u32 = 256;

/// Maximum pages drained during a single node-expiry wake-up pass, bounding
/// burst DB load while covering typical clusters in one pass.
const EXPIRED_BINDING_WAKE_MAX_PAGES: usize = 16;

/// Settle one bounded page of accounts whose connection is gone: its node lost
/// its lease, or its process ended. Such a binding has no connection to retire
/// it, so its resource would stay reserved and its account's claim held. Each
/// account's coordinator detaches the ones whose session can still resume and
/// retires the rest, stating that the recovery paths own the work they
/// accepted. A binding whose liveness is unknown or draining is left alone.
///
/// The sweep takes over an account whose claim nobody holds or whose
/// holder's node lease is lost, because nobody else acts for the rows such
/// an account still has. An account another node holds is skipped.
///
/// One call examines at most `page_limit` accounts after `cursor`. When a
/// full page is returned, `cursor` advances to its last account; when a short
/// page is returned, `cursor` resets to `None` immediately so the next sweep
/// starts from the beginning instead of issuing an empty pass. Returns how
/// many bindings were retired.
pub async fn retire_expired_bindings_page(
    coordinators: &Arc<AccountCoordinators>,
    cursor: &mut Option<jid::BareJid>,
    page_limit: u32,
) -> usize {
    retire_expired_bindings_page_checked(coordinators, cursor, page_limit)
        .await
        .unwrap_or(0)
}

async fn retire_expired_bindings_page_checked(
    coordinators: &Arc<AccountCoordinators>,
    cursor: &mut Option<jid::BareJid>,
    page_limit: u32,
) -> Option<usize> {
    let accounts = match coordinators
        .store()
        .bound_accounts(cursor.as_ref(), page_limit)
        .await
    {
        Ok(accounts) => accounts,
        Err(error) => {
            tracing::warn!(%error, "account coordinator reaper: bound accounts could not be read");
            return None;
        }
    };
    let full_page = accounts.len() >= page_limit as usize;
    *cursor = full_page.then(|| accounts.last().cloned()).flatten();
    let mut retired = 0;
    for account in accounts {
        match coordinators.settle_expired(&account).await {
            Ok(settled) => retired += settled,
            Err(super::error::AccountCoordinatorError::Claim(
                super::claims::AccountClaimError::HeldElsewhere,
            )) => {
                tracing::debug!(%account, "account coordinator reaper: account held by another node");
            }
            Err(error) => {
                tracing::warn!(%account, %error, "account coordinator reaper: account not settled");
            }
        }
    }
    Some(retired)
}

/// Sweep expired bindings and reclaim idle coordinators for as long as the
/// directory exists.
///
/// Expired-binding retirement runs on `config.expired_binding_sweep_interval`
/// (default 30 s) and also wakes immediately when `wake` is notified (for
/// example after the stale-node watchdog commits `expired = true`). Idle
/// coordinator eviction stays on `config.idle_after` (default 300 s) and does
/// not run on burst wake-ups. Each periodic sweep runs in a `janitor.sweep`
/// span and records a janitor heartbeat.
async fn reclaim_idle_coordinators(
    coordinators: Weak<AccountCoordinators>,
    config: AccountCoordinatorConfig,
    wake: Arc<tokio::sync::Notify>,
) {
    // Interval periods must be positive.
    let sweep_period = config
        .expired_binding_sweep_interval
        .max(std::time::Duration::from_secs(1));
    let idle_period = config.idle_after.max(std::time::Duration::from_secs(1));
    let mut sweep_ticks = tokio::time::interval(sweep_period);
    sweep_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut idle_ticks = tokio::time::interval(idle_period);
    idle_ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Skip the immediate idle tick at startup; no coordinators are idle yet.
    idle_ticks.tick().await;
    let mut cursor = None;
    loop {
        enum Trigger {
            Sweep,
            Idle,
            Wake,
        }
        let trigger = tokio::select! {
            _ = wake.notified() => Trigger::Wake,
            _ = sweep_ticks.tick() => Trigger::Sweep,
            _ = idle_ticks.tick() => Trigger::Idle,
        };
        let Some(coordinators) = coordinators.upgrade() else {
            return;
        };
        match trigger {
            Trigger::Wake => {
                // Reset cursor so accounts before the periodic sweep's current
                // position are settled immediately after a node expires.
                cursor = None;
                let mut retired = 0;
                for _ in 0..EXPIRED_BINDING_WAKE_MAX_PAGES {
                    let Some(page_retired) = retire_expired_bindings_page_checked(
                        &coordinators,
                        &mut cursor,
                        EXPIRED_BINDING_SWEEP_ACCOUNTS,
                    )
                    .await
                    else {
                        break;
                    };
                    retired += page_retired;
                    if cursor.is_none() {
                        break;
                    }
                }
                tracing::debug!(
                    retired,
                    "account coordinator reaper: node-expiry wake sweep finished"
                );
            }
            Trigger::Sweep => {
                async {
                    let retired = retire_expired_bindings_page(
                        &coordinators,
                        &mut cursor,
                        EXPIRED_BINDING_SWEEP_ACCOUNTS,
                    )
                    .await;
                    tracing::debug!(
                        retired,
                        "account coordinator reaper: expired-binding sweep finished"
                    );
                    record_janitor_sweep(
                        Janitor::AccountCoordinatorReaper,
                        SweepOutcome::Completed,
                    );
                }
                .instrument(tracing::info_span!(
                    parent: None,
                    "janitor.sweep",
                    janitor = Janitor::AccountCoordinatorReaper.value()
                ))
                .await;
            }
            Trigger::Idle => {
                let reclaimed = coordinators.reclaim_idle().await;
                tracing::debug!(
                    reclaimed,
                    "account coordinator reaper: idle coordinator reclamation finished"
                );
            }
        }
    }
}
