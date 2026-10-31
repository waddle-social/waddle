use super::{mirror_register, mirror_unregister, MirrorUnregisterOutcome};
use crate::clustering::CriticalNodeFailure;
use crate::server::routes::websocket::WebSocketState;
use crate::server::session_janitors::spawn_critical_registry_supervisor;
use jid::FullJid;
use kameo::actor::ActorRef;
use kameo::error::SendError;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Notify};
use waddle_xmpp::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::registry::user_actor::test_support::{GateMailbox, MailboxNoop};
use waddle_xmpp::registry::user_actor::{RegisterConnection, UserActor};
use waddle_xmpp::registry::user_registry::test_support::PendingUnregisterCount;
use waddle_xmpp::registry::{
    ConnectionEntry, GetConnectionEntry, GetUser, OutboundStanza, RetryUserRegistryConvergence,
    UnregisterUserResource, UserCount, UserRegistryConvergenceReport, UserRegistryError,
    WireUserClusteringClaims,
};

struct Fixture {
    state: Arc<WebSocketState>,
    jid: FullJid,
    actor: ActorRef<UserActor>,
    owner: Arc<AtomicBool>,
    claims: Arc<InProcessClaimStore>,
    // Keep the registered socket's outbound channel open.
    _outbound: mpsc::Receiver<OutboundStanza>,
}

impl Fixture {
    async fn new() -> Self {
        #[cfg(not(feature = "clustering"))]
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        #[cfg(feature = "clustering")]
        let state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
                crate::clustering::ClusteringHandles {
                    stop_token: Some(tokio_util::sync::CancellationToken::new()),
                    ..Default::default()
                },
                Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
            )
            .await;
        spawn_critical_registry_supervisor(&state).await;
        let claims = Arc::new(InProcessClaimStore::new());
        let registry = &state.deps.protocol.user_registry;
        registry
            .ask(WireUserClusteringClaims {
                claim_store: claims.clone(),
                node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            })
            .await
            .expect("wire observable claim store");
        let jid: FullJid = "alice@example.com/phone".parse().expect("jid");
        let (tx, outbound) = mpsc::channel(16);
        let entry = ConnectionEntry::new(tx);
        let owner = entry.carbons_handle();
        assert!(mirror_register(registry, jid.clone(), entry).await);
        let actor = registry
            .ask(GetUser {
                bare_jid: jid.to_bare(),
            })
            .await
            .expect("get user")
            .expect("registered user");
        Self {
            state,
            jid,
            actor,
            owner,
            claims,
            _outbound: outbound,
        }
    }

    async fn gate(&self) -> oneshot::Sender<()> {
        let entered = Arc::new(Notify::new());
        let (release_tx, release_rx) = oneshot::channel();
        self.actor
            .tell(GateMailbox {
                entered: entered.clone(),
                release_rx,
            })
            .await
            .expect("queue gate");
        entered.notified().await;
        release_tx
    }

    async fn mirror(&self) {
        assert_eq!(
            mirror_unregister(
                &self.state.deps.protocol.user_registry,
                &self.jid,
                Some(self.owner.clone()),
            )
            .await,
            MirrorUnregisterOutcome::Completed,
            "teardown waits for registry admission only"
        );
    }

    async fn assert_pending(&self, expected: usize) {
        // This ask is a FIFO barrier after the enqueue-only mirror handler.
        assert_eq!(
            self.state
                .deps
                .protocol
                .user_registry
                .ask(PendingUnregisterCount)
                .await
                .expect("registry must still process asks"),
            expected
        );
        self.assert_healthy();
    }

    fn assert_healthy(&self) {
        assert!(self.state.deps.protocol.user_registry.is_alive());
        assert_eq!(
            self.state.deps.app_state.node_lifecycle.critical_failure(),
            None
        );
        assert!(self.state.deps.app_state.node_lifecycle.admit().is_ok());
        assert!(!self.state.deps.shutdown.stop_token().is_cancelled());
    }

    async fn assert_claim(&self, held: bool) {
        assert_eq!(
            self.claims
                .current_claim(&Entity::new(
                    EntityType::UserActor,
                    self.jid.to_bare().to_string()
                ))
                .await
                .expect("claim snapshot")
                .is_some(),
            held
        );
    }

    async fn retry(&self) {
        assert_eq!(
            self.state
                .deps
                .protocol
                .user_registry
                .ask(RetryUserRegistryConvergence)
                .await
                .expect("retry retained unregister"),
            UserRegistryConvergenceReport::default()
        );
        self.assert_pending(0).await;
    }

    async fn assert_watch_death_control(&self) {
        self.assert_healthy();
        self.state.deps.protocol.user_registry.kill();
        self.state
            .deps
            .protocol
            .user_registry
            .wait_for_shutdown()
            .await;
        tokio::time::timeout(
            Duration::from_secs(2),
            self.state.deps.shutdown.stop_token().cancelled(),
        )
        .await
        .expect("armed production watch must cancel the process on registry death");
        assert_eq!(
            self.state.deps.app_state.node_lifecycle.critical_failure(),
            Some(CriticalNodeFailure::UserRegistryTerminated)
        );
        assert!(self.state.deps.app_state.node_lifecycle.admit().is_err());
    }
}

