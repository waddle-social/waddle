use super::*;
use crate::clustering::route_bridge::tests::{
    origin_identity, receiver_identity, services_with_claims, test_peer_id,
};
use std::collections::VecDeque;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ResumeIdentityProof, StalePredicate,
};

/// The claim database can expire an owner while a relay lookup is in flight.
struct ScriptedClaims {
    reads: std::sync::Mutex<VecDeque<Result<Option<ClaimSnapshot>, ClaimError>>>,
}

#[async_trait::async_trait]
impl ClaimStore for ScriptedClaims {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn acquire(&self, _: &Entity, _: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn ensure_claimed(&self, _: &Entity, _: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn steal_stale(
        &self,
        _: &Entity,
        _: ClaimEpoch,
        _: StalePredicate,
        _: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("bridge never authorizes takeover")
    }
    async fn steal_for_resume(
        &self,
        _: &Entity,
        _: ClaimEpoch,
        _: ResumeIdentityProof,
        _: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn current_claim(&self, _: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.reads
            .lock()
            .expect("script lock")
            .pop_front()
            .expect("unexpected claim read")
    }
    async fn fence(&self, _: &Entity, _: &NodeIdentity, _: ClaimEpoch) -> Result<bool, ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn release(&self, _: &Entity, _: &NodeIdentity, _: ClaimEpoch) -> Result<(), ClaimError> {
        unreachable!("read-only bridge")
    }
    async fn release_many(&self, _: &[Entity], _: &NodeIdentity) -> Result<(), ClaimError> {
        unreachable!("read-only bridge")
    }
}

fn foreign_claim(fresh: bool) -> Result<Option<ClaimSnapshot>, ClaimError> {
    Ok(Some(ClaimSnapshot {
        owner: NodeIdentity::new("registration-unreachable-owner", "test-epoch"),
        claim_epoch: ClaimEpoch(1),
        owner_lease_fresh: fresh,
    }))
}

async fn register_with_claim_reads(
    reads: impl IntoIterator<Item = Result<Option<ClaimSnapshot>, ClaimError>>,
) -> RemoteResourceRegisterOutcome {
    let mut services = services_with_claims(
        origin_identity(),
        origin_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.claim_store = Arc::new(ScriptedClaims {
        reads: std::sync::Mutex::new(reads.into_iter().collect()),
    });
    let jid: jid::FullJid = "alice@example.test/reconnect".parse().expect("jid");
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = services
        .connection_registry
        .register_entry(jid.clone(), entry.clone());
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::new(services));
    bridge
        .try_register_remote_user_resource(&jid, entry, owner)
        .await
}

#[tokio::test]
async fn registration_rechecks_owner_expiry_after_remote_lookup_failure() {
    assert_eq!(
        register_with_claim_reads([foreign_claim(true), foreign_claim(false)]).await,
        RemoteResourceRegisterOutcome::NotRemote,
        "committed expiry during relay failure must return to local CAS registration",
    );
}

#[tokio::test]
async fn registration_preserves_fresh_foreign_owner_after_remote_lookup_failure() {
    assert_eq!(
        register_with_claim_reads([foreign_claim(true), foreign_claim(true)]).await,
        RemoteResourceRegisterOutcome::Retryable,
        "a disconnected relay cannot authorize local takeover of a live owner",
    );
}

#[tokio::test]
async fn registration_does_not_treat_initial_claim_backend_failure_as_a_missing_owner() {
    assert_eq!(
        register_with_claim_reads([Err(ClaimError::Backend("unavailable".into()))]).await,
        RemoteResourceRegisterOutcome::Failed,
        "failed owner lookup is not evidence that the claim is absent",
    );
}

#[tokio::test]
async fn registration_does_not_fallback_when_owner_recheck_fails() {
    assert_eq!(
        register_with_claim_reads([
            foreign_claim(true),
            Err(ClaimError::Backend("unavailable".into())),
        ])
        .await,
        RemoteResourceRegisterOutcome::Failed,
        "a relay failure plus unknown owner liveness cannot authorize local takeover",
    );
}
