//! #2046 W1.10: the node-lease shutdown path runs inside one enclosing
//! shutdown deadline.
//!
//! These drive the production `run_node_lease` loop with an in-memory
//! node-lease store and injected stalled stages, on paused Tokio time:
//!
//! - SIGTERM with every drain stage stalled still retires the node's
//!   identity, which hands every claim it holds to survivors, and the loop
//!   ends inside the enclosing deadline;
//! - if the publication barrier cannot close inside the deadline, the
//!   identity is not retired;
//! - SIGTERM while the node is re-registering after a self-fence (#1985
//!   follow-up) is an ordinary shutdown: both identities are retired after
//!   the publication barrier, instead of waiting for the orphan reaper.
#![cfg(feature = "clustering")]

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use waddle_server::clustering::claims::{
    NodeLeaseStore, OrphanedRoomActorClaimPage, OrphanedSmSessionClaim, RoomOrphanScanCursor,
};
use waddle_server::clustering::self_fence::{
    run_node_lease, ConnectedPeerCount, LocallyClaimedEntities, NodeLeaseRunConfig,
};
use waddle_server::clustering::{NodeLifecycle, ShutdownSmDrain};
use waddle_server::config::{ClusteringNodeLeaseConfig, ClusteringSelfFenceConfig};
use waddle_server::shutdown_deadline::{
    ShutdownBudget, ShutdownDeadline, SHUTDOWN_TERMINAL_SWEEP_BUDGET,
};
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, Entity, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};

type RetirementProbe = Box<dyn Fn(&NodeIdentity) -> bool + Send + Sync>;

/// In-memory node-lease store: records `retire_after_shutdown` calls and
/// what `probe` observed at each one.
struct RecordingLease {
    heartbeat_ok: bool,
    registration_entered: Option<Arc<Notify>>,
    registration_release: Option<Arc<Notify>>,
    retirements: Arc<Mutex<Vec<(NodeIdentity, bool)>>>,
    probe: RetirementProbe,
}

impl RecordingLease {
    fn new(heartbeat_ok: bool, probe: RetirementProbe) -> Self {
        Self {
            heartbeat_ok,
            registration_entered: None,
            registration_release: None,
            retirements: Arc::new(Mutex::new(Vec::new())),
            probe,
        }
    }
}