#[tokio::test]
async fn mirror_reply_timeout_retains_custody_without_stopping_registry() {
    let f = Fixture::new().await;
    let release = f.gate().await;
    // The child mailbox has room, so the unregister ask is admitted behind
    // the gate and only its reply times out. It can still execute later.
    f.mirror().await;
    f.assert_pending(1).await;
    f.assert_claim(true).await;

    release.send(()).expect("release gate");
    assert!(f
        .actor
        .ask(GetConnectionEntry { jid: f.jid.clone() })
        .await
        .expect("barrier after accepted late unregister")
        .is_none());
    // The late child removal alone cannot retire the registry's claim.
    f.assert_claim(true).await;
    f.retry().await;
    assert_eq!(
        f.state
            .deps
            .protocol
            .user_registry
            .ask(UserCount)
            .await
            .expect("count"),
        0
    );
    f.assert_claim(false).await;
    f.assert_watch_death_control().await;
}

#[tokio::test]
async fn mirror_full_child_mailbox_retains_custody_until_retry() {
    let f = Fixture::new().await;
    let release = f.gate().await;
    let mut full = false;
    for _ in 0..128 {
        match f.actor.tell(MailboxNoop).try_send() {
            Ok(()) => {}
            Err(SendError::MailboxFull(_)) => {
                full = true;
                break;
            }
            Err(error) => panic!("unexpected filler error: {error}"),
        }
    }
    assert!(full, "child admission must be blocked before mirroring");
    f.mirror().await;
    f.assert_pending(1).await;
    f.assert_claim(true).await;

    release.send(()).expect("release gate");
    let entry = f
        .actor
        .ask(GetConnectionEntry { jid: f.jid.clone() })
        .await
        .expect("barrier after full mailbox")
        .expect("unregister was not admitted to the full child mailbox");
    assert!(Arc::ptr_eq(&entry.carbons_handle(), &f.owner));
    f.retry().await;
    assert!(f
        .actor
        .ask(GetConnectionEntry { jid: f.jid.clone() })
        .await
        .expect("entry")
        .is_none());
    f.assert_claim(false).await;
    f.assert_watch_death_control().await;
}

#[tokio::test]
async fn timed_out_old_owner_mirror_and_retry_preserve_successor() {
    let f = Fixture::new().await;
    let release = f.gate().await;
    let (tx, _outbound) = mpsc::channel(16);
    let fresh_entry = ConnectionEntry::new(tx);
    let fresh_owner = fresh_entry.carbons_handle();
    // Put the successor before the old unregister in the child's FIFO while
    // both remain behind the gate. The admitted unregister will execute only
    // after its registry caller has already timed out and retained a retry.
    f.actor
        .tell(RegisterConnection {
            jid: f.jid.clone(),
            entry: fresh_entry.clone(),
        })
        .await
        .expect("queue successor");
    f.mirror().await;
    f.assert_pending(1).await;
    release.send(()).expect("release gate");
    let entry = f
        .actor
        .ask(GetConnectionEntry { jid: f.jid.clone() })
        .await
        .expect("barrier after successor and late old unregister")
        .expect("successor");
    assert!(Arc::ptr_eq(&entry.carbons_handle(), &fresh_owner));
    // Retry the retained old-owner removal before reconciling the registry's
    // resource mirror, so this exercises the convergence handler itself.
    f.retry().await;
    let entry = f
        .actor
        .ask(GetConnectionEntry { jid: f.jid.clone() })
        .await
        .expect("entry after old-owner retry")
        .expect("successor");
    assert!(Arc::ptr_eq(&entry.carbons_handle(), &fresh_owner));
    assert!(
        mirror_register(
            &f.state.deps.protocol.user_registry,
            f.jid.clone(),
            fresh_entry
        )
        .await
    );
    f.assert_claim(true).await;
    f.assert_watch_death_control().await;
}

#[tokio::test]
async fn unregister_ask_preserves_typed_busy_error_and_pending_retry() {
    let f = Fixture::new().await;
    let release = f.gate().await;
    let result = f
        .state
        .deps
        .protocol
        .user_registry
        .ask(UnregisterUserResource {
            jid: f.jid.clone(),
            owner: Some(f.owner.clone()),
        })
        .await;
    assert!(
        matches!(result, Err(SendError::HandlerError(UserRegistryError::UserActorBusy(jid))) if jid == f.jid.to_bare())
    );
    f.assert_pending(1).await;
    release.send(()).expect("release gate");
    f.actor.ask(MailboxNoop).await.expect("child barrier");
    f.retry().await;
    f.assert_claim(false).await;
    f.assert_watch_death_control().await;
}

#[tokio::test]
async fn mirror_dead_child_is_already_absent_without_pending_retry() {
    let f = Fixture::new().await;
    f.actor.kill();
    f.actor.wait_for_shutdown().await;
    f.mirror().await;
    f.assert_pending(0).await;
    assert_eq!(
        f.state
            .deps
            .protocol
            .user_registry
            .ask(UserCount)
            .await
            .expect("count"),
        0
    );
    f.assert_claim(false).await;
    f.retry().await;
    f.assert_watch_death_control().await;
}