#[async_trait]
impl NodeLeaseStore for RecordingLease {
    async fn register(
        &self,
        _me: &NodeIdentity,
        _pod_template_hash: Option<String>,
    ) -> Result<(), ClaimError> {
        if let Some(entered) = &self.registration_entered {
            entered.notify_one();
        }
        if let Some(release) = &self.registration_release {
            release.notified().await;
        }
        Ok(())
    }
    async fn heartbeat(
        &self,
        _me: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<bool, ClaimError> {
        Ok(self.heartbeat_ok)
    }
    async fn expire(
        &self,
        _owner: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<bool, ClaimError> {
        Ok(true)
    }
    async fn retire_after_shutdown(&self, me: &NodeIdentity) -> Result<bool, ClaimError> {
        let observed = (self.probe)(me);
        self.retirements
            .lock()
            .expect("lock")
            .push((me.clone(), observed));
        Ok(true)
    }
    async fn mark_draining(&self, _me: &NodeIdentity) -> Result<(), ClaimError> {
        Ok(())
    }
    async fn retire(&self, _me: &NodeIdentity) -> Result<(), ClaimError> {
        Ok(())
    }
    async fn count_other_live_nodes(
        &self,
        _me: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<usize, ClaimError> {
        Ok(0)
    }
    async fn list_other_unexpired_nodes(
        &self,
        _me: &NodeIdentity,
        _limit: usize,
    ) -> Result<Vec<NodeIdentity>, ClaimError> {
        Ok(Vec::new())
    }
    async fn reconcile(
        &self,
        _me: &NodeIdentity,
        _locally_owned: &[Entity],
    ) -> Result<Vec<Entity>, ClaimError> {
        Ok(Vec::new())
    }
    async fn report_steal_intent(
        &self,
        _entity: &Entity,
        _reporter: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        Ok(())
    }
    async fn owner_steal_intents(
        &self,
        _me: &NodeIdentity,
    ) -> Result<Vec<(Entity, ClaimEpoch)>, ClaimError> {
        Ok(Vec::new())
    }
    async fn clear_steal_intent(
        &self,
        _entity: &Entity,
        _me: &NodeIdentity,
        _mine: ClaimEpoch,
    ) -> Result<u64, ClaimError> {
        Ok(1)
    }
    async fn list_orphaned_sm_session_claims(
        &self,
    ) -> Result<Vec<OrphanedSmSessionClaim>, ClaimError> {
        Ok(Vec::new())
    }
    async fn list_orphaned_room_actor_claims_page(
        &self,
        _after: Option<RoomOrphanScanCursor>,
        _limit: usize,
    ) -> Result<OrphanedRoomActorClaimPage, ClaimError> {
        Ok(OrphanedRoomActorClaimPage {
            candidates: Vec::new(),
            next_cursor: None,
            has_more: false,
            quarantined: 0,
        })
    }
    async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
        Ok(None)
    }
}

/// Local claims whose room-drain inventory read and terminal sweep never
/// return (when `stall` is set). Records every exact-owner demotion.
struct StallingLocalClaims {
    stall: bool,
    demoted_owners: Arc<Mutex<Vec<NodeIdentity>>>,
}

#[async_trait]
impl LocallyClaimedEntities for StallingLocalClaims {
    async fn owned(&self) -> Vec<Entity> {
        if self.stall {
            std::future::pending::<()>().await;
        }
        Vec::new()
    }
    async fn terminal_sweep(&self) -> Vec<Entity> {
        if self.stall {
            std::future::pending::<()>().await;
        }
        Vec::new()
    }
    async fn demote(&self, _entity: &Entity) {}
    async fn demote_owned_by(&self, owner: &NodeIdentity) {
        self.demoted_owners
            .lock()
            .expect("lock")
            .push(owner.clone());
    }
    async fn health_check(&self, _entity: &Entity) -> bool {
        true
    }
}

fn identity() -> NodeIdentity {
    NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    )
}

fn run_config(
    heartbeat_interval: Duration,
    local_claims: Arc<dyn LocallyClaimedEntities>,
    readiness: NodeLifecycle,
    live_identity: SharedNodeIdentity,
    shutdown_sm_drain: Option<ShutdownSmDrain>,
) -> NodeLeaseRunConfig {
    NodeLeaseRunConfig {
        pod_template_hash: None,
        lease_config: ClusteringNodeLeaseConfig {
            heartbeat_interval,
            lease_ttl: heartbeat_interval * 3,
            claim_release_budget: Duration::from_secs(1),
        },
        self_fence_config: ClusteringSelfFenceConfig {
            isolation_intervals: 1_000,
            reregister_backoff_base: Duration::from_millis(10),
            reregister_backoff_max: Duration::from_millis(20),
        },
        connected_peers: ConnectedPeerCount::new(),
        local_claims,
        readiness,
        live_identity,
        peer_id: None,
        claim_store: Arc::new(InProcessClaimStore::new()),
        claim_release_budget: Duration::from_secs(1),
        shutdown_sm_drain,
    }
}

/// Production-shaped process shutdown coordination whose Q6 drain never
/// settles.
fn stalled_q6(process_stop: &CancellationToken, deadline: &ShutdownDeadline) -> ShutdownSmDrain {
    ShutdownSmDrain {
        process_stop: process_stop.clone(),
        complete: CancellationToken::new(),
        connections_drained: Arc::new(AtomicBool::new(false)),
        deadline: deadline.clone(),
    }
}

fn ten_second_deadline() -> ShutdownDeadline {
    // 2 s drain + 1 s room drain + 5 s retirement + 2 s sweep.
    ShutdownDeadline::new(ShutdownBudget::new(
        Duration::from_secs(2),
        Duration::from_secs(1),
    ))
}

/// SIGTERM with every stage stalled: Q6 never settles, the room drain's
/// inventory read and the terminal sweep never return, and the control
/// plane is production-sized (10 s per write). The identity is still
/// retired — after the `disable()` barrier and exact demotion — and the
/// node-lease loop ends inside the enclosing deadline. Before W1.10 the
/// stalled room drain held this loop until the process was killed.
#[tokio::test(start_paused = true)]
async fn sigterm_with_stalled_stages_retires_inside_the_enclosing_deadline() {
    let owner = identity();
    let live_identity = SharedNodeIdentity::new(owner.clone());
    let deadline = ten_second_deadline();
    let demoted_owners = Arc::new(Mutex::new(Vec::new()));
    let probe_identity = live_identity.clone();
    let probe_demoted = Arc::clone(&demoted_owners);
    let probe_deadline = deadline.clone();
    let lease = RecordingLease::new(
        true,
        Box::new(move |retired| {
            !probe_identity.current().is_active()
                && probe_demoted.lock().expect("lock").contains(retired)
                && probe_deadline
                    .started()
                    .is_some_and(|shutdown| tokio::time::Instant::now() <= shutdown.deadline())
        }),
    );
    let retirements = Arc::clone(&lease.retirements);
    let process_stop = CancellationToken::new();
    let task = tokio::spawn(run_node_lease(
        lease,
        owner.clone(),
        process_stop.child_token(),
        run_config(
            Duration::from_secs(10),
            Arc::new(StallingLocalClaims {
                stall: true,
                demoted_owners: Arc::clone(&demoted_owners),
            }),
            NodeLifecycle::new(),
            live_identity.clone(),
            Some(stalled_q6(&process_stop, &deadline)),
        ),
    ));
    tokio::task::yield_now().await;

    // SIGTERM.
    process_stop.cancel();
    let enclosing = deadline.start().deadline();
    tokio::time::timeout_at(enclosing + Duration::from_millis(1), task)
        .await
        .expect("node-lease shutdown must end inside the enclosing deadline")
        .expect("node-lease task");

    assert!(tokio::time::Instant::now() <= enclosing);
    assert!(!live_identity.current().is_active());
    assert_eq!(
        retirements.lock().expect("lock").as_slice(),
        &[(owner, true)],
        "retired once, after disable and demotion, inside the deadline"
    );
}

/// An in-flight publication still holds the identity when SIGTERM arrives,
/// so `disable()` cannot close the barrier inside the deadline. The
/// identity must then be left to lease expiry, never retired, and shutdown
/// still ends inside the deadline.
#[tokio::test(start_paused = true)]
async fn sigterm_with_a_held_publication_guard_does_not_retire() {
    let owner = identity();
    let live_identity = SharedNodeIdentity::new(owner.clone());
    let deadline = ten_second_deadline();
    let lease = RecordingLease::new(true, Box::new(|_| true));
    let retirements = Arc::clone(&lease.retirements);
    let process_stop = CancellationToken::new();
    let task = tokio::spawn(run_node_lease(
        lease,
        owner.clone(),
        process_stop.child_token(),
        run_config(
            Duration::from_secs(10),
            Arc::new(StallingLocalClaims {
                stall: false,
                demoted_owners: Arc::new(Mutex::new(Vec::new())),
            }),
            NodeLifecycle::new(),
            live_identity.clone(),
            Some(stalled_q6(&process_stop, &deadline)),
        ),
    ));
    tokio::task::yield_now().await;
    let publication = live_identity
        .guard_if_current(&owner)
        .await
        .expect("identity is current before shutdown");

    process_stop.cancel();
    let shutdown = deadline.start();
    tokio::time::timeout_at(shutdown.deadline() + Duration::from_millis(1), task)
        .await
        .expect("node-lease shutdown must end inside the enclosing deadline")
        .expect("node-lease task");

    assert!(
        tokio::time::Instant::now() <= shutdown.deadline_before(SHUTDOWN_TERMINAL_SWEEP_BUDGET),
        "the barrier wait is cut at the retirement stage's end"
    );
    assert!(
        retirements.lock().expect("lock").is_empty(),
        "an identity whose barrier did not close is never retired"
    );
    assert!(live_identity.current().is_active());
    drop(publication);
}

/// Local claims whose room-drain inventory read waits for `release`.
struct GatedRoomDrain {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl LocallyClaimedEntities for GatedRoomDrain {
    async fn owned(&self) -> Vec<Entity> {
        self.entered.notify_one();
        self.release.notified().await;
        Vec::new()
    }
    async fn demote(&self, _entity: &Entity) {}
    async fn health_check(&self, _entity: &Entity) -> bool {
        true
    }
}

/// A clustering-only stop (for example the swarm's keypair-slot self-fence)
/// starts the node-lease shutdown; SIGTERM then arrives while the room
/// drain is still running. The node must still keep its identity
/// authoritative for the Q6 drain, bounded by the drain phase of the
/// enclosing deadline, before `disable()` and retirement.
#[tokio::test(start_paused = true)]
async fn sigterm_during_a_clustering_only_room_drain_still_waits_for_q6() {
    let owner = identity();
    let live_identity = SharedNodeIdentity::new(owner.clone());
    let deadline = ten_second_deadline();
    let probe_deadline = deadline.clone();
    let lease = RecordingLease::new(
        true,
        Box::new(move |_| {
            probe_deadline.started().is_some_and(|shutdown| {
                let now = tokio::time::Instant::now();
                now >= shutdown.drain_deadline() && now <= shutdown.deadline()
            })
        }),
    );
    let retirements = Arc::clone(&lease.retirements);
    let process_stop = CancellationToken::new();
    let clustering_stop = CancellationToken::new();
    let room_drain = GatedRoomDrain {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let entered = Arc::clone(&room_drain.entered);
    let release = Arc::clone(&room_drain.release);
    let task = tokio::spawn(run_node_lease(
        lease,
        owner.clone(),
        clustering_stop.clone(),
        run_config(
            Duration::from_secs(10),
            Arc::new(room_drain),
            NodeLifecycle::new(),
            live_identity.clone(),
            Some(stalled_q6(&process_stop, &deadline)),
        ),
    ));
    tokio::task::yield_now().await;

    // Clustering-only stop: the room drain starts without a process stop.
    clustering_stop.cancel();
    entered.notified().await;
    // SIGTERM while the room drain is still running.
    process_stop.cancel();
    let shutdown = deadline.start();
    release.notify_one();
    tokio::time::timeout_at(shutdown.deadline() + Duration::from_millis(1), task)
        .await
        .expect("node-lease shutdown must end inside the enclosing deadline")
        .expect("node-lease task");

    assert_eq!(
        retirements.lock().expect("lock").as_slice(),
        &[(owner, true)],
        "retired once, only after the Q6 wait reached the drain stage's end"
    );
}
