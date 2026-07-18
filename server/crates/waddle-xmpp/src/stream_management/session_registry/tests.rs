use super::core::PendingClaimAcquisitionDisposition;
use super::*;
use std::time::{Duration, Instant};

use chrono::Utc;
use jid::FullJid;
use xmpp_parsers::presence::Show;

use super::super::persistence::SmUnackedStanzaPurpose;
use crate::Stanza;

fn make_test_jid() -> FullJid {
    "user@example.com/resource".parse().unwrap()
}

fn bare(s: &str) -> jid::BareJid {
    s.parse().expect("valid bare jid")
}

fn direct_target(wire_id: &str, author: &str, archive: &str) -> crate::tombstone::TombstoneTarget {
    crate::tombstone::TombstoneTarget::Direct {
        wire_id: wire_id.to_string(),
        author: bare(author),
        archive: bare(archive),
    }
}

fn groupchat_target(stanza_id: &str, room: &str) -> crate::tombstone::TombstoneTarget {
    crate::tombstone::TombstoneTarget::Groupchat {
        stanza_id: stanza_id.to_string(),
        room: bare(room),
    }
}

fn message_stanza_xml_with_id(id: String) -> String {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.id = Some(xmpp_parsers::message::Id(id));
    let element = Stanza::Message(message).to_element();
    let mut buffer = Vec::new();
    element.write_to(&mut buffer).expect("serialize message");
    String::from_utf8(buffer).expect("message stanza xml is utf-8")
}

fn make_test_session(stream_id: &str) -> DetachedSession {
    make_test_session_for_jid(stream_id, make_test_jid())
}

fn make_test_session_for_jid(stream_id: &str, jid: FullJid) -> DetachedSession {
    DetachedSession {
        stream_id: stream_id.to_string(),
        user_id: "user@example.com".to_string(),
        jid,
        inbound_count: 10,
        outbound_count: 15,
        last_acked: 12,
        replay_gap_through: None,
        unacked_stanzas: vec![
            DetachedUnackedStanza {
                sequence: 13,
                stanza_xml: "<msg1/>".to_string(),
                original_receipt_at: Utc::now(),
                purpose: SmUnackedStanzaPurpose::Application,
            },
            DetachedUnackedStanza {
                sequence: 14,
                stanza_xml: "<msg2/>".to_string(),
                original_receipt_at: Utc::now(),
                purpose: SmUnackedStanzaPurpose::Application,
            },
            DetachedUnackedStanza {
                sequence: 15,
                stanza_xml: "<msg3/>".to_string(),
                original_receipt_at: Utc::now(),
                purpose: SmUnackedStanzaPurpose::Application,
            },
        ],
        max_resume_time: Some(300),
        detached_at: Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

fn make_test_session_with_unacked(stream_id: &str, unacked: Vec<(u32, String)>) -> DetachedSession {
    let now = Utc::now();
    let mut s = make_test_session(stream_id);
    s.unacked_stanzas = unacked
        .into_iter()
        .map(|(sequence, stanza_xml)| DetachedUnackedStanza {
            sequence,
            stanza_xml,
            original_receipt_at: now,
            purpose: SmUnackedStanzaPurpose::Application,
        })
        .collect();
    s
}

enum EnsureClaimTestAction {
    PoisonFenceCache(std::sync::Weak<InMemorySmSessionRegistry>),
    RotateIdentity {
        identity: crate::ownership::SharedNodeIdentity,
        next: crate::ownership::NodeIdentity,
    },
    Pause {
        reached: std::sync::Arc<tokio::sync::Notify>,
        proceed: std::sync::Arc<tokio::sync::Notify>,
    },
    ReplaceClaimThenBackendError,
}

struct HangingReleaseClaimStore {
    inner: crate::ownership::InProcessClaimStore,
    hang_release: std::sync::atomic::AtomicBool,
    hang_ensure: std::sync::atomic::AtomicBool,
    commit_then_hang_ensure_once: std::sync::atomic::AtomicBool,
    poison_fence_cache_after_ensure: std::sync::Mutex<Option<EnsureClaimTestAction>>,
}

#[async_trait::async_trait]
impl crate::ownership::ClaimStore for HangingReleaseClaimStore {
    async fn ensure_schema(&self) -> Result<(), crate::ownership::ClaimError> {
        self.inner.ensure_schema().await
    }

    async fn acquire(
        &self,
        entity: &crate::ownership::Entity,
        me: &crate::ownership::NodeIdentity,
    ) -> Result<crate::ownership::ClaimEpoch, crate::ownership::ClaimError> {
        self.inner.acquire(entity, me).await
    }

    async fn ensure_claimed(
        &self,
        entity: &crate::ownership::Entity,
        me: &crate::ownership::NodeIdentity,
    ) -> Result<crate::ownership::ClaimEpoch, crate::ownership::ClaimError> {
        if self
            .commit_then_hang_ensure_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.inner.ensure_claimed(entity, me).await?;
            return std::future::pending().await;
        }
        if self.hang_ensure.load(std::sync::atomic::Ordering::SeqCst) {
            return std::future::pending().await;
        }
        let result = self.inner.ensure_claimed(entity, me).await;
        let action = self
            .poison_fence_cache_after_ensure
            .lock()
            .expect("poison injection lock")
            .take();
        match action {
            Some(EnsureClaimTestAction::PoisonFenceCache(registry)) => {
                if let Some(registry) = registry.upgrade() {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _fences = registry.claim_fences.write().expect("claim fences");
                        panic!("inject claim-fence publication failure");
                    }));
                }
            }
            Some(EnsureClaimTestAction::RotateIdentity { identity, next }) => {
                identity.rotate(next).await;
            }
            Some(EnsureClaimTestAction::Pause { reached, proceed }) => {
                reached.notify_one();
                proceed.notified().await;
            }
            Some(EnsureClaimTestAction::ReplaceClaimThenBackendError) => {
                let current = self
                    .inner
                    .current_claim(entity)
                    .await?
                    .ok_or(crate::ownership::ClaimError::Conflict)?;
                self.inner
                    .release(entity, &current.owner, current.claim_epoch)
                    .await?;
                self.inner.ensure_claimed(entity, me).await?;
                return Err(crate::ownership::ClaimError::Backend(
                    "injected lost response after replacement claim".to_string(),
                ));
            }
            None => {}
        }
        result
    }

    async fn steal_stale(
        &self,
        entity: &crate::ownership::Entity,
        observed: crate::ownership::ClaimEpoch,
        staleness: crate::ownership::StalePredicate,
        me: &crate::ownership::NodeIdentity,
    ) -> Result<crate::ownership::ClaimEpoch, crate::ownership::ClaimError> {
        self.inner
            .steal_stale(entity, observed, staleness, me)
            .await
    }

    async fn steal_for_resume(
        &self,
        entity: &crate::ownership::Entity,
        observed: crate::ownership::ClaimEpoch,
        witness: crate::ownership::ResumeIdentityProof,
        me: &crate::ownership::NodeIdentity,
    ) -> Result<crate::ownership::ClaimEpoch, crate::ownership::ClaimError> {
        self.inner
            .steal_for_resume(entity, observed, witness, me)
            .await
    }

    async fn current_claim(
        &self,
        entity: &crate::ownership::Entity,
    ) -> Result<Option<crate::ownership::ClaimSnapshot>, crate::ownership::ClaimError> {
        self.inner.current_claim(entity).await
    }

    async fn fence(
        &self,
        entity: &crate::ownership::Entity,
        me: &crate::ownership::NodeIdentity,
        mine: crate::ownership::ClaimEpoch,
    ) -> Result<bool, crate::ownership::ClaimError> {
        self.inner.fence(entity, me, mine).await
    }

    async fn release(
        &self,
        _entity: &crate::ownership::Entity,
        _me: &crate::ownership::NodeIdentity,
        _mine: crate::ownership::ClaimEpoch,
    ) -> Result<(), crate::ownership::ClaimError> {
        if self.hang_release.load(std::sync::atomic::Ordering::SeqCst) {
            std::future::pending().await
        } else {
            self.inner.release(_entity, _me, _mine).await
        }
    }

    async fn release_exact(
        &self,
        entity: &crate::ownership::Entity,
        me: &crate::ownership::NodeIdentity,
        mine: crate::ownership::ClaimEpoch,
    ) -> Result<crate::ownership::ExactReleaseOutcome, crate::ownership::ClaimError> {
        if self.hang_release.load(std::sync::atomic::Ordering::SeqCst) {
            std::future::pending().await
        } else {
            self.inner.release_exact(entity, me, mine).await
        }
    }

    async fn release_many(
        &self,
        entities: &[crate::ownership::Entity],
        me: &crate::ownership::NodeIdentity,
    ) -> Result<(), crate::ownership::ClaimError> {
        self.inner.release_many(entities, me).await
    }
}

#[tokio::test(start_paused = true)]
async fn hung_claim_release_is_bounded_and_retains_exact_fence() {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        std::sync::Arc::new(HangingReleaseClaimStore {
            inner: crate::ownership::InProcessClaimStore::new(),
            hang_release: std::sync::atomic::AtomicBool::new(true),
            hang_ensure: std::sync::atomic::AtomicBool::new(false),
            commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
            poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
        }),
        crate::ownership::SharedNodeIdentity::new(me),
    );
    let stream_id = "hung-release";
    registry
        .store_session(make_test_session(stream_id))
        .await
        .expect("store claimed session");

    let taken = tokio::time::timeout(
        super::core::CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT + Duration::from_millis(1),
        registry.take_session(stream_id),
    )
    .await
    .expect("hung release must remain bounded")
    .expect("bounded take");
    assert!(taken.is_some());
    assert!(registry
        .claim_fences
        .read()
        .expect("fences")
        .contains_key(stream_id));
}

#[tokio::test]
async fn uncertain_reclaimed_claim_without_observed_owner_retains_capacity() {
    let attempted_owner = crate::ownership::NodeIdentity::new("sweeper", "uncertain");
    let store: std::sync::Arc<dyn crate::ownership::ClaimStore> =
        std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let registry = InMemorySmSessionRegistry::with_capacity(1).with_claim_store(
        store,
        crate::ownership::SharedNodeIdentity::new(attempted_owner.clone()),
    );
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "unobserved-uncertain-claim",
    );
    let replacement = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "replacement-after-uncertain-claim",
    );
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reserve uncertain claim capacity");

    assert!(registry
        .retire_uncertain_reclaimed_claim(&entity, &attempted_owner, reservation)
        .await
        .is_err());
    assert!(
        registry
            .reserve_reclaimed_claim_capacity(&replacement)
            .is_none(),
        "a missing snapshot is not proof that the in-flight ownership CAS cannot still commit"
    );
}

#[tokio::test(start_paused = true)]
async fn claim_session_commit_before_timeout_retains_acquisition_for_reconciliation() {
    use crate::ownership::ClaimStore as _;

    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(true),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(me.clone()),
    );
    let stream_id = "claim-commit-before-timeout";
    registry
        .sessions
        .write()
        .expect("sessions")
        .insert(stream_id.to_string(), make_test_session(stream_id));

    registry
        .claim_session(stream_id)
        .await
        .expect_err("the bounded attempt must report its ambiguous timeout");

    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    assert_eq!(
        store
            .current_claim(&entity)
            .await
            .expect("claim lookup")
            .expect("the test double committed before hanging")
            .owner,
        me
    );
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .expect("pending acquisitions")
        .iter()
        .any(|(id, _, disposition)| id == stream_id
            && *disposition == PendingClaimAcquisitionDisposition::RetainDetachedSession));

    registry.retry_pending_claim_releases(1).await;
    assert!(registry
        .claim_fences
        .read()
        .expect("claim fences")
        .contains_key(stream_id));
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .expect("pending acquisitions")
        .is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancelled_rejected_enable_release_retains_terminal_exact_inventory() {
    use crate::ownership::ClaimStore as _;

    let identity = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(identity.clone()),
    );
    let stream_id = "cancelled-rejected-enable-release";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    store
        .ensure_claimed(&entity, &identity)
        .await
        .expect("seed rejected enable claim");
    assert!(registry.reserve_claim_fence_capacity(stream_id));
    registry
        .pending_claim_acquisitions
        .write()
        .expect("pending acquisitions")
        .insert((
            stream_id.to_string(),
            identity,
            PendingClaimAcquisitionDisposition::ReleaseRejectedEnable,
        ));
    store
        .hang_release
        .store(true, std::sync::atomic::Ordering::SeqCst);

    assert!(tokio::time::timeout(
        Duration::from_millis(1),
        registry.retry_pending_claim_releases(1),
    )
    .await
    .is_err());
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .expect("pending acquisitions")
        .is_empty());
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert!(!registry
        .claim_fences
        .read()
        .expect("active fences")
        .contains_key(stream_id));
}

#[test]
fn reclaimed_claim_admission_is_bounded_before_the_ownership_cas() {
    let registry = InMemorySmSessionRegistry::with_capacity(1);
    let first =
        crate::ownership::Entity::new(crate::ownership::EntityType::SmSession, "bounded-reclaim-1");
    let second =
        crate::ownership::Entity::new(crate::ownership::EntityType::SmSession, "bounded-reclaim-2");

    let first_reservation = registry
        .reserve_reclaimed_claim_capacity(&first)
        .expect("first reservation");
    assert!(
        registry.reserve_reclaimed_claim_capacity(&first).is_none(),
        "same-stream ownership mutations must not share one cancellable reservation"
    );
    assert!(registry.reserve_reclaimed_claim_capacity(&second).is_none());
    registry.cancel_reclaimed_claim_capacity(&first, first_reservation);
    assert!(registry.reserve_reclaimed_claim_capacity(&second).is_some());
}

#[test]
fn ambiguous_reclaim_lookup_retains_its_reserved_capacity() {
    let registry = InMemorySmSessionRegistry::with_capacity(1);
    let first =
        crate::ownership::Entity::new(crate::ownership::EntityType::SmSession, "lookup-reclaim-1");
    let second =
        crate::ownership::Entity::new(crate::ownership::EntityType::SmSession, "lookup-reclaim-2");

    let reservation = registry
        .reserve_reclaimed_claim_capacity(&first)
        .expect("reclaim reservation");
    registry.defer_uncertain_reclaimed_claim(
        &first,
        &crate::ownership::NodeIdentity::new("reclaimer", "incarnation"),
        reservation,
    );

    assert!(
        registry.reserve_reclaimed_claim_capacity(&second).is_none(),
        "the reservation retained alongside an ambiguous lookup must bound ownership without double-counting the retry map"
    );
}

#[test]
fn stale_reclaim_completion_cannot_cancel_a_newer_same_stream_reservation() {
    let registry = InMemorySmSessionRegistry::with_capacity(2);
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "reclaim-reservation-generation",
    );
    let owner = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let fence =
        super::super::persistence::SmClaimFence::new(owner, crate::ownership::ClaimEpoch(1));
    let first = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("first reservation");
    assert!(registry.try_record_verified_reclaimed_fence(&entity.id, fence.clone(), first));
    let second = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("newer reservation");

    assert!(registry.try_record_verified_reclaimed_fence(&entity.id, fence.clone(), first));
    registry.cancel_reclaimed_claim_capacity(&entity, first);
    assert!(!registry
        .transfer_reclaimed_claim_to_exact_release(&entity, &fence, first)
        .expect("inspect stale transfer"));

    assert_eq!(
        registry
            .reclaimed_claim_reservations
            .read()
            .expect("reclaimed reservations")
            .get(&entity.id),
        Some(&second),
        "an older operation may only consume or cancel its own reservation token"
    );
}

#[test]
fn local_ownership_includes_ambiguous_and_in_flight_claim_admission() {
    let registry = InMemorySmSessionRegistry::with_capacity(4);
    let identity = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let ambiguous = "ambiguous-acquisition";
    let in_flight = "reservation-only";

    assert!(registry.reserve_claim_fence_capacity(ambiguous));
    registry
        .pending_claim_acquisitions
        .write()
        .expect("pending acquisitions")
        .insert((
            ambiguous.to_string(),
            identity,
            PendingClaimAcquisitionDisposition::RetainDetachedSession,
        ));
    assert!(registry.reserve_claim_fence_capacity(in_flight));

    assert_eq!(
        registry.locally_owned_claim_ids().expect("owned inventory"),
        vec![ambiguous.to_string(), in_flight.to_string()],
        "self-fence and drain snapshots must conservatively include both an ambiguous CAS and its reservation-only in-flight window without duplicates"
    );
}

#[tokio::test]
async fn local_demotion_clears_confirmed_reclaim_retry_inventory() {
    let registry = InMemorySmSessionRegistry::with_capacity(3);
    let stream_id = "demoted-reclaim-inventory";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    let owner = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let fence = super::super::persistence::SmClaimFence::new(
        owner.clone(),
        crate::ownership::ClaimEpoch(1),
    );
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaim reservation");
    assert!(registry.try_record_verified_reclaimed_fence(stream_id, fence.clone(), reservation));
    registry
        .pending_reclaimed_hydrations
        .write()
        .unwrap()
        .insert(
            (stream_id.to_string(), fence.clone(), reservation),
            entity.clone(),
        );
    registry
        .pending_epoch_failure_reconciliations
        .write()
        .unwrap()
        .insert(stream_id.to_string());
    registry.pending_claim_releases.write().unwrap().insert((
        stream_id.to_string(),
        super::super::persistence::SmClaimFence::new(owner, crate::ownership::ClaimEpoch(0)),
    ));

    registry.forget_claim_locally(stream_id).await;

    assert!(registry.locally_owned_claim_ids().unwrap().is_empty());
    assert_eq!(registry.pending_reclaimed_hydration_count(), 0);
    assert_eq!(registry.pending_claim_release_count(), 0);
    let replacement = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "replacement-after-demotion",
    );
    assert!(registry
        .reserve_reclaimed_claim_capacity(&replacement)
        .is_some());
}

#[tokio::test]
async fn local_demotion_preserves_ambiguous_reclaim_until_lookup_disproves_it() {
    let registry = InMemorySmSessionRegistry::with_capacity(1);
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "demoted-ambiguous-reclaim",
    );
    let owner = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaim reservation");
    registry.defer_uncertain_reclaimed_claim(&entity, &owner, reservation);

    registry.forget_claim_locally(&entity.id).await;

    assert_eq!(
        registry.locally_owned_claim_ids().unwrap(),
        vec![entity.id.clone()]
    );
    let replacement = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "replacement-before-ambiguous-resolution",
    );
    assert!(registry
        .reserve_reclaimed_claim_capacity(&replacement)
        .is_none());

    assert_eq!(registry.retry_pending_reclaimed_hydrations(1).await, 1);
    assert!(registry.locally_owned_claim_ids().unwrap().is_empty());
    assert!(registry
        .reserve_reclaimed_claim_capacity(&replacement)
        .is_some());
}

#[tokio::test]
async fn local_demotion_discards_hydration_from_an_older_reclaim_generation() {
    let registry = InMemorySmSessionRegistry::with_capacity(2);
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "demoted-stale-reclaim-hydration",
    );
    let owner = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let fence = super::super::persistence::SmClaimFence::new(
        owner.clone(),
        crate::ownership::ClaimEpoch(1),
    );
    let first = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("first reservation");
    assert!(registry.try_record_verified_reclaimed_fence(&entity.id, fence.clone(), first));
    registry
        .pending_reclaimed_hydrations
        .write()
        .unwrap()
        .insert((entity.id.clone(), fence, first), entity.clone());
    let second = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("newer reservation");
    registry.defer_uncertain_reclaimed_claim(&entity, &owner, second);

    registry.forget_claim_locally(&entity.id).await;

    assert_eq!(registry.pending_reclaimed_hydration_count(), 0);
    assert_eq!(
        registry
            .reclaimed_claim_reservations
            .read()
            .unwrap()
            .get(&entity.id),
        Some(&second)
    );
    assert_eq!(registry.retry_pending_reclaimed_hydrations(1).await, 1);
    assert!(registry.locally_owned_claim_ids().unwrap().is_empty());
}

#[tokio::test]
async fn active_fence_readmission_publishes_a_demotion_safe_reservation() {
    let registry = InMemorySmSessionRegistry::with_capacity(1);
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "active-fence-readmission",
    );
    let owner = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let fence = super::super::persistence::SmClaimFence::new(
        owner.clone(),
        crate::ownership::ClaimEpoch(1),
    );
    let first_reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("initial reclaim reservation");
    assert!(registry.try_record_verified_reclaimed_fence(&entity.id, fence, first_reservation));

    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("readmission reservation");
    registry.defer_uncertain_reclaimed_claim(&entity, &owner, reservation);
    registry.forget_claim_locally(&entity.id).await;

    assert!(
        registry
            .reclaimed_claim_reservations
            .read()
            .unwrap()
            .get(&entity.id)
            == Some(&reservation)
    );
    assert!(registry
        .pending_reclaimed_claim_lookups
        .read()
        .unwrap()
        .contains_key(&(entity.id.clone(), owner, reservation)));
    let replacement = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "replacement-after-active-readmission",
    );
    assert!(registry
        .reserve_reclaimed_claim_capacity(&replacement)
        .is_none());

    assert_eq!(registry.retry_pending_reclaimed_hydrations(1).await, 1);
    assert!(registry.locally_owned_claim_ids().unwrap().is_empty());
    assert!(registry
        .reserve_reclaimed_claim_capacity(&replacement)
        .is_some());
}

#[tokio::test]
async fn local_ownership_includes_a_confirmed_enabled_claim_before_detach() {
    let registry = InMemorySmSessionRegistry::new();
    let stream_id = "confirmed-enable-before-detach";

    assert!(registry.ensure_session_claim(stream_id).await.is_some());
    assert!(registry
        .live_session_ids()
        .expect("live sessions")
        .is_empty());
    assert_eq!(
        registry.locally_owned_claim_ids().expect("owned inventory"),
        vec![stream_id.to_string()],
        "a confirmed enable-time fence is locally owned even before the live socket detaches into the session registry"
    );
}

#[tokio::test]
async fn enable_claim_publication_guard_blocks_identity_rotation_until_caller_publishes() {
    let old = crate::ownership::NodeIdentity::new("sm-node", "old-incarnation");
    let fresh = crate::ownership::NodeIdentity::new("sm-node", "fresh-incarnation");
    let shared = crate::ownership::SharedNodeIdentity::new(old);
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        std::sync::Arc::new(crate::ownership::InProcessClaimStore::new()),
        shared.clone(),
    );
    let publication = registry
        .ensure_session_claim("guarded-enable-publication")
        .await
        .expect("claim admission");
    let rotating = shared.clone();
    let rotation = tokio::spawn(async move { rotating.rotate(fresh).await });

    tokio::task::yield_now().await;
    assert!(
        !rotation.is_finished(),
        "self-fence rotation must wait until the caller publishes enabled state"
    );
    drop(publication);
    rotation.await.expect("rotation task");
}

#[tokio::test]
async fn enable_claim_publication_guard_blocks_local_demotion_until_caller_publishes() {
    let registry = std::sync::Arc::new(InMemorySmSessionRegistry::new());
    let stream_id = "guarded-enable-demotion";
    let publication = registry
        .ensure_session_claim(stream_id)
        .await
        .expect("claim admission");
    let demoting = {
        let registry = registry.clone();
        tokio::spawn(async move { registry.forget_claim_locally(stream_id).await })
    };

    tokio::task::yield_now().await;
    assert!(
        !demoting.is_finished(),
        "self-fence demotion must wait until transport publication completes"
    );
    assert_eq!(
        registry.locally_owned_claim_ids().expect("owned inventory"),
        vec![stream_id.to_string()]
    );

    drop(publication);
    demoting.await.expect("demotion task");
    assert!(registry
        .locally_owned_claim_ids()
        .expect("owned inventory")
        .is_empty());
}

#[tokio::test]
async fn exact_owner_inventory_excludes_fresh_post_rotation_admissions() {
    let old = crate::ownership::NodeIdentity::new("sm-node", "old-incarnation");
    let fresh = crate::ownership::NodeIdentity::new("sm-node", "fresh-incarnation");
    let shared = crate::ownership::SharedNodeIdentity::new(old.clone());
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        std::sync::Arc::new(crate::ownership::InProcessClaimStore::new()),
        shared.clone(),
    );
    let old_publication = registry
        .ensure_session_claim("old-enable")
        .await
        .expect("old claim admission");
    drop(old_publication);

    shared.rotate(fresh.clone()).await;
    let fresh_publication = registry
        .ensure_session_claim("fresh-enable")
        .await
        .expect("fresh claim admission");
    drop(fresh_publication);

    assert_eq!(
        registry
            .locally_owned_claim_ids_for_owner(&old)
            .expect("old-owner inventory"),
        vec!["old-enable".to_string()]
    );
    assert_eq!(
        registry
            .locally_owned_claim_ids_for_owner(&fresh)
            .expect("fresh-owner inventory"),
        vec!["fresh-enable".to_string()]
    );
}

#[tokio::test]
async fn pending_claim_retry_limit_is_shared_across_acquisitions_and_releases() {
    use crate::ownership::ClaimStore as _;

    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(me.clone()),
    );
    let acquisition_id = "budgeted-acquisition";
    assert!(registry.reserve_claim_fence_capacity(acquisition_id));
    registry.sessions.write().expect("sessions").insert(
        acquisition_id.to_string(),
        make_test_session(acquisition_id),
    );
    registry
        .pending_claim_acquisitions
        .write()
        .expect("pending acquisitions")
        .insert((
            acquisition_id.to_string(),
            me.clone(),
            PendingClaimAcquisitionDisposition::RetainDetachedSession,
        ));

    let release_id = "budgeted-release";
    let release_entity =
        crate::ownership::Entity::new(crate::ownership::EntityType::SmSession, release_id);
    let release_epoch = store
        .acquire(&release_entity, &me)
        .await
        .expect("release claim");
    registry
        .pending_claim_releases
        .write()
        .expect("pending releases")
        .insert((
            release_id.to_string(),
            super::super::persistence::SmClaimFence::new(me, release_epoch),
        ));

    assert_eq!(registry.retry_pending_claim_releases(1).await, 0);
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .expect("pending acquisitions")
        .is_empty());
    assert_eq!(
        registry.pending_claim_release_count(),
        1,
        "the exact release must retain the shared budget's next slot"
    );
    assert!(store
        .current_claim(&release_entity)
        .await
        .expect("release lookup")
        .is_some());
}

#[tokio::test(start_paused = true)]
async fn terminal_release_retry_clears_retained_exact_fence() {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(true),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = InMemorySmSessionRegistry::new()
        .with_claim_store(store.clone(), crate::ownership::SharedNodeIdentity::new(me));
    let stream_id = "retry-release";
    registry
        .store_session(make_test_session(stream_id))
        .await
        .expect("store session");
    registry
        .take_session(stream_id)
        .await
        .expect("take session");
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert!(
        registry.live_session_ids().expect("live session snapshot").is_empty(),
        "a terminal exact-release retry is not a resumable session and must not shield pending-delivery claims"
    );
    assert_eq!(
        registry
            .locally_owned_claim_ids()
            .expect("local claim snapshot"),
        vec![stream_id.to_string()],
        "ownership reconciliation still accounts for the ambiguous exact release"
    );

    store
        .hang_release
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(registry.retry_pending_claim_releases(8).await, 1);
    assert_eq!(registry.pending_claim_release_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn terminal_release_retry_skips_stream_that_became_live_again() {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(true),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = InMemorySmSessionRegistry::new()
        .with_claim_store(store.clone(), crate::ownership::SharedNodeIdentity::new(me));
    let stream_id = "retry-became-live";
    registry
        .store_session(make_test_session(stream_id))
        .await
        .expect("initial store");
    registry
        .take_session(stream_id)
        .await
        .expect("terminal take");
    assert_eq!(registry.pending_claim_release_count(), 1);

    registry
        .store_session(make_test_session(stream_id))
        .await
        .expect("same stream becomes resumable again");
    store
        .hang_release
        .store(false, std::sync::atomic::Ordering::SeqCst);

    assert_eq!(
        registry.retry_pending_claim_releases(8).await,
        0,
        "a terminal retry must not release a stream that is live again"
    );
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert!(registry
        .peek_session(stream_id)
        .await
        .expect("live session probe")
        .is_some());
}

#[tokio::test(start_paused = true)]
async fn rejected_enable_reconciliation_skips_stream_that_became_live_again() {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(true),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = InMemorySmSessionRegistry::new()
        .with_claim_store(store.clone(), crate::ownership::SharedNodeIdentity::new(me));
    let stream_id = "rejected-enable-became-live";

    assert!(registry.ensure_session_claim(stream_id).await.is_none());
    assert_eq!(
        registry
            .pending_claim_acquisitions
            .read()
            .expect("pending acquisition")
            .len(),
        1
    );
    store
        .hang_ensure
        .store(false, std::sync::atomic::Ordering::SeqCst);
    registry
        .store_session(make_test_session(stream_id))
        .await
        .expect("same id becomes a live detached session");

    assert_eq!(registry.retry_pending_claim_releases(8).await, 0);
    assert!(
        registry
            .pending_claim_acquisitions
            .read()
            .expect("pending acquisition")
            .is_empty(),
        "the live session lifecycle supersedes the rejected-enable retry"
    );
    assert!(!registry.has_claim_fence_reservation(stream_id));
    assert!(registry
        .peek_session(stream_id)
        .await
        .expect("live session probe")
        .is_some());
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup")
            .is_some()
    );

    registry
        .take_session(stream_id)
        .await
        .expect("take live session")
        .expect("live session existed");
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup after live lifecycle ends")
            .is_none()
    );
    assert_eq!(registry.retry_pending_claim_releases(8).await, 0);
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup after later janitor pass")
            .is_none(),
        "a superseded rejected-enable retry must never manufacture a fresh claim"
    );
}

#[tokio::test(start_paused = true)]
async fn conflicting_detach_preserves_borrowed_rejected_enable_reservation() {
    use crate::ownership::ClaimStore as _;

    let old = crate::ownership::NodeIdentity::new("sm-node", "old-incarnation");
    let current = crate::ownership::NodeIdentity::new("sm-node", "current-incarnation");
    let identity = crate::ownership::SharedNodeIdentity::new(old.clone());
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(true),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(true),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = InMemorySmSessionRegistry::with_capacity(1)
        .with_claim_store(store.clone(), identity.clone());
    let stream_id = "borrowed-rejected-enable-reservation";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );

    assert!(registry.ensure_session_claim(stream_id).await.is_none());
    assert!(registry.has_claim_fence_reservation(stream_id));
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .unwrap()
        .iter()
        .any(|(id, owner, disposition)| {
            id == stream_id
                && owner == &old
                && *disposition == PendingClaimAcquisitionDisposition::ReleaseRejectedEnable
        }));
    store
        .hang_ensure
        .store(false, std::sync::atomic::Ordering::SeqCst);
    identity.rotate(current).await;
    registry
        .store_session(make_test_session(stream_id))
        .await
        .expect("detach remains best effort after the old claim wins");

    assert!(registry.has_claim_fence_reservation(stream_id));
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .unwrap()
        .iter()
        .any(|(id, owner, disposition)| {
            id == stream_id
                && owner == &old
                && *disposition == PendingClaimAcquisitionDisposition::ReleaseRejectedEnable
        }));
    assert!(
        !registry.reserve_claim_fence_capacity("another-stream"),
        "the borrowed marker must remain capacity-counted until reconciliation"
    );

    assert_eq!(registry.retry_pending_claim_releases(1).await, 0);
    assert!(!registry.has_claim_fence_reservation(stream_id));
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .unwrap()
        .is_empty());
    assert!(store.current_claim(&entity).await.unwrap().is_none());
}

#[tokio::test]
async fn cancelled_borrowed_detach_acquisition_retains_current_claim_reconciliation() {
    use crate::ownership::ClaimStore as _;

    let old = crate::ownership::NodeIdentity::new("sm-node", "old-incarnation");
    let current = crate::ownership::NodeIdentity::new("sm-node", "current-incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(true),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::with_capacity(1).with_claim_store(
            store.clone(),
            crate::ownership::SharedNodeIdentity::new(current.clone()),
        ),
    );
    let stream_id = "cancelled-borrowed-detach-acquisition";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    assert!(registry.reserve_claim_fence_capacity(stream_id));
    registry
        .pending_claim_acquisitions
        .write()
        .unwrap()
        .insert((
            stream_id.to_string(),
            old,
            PendingClaimAcquisitionDisposition::ReleaseRejectedEnable,
        ));

    let storing_registry = registry.clone();
    let storing = tokio::spawn(async move {
        storing_registry
            .store_session(realistic_test_session(stream_id))
            .await
    });
    loop {
        if store.current_claim(&entity).await.unwrap().is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    storing.abort();
    assert!(storing.await.unwrap_err().is_cancelled());

    assert!(registry.has_claim_fence_reservation(stream_id));
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .unwrap()
        .iter()
        .any(|(id, owner, disposition)| {
            id == stream_id
                && owner == &current
                && *disposition == PendingClaimAcquisitionDisposition::RetainDetachedSession
        }));
}

#[tokio::test]
async fn live_stream_releases_timed_out_old_identity_without_adopting_its_fence() {
    use crate::ownership::ClaimStore as _;

    let old = crate::ownership::NodeIdentity::new("sm-node", "old-incarnation");
    let current = crate::ownership::NodeIdentity::new("sm-node", "new-incarnation");
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let stream_id = "old-enable-claim-new-live-session";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    store.acquire(&entity, &old).await.expect("old claim");
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(current),
    );
    assert!(registry.reserve_claim_fence_capacity(stream_id));
    registry
        .pending_claim_acquisitions
        .write()
        .expect("pending acquisitions")
        .insert((
            stream_id.to_string(),
            old.clone(),
            PendingClaimAcquisitionDisposition::ReleaseRejectedEnable,
        ));
    registry
        .sessions
        .write()
        .expect("sessions")
        .insert(stream_id.to_string(), make_test_session(stream_id));

    assert_eq!(registry.retry_pending_claim_releases(8).await, 0);
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .expect("pending acquisitions")
        .is_empty());
    assert!(
        !registry
            .claim_fences
            .read()
            .expect("claim fences")
            .contains_key(stream_id),
        "an advisory old-incarnation snapshot must never authorize the new live lifecycle"
    );
    assert!(store
        .current_claim(&entity)
        .await
        .expect("claim lookup")
        .is_none());
    assert_eq!(registry.pending_claim_release_count(), 0);
}

#[tokio::test]
async fn old_claim_cleanup_waits_for_shared_detach_reservation_to_resolve() {
    use crate::ownership::ClaimStore as _;

    let old = crate::ownership::NodeIdentity::new("sm-node", "old-incarnation");
    let current = crate::ownership::NodeIdentity::new("sm-node", "new-incarnation");
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let stream_id = "shared-pending-acquisition-reservation";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    store.acquire(&entity, &old).await.expect("old claim");
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(current.clone()),
    );
    assert!(registry.reserve_claim_fence_capacity(stream_id));
    {
        let mut pending = registry
            .pending_claim_acquisitions
            .write()
            .expect("pending acquisitions");
        pending.insert((
            stream_id.to_string(),
            old.clone(),
            PendingClaimAcquisitionDisposition::ReleaseRejectedEnable,
        ));
        pending.insert((
            stream_id.to_string(),
            current.clone(),
            PendingClaimAcquisitionDisposition::RetainDetachedSession,
        ));
    }
    registry
        .sessions
        .write()
        .expect("sessions")
        .insert(stream_id.to_string(), make_test_session(stream_id));

    assert_eq!(
        registry.reserve_detach_claim_fence_capacity(stream_id),
        None,
        "a marker already backing uncertain detach work must not be borrowed again"
    );

    assert!(
        registry
            .reconcile_rejected_enable_while_live(stream_id, &old)
            .await
    );
    {
        let pending = registry
            .pending_claim_acquisitions
            .read()
            .expect("pending acquisitions");
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().any(|(id, _, disposition)| {
            id == stream_id
                && *disposition == PendingClaimAcquisitionDisposition::RetainDetachedSession
        }));
    }
    assert!(
        registry.has_claim_fence_reservation(stream_id),
        "old-claim cleanup must not consume the detach reconciliation's reservation"
    );
    assert!(store
        .current_claim(&entity)
        .await
        .expect("old claim lookup")
        .is_some());

    registry
        .reconcile_uncertain_claim_acquisition(
            stream_id,
            current,
            PendingClaimAcquisitionDisposition::RetainDetachedSession,
        )
        .await;
    {
        let pending = registry
            .pending_claim_acquisitions
            .read()
            .expect("pending acquisitions");
        assert_eq!(pending.len(), 1);
        assert!(pending.iter().any(|(id, _, disposition)| {
            id == stream_id
                && *disposition == PendingClaimAcquisitionDisposition::ReleaseRejectedEnable
        }));
    }
    assert!(
        registry.has_claim_fence_reservation(stream_id),
        "a conflicting detach reconciliation must leave the slot to the remaining rejected-enable cleanup"
    );
    assert!(
        registry
            .reconcile_rejected_enable_while_live(stream_id, &old)
            .await
    );
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .expect("pending acquisitions")
        .is_empty());
    assert!(store
        .current_claim(&entity)
        .await
        .expect("claim lookup after terminal conversion")
        .is_none());
    assert!(!registry
        .claim_fences
        .read()
        .expect("claim fences")
        .contains_key(stream_id));
}

#[tokio::test(start_paused = true)]
async fn terminal_old_identity_release_retries_while_replacement_stream_is_live() {
    use crate::ownership::ClaimStore as _;

    let old = crate::ownership::NodeIdentity::new("sm-node", "old-incarnation");
    let current = crate::ownership::NodeIdentity::new("sm-node", "new-incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(true),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let stream_id = "retry-old-terminal-fence-while-live";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    store.acquire(&entity, &old).await.expect("old claim");
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(current),
    );
    assert!(registry.reserve_claim_fence_capacity(stream_id));
    registry
        .pending_claim_acquisitions
        .write()
        .expect("pending acquisitions")
        .insert((
            stream_id.to_string(),
            old.clone(),
            PendingClaimAcquisitionDisposition::ReleaseRejectedEnable,
        ));
    registry
        .sessions
        .write()
        .expect("sessions")
        .insert(stream_id.to_string(), make_test_session(stream_id));

    assert!(
        registry
            .reconcile_rejected_enable_while_live(stream_id, &old)
            .await
    );
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert!(!registry
        .claim_fences
        .read()
        .expect("claim fences")
        .contains_key(stream_id));
    assert!(store
        .current_claim(&entity)
        .await
        .expect("old claim lookup")
        .is_some());

    store
        .hang_release
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(registry.retry_pending_claim_releases(8).await, 1);
    assert_eq!(registry.pending_claim_release_count(), 0);
    assert!(store
        .current_claim(&entity)
        .await
        .expect("claim lookup after retry")
        .is_none());
    assert!(registry
        .sessions
        .read()
        .expect("sessions")
        .contains_key(stream_id));
}

#[tokio::test(start_paused = true)]
async fn reclaimed_terminal_release_failure_retains_exact_retry() {
    use crate::ownership::ClaimStore as _;

    let me = crate::ownership::NodeIdentity::new("reclaimer", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(true),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "missing-reclaimed-terminal",
    );
    let epoch = store.acquire(&entity, &me).await.expect("claim");
    let fence = super::super::persistence::SmClaimFence::new(me.clone(), epoch);
    let registry = InMemorySmSessionRegistry::new()
        .with_claim_store(store, crate::ownership::SharedNodeIdentity::new(me));

    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaim reservation");
    assert_eq!(
        registry
            .hydrate_reclaimed_typed(&entity, &fence, reservation)
            .await
            .expect("typed hydration"),
        ReclaimedHydrationOutcome::MissingDurable
    );
    assert!(registry
        .release_reclaimed_claim(&entity, &fence, reservation)
        .await
        .is_err());
    assert_eq!(
        registry.pending_claim_release_count(),
        1,
        "failed inline terminal release must retain the supplied exact fence for retry"
    );
}

#[tokio::test]
async fn reclaimed_terminal_release_retains_caller_work_when_liveness_is_poisoned() {
    use crate::ownership::ClaimStore as _;

    let me = crate::ownership::NodeIdentity::new("reclaimer", "incarnation");
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "poisoned-reclaimed-terminal",
    );
    let epoch = store.acquire(&entity, &me).await.expect("claim");
    let fence = super::super::persistence::SmClaimFence::new(me.clone(), epoch);
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::new()
            .with_claim_store(store.clone(), crate::ownership::SharedNodeIdentity::new(me)),
    );
    let poison_target = std::sync::Arc::clone(&registry);
    assert!(std::thread::spawn(move || {
        let _sessions = poison_target.sessions.write().expect("sessions lock");
        panic!("poison sessions lock");
    })
    .join()
    .is_err());
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaim reservation");

    assert!(
        registry
            .release_reclaimed_claim(&entity, &fence, reservation)
            .await
            .is_err(),
        "unknown liveness must keep the caller's exact work retryable"
    );
    assert_eq!(
        registry.pending_claim_release_count(),
        1,
        "one-shot callers must also leave exact retry inventory"
    );
    assert!(
        store
            .fence(&entity, fence.owner(), fence.epoch())
            .await
            .expect("claim fence after uncertain cleanup"),
        "uncertain liveness must not touch the backend claim"
    );
}

#[tokio::test]
async fn self_fence_reclaim_reuses_full_old_incarnation_slot_when_liveness_is_poisoned() {
    use crate::ownership::ClaimStore as _;

    let old_owner = crate::ownership::NodeIdentity::new("sm-node", "old-incarnation");
    let fresh_owner = crate::ownership::NodeIdentity::new("sm-node", "fresh-incarnation");
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let identity_handle = crate::ownership::SharedNodeIdentity::new(old_owner.clone());
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::with_capacity(1)
            .with_claim_store(store.clone(), identity_handle.clone()),
    );
    let stream_id = "self-fence-full-capacity";
    let entity = crate::ownership::Entity::new(crate::ownership::EntityType::SmSession, stream_id);
    let old_epoch = store.acquire(&entity, &old_owner).await.expect("old claim");
    let old_fence = super::super::persistence::SmClaimFence::new(old_owner.clone(), old_epoch);
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("self-fence reclaim reservation");
    assert_eq!(
        store
            .release_exact(&entity, &old_owner, old_epoch)
            .await
            .expect("supersede old claim"),
        crate::ownership::ExactReleaseOutcome::Released
    );
    let fresh_epoch = store
        .acquire(&entity, &fresh_owner)
        .await
        .expect("self-fence reclaim");
    let fresh_fence = super::super::persistence::SmClaimFence::new(fresh_owner, fresh_epoch);
    identity_handle.rotate(fresh_fence.owner().clone()).await;
    assert_eq!(
        registry
            .hydrate_reclaimed_typed(&entity, &fresh_fence, reservation)
            .await
            .expect("verified self-fence hydration"),
        ReclaimedHydrationOutcome::MissingDurable
    );
    let poison_target = std::sync::Arc::clone(&registry);
    assert!(std::thread::spawn(move || {
        let _sessions = poison_target.sessions.write().expect("sessions lock");
        panic!("poison sessions lock");
    })
    .join()
    .is_err());

    assert!(registry
        .release_reclaimed_claim(&entity, &fresh_fence, reservation)
        .await
        .is_err());
    assert_eq!(
        registry.pending_claim_release_count(),
        1,
        "the fresh exact fence must replace the old incarnation in the full bounded slot"
    );
    assert!(
        registry
            .release_reclaimed_claim(&entity, &old_fence, reservation)
            .await
            .is_err(),
        "a delayed obsolete cleanup must remain with its supervised caller"
    );
    {
        let pending = registry
            .pending_claim_releases
            .read()
            .expect("pending exact releases");
        assert!(pending.contains(&(stream_id.to_string(), fresh_fence.clone())));
        assert!(!pending.contains(&(stream_id.to_string(), old_fence)));
    }
    assert!(store
        .fence(&entity, fresh_fence.owner(), fresh_fence.epoch())
        .await
        .expect("fresh fence remains owned"));
}

#[tokio::test(start_paused = true)]
async fn identity_rotation_release_timeout_keeps_exact_retry_after_local_forget() {
    use crate::ownership::ClaimStore as _;

    let me = crate::ownership::NodeIdentity::new("sm-node", "old-incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(true),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(me.clone()),
    );
    let stream_id = "rotated-release-timeout";
    registry
        .store_session(make_test_session(stream_id))
        .await
        .expect("store detached session");
    assert!(
        registry
            .claim_session(stream_id)
            .await
            .expect("claim detached session")
            .is_some(),
        "the regression must exercise the claimed-session rotation path"
    );
    let entity = crate::ownership::Entity::new(crate::ownership::EntityType::SmSession, stream_id);
    let epoch = store
        .current_claim(&entity)
        .await
        .expect("claim lookup")
        .expect("stored claim")
        .claim_epoch;
    let fence = super::super::persistence::SmClaimFence::new(me, epoch);

    registry
        .abandon_claim_after_identity_rotation(stream_id, &fence)
        .await
        .expect("bounded rotated-owner cleanup");

    assert!(registry
        .live_session_ids()
        .expect("live inventory")
        .is_empty());
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert!(store
        .fence(&entity, fence.owner(), fence.epoch())
        .await
        .expect("old claim remains after timeout"));

    store
        .hang_release
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(registry.retry_pending_claim_releases(8).await, 1);
    assert_eq!(registry.pending_claim_release_count(), 0);
    assert!(store
        .current_claim(&entity)
        .await
        .expect("claim lookup after retry")
        .is_none());
}

#[tokio::test]
async fn stale_identity_rotation_cleanup_cannot_remove_a_newer_lifecycle() {
    use crate::ownership::ClaimStore as _;

    let me = crate::ownership::NodeIdentity::new("sm-node", "current-incarnation");
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(me.clone()),
    );
    let stream_id = "rotation-cleanup-stale-fence";
    registry
        .store_session(make_test_session(stream_id))
        .await
        .expect("store replacement session");
    let stale = super::super::persistence::SmClaimFence::new(
        crate::ownership::NodeIdentity::new("sm-node", "old-incarnation"),
        crate::ownership::ClaimEpoch(1),
    );

    assert!(registry
        .abandon_claim_after_identity_rotation(stream_id, &stale)
        .await
        .is_err());
    assert_eq!(
        registry.live_session_ids().expect("live inventory"),
        vec![stream_id.to_string()]
    );
    let entity = crate::ownership::Entity::new(crate::ownership::EntityType::SmSession, stream_id);
    assert_eq!(
        store
            .current_claim(&entity)
            .await
            .expect("current claim")
            .expect("replacement claim")
            .owner,
        me
    );
}

#[tokio::test]
async fn reclaimed_terminal_release_reports_not_owned_without_touching_replacement() {
    use crate::ownership::ClaimStore as _;

    let old_owner = crate::ownership::NodeIdentity::new("old-reclaimer", "old-incarnation");
    let replacement = crate::ownership::NodeIdentity::new("replacement", "new-incarnation");
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "reclaimed-release-lost-race",
    );
    let old_epoch = store.acquire(&entity, &old_owner).await.expect("old claim");
    assert_eq!(
        store
            .release_exact(&entity, &old_owner, old_epoch)
            .await
            .expect("release old generation"),
        crate::ownership::ExactReleaseOutcome::Released
    );
    let replacement_epoch = store
        .acquire(&entity, &replacement)
        .await
        .expect("replacement claim");
    let old_fence = super::super::persistence::SmClaimFence::new(old_owner.clone(), old_epoch);
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(old_owner),
    );
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("obsolete cleanup reservation");

    assert_eq!(
        registry
            .release_reclaimed_claim(&entity, &old_fence, reservation)
            .await
            .expect("exact lost-race outcome"),
        crate::ownership::ExactReleaseOutcome::NotOwned
    );
    assert!(store
        .fence(&entity, &replacement, replacement_epoch)
        .await
        .expect("replacement fence"));
    assert_eq!(registry.pending_claim_release_count(), 0);
}

#[tokio::test]
async fn shutdown_drained_session_is_not_a_terminal_release_retry() {
    let registry = InMemorySmSessionRegistry::new();
    registry
        .store_session(make_test_session("shutdown-in-flight"))
        .await
        .expect("store session");
    assert_eq!(
        registry
            .drain_all_for_shutdown()
            .await
            .expect("drain")
            .len(),
        1
    );
    assert_eq!(registry.pending_claim_release_count(), 0);
    assert_eq!(registry.retry_pending_claim_releases(8).await, 0);
}

#[tokio::test(start_paused = true)]
async fn hung_enable_claim_is_bounded_and_not_recorded() {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        std::sync::Arc::new(HangingReleaseClaimStore {
            inner: crate::ownership::InProcessClaimStore::new(),
            hang_release: std::sync::atomic::AtomicBool::new(false),
            hang_ensure: std::sync::atomic::AtomicBool::new(true),
            commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
            poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
        }),
        crate::ownership::SharedNodeIdentity::new(me),
    );
    assert!(registry.ensure_session_claim("hung-enable").await.is_none());
    assert!(!registry
        .claim_fences
        .read()
        .expect("fences")
        .contains_key("hung-enable"));
}

#[tokio::test]
async fn cancelled_enable_acquisition_retains_and_releases_commit_before_drop() {
    use crate::ownership::ClaimStore as _;

    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(true),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = std::sync::Arc::new(InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(me.clone()),
    ));
    let stream_id = "cancelled-commit-before-ensure-return";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    let task = {
        let registry = registry.clone();
        tokio::spawn(async move { registry.ensure_session_claim(stream_id).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if store
                .current_claim(&entity)
                .await
                .expect("claim lookup")
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("claim committed before cancellation");

    task.abort();
    assert!(task.await.is_err_and(|error| error.is_cancelled()));
    assert_eq!(
        registry
            .pending_claim_acquisitions
            .read()
            .expect("pending acquisition")
            .len(),
        1
    );
    assert!(registry.has_claim_fence_reservation(stream_id));

    assert_eq!(registry.retry_pending_claim_releases(8).await, 0);
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .expect("pending acquisition")
        .is_empty());
    assert!(store
        .current_claim(&entity)
        .await
        .expect("claim lookup after retry")
        .is_none());
    assert_eq!(registry.claim_fence_capacity_used(), 0);
}

#[tokio::test(start_paused = true)]
async fn enable_claim_commit_before_timeout_is_reconciled_and_released() {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(true),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = InMemorySmSessionRegistry::new()
        .with_claim_store(store.clone(), crate::ownership::SharedNodeIdentity::new(me));
    let stream_id = "commit-before-timeout";

    assert!(registry.ensure_session_claim(stream_id).await.is_none());

    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("read reconciled claim")
            .is_none()
    );
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .expect("pending acquisitions")
        .is_empty());
    assert_eq!(registry.pending_claim_release_count(), 0);
    assert_eq!(registry.claim_fence_capacity_used(), 0);
}

#[tokio::test(start_paused = true)]
async fn detach_claim_commit_before_timeout_is_reconciled_and_retained() {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(true),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let persistence = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(persistence.clone())
        .with_claim_store(store.clone(), crate::ownership::SharedNodeIdentity::new(me));
    let stream_id = "detach-commit-before-timeout";

    registry
        .store_session(realistic_test_session(stream_id))
        .await
        .expect("store detached session");

    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup")
            .is_some()
    );
    assert!(!registry
        .claim_fences
        .read()
        .expect("claim fences")
        .contains_key(stream_id));
    assert_eq!(
        registry
            .pending_claim_acquisitions
            .read()
            .expect("pending acquisitions")
            .len(),
        1,
        "detach records uncertainty without a second ClaimStore call under the shard lock"
    );
    assert_eq!(registry.pending_claim_release_count(), 0);
    assert!(persistence
        .get_session(&crate::pending_delivery::SmSessionId::new(stream_id))
        .await
        .expect("durable snapshot lookup")
        .is_some());

    assert_eq!(registry.retry_pending_claim_releases(8).await, 0);
    assert!(registry
        .claim_fences
        .read()
        .expect("claim fences after janitor reconciliation")
        .contains_key(stream_id));
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .expect("pending acquisitions")
        .is_empty());
    assert_eq!(registry.pending_claim_release_count(), 0);
}

#[tokio::test]
async fn detach_capacity_rejection_precedes_memory_durable_and_claim_publication() {
    let owner = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let claim_store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let persistence = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::with_capacity(1)
        .with_persistence(persistence.clone())
        .with_claim_store(
            claim_store.clone(),
            crate::ownership::SharedNodeIdentity::new(owner.clone()),
        );
    {
        let mut pending = registry
            .pending_claim_releases
            .write()
            .expect("pending releases");
        pending.insert((
            "existing-cleanup-1".to_string(),
            super::super::persistence::SmClaimFence::new(
                owner.clone(),
                crate::ownership::ClaimEpoch(1),
            ),
        ));
        pending.insert((
            "existing-cleanup-2".to_string(),
            super::super::persistence::SmClaimFence::new(owner, crate::ownership::ClaimEpoch(2)),
        ));
    }
    let stream_id = "capacity-rejected-detach";

    assert!(registry
        .store_session(realistic_test_session(stream_id))
        .await
        .is_err());
    let session_id = crate::pending_delivery::SmSessionId::new(stream_id);
    assert!(persistence
        .get_session(&session_id)
        .await
        .expect("durable snapshot lookup")
        .is_none());
    assert!(registry
        .peek_session(stream_id)
        .await
        .expect("in-memory session lookup")
        .is_none());
    assert!(crate::ownership::ClaimStore::current_claim(
        claim_store.as_ref(),
        &crate::ownership::Entity::new(
            crate::ownership::EntityType::SmSession,
            stream_id.to_string(),
        ),
    )
    .await
    .expect("claim lookup")
    .is_none());
}

#[tokio::test(start_paused = true)]
async fn enable_claim_publication_failure_retains_bounded_acquisition_responsibility() {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(true),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::new()
            .with_claim_store(store.clone(), crate::ownership::SharedNodeIdentity::new(me)),
    );
    *store
        .poison_fence_cache_after_ensure
        .lock()
        .expect("poison injection lock") = Some(EnsureClaimTestAction::PoisonFenceCache(
        std::sync::Arc::downgrade(&registry),
    ));
    let stream_id = "publication-failure";

    assert!(registry.ensure_session_claim(stream_id).await.is_none());
    assert_eq!(registry.pending_claim_release_count(), 0);
    assert!(registry.has_claim_fence_reservation(stream_id));
    assert!(registry
        .pending_claim_acquisitions
        .read()
        .unwrap()
        .iter()
        .any(|(id, owner, disposition)| {
            id == stream_id
                && owner.node_epoch == "incarnation"
                && *disposition == PendingClaimAcquisitionDisposition::ReleaseRejectedEnable
        }));

    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup")
            .is_some()
    );
}

#[tokio::test]
async fn cancelled_enable_compensation_retains_exact_release_responsibility() {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let identity = crate::ownership::SharedNodeIdentity::new(me.clone());
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(true),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::new().with_claim_store(store.clone(), identity.clone()),
    );
    *store
        .poison_fence_cache_after_ensure
        .lock()
        .expect("poison injection lock") = Some(EnsureClaimTestAction::RotateIdentity {
        identity,
        next: crate::ownership::NodeIdentity::new("sm-node", "next-incarnation"),
    });
    let stream_id = "cancelled-enable-compensation";
    let claiming_registry = registry.clone();
    let claiming =
        tokio::spawn(async move { claiming_registry.ensure_session_claim(stream_id).await });

    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.pending_claim_release_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("exact release responsibility should publish before compensating release");
    claiming.abort();
    let cancellation = claiming
        .await
        .err()
        .expect("claim task should be cancelled");
    assert!(cancellation.is_cancelled());

    assert_eq!(registry.pending_claim_release_count(), 1);
    assert!(
        !registry.has_claim_fence_reservation(stream_id),
        "the exact release supersedes the acquisition reservation"
    );
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup")
            .is_some()
    );

    store
        .hang_release
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(registry.retry_pending_claim_releases(8).await, 1);
    assert_eq!(registry.pending_claim_release_count(), 0);
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup after retry")
            .is_none()
    );
}

#[tokio::test]
async fn cancelled_claim_compensation_retains_exact_release_responsibility() {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let identity = crate::ownership::SharedNodeIdentity::new(me);
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(true),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::new().with_claim_store(store.clone(), identity.clone()),
    );
    let stream_id = "cancelled-claim-compensation";
    registry
        .store_session(make_test_session(stream_id))
        .await
        .expect("seed claimed detached session");
    *store
        .poison_fence_cache_after_ensure
        .lock()
        .expect("rotation injection lock") = Some(EnsureClaimTestAction::RotateIdentity {
        identity,
        next: crate::ownership::NodeIdentity::new("sm-node", "next-incarnation"),
    });

    let claiming_registry = registry.clone();
    let claiming =
        tokio::spawn(async move { claiming_registry.claim_session_typed(stream_id).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.pending_claim_release_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal inventory must publish before the hanging release");
    claiming.abort();
    assert!(claiming
        .await
        .expect_err("claim task should be cancelled")
        .is_cancelled());

    assert_eq!(registry.pending_claim_release_count(), 1);
    assert!(!registry
        .sessions
        .read()
        .expect("sessions")
        .contains_key(stream_id));
    assert!(!registry
        .claimed_sessions
        .read()
        .expect("claimed sessions")
        .contains_key(stream_id));
    assert!(!registry
        .claim_fences
        .read()
        .expect("claim fences")
        .contains_key(stream_id));
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup")
            .is_some()
    );

    store
        .hang_release
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(registry.retry_pending_claim_releases(1).await, 1);
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup")
            .is_none()
    );
    assert_eq!(registry.pending_claim_release_count(), 0);
    assert_eq!(registry.claim_fence_capacity_used(), 0);
}

#[test]
fn active_fence_ambiguity_marker_does_not_double_charge_capacity() {
    let registry = InMemorySmSessionRegistry::with_capacity(2);
    let owner = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let fence =
        super::super::persistence::SmClaimFence::new(owner, crate::ownership::ClaimEpoch(1));

    assert!(registry.reserve_claim_fence_capacity("stream-a"));
    assert!(registry.try_record_claim_fence("stream-a", fence));
    assert!(registry.reserve_claim_fence_capacity("stream-a"));
    assert_eq!(registry.claim_fence_capacity_used(), 1);

    assert!(registry.reserve_claim_fence_capacity("stream-b"));
    assert_eq!(registry.claim_fence_capacity_used(), 2);
    assert!(!registry.reserve_claim_fence_capacity("stream-c"));
}

#[test]
fn timed_out_release_generations_cannot_outgrow_claim_capacity() {
    let registry = InMemorySmSessionRegistry::with_capacity(2);
    let owner = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let first = super::super::persistence::SmClaimFence::new(
        owner.clone(),
        crate::ownership::ClaimEpoch(1),
    );
    let second =
        super::super::persistence::SmClaimFence::new(owner, crate::ownership::ClaimEpoch(2));
    let stream_id = "generation-churn";

    assert!(registry.reserve_claim_fence_capacity(stream_id));
    assert!(registry.try_record_claim_fence(stream_id, first.clone()));
    registry
        .pending_claim_releases
        .write()
        .expect("pending releases")
        .insert((stream_id.to_string(), first.clone()));

    assert!(registry.reserve_claim_fence_capacity(stream_id));
    assert!(registry.try_record_claim_fence(stream_id, second.clone()));
    assert_eq!(registry.claim_fence_capacity_used(), 2);
    assert!(registry
        .pending_claim_releases
        .read()
        .expect("pending releases")
        .contains(&(stream_id.to_string(), first)));
    assert_eq!(
        registry
            .claim_fences
            .read()
            .expect("current fences")
            .get(stream_id),
        Some(&second)
    );
    registry
        .pending_claim_releases
        .write()
        .expect("pending releases")
        .insert((stream_id.to_string(), second));
    assert!(!registry.reserve_claim_fence_capacity(stream_id));
    assert_eq!(registry.claim_fence_capacity_used(), 2);
}

#[test]
fn stream_locks_are_fixed_shards_not_per_stream_entries() {
    let registry = InMemorySmSessionRegistry::new();
    let shard_count = registry.stream_locks.len();

    assert!(
        shard_count > 0,
        "registry must have at least one lock shard"
    );

    for index in 0..(shard_count * 4) {
        let _lock = registry
            .stream_lock(&format!("historical-stream-{index}"))
            .expect("stream lock");
    }

    assert_eq!(
        registry.stream_locks.len(),
        shard_count,
        "unique SM stream ids must not grow an unbounded lock map"
    );
}

#[test]
fn detached_session_overflow_blocks_resume_for_older_client_h() {
    let mut session = make_test_session_with_unacked("stream-overflow", Vec::new());
    session.outbound_count = 0;
    session.last_acked = 0;

    for sequence in 1..=(crate::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE as u32 + 1) {
        session
            .record_detached_outbound_at(
                sequence,
                message_stanza_xml_with_id(format!("m{sequence}")),
                Utc::now(),
                SmUnackedStanzaPurpose::Application,
            )
            .unwrap();
    }

    assert_eq!(session.replay_gap_through, Some(1));
    assert!(
        !session.can_resume_from(0),
        "resume must fail when the client still needs an evicted detached stanza"
    );
    assert!(
        session.can_resume_from(1),
        "resume can proceed once the client's h covers the evicted sequence"
    );
}

/// XEP-0198 §4 too-high detection on the detached/resume path must be
/// the same exact mod-2^32 window as the live ack path, measured from
/// `last_acked`: `sequence_gt` is false at exactly distance 2^31, so
/// `h == outbound_count + 0x8000_0000` used to pass as valid and
/// corrupt the restored counters on resume.
#[test]
fn handled_count_exceeds_outbound_is_an_exact_window_from_last_acked() {
    let mut session = make_test_session_with_unacked("stream-window", Vec::new());
    session.outbound_count = 2;
    session.last_acked = 2;

    assert!(
        !session.handled_count_exceeds_outbound(2),
        "h == outbound is a valid full ack"
    );
    assert!(
        session.handled_count_exceeds_outbound(2u32.wrapping_add(0x7fff_ffff)),
        "h just below the half-space boundary is too high"
    );
    assert!(
        session.handled_count_exceeds_outbound(2u32.wrapping_add(0x8000_0000)),
        "h at exactly distance 2^31 is too high (the sequence_gt corner)"
    );
    assert!(
        session.handled_count_exceeds_outbound(2u32.wrapping_add(0x8000_0001)),
        "the regressed half-space measures outside the exact window \
         (the resume path classifies it as a failed resume via \
         can_resume_from running first)"
    );

    // Wrap-awareness: outbound wrapped to 2, client acked u32::MAX - 1.
    session.last_acked = u32::MAX - 1;
    assert!(!session.handled_count_exceeds_outbound(u32::MAX));
    assert!(!session.handled_count_exceeds_outbound(2));
    assert!(session.handled_count_exceeds_outbound(3));
}

#[tokio::test]
async fn xep_0198_scrub_for_tombstone_removes_matching_1on1_message() {
    // XEP-0424 §"prevent further distribution" + XEP-0198 resume
    // safety: when a tombstone is applied, the original
    // `<message id='target'>` must not replay on a recipient's
    // resume. Locks the matcher against false negatives (matching
    // messages must be removed) and false positives (non-matching
    // messages and non-message frames must be preserved). Scoped
    // by the recipient's bare JID so the matcher cannot reach
    // outside the conversation.
    let registry = InMemorySmSessionRegistry::new();
    let session = make_test_session_with_unacked(
            "stream-tomb",
            vec![
                (
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='target' type='chat'><body>secret</body><thread parent='root'>child</thread></message>"
                        .to_string(),
                ),
                (
                    2,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='other' type='chat'><body>safe</body></message>"
                        .to_string(),
                ),
                (3, "<presence/>".to_string()),
                (4, "<iq type='result' id='not-a-message'/>".to_string()),
            ],
        );
    registry.store_session(session).await.unwrap();

    let removed = registry
        .scrub_unacked_for_tombstone(&direct_target(
            "target",
            "alice@example.com",
            "user@example.com",
        ))
        .await
        .unwrap();
    assert_eq!(removed, 1, "exactly one matching message should be removed");

    let again = registry
        .peek_session("stream-tomb")
        .await
        .unwrap()
        .expect("session still present");
    assert_eq!(again.unacked_stanzas.len(), 3);
    assert!(
        !again
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("id='target'")),
        "scrubbed message must not appear in queue"
    );
    assert!(
        again
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("id='other'")),
        "non-matching message must remain"
    );
    assert!(
        again
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("<presence")),
        "presence frame must remain (not a message)"
    );
    assert!(
        again
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("<iq")),
        "iq frame must remain (not a message)"
    );
}

#[tokio::test]
async fn xep_0198_detached_replay_preserves_xep_0201_thread_metadata() {
    use xmpp_parsers::message::{Message, MessageType, Thread};

    let registry = InMemorySmSessionRegistry::new();
    let jid = make_test_jid();
    let session = make_test_session_for_jid("stream-threaded-replay", jid.clone());
    registry.store_session(session).await.unwrap();

    let mut msg = Message::new(Some(jid::Jid::from(jid.clone())));
    msg.from = Some(jid::Jid::from(
        "sender@example.com/web".parse::<FullJid>().expect("jid"),
    ));
    msg.id = Some(xmpp_parsers::message::Id(
        "detached-threaded-message".to_string(),
    ));
    msg.type_ = MessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "threaded".to_string());
    msg.thread = Some(Thread {
        id: "conversation-thread".to_string(),
        parent: None,
    });
    msg.payloads.push(
        minidom::Element::builder("thread", "urn:example:other:0")
            .attr(
                <minidom::rxml::NcName as std::convert::TryFrom<&str>>::try_from("kind")
                    .expect("validated NcName"),
                "extension",
            )
            .append("not-xep-0201")
            .build(),
    );

    assert!(registry
        .record_stanza_for_detached_bound_resource(&jid, &Stanza::Message(msg), Utc::now())
        .await
        .unwrap());
    let stored = registry
        .peek_session("stream-threaded-replay")
        .await
        .unwrap()
        .expect("detached session remains");
    let replay = stored
        .unacked_stanzas
        .last()
        .map(|entry| &entry.stanza_xml)
        .expect("recorded replay stanza");
    let element = replay
        .parse::<minidom::Element>()
        .expect("valid stanza xml");

    assert!(element.children().any(|child| {
        child.name() == "thread"
            && child.ns() == "jabber:client"
            && child.text() == "conversation-thread"
    }));
    assert!(element.children().any(|child| {
        child.name() == "thread"
            && child.ns() == "urn:example:other:0"
            && child.text() == "not-xep-0201"
    }));
}

#[tokio::test]
async fn xep_0198_scrub_for_tombstone_matches_groupchat_stanza_id() {
    // Groupchat retractions key off the room's XEP-0359 stanza-id
    // per the "archive id == wire stanza-id" invariant
    // (`archive_groupchat_message`). The cached reflection
    // preserves the sender's original `message.id` AND carries
    // `<stanza-id by='room' id='canonical'/>`; the retraction
    // request targets `canonical`, not the sender's id. The
    // matcher must therefore check stanza-id children too —
    // surfaced by Copilot review on PR #305.
    let registry = InMemorySmSessionRegistry::new();
    let session = make_test_session_with_unacked(
            "stream-muc",
            vec![(
                1,
                "<message xmlns='jabber:client' from='room@conf.example.com/alice' to='user@example.com/resource' id='sender-wire-id' type='groupchat'><body>moderated</body><stanza-id xmlns='urn:xmpp:sid:0' by='room@conf.example.com' id='canonical-archive-id'/></message>"
                    .to_string(),
            )],
        );
    registry.store_session(session).await.unwrap();

    let removed = registry
        .scrub_unacked_for_tombstone(&groupchat_target(
            "canonical-archive-id",
            "room@conf.example.com",
        ))
        .await
        .unwrap();
    assert_eq!(
        removed, 1,
        "groupchat tombstone keyed by stanza-id must scrub the reflection"
    );
}

#[tokio::test]
async fn xep_0198_scrub_for_tombstone_does_not_cross_conversations() {
    // Two clients independently use `id='msg-1'` in different
    // conversations. Retracting in conversation A must not delete
    // the queued message in conversation B that happens to share
    // the same wire id. Codex P1 review on PR #305.
    let registry = InMemorySmSessionRegistry::new();
    let session = make_test_session_with_unacked(
            "stream-cross",
            vec![
                (
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='msg-1' type='chat'><body>conv-A</body></message>"
                        .to_string(),
                ),
                (
                    2,
                    "<message xmlns='jabber:client' from='carol@elsewhere.com/web' to='user@example.com/resource' id='msg-1' type='chat'><body>conv-B</body></message>"
                        .to_string(),
                ),
            ],
        );
    registry.store_session(session).await.unwrap();

    // Tombstone is scoped to alice@example.com (the sender of
    // conversation A's archive context). The matcher must NOT
    // remove the carol→user message even though it shares the
    // wire id, because alice is neither its `from` nor `to`.
    let removed = registry
        .scrub_unacked_for_tombstone(&direct_target(
            "msg-1",
            "alice@example.com",
            "alice@example.com",
        ))
        .await
        .unwrap();
    assert_eq!(
        removed, 1,
        "only the alice-scoped message should be removed"
    );

    let again = registry
        .peek_session("stream-cross")
        .await
        .unwrap()
        .expect("session still present");
    assert!(
        again
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("conv-B")),
        "conversation B's message must survive — different scope"
    );
}

#[tokio::test]
async fn xep_0198_scrub_for_tombstone_ignores_non_xep0359_stanza_id_namespace() {
    // XEP-0359 §3 scopes `<stanza-id/>` to `urn:xmpp:sid:0`. An
    // unrelated extension element that happens to be named
    // "stanza-id" in a different namespace must NOT trigger a
    // tombstone scrub (Copilot review on PR #305).
    let registry = InMemorySmSessionRegistry::new();
    let session = make_test_session_with_unacked(
            "stream-ns",
            vec![(
                1,
                "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='wire-id' type='chat'><body>safe</body><stanza-id xmlns='urn:example:other:0' id='target'/></message>"
                    .to_string(),
            )],
        );
    registry.store_session(session).await.unwrap();

    let removed = registry
        .scrub_unacked_for_tombstone(&direct_target(
            "target",
            "alice@example.com",
            "user@example.com",
        ))
        .await
        .unwrap();
    assert_eq!(
        removed, 0,
        "stanza-id in non-XEP-0359 namespace must not be matched"
    );
}

#[tokio::test]
async fn xep_0198_scrub_for_tombstone_handles_no_match() {
    let registry = InMemorySmSessionRegistry::new();
    registry
            .store_session(make_test_session_with_unacked(
                "stream-nomatch",
                vec![(
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com' id='other' type='chat'><body>x</body></message>"
                        .to_string(),
                )],
            ))
            .await
            .unwrap();
    let removed = registry
        .scrub_unacked_for_tombstone(&direct_target(
            "not-here",
            "alice@example.com",
            "user@example.com",
        ))
        .await
        .unwrap();
    assert_eq!(removed, 0);
}

#[tokio::test]
async fn test_store_and_take_session() {
    let registry = InMemorySmSessionRegistry::new();

    let session = make_test_session("stream-123");
    registry.store_session(session).await.unwrap();

    assert_eq!(registry.session_count().await, 1);

    // Take the session
    let retrieved = registry.take_session("stream-123").await.unwrap();
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.stream_id, "stream-123");
    assert_eq!(retrieved.outbound_count, 15);

    // Session should be gone now
    assert_eq!(registry.session_count().await, 0);
    let again = registry.take_session("stream-123").await.unwrap();
    assert!(again.is_none());
}

#[tokio::test]
async fn test_store_session_replaces_existing_session_for_same_full_jid() {
    let registry = InMemorySmSessionRegistry::new();
    let mut first = make_test_session("stream-old");
    first.roster_interested = true;
    let mut second = make_test_session("stream-new");
    second.roster_interested = true;

    registry.store_session(first).await.unwrap();
    registry.store_session(second).await.unwrap();

    assert!(registry.take_session("stream-old").await.unwrap().is_none());
    let current = registry
        .take_session("stream-new")
        .await
        .unwrap()
        .expect("newer detached session should remain");
    assert_eq!(current.stream_id, "stream-new");
}

#[tokio::test]
async fn test_peek_session() {
    let registry = InMemorySmSessionRegistry::new();

    let session = make_test_session("stream-456");
    registry.store_session(session).await.unwrap();

    // Peek should not remove
    let peeked = registry.peek_session("stream-456").await.unwrap();
    assert!(peeked.is_some());
    assert_eq!(registry.session_count().await, 1);

    // Peek again
    let peeked2 = registry.peek_session("stream-456").await.unwrap();
    assert!(peeked2.is_some());
}

#[tokio::test]
async fn test_claimed_session_remains_writable_for_handoff_fanout() {
    let registry = InMemorySmSessionRegistry::new();

    let mut session = make_test_session("stream-claimed");
    session.roster_interested = true;
    let jid = session.jid.clone();
    registry.store_session(session).await.unwrap();

    let claimed = registry
        .claim_session("stream-claimed")
        .await
        .unwrap()
        .expect("claim");
    assert_eq!(claimed.stream_id, "stream-claimed");
    assert_eq!(
        registry.session_count().await,
        0,
        "claimed sessions must move out of the normal detached map"
    );

    assert!(
        registry
            .record_stanza_for_detached_resource(
                &jid,
                &{
                    let mut presence =
                        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
                    presence.statuses.insert(
                        xmpp_parsers::message::Lang(String::new()),
                        "during-claim".to_string(),
                    );
                    Stanza::Presence(presence)
                },
                Utc::now(),
            )
            .await
            .unwrap(),
        "fanout during resume handoff must write to the claimed session"
    );

    let completed = registry
        .complete_claim("stream-claimed")
        .await
        .unwrap()
        .expect("completed claim");
    match completed {
        SmClaimCompletion::Resumed(completed) => {
            assert!(
                completed
                    .unacked_stanzas
                    .iter()
                    .any(|entry| entry.stanza_xml.contains("during-claim")),
                "completed claim must include fanout recorded during handoff"
            );
        }
        SmClaimCompletion::Expired(_) => panic!("claim should still be resumable"),
        SmClaimCompletion::ReplayWindowTruncated(_) => {
            panic!("claim should still have a complete replay window")
        }
        SmClaimCompletion::HandledCountTooHigh(_) => {
            panic!("claim should not complete with a too-high client count")
        }
    }
}

#[tokio::test]
async fn blocklist_interested_detached_resources_include_claimed_sessions_and_record_pushes() {
    let registry = InMemorySmSessionRegistry::new();

    let mut stored = make_test_session_for_jid(
        "stream-blocklist-stored",
        "user@example.com/web".parse().unwrap(),
    );
    stored.blocklist_interested = true;
    let mut claimed = make_test_session_for_jid(
        "stream-blocklist-claimed",
        "user@example.com/phone".parse().unwrap(),
    );
    claimed.blocklist_interested = true;
    let claimed_jid = claimed.jid.clone();

    registry.store_session(stored).await.unwrap();
    registry.store_session(claimed).await.unwrap();
    registry
        .claim_session("stream-blocklist-claimed")
        .await
        .unwrap()
        .expect("claim");

    let bare: jid::BareJid = "user@example.com".parse().unwrap();
    let resources = registry
        .blocklist_interested_detached_resources_for_user(&bare)
        .await
        .unwrap();
    assert_eq!(resources.len(), 2);
    assert!(resources.contains(&"user@example.com/web".parse().unwrap()));
    assert!(resources.contains(&claimed_jid));

    let mut message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(claimed_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id("block-push-test".to_string()));
    assert!(
        registry
            .record_stanza_for_detached_blocklist_resource(
                &claimed_jid,
                &Stanza::Message(message),
                Utc::now(),
            )
            .await
            .unwrap(),
        "blocklist push should record to a claimed blocklist-interested session"
    );

    let completed = registry
        .complete_claim("stream-blocklist-claimed")
        .await
        .unwrap()
        .expect("completed claim");
    match completed {
        SmClaimCompletion::Resumed(completed) => assert!(
            completed
                .unacked_stanzas
                .iter()
                .any(|entry| entry.stanza_xml.contains("block-push-test")),
            "completed claim must include blocklist push recorded during handoff"
        ),
        SmClaimCompletion::Expired(_) => panic!("claim should still be resumable"),
        SmClaimCompletion::ReplayWindowTruncated(_) => {
            panic!("claim should still have a complete replay window")
        }
        SmClaimCompletion::HandledCountTooHigh(_) => {
            panic!("claim should not complete with a too-high client count")
        }
    }
}

#[tokio::test]
async fn complete_claim_releases_when_handoff_creates_replay_gap() {
    let registry = InMemorySmSessionRegistry::new();
    let mut session = make_test_session_with_unacked("stream-handoff-gap", Vec::new());
    session.outbound_count = 0;
    session.last_acked = 0;

    registry
        .store_session(session)
        .await
        .expect("store session");
    registry
        .claim_session("stream-handoff-gap")
        .await
        .expect("claim")
        .expect("session exists");

    for sequence in 1..=(crate::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE as u32 + 1) {
        registry
            .record_outbound_for_detached_stream_at(
                "stream-handoff-gap",
                sequence,
                message_stanza_xml_with_id(format!("m{sequence}")),
                Utc::now(),
            )
            .await
            .expect("record detached outbound");
    }

    let completed = registry
        .complete_claim_if_resumable("stream-handoff-gap", 0)
        .await
        .expect("complete checked claim")
        .expect("claim still exists");
    let SmClaimCompletion::ReplayWindowTruncated(truncated) = completed else {
        panic!("late replay gap must fail resume completion")
    };
    assert_eq!(truncated.replay_gap_through, Some(1));

    let restored = registry
        .peek_session("stream-handoff-gap")
        .await
        .expect("peek restored session")
        .expect("truncated claim is restored to detached pool");
    assert_eq!(restored.replay_gap_through, Some(1));
    assert!(
        !restored.can_resume_from(0),
        "restored session must continue rejecting the stale h value"
    );
}

#[tokio::test]
async fn complete_claim_releases_when_client_handled_count_is_too_high() {
    // The HandledCountTooHigh terminal path ends the claim (the caller
    // closes the connection on it), so it must release the ClaimStore
    // entry — the origin/main merge dropped this side effect when it
    // removed the naive pre-#1099 resume check it was co-located with.
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let me = crate::ownership::NodeIdentity::local();
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(crate::ownership::NodeIdentity::local()),
    );
    let mut session = make_test_session_with_unacked("stream-h-too-high", Vec::new());
    session.outbound_count = 2;
    session.last_acked = 0;

    registry
        .store_session(session)
        .await
        .expect("store session");
    registry
        .claim_session("stream-h-too-high")
        .await
        .expect("claim")
        .expect("session exists");

    let completed = registry
        .complete_claim_if_resumable("stream-h-too-high", 3)
        .await
        .expect("complete checked claim")
        .expect("claim still exists");
    let SmClaimCompletion::HandledCountTooHigh(restored) = completed else {
        panic!("client h higher than send-count must fail resume completion")
    };
    assert_eq!(restored.outbound_count, 2);

    let restored = registry
        .peek_session("stream-h-too-high")
        .await
        .expect("peek restored session")
        .expect("invalid claim is restored to detached pool");
    assert_eq!(restored.outbound_count, 2);

    // The terminal HandledCountTooHigh completion released the claim: the
    // entity is claimable again (a leak would leave it AlreadyClaimed).
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "stream-h-too-high".to_string(),
    );
    store
        .acquire(&entity, &me)
        .await
        .expect("claim released on HandledCountTooHigh — entity re-acquirable");
}

#[tokio::test]
async fn test_session_not_found() {
    let registry = InMemorySmSessionRegistry::new();

    let result = registry.take_session("nonexistent").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_session_expired() {
    let registry = InMemorySmSessionRegistry::new();

    // Create an already-expired session
    let mut session = make_test_session("stream-expired");
    session.max_resume_time = Some(0); // 0 seconds means expired immediately

    registry.store_session(session).await.unwrap();

    // Wait a tiny bit to ensure expiration
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Should return None because expired
    let result = registry.take_session("stream-expired").await.unwrap();
    assert!(result.is_none());
    assert_eq!(registry.session_count().await, 0);
}

#[tokio::test]
async fn test_cleanup_expired() {
    let registry = InMemorySmSessionRegistry::new();

    // Store some sessions
    let mut expired = make_test_session("stream-exp1");
    expired.max_resume_time = Some(0);
    registry.store_session(expired).await.unwrap();

    let valid =
        make_test_session_for_jid("stream-valid", "user@example.com/valid".parse().unwrap());
    registry.store_session(valid).await.unwrap();

    // Wait for expiration
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Cleanup
    let removed = registry.cleanup_expired().await.unwrap();
    assert_eq!(removed, 1);
    assert_eq!(registry.session_count().await, 1);

    // Valid session should still be there
    let result = registry.take_session("stream-valid").await.unwrap();
    assert!(result.is_some());
}

#[tokio::test]
async fn test_capacity_limit() {
    let registry = InMemorySmSessionRegistry::with_capacity(3);

    // Store 3 sessions
    for i in 0..3 {
        let session = make_test_session_for_jid(
            &format!("stream-{}", i),
            format!("user@example.com/resource-{i}").parse().unwrap(),
        );
        registry.store_session(session).await.unwrap();
    }

    assert_eq!(registry.session_count().await, 3);

    // Store a 4th - should evict oldest
    let session = make_test_session_for_jid(
        "stream-new",
        "user@example.com/resource-new".parse().unwrap(),
    );
    registry.store_session(session).await.unwrap();

    assert_eq!(registry.session_count().await, 3);

    // stream-0 should be gone (oldest)
    let result = registry.take_session("stream-0").await.unwrap();
    assert!(result.is_none());

    // stream-new should be there
    let result = registry.take_session("stream-new").await.unwrap();
    assert!(result.is_some());
}

#[test]
fn test_stanzas_to_resend_count() {
    let session = make_test_session("test");

    // Client says h=12, we have 13, 14, 15 - all 3 need resending
    assert_eq!(session.stanzas_to_resend_count(12), 3);

    // Client says h=14, we have 13, 14, 15 - only 15 needs resending
    assert_eq!(session.stanzas_to_resend_count(14), 1);

    // Client says h=15, we have 13, 14, 15 - none need resending
    assert_eq!(session.stanzas_to_resend_count(15), 0);
}

#[test]
fn test_remaining_time() {
    let session = make_test_session("test");

    let remaining = session.remaining_time();
    assert!(remaining.as_secs() <= 300);
    assert!(remaining.as_secs() >= 299); // Should be close to 300
}

// --- SmPersistenceStorage integration (slice (d) phase 3) -------

use super::super::persistence::SmPersistenceStorage as _;

fn realistic_message_stanza(body: &str) -> String {
    // Build a valid XMPP message via the typed builder so the
    // persistence path can parse it back to a typed Stanza on
    // store_session. The fmt-pinned indentation is what the
    // serializer emits when rebuilt via Element::from(message).
    let mut m = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    let element: xmpp_parsers::minidom::Element = m.into();
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize message");
    String::from_utf8(buf).expect("utf8")
}

fn realistic_test_session(stream_id: &str) -> DetachedSession {
    realistic_test_session_for_jid(stream_id, make_test_jid())
}

fn realistic_test_session_for_jid(stream_id: &str, jid: FullJid) -> DetachedSession {
    DetachedSession {
        stream_id: stream_id.to_string(),
        user_id: "user@example.com".to_string(),
        jid,
        inbound_count: 4,
        outbound_count: 7,
        last_acked: 5,
        replay_gap_through: None,
        unacked_stanzas: vec![
            DetachedUnackedStanza {
                sequence: 6,
                stanza_xml: realistic_message_stanza("first"),
                original_receipt_at: Utc::now(),
                purpose: SmUnackedStanzaPurpose::Application,
            },
            DetachedUnackedStanza {
                sequence: 7,
                stanza_xml: realistic_message_stanza("second"),
                original_receipt_at: Utc::now(),
                purpose: SmUnackedStanzaPurpose::Application,
            },
        ],
        max_resume_time: Some(120),
        detached_at: Instant::now(),
        carbons_enabled: true,
        roster_interested: true,
        blocklist_interested: false,
        presence_available: true,
        presence_show: Some(Show::Chat),
        presence_status: Some("online".to_string()),
        presence_priority: 3,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

#[tokio::test]
async fn store_session_mirrors_to_persistence_when_attached() {
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let session = realistic_test_session("stream-1");
    registry.store_session(session.clone()).await.unwrap();

    let stream_id = crate::pending_delivery::SmSessionId::new("stream-1");
    let persisted = storage.get_session(&stream_id).await.unwrap().unwrap();
    assert_eq!(persisted.user_id, session.user_id);
    assert_eq!(persisted.jid, session.jid);
    assert_eq!(persisted.inbound_count, session.inbound_count);
    assert_eq!(persisted.outbound_count, session.outbound_count);
    assert_eq!(persisted.last_acked, session.last_acked);
    assert_eq!(persisted.carbons_enabled, session.carbons_enabled);
    let unacked = storage.list_unacked(&stream_id).await.unwrap();
    assert_eq!(unacked.len(), 2);
    let seqs: Vec<u32> = unacked.iter().map(|u| u.sequence).collect();
    assert_eq!(seqs, vec![6, 7]);
}

#[tokio::test]
async fn take_session_deletes_from_persistence() {
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    registry
        .store_session(realistic_test_session("stream-1"))
        .await
        .unwrap();
    // Resume — should drain durable storage.
    let _ = registry.take_session("stream-1").await.unwrap();
    let stream_id = crate::pending_delivery::SmSessionId::new("stream-1");
    assert!(storage.get_session(&stream_id).await.unwrap().is_none());
    assert!(storage.list_unacked(&stream_id).await.unwrap().is_empty());
}

#[tokio::test]
async fn restore_from_persistence_rebuilds_in_memory_view() {
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    // Pre-populate storage as if a previous server lifecycle had
    // detached two sessions for distinct users. Using distinct
    // JIDs is important: store_session evicts any prior detached
    // session with the same JID (RFC-aligned: a fresh bind for
    // a JID supersedes any older detached stream for that JID),
    // and the durable mirror also deletes the evicted row, so
    // two sessions with the same JID would resolve to one.
    {
        let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
        registry
            .store_session(realistic_test_session_for_jid(
                "stream-1",
                "alice@example.com/web".parse().unwrap(),
            ))
            .await
            .unwrap();
        registry
            .store_session(realistic_test_session_for_jid(
                "stream-2",
                "bob@example.com/laptop".parse().unwrap(),
            ))
            .await
            .unwrap();
    }
    // Simulate restart: brand-new registry, only persistence
    // attached. The in-memory view starts empty.
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    assert_eq!(registry.session_count().await, 0);

    let hydrated = registry.restore_from_persistence().await.unwrap();
    assert_eq!(hydrated, 2);
    assert_eq!(registry.session_count().await, 2);

    // Both sessions resumable post-restart.
    let resumed = registry.take_session("stream-1").await.unwrap();
    assert!(resumed.is_some());
    let resumed = resumed.unwrap();
    assert_eq!(resumed.unacked_stanzas.len(), 2);
    assert!(resumed.carbons_enabled);
    assert_eq!(resumed.presence_priority, 3);
}

#[tokio::test]
async fn restore_is_noop_when_no_persistence_attached() {
    let registry = InMemorySmSessionRegistry::new();
    assert_eq!(registry.restore_from_persistence().await.unwrap(), 0);
}

#[tokio::test]
async fn complete_claim_deletes_durable_session_on_resume() {
    // The real resume path is claim_session -> complete_claim,
    // not take_session. Without durable cleanup at the
    // complete_claim commitment point, a successful resume
    // would leave rows in storage that restart_from_persistence
    // would resurrect. (Codex P1 + Copilot review on PR #344.)
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    registry
        .store_session(realistic_test_session("stream-1"))
        .await
        .unwrap();
    let stream_id = crate::pending_delivery::SmSessionId::new("stream-1");
    assert!(storage.get_session(&stream_id).await.unwrap().is_some());

    let _claimed = registry.claim_session("stream-1").await.unwrap();
    let outcome = registry.complete_claim("stream-1").await.unwrap();
    assert!(matches!(outcome, Some(SmClaimCompletion::Resumed(_))));

    assert!(storage.get_session(&stream_id).await.unwrap().is_none());
    assert!(storage.list_unacked(&stream_id).await.unwrap().is_empty());
}

#[tokio::test]
async fn authoritative_completion_does_not_reenter_identity_gate_behind_rotation() {
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let identity = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let shared_identity = crate::ownership::SharedNodeIdentity::new(identity.clone());
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(storage)
        .with_claim_store(
            std::sync::Arc::new(crate::ownership::InProcessClaimStore::new()),
            shared_identity.clone(),
        );
    registry
        .store_session(realistic_test_session("stream-authoritative-complete"))
        .await
        .unwrap();
    registry
        .claim_session("stream-authoritative-complete")
        .await
        .unwrap();

    let operation = registry
        .lock_session_operation("stream-authoritative-complete")
        .await
        .unwrap();
    let authority = shared_identity.guard_if_current(&identity).await.unwrap();
    let rotating_identity = shared_identity.clone();
    let rotation = tokio::spawn(async move {
        rotating_identity
            .rotate(crate::ownership::NodeIdentity::new(
                "node-a",
                "incarnation-b",
            ))
            .await;
    });
    tokio::task::yield_now().await;

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        registry.complete_claim_with_authority(operation, &authority),
    )
    .await
    .expect("completion must not wait on a nested identity read")
    .unwrap();
    assert!(matches!(outcome, Some(SmClaimCompletion::Resumed(_))));
    drop(authority);
    tokio::time::timeout(std::time::Duration::from_secs(1), rotation)
        .await
        .expect("rotation proceeds after authoritative completion")
        .unwrap();
}

#[tokio::test]
async fn authoritative_completion_rejects_a_foreign_equal_identity_guard() {
    let identity = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let registry_identity = crate::ownership::SharedNodeIdentity::new(identity.clone());
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        std::sync::Arc::new(crate::ownership::InProcessClaimStore::new()),
        registry_identity,
    );
    registry
        .store_session(realistic_test_session("stream-foreign-authority"))
        .await
        .unwrap();
    registry
        .claim_session("stream-foreign-authority")
        .await
        .unwrap();
    let operation = registry
        .lock_session_operation("stream-foreign-authority")
        .await
        .unwrap();
    let foreign_identity = crate::ownership::SharedNodeIdentity::new(identity.clone());
    let foreign_authority = foreign_identity.guard_if_current(&identity).await.unwrap();

    assert!(registry
        .complete_claim_with_authority(operation, &foreign_authority)
        .await
        .is_err());
    assert!(matches!(
        registry
            .complete_claim("stream-foreign-authority")
            .await
            .unwrap(),
        Some(SmClaimCompletion::Resumed(_))
    ));
}

#[tokio::test]
async fn authoritative_release_reinserts_before_queued_rotation() {
    let identity = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let shared_identity = crate::ownership::SharedNodeIdentity::new(identity.clone());
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        std::sync::Arc::new(crate::ownership::InProcessClaimStore::new()),
        shared_identity.clone(),
    );
    registry
        .store_session(realistic_test_session("stream-authoritative-release"))
        .await
        .unwrap();
    registry
        .claim_session("stream-authoritative-release")
        .await
        .unwrap();
    let operation = registry
        .lock_session_operation("stream-authoritative-release")
        .await
        .unwrap();
    let authority = shared_identity.guard_if_current(&identity).await.unwrap();
    let rotating_identity = shared_identity.clone();
    let rotation = tokio::spawn(async move {
        rotating_identity
            .rotate(crate::ownership::NodeIdentity::new(
                "node-a",
                "incarnation-b",
            ))
            .await;
    });
    tokio::task::yield_now().await;

    registry
        .release_claim_with_authority(operation, &authority)
        .await
        .unwrap();
    assert_eq!(registry.session_count().await, 1);
    assert_eq!(shared_identity.current(), identity);
    drop(authority);
    tokio::time::timeout(std::time::Duration::from_secs(1), rotation)
        .await
        .expect("rotation proceeds after authoritative release")
        .unwrap();
}

#[tokio::test]
async fn authoritative_lifecycle_rejects_a_fresh_guard_for_a_stale_fence() {
    let old_identity = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let shared_identity = crate::ownership::SharedNodeIdentity::new(old_identity);
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        std::sync::Arc::new(crate::ownership::InProcessClaimStore::new()),
        shared_identity.clone(),
    );
    for (stream_id, jid) in [
        ("stream-stale-release", "alice@example.com/release"),
        ("stream-stale-complete", "alice@example.com/complete"),
    ] {
        registry
            .store_session(realistic_test_session_for_jid(
                stream_id,
                jid.parse().unwrap(),
            ))
            .await
            .unwrap();
        registry.claim_session(stream_id).await.unwrap();
    }
    let new_identity = crate::ownership::NodeIdentity::new("node-a", "incarnation-b");
    shared_identity.rotate(new_identity.clone()).await;
    let fresh_authority = shared_identity
        .guard_if_current(&new_identity)
        .await
        .unwrap();

    let release_operation = registry
        .lock_session_operation("stream-stale-release")
        .await
        .unwrap();
    assert!(registry
        .release_claim_with_authority(release_operation, &fresh_authority)
        .await
        .is_err());
    let complete_operation = registry
        .lock_session_operation("stream-stale-complete")
        .await
        .unwrap();
    assert!(registry
        .complete_claim_with_authority(complete_operation, &fresh_authority)
        .await
        .is_err());
    drop(fresh_authority);

    assert!(registry
        .complete_claim("stream-stale-release")
        .await
        .unwrap()
        .is_some());
    assert!(registry
        .complete_claim("stream-stale-complete")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn epoch_failure_does_not_reinsert_after_backend_ownership_moves() {
    use crate::ownership::ClaimStore as _;

    let local = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let remote = crate::ownership::NodeIdentity::new("node-b", "incarnation-b");
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(local.clone()),
    );
    let stream_id = "epoch-failure-ownership-moved";
    registry
        .store_session(realistic_test_session(stream_id))
        .await
        .unwrap();
    registry.claim_session(stream_id).await.unwrap();
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    let local_claim = store
        .current_claim(&entity)
        .await
        .unwrap()
        .expect("local claim");
    store
        .release(&entity, &local, local_claim.claim_epoch)
        .await
        .unwrap();
    store.acquire(&entity, &remote).await.unwrap();

    registry
        .reconcile_claim_after_epoch_lookup_failure(stream_id)
        .await
        .unwrap();

    assert!(registry.live_session_ids().unwrap().is_empty());
    assert_eq!(
        store
            .current_claim(&entity)
            .await
            .unwrap()
            .expect("remote claim survives exact old-fence cleanup")
            .owner,
        remote
    );
    assert!(registry
        .pending_epoch_failure_reconciliations
        .read()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn cancelled_epoch_failure_reconciliation_remains_janitor_owned() {
    let identity = crate::ownership::NodeIdentity::new("node-a", "incarnation-a");
    let registry = std::sync::Arc::new(InMemorySmSessionRegistry::new().with_claim_store(
        std::sync::Arc::new(crate::ownership::InProcessClaimStore::new()),
        crate::ownership::SharedNodeIdentity::new(identity),
    ));
    let stream_id = "cancelled-epoch-failure-reconciliation";
    registry
        .store_session(realistic_test_session(stream_id))
        .await
        .unwrap();
    registry.claim_session(stream_id).await.unwrap();
    let blocker = registry.lock_session_operation(stream_id).await.unwrap();
    let reconciling_registry = registry.clone();
    let reconciliation = tokio::spawn(async move {
        reconciling_registry
            .reconcile_claim_after_epoch_lookup_failure(stream_id)
            .await
    });
    tokio::task::yield_now().await;
    reconciliation.abort();
    assert!(reconciliation.await.unwrap_err().is_cancelled());

    assert!(registry
        .pending_epoch_failure_reconciliations
        .read()
        .unwrap()
        .contains(stream_id));
    assert_eq!(registry.session_count().await, 0);
    drop(blocker);

    registry.retry_pending_claim_releases(1).await;
    assert!(registry
        .pending_epoch_failure_reconciliations
        .read()
        .unwrap()
        .is_empty());
    assert_eq!(registry.session_count().await, 1);
}

#[tokio::test]
async fn store_session_returns_jid_collision_eviction_and_preserves_rows_until_confirmed() {
    // Two store_session calls for the same JID with different
    // stream_ids: the second supersedes the first per RFC resume
    // semantics. Issue #1097: the displaced session's unacked queue
    // must NOT be silently dropped — it is returned to the caller for
    // XEP-0198 §5 promotion, and its durable rows survive until the
    // caller confirms via `confirm_drained`.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let displaced = registry
        .store_session(realistic_test_session_for_jid(
            "stream-old",
            "alice@example.com/web".parse().unwrap(),
        ))
        .await
        .unwrap();
    assert!(displaced.is_empty(), "first store displaces nothing");
    let displaced = registry
        .store_session(realistic_test_session_for_jid(
            "stream-new",
            "alice@example.com/web".parse().unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(displaced.len(), 1);
    assert_eq!(displaced[0].stream_id, "stream-old");
    assert_eq!(
        displaced[0].unacked_stanzas.len(),
        2,
        "displaced session carries its full unacked queue for promotion"
    );
    let old_id = crate::pending_delivery::SmSessionId::new("stream-old");
    let new_id = crate::pending_delivery::SmSessionId::new("stream-new");
    assert!(
        registry.peek_session("stream-old").await.unwrap().is_none(),
        "displaced session must leave the in-memory pool"
    );
    assert!(
        storage.get_session(&old_id).await.unwrap().is_some(),
        "displaced rows survive until promotion is confirmed"
    );
    assert!(
        storage.get_session(&new_id).await.unwrap().is_some(),
        "stream-new should remain"
    );

    registry.confirm_drained("stream-old").await;
    assert!(
        storage.get_session(&old_id).await.unwrap().is_none(),
        "confirm_drained erases the displaced session's durable rows"
    );
}

#[tokio::test]
async fn store_session_does_not_clobber_in_flight_resume_claim_for_same_stream() {
    // Issue #1139: race between a new connection resuming a stream and
    // the OLD connection's detach. The new connection claims the
    // session (`claim_session` moves it sessions → claimed_sessions),
    // then the old connection's detach calls `store_session` for the
    // SAME stream id + JID. The claimed entry must survive so the
    // in-flight XEP-0198 §5 resume completes with <resumed/> instead
    // of failing <failed item-not-found/>.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let jid: FullJid = "alice@example.com/web".parse().unwrap();
    registry
        .store_session(realistic_test_session_for_jid("stream-race", jid.clone()))
        .await
        .unwrap();

    // New connection claims the session for resume.
    let claimed = registry.claim_session("stream-race").await.unwrap();
    assert!(claimed.is_some(), "session must be claimable");

    // Old connection detaches late, storing the same stream id + JID.
    let displaced = registry
        .store_session(realistic_test_session_for_jid("stream-race", jid.clone()))
        .await
        .unwrap();
    assert!(
        displaced.is_empty(),
        "a late store for a stream mid-handoff must not displace the claim"
    );

    // The claiming connection still owns the handoff: complete_claim
    // must succeed (resume does NOT fail item-not-found).
    let outcome = registry.complete_claim("stream-race").await.unwrap();
    assert!(
        matches!(outcome, Some(SmClaimCompletion::Resumed(_))),
        "in-flight resume claim was clobbered by store_session: {outcome:?}"
    );

    // No stale detached duplicate may shadow the completed resume.
    assert!(
        registry
            .peek_session("stream-race")
            .await
            .unwrap()
            .is_none(),
        "late store must not leave a stale detached duplicate behind"
    );
    let stream_id = crate::pending_delivery::SmSessionId::new("stream-race");
    assert!(
        storage.get_session(&stream_id).await.unwrap().is_none(),
        "late store must not resurrect durable rows the completed claim erased"
    );
}

#[tokio::test]
async fn store_session_still_evicts_claimed_entries_for_same_jid_other_stream() {
    // Companion to the #1139 fix: a claimed entry for the SAME JID but
    // a DIFFERENT stream id is a superseded identity and must still be
    // evicted, returned via the displaced bookkeeping for XEP-0198 §5
    // promotion, with its durable rows preserved until confirm_drained.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let jid: FullJid = "alice@example.com/web".parse().unwrap();
    registry
        .store_session(realistic_test_session_for_jid("stream-old", jid.clone()))
        .await
        .unwrap();
    let claimed = registry.claim_session("stream-old").await.unwrap();
    assert!(claimed.is_some());

    let displaced = registry
        .store_session(realistic_test_session_for_jid("stream-new", jid.clone()))
        .await
        .unwrap();
    assert_eq!(displaced.len(), 1);
    assert_eq!(displaced[0].stream_id, "stream-old");

    // The claim is gone: the superseded stream can no longer resume.
    let outcome = registry.complete_claim("stream-old").await.unwrap();
    assert!(outcome.is_none(), "superseded claim must be evicted");

    // Durable rows survive until the caller confirms promotion.
    let old_id = crate::pending_delivery::SmSessionId::new("stream-old");
    assert!(storage.get_session(&old_id).await.unwrap().is_some());
    registry.confirm_drained("stream-old").await;
    assert!(storage.get_session(&old_id).await.unwrap().is_none());
}

#[tokio::test]
async fn store_session_returns_capacity_evicted_session_and_preserves_rows() {
    // Issue #1097: max_sessions overflow eviction must not silently
    // drop the oldest session's unacked queue. The evicted session is
    // returned so the waddle-server caller can run the XEP-0198 §5
    // promote → confirm chain; durable rows survive until
    // `confirm_drained`.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::with_capacity(1).with_persistence(storage.clone());
    let mut oldest =
        realistic_test_session_for_jid("stream-oldest", "alice@example.com/web".parse().unwrap());
    oldest.detached_at = Instant::now() - Duration::from_secs(30);
    registry.store_session(oldest).await.unwrap();

    let evicted = registry
        .store_session(realistic_test_session_for_jid(
            "stream-newer",
            "bob@example.com/web".parse().unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].stream_id, "stream-oldest");
    assert_eq!(
        evicted[0].unacked_stanzas.len(),
        2,
        "evicted session carries its full unacked queue for promotion"
    );
    assert!(registry
        .peek_session("stream-oldest")
        .await
        .unwrap()
        .is_none());
    assert!(registry
        .peek_session("stream-newer")
        .await
        .unwrap()
        .is_some());

    let oldest_id = crate::pending_delivery::SmSessionId::new("stream-oldest");
    assert!(
        storage.get_session(&oldest_id).await.unwrap().is_some(),
        "evicted rows survive until promotion is confirmed"
    );
    assert_eq!(
        storage.list_unacked(&oldest_id).await.unwrap().len(),
        2,
        "evicted unacked rows survive until promotion is confirmed"
    );

    registry.confirm_drained("stream-oldest").await;
    assert!(storage.get_session(&oldest_id).await.unwrap().is_none());
    assert!(storage.list_unacked(&oldest_id).await.unwrap().is_empty());
}

fn realistic_dm_stanza_xml(from: &str, to: &str, id: &str, body: &str) -> String {
    let mut m = xmpp_parsers::message::Message::new(Some(to.parse::<jid::Jid>().expect("jid")));
    m.from = Some(from.parse::<jid::Jid>().expect("jid"));
    m.id = Some(xmpp_parsers::message::Id(id.to_string()));
    m.type_ = xmpp_parsers::message::MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    let element: xmpp_parsers::minidom::Element = m.into();
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize message");
    String::from_utf8(buf).expect("utf8")
}

#[tokio::test]
async fn scrub_for_tombstone_deletes_durable_rows_so_restart_cannot_replay() {
    // Issue #1145: the tombstone scrub previously mutated memory only.
    // A restart rehydrated the retracted stanza from sm_unacked and
    // <resumed/> replayed it on the wire, defeating XEP-0424/0425
    // retraction. The scrub must durably delete the matched
    // (stream_id, sequence) rows too.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let session = make_test_session_with_unacked(
        "stream-durable-tomb",
        vec![
            (
                6,
                realistic_dm_stanza_xml(
                    "alice@example.com/web",
                    "user@example.com/resource",
                    "retracted-id",
                    "secret",
                ),
            ),
            (
                7,
                realistic_dm_stanza_xml(
                    "alice@example.com/web",
                    "user@example.com/resource",
                    "kept-id",
                    "safe",
                ),
            ),
        ],
    );
    registry.store_session(session).await.unwrap();

    let removed = registry
        .scrub_unacked_for_tombstone(&direct_target(
            "retracted-id",
            "alice@example.com",
            "user@example.com",
        ))
        .await
        .unwrap();
    assert_eq!(removed, 1);

    // Durable rows for the scrubbed stanza are gone.
    let stream_id = crate::pending_delivery::SmSessionId::new("stream-durable-tomb");
    let rows = storage.list_unacked(&stream_id).await.unwrap();
    let seqs: Vec<u32> = rows.iter().map(|r| r.sequence).collect();
    assert_eq!(
        seqs,
        vec![7],
        "only the non-retracted stanza's row may remain durably"
    );

    // Restart simulation: a fresh registry over the same storage must
    // not resurrect the retracted stanza into the replay queue.
    let restarted = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    restarted.restore_from_persistence().await.unwrap();
    let hydrated = restarted
        .peek_session("stream-durable-tomb")
        .await
        .unwrap()
        .expect("session survives restart");
    assert_eq!(hydrated.unacked_stanzas.len(), 1);
    assert!(
        !hydrated
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("secret")),
        "retracted stanza must not be replayable after restart"
    );
    assert!(hydrated
        .unacked_stanzas
        .iter()
        .any(|entry| entry.stanza_xml.contains("safe")));
}

#[tokio::test]
async fn invalidate_sessions_for_jid_preserves_rows_until_confirmed() {
    // Issue #1097 (fresh-bind invalidation): superseded detached
    // sessions removed on a fresh bind carry unacked queues that the
    // caller must promote (XEP-0198 §5). Durable rows survive until
    // the caller confirms successful promotion via `confirm_drained`
    // so a crash mid-promotion retries after restart.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let jid: FullJid = "alice@example.com/web".parse().unwrap();
    registry
        .store_session(realistic_test_session_for_jid("stream-stale", jid.clone()))
        .await
        .unwrap();

    let removed = registry.invalidate_sessions_for_jid(&jid).await.unwrap();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].stream_id, "stream-stale");
    assert_eq!(
        removed[0].unacked_stanzas.len(),
        2,
        "invalidated session carries its full unacked queue for promotion"
    );
    assert!(registry
        .peek_session("stream-stale")
        .await
        .unwrap()
        .is_none());

    let stale_id = crate::pending_delivery::SmSessionId::new("stream-stale");
    assert!(
        storage.get_session(&stale_id).await.unwrap().is_some(),
        "invalidated rows survive until promotion is confirmed"
    );

    registry.confirm_drained("stream-stale").await;
    assert!(storage.get_session(&stale_id).await.unwrap().is_none());
    assert!(storage.list_unacked(&stale_id).await.unwrap().is_empty());
}

#[tokio::test]
async fn restore_hydrates_expired_sessions_for_promotion_and_preserves_rows() {
    // Issue #1098: sessions whose resume window closed during the
    // server's downtime must NOT be durably deleted at restore time —
    // that silently discards their unacked queues. Instead they are
    // hydrated (expired) so the SM-expiry janitor's next
    // `drain_expired` pass runs the XEP-0198 §5 promote → confirm
    // chain; durable rows survive until `confirm_drained`.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());

    // Manually insert an already-expired session by writing
    // directly to storage with a detached_at + duration in the
    // past.
    let now = chrono::Utc::now();
    let expired = super::super::persistence::PersistedSession {
        stream_id: crate::pending_delivery::SmSessionId::new("stream-expired"),
        user_id: "alice".to_string(),
        jid: "alice@example.com/web".parse().unwrap(),
        inbound_count: 0,
        outbound_count: 1,
        last_acked: 0,
        replay_gap_through: None,
        max_resume_time: Some(60),
        detached_at: now - chrono::Duration::seconds(120),
        max_resume_duration: Duration::from_secs(60),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
    };
    storage.upsert_session(expired).await.unwrap();
    let mut queued =
        xmpp_parsers::message::Message::new(Some("alice@example.com".parse::<jid::Jid>().unwrap()));
    queued
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "missed".to_string());
    storage
        .append_unacked(super::super::persistence::PersistedUnackedStanza {
            stream_id: crate::pending_delivery::SmSessionId::new("stream-expired"),
            sequence: 1,
            stanza: Box::new(Stanza::Message(queued)),
            original_receipt_at: now - chrono::Duration::seconds(130),
            purpose: SmUnackedStanzaPurpose::Application,
        })
        .await
        .unwrap();

    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let hydrated = registry.restore_from_persistence().await.unwrap();
    assert_eq!(
        hydrated, 1,
        "expired session must be hydrated for the janitor"
    );

    // Expired sessions are never resumable on the wire.
    assert!(registry
        .peek_session("stream-expired")
        .await
        .unwrap()
        .is_none());

    // Durable rows survive restore — deletion happens only after the
    // caller confirms successful promotion.
    assert!(storage
        .get_session(&crate::pending_delivery::SmSessionId::new("stream-expired"))
        .await
        .unwrap()
        .is_some());

    // The janitor's drain pass sees the session with its full queue.
    let drained = registry.drain_expired().await.unwrap();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].stream_id, "stream-expired");
    assert_eq!(drained[0].unacked_stanzas.len(), 1);
    assert!(drained[0].unacked_stanzas[0].stanza_xml.contains("missed"));

    // Only confirm_drained erases the durable rows.
    registry.confirm_drained("stream-expired").await;
    assert!(storage
        .get_session(&crate::pending_delivery::SmSessionId::new("stream-expired"))
        .await
        .unwrap()
        .is_none());
    assert!(storage
        .list_unacked(&crate::pending_delivery::SmSessionId::new("stream-expired"))
        .await
        .unwrap()
        .is_empty());
}

/// Persistence wrapper that pauses inside `store_session_atomic` for
/// one designated stream so a test can deterministically interleave a
/// displacement (`store_session` for the same JID under a different
/// stream id) between a detached-append's durable snapshot write and
/// its post-write in-memory recheck.
struct GatedSnapshotPersistence {
    inner: super::super::persistence::InMemorySmPersistence,
    gate_stream: String,
    armed: std::sync::atomic::AtomicBool,
    fail_after_gate: std::sync::atomic::AtomicBool,
    reached: tokio::sync::Notify,
    proceed: tokio::sync::Notify,
}

impl GatedSnapshotPersistence {
    fn new(gate_stream: &str) -> Self {
        Self {
            inner: super::super::persistence::InMemorySmPersistence::new(),
            gate_stream: gate_stream.to_string(),
            armed: std::sync::atomic::AtomicBool::new(false),
            fail_after_gate: std::sync::atomic::AtomicBool::new(false),
            reached: tokio::sync::Notify::new(),
            proceed: tokio::sync::Notify::new(),
        }
    }
}

#[async_trait::async_trait]
impl super::super::persistence::SmPersistenceStorage for GatedSnapshotPersistence {
    async fn upsert_session(
        &self,
        session: super::super::persistence::PersistedSession,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        self.inner.upsert_session(session).await
    }

    async fn get_session(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<
        Option<super::super::persistence::PersistedSession>,
        super::super::persistence::SmPersistenceError,
    > {
        self.inner.get_session(stream_id).await
    }

    async fn delete_session(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        self.inner.delete_session(stream_id).await
    }

    async fn append_unacked(
        &self,
        stanza: super::super::persistence::PersistedUnackedStanza,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        self.inner.append_unacked(stanza).await
    }

    async fn ack_through(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, super::super::persistence::SmPersistenceError> {
        self.inner.ack_through(stream_id, up_to_sequence).await
    }

    async fn delete_unacked(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, super::super::persistence::SmPersistenceError> {
        self.inner.delete_unacked(stream_id, sequences).await
    }

    async fn list_unacked(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<super::super::persistence::PersistedUnackedStanza>,
        super::super::persistence::SmPersistenceError,
    > {
        self.inner.list_unacked(stream_id).await
    }

    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<
        Vec<super::super::persistence::PersistedSession>,
        super::super::persistence::SmPersistenceError,
    > {
        self.inner.list_expired_sessions(now).await
    }

    async fn list_all_sessions(
        &self,
    ) -> Result<
        Vec<super::super::persistence::PersistedSession>,
        super::super::persistence::SmPersistenceError,
    > {
        self.inner.list_all_sessions().await
    }

    async fn store_session_atomic(
        &self,
        session: super::super::persistence::PersistedSession,
        unacked: Vec<super::super::persistence::PersistedUnackedStanza>,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        if self.armed.load(std::sync::atomic::Ordering::SeqCst)
            && session.stream_id.as_str() == self.gate_stream
        {
            self.reached.notify_one();
            self.proceed.notified().await;
        }
        if self
            .fail_after_gate
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(super::super::persistence::SmPersistenceError::Other(
                "injected failure after displacement gate".to_string(),
            ));
        }
        self.inner.store_session_atomic(session, unacked).await
    }
}

async fn assert_ambiguous_claim_survives_displacement(
    replacement_snapshot_fails: bool,
    rotate_identity: bool,
    reconcile_before_replacement_finishes: bool,
) {
    let old_stream = "ambiguous-displaced-stream";
    let shard_probe = InMemorySmSessionRegistry::new();
    let old_lock = shard_probe
        .stream_lock(old_stream)
        .expect("old stream lock");
    let new_stream = (0..1024)
        .map(|index| format!("ambiguous-displacing-stream-{index}"))
        .find(|candidate| {
            let candidate_lock = shard_probe.stream_lock(candidate).expect("candidate lock");
            !std::sync::Arc::ptr_eq(&old_lock, &candidate_lock)
        })
        .expect("a stream on another shard");
    let storage = std::sync::Arc::new(GatedSnapshotPersistence::new(&new_stream));
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let identity = crate::ownership::SharedNodeIdentity::new(me.clone());
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(store.clone(), identity.clone()),
    );
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    registry
        .store_session(realistic_test_session_for_jid(old_stream, jid.clone()))
        .await
        .expect("seed displaced session");
    *store
        .poison_fence_cache_after_ensure
        .lock()
        .expect("claim error injection lock") =
        Some(EnsureClaimTestAction::ReplaceClaimThenBackendError);
    registry
        .claim_session_typed(old_stream)
        .await
        .expect_err("lost ensure response must remain ambiguous");
    assert!(registry.has_claim_fence_reservation(old_stream));

    storage
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    storage.fail_after_gate.store(
        replacement_snapshot_fails,
        std::sync::atomic::Ordering::SeqCst,
    );
    let replacement_registry = registry.clone();
    let replacement_stream = new_stream.clone();
    let replacement = tokio::spawn(async move {
        replacement_registry
            .store_session(realistic_test_session_for_jid(&replacement_stream, jid))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), storage.reached.notified())
        .await
        .expect("replacement should pause after displacement");
    assert!(registry
        .pending_promotions
        .read()
        .expect("displaced sessions")
        .contains(old_stream));
    assert!(!registry
        .sessions
        .read()
        .expect("sessions")
        .contains_key(old_stream));

    let mut replacement = Some(replacement);
    let completed_replacement = if reconcile_before_replacement_finishes {
        None
    } else {
        storage.proceed.notify_one();
        Some(
            replacement
                .take()
                .expect("replacement task")
                .await
                .expect("replacement task"),
        )
    };

    if rotate_identity {
        identity
            .rotate(crate::ownership::NodeIdentity::new(
                "sm-node",
                "next-incarnation",
            ))
            .await;
    }
    registry.retry_pending_claim_releases(1).await;
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        old_stream.to_string(),
    );
    let authoritative = crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
        .await
        .expect("claim lookup")
        .expect("ambiguous claim remains held");
    assert_eq!(
        registry
            .claim_fences
            .read()
            .expect("claim fences")
            .get(old_stream)
            .map(super::super::persistence::SmClaimFence::epoch),
        Some(authoritative.claim_epoch)
    );
    assert!(!registry.has_claim_fence_reservation(old_stream));

    let replacement = match completed_replacement {
        Some(result) => result,
        None => {
            storage.proceed.notify_one();
            replacement
                .take()
                .expect("replacement task")
                .await
                .expect("replacement task")
        }
    };
    if replacement_snapshot_fails {
        assert!(replacement.is_err(), "replacement snapshot should fail");
        assert!(registry
            .pending_promotions
            .read()
            .expect("pending promotions")
            .contains(old_stream));
        let drained = registry.drain_expired().await.expect("drain retry session");
        assert!(drained
            .iter()
            .any(|session| session.stream_id == old_stream));
    } else {
        let displaced = replacement.expect("replacement snapshot");
        assert!(displaced
            .iter()
            .any(|session| session.stream_id == old_stream));
        assert!(registry
            .pending_promotions
            .read()
            .expect("displaced sessions")
            .contains(old_stream));
    }
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup")
            .is_some()
    );

    assert!(registry.confirm_drained(old_stream).await);
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup")
            .is_none()
    );
    assert!(!registry
        .pending_promotions
        .read()
        .expect("displaced sessions")
        .contains(old_stream));
    assert!(!registry
        .claim_fences
        .read()
        .expect("claim fences")
        .contains_key(old_stream));
    assert!(registry
        .pending_claim_releases
        .read()
        .expect("pending releases")
        .iter()
        .all(|(stream_id, _)| stream_id != old_stream));
}

#[tokio::test]
async fn ambiguous_claim_stays_held_through_successful_displacement() {
    assert_ambiguous_claim_survives_displacement(false, false, true).await;
}

#[tokio::test]
async fn ambiguous_old_identity_claim_stays_held_through_displacement_rollback() {
    assert_ambiguous_claim_survives_displacement(true, true, true).await;
}

#[tokio::test]
async fn rolled_back_promotion_stays_held_when_reconciled_after_identity_rotation() {
    assert_ambiguous_claim_survives_displacement(true, true, false).await;
}

#[tokio::test]
async fn cancelled_displacement_reconciles_the_pending_promotion_before_reinsertion() {
    let old_stream = "cancelled-displaced-stream";
    let shard_probe = InMemorySmSessionRegistry::new();
    let old_lock = shard_probe
        .stream_lock(old_stream)
        .expect("old stream lock");
    let new_stream = (0..1024)
        .map(|index| format!("cancelled-displacing-stream-{index}"))
        .find(|candidate| {
            let candidate_lock = shard_probe.stream_lock(candidate).expect("candidate lock");
            !std::sync::Arc::ptr_eq(&old_lock, &candidate_lock)
        })
        .expect("a stream on another shard");
    let storage = std::sync::Arc::new(GatedSnapshotPersistence::new(&new_stream));
    let registry =
        std::sync::Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let mut old_session = realistic_test_session_for_jid(old_stream, jid.clone());
    old_session.unacked_stanzas = vec![
        DetachedUnackedStanza {
            sequence: 6,
            stanza_xml: realistic_dm_stanza_xml(
                "alice@example.com/web",
                "alice@example.com/web",
                "retract-during-displacement",
                "secret",
            ),
            original_receipt_at: Utc::now(),
            purpose: SmUnackedStanzaPurpose::Application,
        },
        DetachedUnackedStanza {
            sequence: 7,
            stanza_xml: realistic_dm_stanza_xml(
                "alice@example.com/web",
                "alice@example.com/web",
                "keep-during-displacement",
                "safe",
            ),
            original_receipt_at: Utc::now(),
            purpose: SmUnackedStanzaPurpose::Application,
        },
    ];
    registry
        .store_session(old_session)
        .await
        .expect("seed displaced session");
    storage
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let replacement_registry = registry.clone();
    let replacement = tokio::spawn(async move {
        replacement_registry
            .store_session(realistic_test_session_for_jid(&new_stream, jid))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), storage.reached.notified())
        .await
        .expect("replacement should pause after displacement");

    // The displaced payload is now off-map. Scrub its durable row while the
    // replacement snapshot is suspended; cancellation must not republish the
    // stale pre-scrub copy held by the guard.
    assert_eq!(
        registry
            .scrub_unacked_for_tombstone(&direct_target(
                "retract-during-displacement",
                "alice@example.com",
                "alice@example.com",
            ))
            .await
            .expect("scrub displaced durable row"),
        1
    );
    replacement.abort();
    assert!(replacement
        .await
        .expect_err("replacement should be cancelled")
        .is_cancelled());

    assert!(registry
        .pending_promotions
        .read()
        .expect("pending promotions")
        .contains(old_stream));
    assert!(!registry
        .sessions
        .read()
        .expect("sessions")
        .contains_key(old_stream));
    assert!(registry
        .pending_promotion_retries
        .read()
        .expect("pending promotion retries")
        .contains_key(old_stream));
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        old_stream.to_string(),
    );
    assert!(registry
        .claim_store
        .current_claim(&entity)
        .await
        .expect("claim lookup")
        .is_some());
    let drained = registry.drain_expired().await.expect("drain retry session");
    let retried = drained
        .iter()
        .find(|session| session.stream_id == old_stream)
        .expect("reconciled promotion retry");
    assert_eq!(retried.unacked_stanzas.len(), 1);
    assert_eq!(retried.unacked_stanzas[0].sequence, 7);
    assert!(!registry
        .pending_promotion_retries
        .read()
        .expect("pending promotion retries")
        .contains_key(old_stream));
    assert!(registry.confirm_drained(old_stream).await);
    assert!(!registry
        .pending_promotions
        .read()
        .expect("pending promotions")
        .contains(old_stream));
    assert!(registry
        .claim_store
        .current_claim(&entity)
        .await
        .expect("claim lookup")
        .is_none());
}

#[tokio::test]
async fn cancelled_confirm_retains_exact_release_after_durable_delete() {
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let persistence = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(persistence.clone())
            .with_claim_store(
                store.clone(),
                crate::ownership::SharedNodeIdentity::new(crate::ownership::NodeIdentity::local()),
            ),
    );
    let stream_id = "cancelled-confirm-release";
    let mut session = realistic_test_session_for_jid(
        stream_id,
        "alice@example.com/web".parse().expect("valid jid"),
    );
    session.max_resume_time = Some(0);
    registry
        .store_session(session)
        .await
        .expect("seed expired session");
    let drained = registry.drain_expired().await.expect("drain expired");
    assert_eq!(drained.len(), 1);
    assert!(registry
        .pending_promotions
        .read()
        .expect("pending promotions")
        .contains(stream_id));
    store
        .hang_release
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let confirming_registry = registry.clone();
    let confirming =
        tokio::spawn(async move { confirming_registry.confirm_drained(stream_id).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.pending_claim_release_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("exact release must publish before the hanging backend call");
    confirming.abort();
    assert!(confirming
        .await
        .expect_err("confirm task should be cancelled")
        .is_cancelled());
    assert!(!registry
        .pending_promotions
        .read()
        .expect("pending promotions")
        .contains(stream_id));
    assert!(persistence
        .get_session(&crate::pending_delivery::SmSessionId::new(stream_id))
        .await
        .expect("durable lookup")
        .is_none());
    assert_eq!(registry.pending_claim_release_count(), 1);
    registry
        .retain_pending_promotion_for_retry(drained[0].clone())
        .expect("cancelled outer promotion fallback");
    assert!(!registry
        .sessions
        .read()
        .expect("sessions")
        .contains_key(stream_id));
    assert!(!registry
        .pending_promotion_retries
        .read()
        .expect("promotion retries")
        .contains_key(stream_id));

    store
        .hang_release
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(registry.retry_pending_claim_releases(1).await, 1);
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("claim lookup")
            .is_none()
    );
}

#[tokio::test]
async fn already_present_reclaim_consumes_its_reservation_into_the_active_fence() {
    let registry = InMemorySmSessionRegistry::with_capacity(1);
    let stream_id = "already-present-reclaimed";
    registry
        .store_session(realistic_test_session(stream_id))
        .await
        .expect("seed active session");
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    let fence = registry
        .claim_fences
        .read()
        .expect("claim fences")
        .get(stream_id)
        .cloned()
        .expect("active fence");
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaimed reservation");

    let outcome = registry
        .hydrate_reclaimed_typed(&entity, &fence, reservation)
        .await
        .expect("already-present hydration");

    assert_eq!(outcome, ReclaimedHydrationOutcome::AlreadyPresent);
    assert!(!registry
        .reclaimed_claim_reservations
        .read()
        .expect("reclaimed reservations")
        .contains_key(stream_id));
    assert_eq!(registry.claim_fence_capacity_used(), 1);
}

#[tokio::test]
async fn pending_promotion_is_live_until_durable_confirmation() {
    let registry = InMemorySmSessionRegistry::new();
    let stream_id = "live-through-promotion";
    registry
        .store_session(realistic_test_session(stream_id))
        .await
        .expect("seed session");

    let drained = registry
        .drain_all_for_shutdown()
        .await
        .expect("begin promotion");
    assert_eq!(drained.len(), 1);
    assert_eq!(
        registry.live_session_ids().expect("live inventory"),
        vec![stream_id.to_string()],
        "the claim janitor must protect an off-map promote-to-confirm lifecycle"
    );

    assert!(registry.confirm_drained(stream_id).await);
    assert!(registry
        .live_session_ids()
        .expect("live inventory")
        .is_empty());
}

#[tokio::test]
async fn reclaimed_hydration_does_not_publish_over_a_pending_promotion() {
    let registry = InMemorySmSessionRegistry::new();
    let stream_id = "hydrate-during-promotion";
    registry
        .store_session(realistic_test_session(stream_id))
        .await
        .expect("seed session");
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    let fence = registry
        .claim_fences
        .read()
        .expect("claim fences")
        .get(stream_id)
        .cloned()
        .expect("active fence");
    let drained = registry
        .drain_all_for_shutdown()
        .await
        .expect("begin promotion");
    assert_eq!(drained.len(), 1);
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaimed reservation");

    let outcome = registry
        .hydrate_reclaimed_typed(&entity, &fence, reservation)
        .await
        .expect("reclaimed hydration");

    assert_eq!(outcome, ReclaimedHydrationOutcome::AlreadyPresent);
    assert!(!registry
        .sessions
        .read()
        .expect("sessions")
        .contains_key(stream_id));
    assert!(!registry
        .claimed_sessions
        .read()
        .expect("claimed sessions")
        .contains_key(stream_id));
    assert!(registry
        .pending_promotions
        .read()
        .expect("pending promotions")
        .contains(stream_id));
}

#[tokio::test]
async fn concurrent_retry_drains_lease_a_promotion_exactly_once() {
    let registry = std::sync::Arc::new(InMemorySmSessionRegistry::new());
    let stream_id = "concurrent-retry-drain";
    registry
        .store_session(realistic_test_session(stream_id))
        .await
        .expect("seed session");
    let session = registry
        .drain_all_for_shutdown()
        .await
        .expect("begin promotion")
        .into_iter()
        .next()
        .expect("drained session");
    registry
        .retain_pending_promotion_for_retry(session)
        .expect("queue retry");

    let first_registry = registry.clone();
    let second_registry = registry.clone();
    let (first, second) = tokio::join!(
        async move { first_registry.drain_expired().await.expect("first drain") },
        async move { second_registry.drain_expired().await.expect("second drain") }
    );

    assert_eq!(
        first.len() + second.len(),
        1,
        "two concurrent janitor sweeps must never return the same promotion payload twice"
    );
    assert_eq!(
        first
            .iter()
            .chain(second.iter())
            .map(|session| session.stream_id.as_str())
            .collect::<Vec<_>>(),
        vec![stream_id]
    );
}

#[test]
fn pending_promotion_blocks_cross_node_exact_repair_transfer() {
    let registry = InMemorySmSessionRegistry::with_capacity(1);
    let stream_id = "promoting-reclaimed";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaimed reservation");
    let fence = super::super::persistence::SmClaimFence::new(
        crate::ownership::NodeIdentity::local(),
        crate::ownership::ClaimEpoch(7),
    );
    assert!(registry.try_record_verified_reclaimed_fence(stream_id, fence.clone(), reservation,));
    registry
        .pending_promotions
        .write()
        .expect("pending promotions")
        .insert(stream_id.to_string());

    assert!(!registry
        .transfer_reclaimed_claim_to_exact_release(&entity, &fence, reservation)
        .expect("inspect repair transfer"));
    assert_eq!(
        registry
            .claim_fences
            .read()
            .expect("claim fences")
            .get(stream_id),
        Some(&fence)
    );
    assert_eq!(registry.pending_claim_release_count(), 0);
}

#[tokio::test]
async fn promotion_handoff_cancels_stale_identity_reclaimed_reservation() {
    let registry = InMemorySmSessionRegistry::with_capacity(1);
    let stream_id = "promoting-stale-reclaim";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaimed reservation");
    let fence = super::super::persistence::SmClaimFence::new(
        crate::ownership::NodeIdentity::new("old-node", "old-incarnation"),
        crate::ownership::ClaimEpoch(7),
    );
    registry
        .pending_promotions
        .write()
        .expect("pending promotions")
        .insert(stream_id.to_string());

    let outcome = registry
        .release_reclaimed_claim(&entity, &fence, reservation)
        .await
        .expect("promotion owns the stale reclaim lifecycle");

    assert_eq!(outcome, crate::ownership::ExactReleaseOutcome::NotOwned);
    assert!(!registry
        .reclaimed_claim_reservations
        .read()
        .expect("reclaimed reservations")
        .contains_key(stream_id));
    assert_eq!(registry.claim_fence_capacity_used(), 0);
}

#[tokio::test]
async fn cancelled_detach_snapshot_releases_its_owned_claim_reservation() {
    let stream_id = "cancelled-detach-snapshot";
    let storage = std::sync::Arc::new(GatedSnapshotPersistence::new(stream_id));
    storage
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::with_capacity(1).with_persistence(storage.clone()),
    );
    let storing_registry = registry.clone();
    let mut storing = tokio::spawn(async move {
        storing_registry
            .store_session(realistic_test_session(stream_id))
            .await
    });
    tokio::select! {
        _ = storage.reached.notified() => {}
        result = &mut storing => panic!("store completed before the snapshot gate: {result:?}"),
    }
    assert!(registry.has_claim_fence_reservation(stream_id));

    storing.abort();
    assert!(storing.await.unwrap_err().is_cancelled());

    assert!(
        !registry.has_claim_fence_reservation(stream_id),
        "cancellation before claim acquisition must release a detach-owned marker"
    );
}

#[tokio::test]
async fn detached_append_losing_race_to_displacement_preserves_durable_rows() {
    // Regression: `update_detached_session_snapshot`'s fail-closed
    // branch durably deleted the stream's rows when the session
    // vanished from both maps between the stream-lock read and the
    // post-write recheck. Displacement by `store_session` (which holds
    // only the NEW stream's shard lock) is exactly such a removal —
    // and displaced sessions follow the persist-until-confirmed
    // contract: their durable rows must survive until
    // `confirm_drained`. A crash mid-promotion after the fail-closed
    // delete lost the whole queue.
    let victim_jid: FullJid = "race@example.com/old".parse().unwrap();
    let storage = std::sync::Arc::new(GatedSnapshotPersistence::new("stream-race-victim"));
    let registry =
        std::sync::Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));

    // Pick a displacing stream id on a DIFFERENT lock shard so the
    // interleaving below cannot self-deadlock on a shared shard.
    let victim_lock = registry.stream_lock("stream-race-victim").unwrap();
    let displacing_id = (0..10_000)
        .map(|i| format!("stream-race-new-{i}"))
        .find(|candidate| {
            let lock = registry.stream_lock(candidate).expect("stream lock");
            !std::sync::Arc::ptr_eq(&lock, &victim_lock)
        })
        .expect("some candidate hashes to a different shard");

    registry
        .store_session(realistic_test_session_for_jid(
            "stream-race-victim",
            victim_jid.clone(),
        ))
        .await
        .unwrap();

    // Arm the gate, then run the detached-append snapshot; it pauses
    // inside its durable write.
    storage
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let append_registry = std::sync::Arc::clone(&registry);
    let append = tokio::spawn(async move {
        append_registry
            .update_detached_session_snapshot(
                "stream-race-victim",
                |_| true,
                |session| {
                    session.record_detached_outbound(
                        message_stanza_xml_with_id("race-append".to_string()),
                        Utc::now(),
                        SmUnackedStanzaPurpose::Application,
                    );
                    Ok(())
                },
            )
            .await
    });
    storage.reached.notified().await;
    storage
        .armed
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // Same client fresh-binds under a new stream id: store_session
    // displaces the victim from the in-memory maps (jid collision)
    // while the append is parked mid-write.
    let displaced = registry
        .store_session(realistic_test_session_for_jid(&displacing_id, victim_jid))
        .await
        .unwrap();
    assert_eq!(displaced.len(), 1, "victim must be displaced");

    // Let the append finish; it must observe the loss of ownership...
    storage.proceed.notify_one();
    let updated = append.await.unwrap().unwrap();
    assert!(!updated, "append must report the session as gone");

    // ...WITHOUT durably deleting rows it no longer owns. Deletion is
    // owned by confirm_drained / the janitor after promotion.
    assert!(
        storage
            .get_session(&crate::pending_delivery::SmSessionId::new(
                "stream-race-victim"
            ))
            .await
            .unwrap()
            .is_some(),
        "displaced session's durable rows must survive until promotion confirms"
    );
}

#[tokio::test]
async fn tombstone_scrub_reaches_durable_rows_of_off_map_streams() {
    // S5 regression: the scrub's in-map phases snapshot only the two
    // in-memory maps. A stream that is off-map but still durable
    // (displaced mid-promotion, janitor-drained mid-promotion, or a
    // promotion-failure retry window) was unreachable — a retraction
    // arriving in that window was resurrected at the next restart and
    // promoted verbatim.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let session = realistic_test_session_for_jid(
        "stream-offmap",
        "user@example.com/resource".parse().unwrap(),
    );
    let mut session = session;
    session.unacked_stanzas = vec![
        DetachedUnackedStanza {
            sequence: 6,
            stanza_xml: realistic_dm_stanza_xml(
                "alice@example.com/web",
                "user@example.com/resource",
                "retract-me",
                "secret",
            ),
            original_receipt_at: Utc::now(),
            purpose: SmUnackedStanzaPurpose::Application,
        },
        DetachedUnackedStanza {
            sequence: 7,
            stanza_xml: realistic_dm_stanza_xml(
                "alice@example.com/web",
                "user@example.com/resource",
                "keep-me",
                "safe",
            ),
            original_receipt_at: Utc::now(),
            purpose: SmUnackedStanzaPurpose::Application,
        },
    ];
    registry.store_session(session).await.unwrap();

    // Simulate mid-promotion: the stream leaves memory (janitor drain /
    // displacement) but its durable rows survive un-confirmed.
    let drained = registry.drain_all_for_shutdown().await.unwrap();
    assert_eq!(drained.len(), 1);

    // Retraction arrives during the window.
    let removed = registry
        .scrub_unacked_for_tombstone(&direct_target(
            "retract-me",
            "alice@example.com",
            "user@example.com",
        ))
        .await
        .unwrap();
    assert_eq!(
        removed, 1,
        "durable-side sweep must scrub off-map streams' rows"
    );

    // Restart-style read: the retracted stanza must be gone, the
    // non-matching one preserved.
    let restarted = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    assert_eq!(restarted.restore_from_persistence().await.unwrap(), 1);
    let hydrated = restarted
        .peek_session("stream-offmap")
        .await
        .unwrap()
        .expect("session rehydrates");
    assert_eq!(hydrated.unacked_stanzas.len(), 1);
    assert!(
        !hydrated
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("secret")),
        "retracted stanza must not survive the restart"
    );
    assert!(hydrated
        .unacked_stanzas
        .iter()
        .any(|entry| entry.stanza_xml.contains("safe")));
}

#[tokio::test]
async fn displace_stored_session_if_unclaimed_preserves_durable_rows() {
    // S2 regression (ownership-moved detach path): when a cleanup
    // loses the registry ownership race, the just-stored detached
    // session must be removed from memory FOR PROMOTION — its durable
    // rows must survive until the promote → confirm_drained chain, not
    // be durably deleted up-front with the returned session discarded.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    registry
        .store_session(realistic_test_session("stream-owner-moved"))
        .await
        .unwrap();

    let displaced = registry
        .displace_stored_session_if_unclaimed("stream-owner-moved")
        .await
        .unwrap()
        .expect("unclaimed stored session must be displaced");
    assert_eq!(displaced.stream_id, "stream-owner-moved");
    assert_eq!(displaced.unacked_stanzas.len(), 2);

    // Removed from the in-memory view...
    assert!(registry
        .peek_session("stream-owner-moved")
        .await
        .unwrap()
        .is_none());
    // ...but durable rows survive until the caller confirms promotion.
    assert!(storage
        .get_session(&crate::pending_delivery::SmSessionId::new(
            "stream-owner-moved"
        ))
        .await
        .unwrap()
        .is_some());
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "stream-owner-moved".to_string(),
    );
    assert!(registry
        .claim_store
        .current_claim(&entity)
        .await
        .expect("claim lookup")
        .is_some());
    assert!(registry
        .pending_promotions
        .read()
        .expect("pending promotions")
        .contains("stream-owner-moved"));

    registry.confirm_drained("stream-owner-moved").await;
    assert!(storage
        .get_session(&crate::pending_delivery::SmSessionId::new(
            "stream-owner-moved"
        ))
        .await
        .unwrap()
        .is_none());
    assert!(registry
        .claim_store
        .current_claim(&entity)
        .await
        .expect("claim lookup")
        .is_none());
    assert!(!registry
        .pending_promotions
        .read()
        .expect("pending promotions")
        .contains("stream-owner-moved"));
}

#[tokio::test]
async fn displace_stored_session_if_unclaimed_leaves_claimed_sessions_alone() {
    let registry = InMemorySmSessionRegistry::new();
    registry
        .store_session(make_test_session("stream-claimed-keep"))
        .await
        .unwrap();
    assert!(registry
        .claim_session("stream-claimed-keep")
        .await
        .unwrap()
        .is_some());

    assert!(registry
        .displace_stored_session_if_unclaimed("stream-claimed-keep")
        .await
        .unwrap()
        .is_none());

    // The in-flight resume claim is untouched.
    assert!(registry
        .complete_claim("stream-claimed-keep")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn reinsert_for_retry_makes_session_drainable_but_never_resumable() {
    // S4 support: a session re-inserted after a promotion failure must
    // be visible to the janitor's next `drain_expired` pass, must NOT
    // be resumable on the wire (peek/claim), and must not touch
    // durable state.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let session = realistic_test_session("stream-reinsert");
    registry.store_session(session).await.unwrap();

    let drained = registry.drain_all_for_shutdown().await.unwrap();
    assert_eq!(drained.len(), 1);
    assert!(registry.drain_expired().await.unwrap().is_empty());

    // Promotion failed → re-insert for retry.
    registry
        .reinsert_for_retry(drained.into_iter().next().unwrap())
        .await
        .unwrap();

    // Not resumable on the wire.
    assert!(registry
        .peek_session("stream-reinsert")
        .await
        .unwrap()
        .is_none());
    assert!(registry
        .claim_session("stream-reinsert")
        .await
        .unwrap()
        .is_none());

    // Drainable on the janitor's next pass, durable rows untouched.
    let retried = registry.drain_expired().await.unwrap();
    assert_eq!(retried.len(), 1);
    assert_eq!(retried[0].stream_id, "stream-reinsert");
    assert!(storage
        .get_session(&crate::pending_delivery::SmSessionId::new(
            "stream-reinsert"
        ))
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn scrub_records_recent_tombstone_for_promotion_time_recheck() {
    // R2 (round-2 review): a retraction racing an in-flight promotion
    // finds the drained session in neither map and its pending row not
    // yet inserted. The scrub must leave a recent-tombstone record the
    // promotion path can re-check before inserting into pending
    // delivery.
    let registry = InMemorySmSessionRegistry::new();
    let target = direct_target("retract-me", "alice@example.com", "user@example.com");
    let before = chrono::Utc::now();
    registry.scrub_unacked_for_tombstone(&target).await.unwrap();
    let after = chrono::Utc::now();

    let records = registry.recent_tombstones().unwrap();
    assert_eq!(
        records.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
        vec![target],
        "the scrub must record its tombstone identity for promotion-time re-check"
    );
    // Round-3 review finding 2: the record carries the wall-clock
    // recording time so promotion can scope the match backward in
    // time (a stanza received after the retraction must not match).
    let recorded_at_utc = records[0].recorded_at_utc;
    assert!(
        before <= recorded_at_utc && recorded_at_utc <= after,
        "recorded_at_utc must be the wall-clock time of the scrub \
         (got {recorded_at_utc}, expected within [{before}, {after}])"
    );
}

#[tokio::test]
async fn recent_tombstones_evicts_entries_past_ttl() {
    let registry = InMemorySmSessionRegistry::new();
    let stale = Instant::now() - (tombstones::RECENT_TOMBSTONE_TTL + Duration::from_secs(1));
    registry
        .record_recent_tombstone_at(
            &direct_target("old-target", "alice@example.com", "user@example.com"),
            stale,
        )
        .unwrap();
    let fresh = direct_target("fresh-target", "alice@example.com", "user@example.com");
    registry
        .record_recent_tombstone_at(&fresh, Instant::now())
        .unwrap();

    let records = registry.recent_tombstones().unwrap();
    assert_eq!(
        records.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
        vec![fresh],
        "entries older than the TTL must be evicted; fresh ones retained"
    );
}

#[tokio::test]
async fn recent_tombstones_bounds_entry_count_across_archives() {
    // Global backstop: distinct archives each get one record, so the
    // per-archive cap never trips; the global hard cap must still
    // bound the list, evicting the oldest overall.
    let registry = InMemorySmSessionRegistry::new();
    for i in 0..(tombstones::MAX_RECENT_TOMBSTONES + 5) {
        registry
            .record_recent_tombstone_at(
                &direct_target(
                    &format!("target-{i}"),
                    "alice@example.com",
                    &format!("user{i}@example.com"),
                ),
                Instant::now(),
            )
            .unwrap();
    }
    let records = registry.recent_tombstones().unwrap();
    assert_eq!(
        records.len(),
        tombstones::MAX_RECENT_TOMBSTONES,
        "the recent-tombstone record must stay bounded"
    );
    assert_eq!(
        records.last().map(|r| r.key.id()),
        Some(format!("target-{}", tombstones::MAX_RECENT_TOMBSTONES + 4).as_str()),
        "overflow must evict the oldest entries, keeping the newest"
    );
}

#[tokio::test]
async fn tombstone_flood_on_one_archive_does_not_evict_other_archives_record() {
    // FINDING C repro: an authenticated user retracting 1024+ of their
    // own messages (all one archive) used to flush every other
    // archive's unexpired record oldest-first, disabling the
    // promotion-time re-check for the victim's retraction.
    let registry = InMemorySmSessionRegistry::new();
    let victim = direct_target("victim-retraction", "bob@example.com", "victim@example.com");
    registry
        .record_recent_tombstone_at(&victim, Instant::now())
        .unwrap();

    // Attacker floods well past the global cap from a single archive.
    for i in 0..(tombstones::MAX_RECENT_TOMBSTONES + 100) {
        registry
            .record_recent_tombstone_at(
                &direct_target(
                    &format!("spam-{i}"),
                    "mallory@example.com",
                    "mallory-chat@example.com",
                ),
                Instant::now(),
            )
            .unwrap();
    }

    let records = registry.recent_tombstones().unwrap();
    assert!(
        records.iter().any(|r| r.key == victim),
        "an unexpired record for another archive must survive a single-archive flood"
    );
    let flood_records = records
        .iter()
        .filter(|r| r.key.archive_jid() == &bare("mallory-chat@example.com"))
        .count();
    assert!(
        flood_records <= tombstones::MAX_RECENT_TOMBSTONES_PER_ARCHIVE,
        "one archive's records must be bounded by the per-archive cap \
         (got {flood_records})"
    );
}

#[tokio::test]
async fn per_archive_cap_evicts_oldest_within_that_archive_only() {
    let registry = InMemorySmSessionRegistry::new();
    for i in 0..(tombstones::MAX_RECENT_TOMBSTONES_PER_ARCHIVE + 3) {
        registry
            .record_recent_tombstone_at(
                &direct_target(&format!("id-{i}"), "alice@example.com", "chat@example.com"),
                Instant::now(),
            )
            .unwrap();
    }
    let records = registry.recent_tombstones().unwrap();
    assert_eq!(
        records.len(),
        tombstones::MAX_RECENT_TOMBSTONES_PER_ARCHIVE,
        "same-archive records are bounded by the per-archive cap"
    );
    assert!(
        !records.iter().any(|r| r.key.id() == "id-0"),
        "the oldest same-archive record is the eviction victim"
    );
    assert_eq!(
        records.last().map(|r| r.key.id()),
        Some(format!("id-{}", tombstones::MAX_RECENT_TOMBSTONES_PER_ARCHIVE + 2).as_str()),
        "the newest record survives"
    );
}

#[tokio::test]
async fn reinsert_for_retry_preserves_detached_at_for_eviction_ordering() {
    // R3 (round-2 review): `reinsert_for_retry` used to reset
    // `detached_at` to ≈now, so under a degraded backend a repeatedly
    // failing session looked perpetually fresh and the max_sessions
    // min-by-detached_at eviction kept sacrificing HEALTHY resumable
    // sessions instead. The reinserted session must keep its original
    // detach time so it sorts as the oldest eviction candidate.
    let registry = InMemorySmSessionRegistry::with_capacity(2);

    let mut failing =
        make_test_session_for_jid("stream-failing", "alice@example.com/web".parse().unwrap());
    failing.detached_at = Instant::now() - Duration::from_secs(100);
    registry.reinsert_for_retry(failing).await.unwrap();

    let mut healthy =
        make_test_session_for_jid("stream-healthy", "bob@example.com/web".parse().unwrap());
    healthy.detached_at = Instant::now() - Duration::from_secs(50);
    assert!(registry.store_session(healthy).await.unwrap().is_empty());

    // Capacity overflow: the eviction victim must be the genuinely
    // oldest session (the reinserted one), not the healthy fresher one.
    let displaced = registry
        .store_session(make_test_session_for_jid(
            "stream-newest",
            "carol@example.com/web".parse().unwrap(),
        ))
        .await
        .unwrap();
    let displaced_ids: Vec<&str> = displaced.iter().map(|s| s.stream_id.as_str()).collect();
    assert_eq!(
        displaced_ids,
        vec!["stream-failing"],
        "the reinserted session must keep its original detached_at and \
         be evicted before a fresher healthy session"
    );
    assert!(
        registry
            .peek_session("stream-healthy")
            .await
            .unwrap()
            .is_some(),
        "the healthy resumable session must survive the eviction"
    );
}

#[tokio::test]
async fn reinsert_for_retry_drops_entries_whose_durable_rows_were_scrubbed() {
    // FINDING D repro: the uncounted janitor retry path
    // (record_promotion_failure erroring) re-inserted the drained
    // session's IN-MEMORY queue verbatim. A XEP-0424/0425 scrub that
    // ran while the session was off-map (durable phase-4 sweep)
    // deleted only the durable rows, so after RECENT_TOMBSTONE_TTL
    // expired the retained in-memory copy promoted the retracted
    // stanza anyway. reinsert_for_retry must diff against durable
    // rows and drop scrubbed entries.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let mut session = realistic_test_session_for_jid(
        "stream-retry-scrubbed",
        "user@example.com/resource".parse().unwrap(),
    );
    session.unacked_stanzas = vec![
        DetachedUnackedStanza {
            sequence: 6,
            stanza_xml: realistic_dm_stanza_xml(
                "alice@example.com/web",
                "user@example.com/resource",
                "retract-me",
                "secret",
            ),
            original_receipt_at: Utc::now(),
            purpose: SmUnackedStanzaPurpose::Application,
        },
        DetachedUnackedStanza {
            sequence: 7,
            stanza_xml: realistic_dm_stanza_xml(
                "alice@example.com/web",
                "user@example.com/resource",
                "keep-me",
                "safe",
            ),
            original_receipt_at: Utc::now(),
            purpose: SmUnackedStanzaPurpose::Application,
        },
    ];
    registry.store_session(session).await.unwrap();

    // Janitor drains the session off both maps (mid-promotion).
    let drained = registry.drain_all_for_shutdown().await.unwrap();
    assert_eq!(drained.len(), 1);

    // Retraction lands during the window: the off-map durable sweep
    // (phase 4) deletes the durable row for sequence 6.
    let removed = registry
        .scrub_unacked_for_tombstone(&direct_target(
            "retract-me",
            "alice@example.com",
            "user@example.com",
        ))
        .await
        .unwrap();
    assert_eq!(removed, 1);

    // Promotion fails AND record_promotion_failure fails → the janitor
    // re-inserts the drained (pre-scrub) copy for retry.
    registry
        .reinsert_for_retry(drained.into_iter().next().unwrap())
        .await
        .unwrap();

    // The retry drain must NOT resurrect the scrubbed stanza.
    let retried = registry.drain_expired().await.unwrap();
    assert_eq!(retried.len(), 1);
    let sequences: Vec<u32> = retried[0]
        .unacked_stanzas
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    assert_eq!(
        sequences,
        vec![7],
        "a durably-scrubbed stanza must not survive reinsert_for_retry"
    );
    assert!(
        !retried[0]
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("secret")),
        "retracted content must not be promotable after the retry re-insert"
    );
}

#[tokio::test]
async fn reinsert_for_retry_keeps_queue_when_session_was_never_persisted() {
    // Round-6 review: the Finding D durable diff must not misread "no
    // durable rows because the snapshot write FAILED" (Finding E's
    // store_session error path leaves the new session in memory with
    // no durable session row) as "rows scrubbed". A phase-4 scrub
    // deletes unacked rows but leaves the durable session row, so the
    // diff is authoritative only when that session row exists; when it
    // does not, dropping the queue would silently lose messages on the
    // very storage blip reinsert_for_retry claims to tolerate.
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let mut session = realistic_test_session_for_jid(
        "stream-never-persisted",
        "user@example.com/resource".parse().unwrap(),
    );
    session.unacked_stanzas = vec![DetachedUnackedStanza {
        sequence: 3,
        stanza_xml: realistic_dm_stanza_xml(
            "alice@example.com/web",
            "user@example.com/resource",
            "keep-me",
            "safe",
        ),
        original_receipt_at: Utc::now(),
        purpose: SmUnackedStanzaPurpose::Application,
    }];

    // The session never went through a successful store_session
    // snapshot: no durable session row, no durable unacked rows.
    registry.reinsert_for_retry(session).await.unwrap();

    let retried = registry.drain_expired().await.unwrap();
    assert_eq!(retried.len(), 1);
    assert_eq!(
        retried[0]
            .unacked_stanzas
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![3],
        "a never-persisted queue must be kept verbatim (at-least-once), \
         not dropped by the durable diff"
    );
}

/// Persistence wrapper whose `store_session_atomic` fails while armed
/// — models a durable-backend outage hitting exactly the snapshot
/// write inside `store_session` (Finding E).
struct FailingSnapshotPersistence {
    inner: super::super::persistence::InMemorySmPersistence,
    fail_snapshots: std::sync::atomic::AtomicBool,
    fail_reads: std::sync::atomic::AtomicBool,
}

impl FailingSnapshotPersistence {
    fn new() -> Self {
        Self {
            inner: super::super::persistence::InMemorySmPersistence::new(),
            fail_snapshots: std::sync::atomic::AtomicBool::new(false),
            fail_reads: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl super::super::persistence::SmPersistenceStorage for FailingSnapshotPersistence {
    async fn upsert_session(
        &self,
        session: super::super::persistence::PersistedSession,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        self.inner.upsert_session(session).await
    }

    async fn get_session(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<
        Option<super::super::persistence::PersistedSession>,
        super::super::persistence::SmPersistenceError,
    > {
        self.inner.get_session(stream_id).await
    }

    async fn delete_session(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        self.inner.delete_session(stream_id).await
    }

    async fn append_unacked(
        &self,
        stanza: super::super::persistence::PersistedUnackedStanza,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        self.inner.append_unacked(stanza).await
    }

    async fn ack_through(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, super::super::persistence::SmPersistenceError> {
        self.inner.ack_through(stream_id, up_to_sequence).await
    }

    async fn delete_unacked(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, super::super::persistence::SmPersistenceError> {
        self.inner.delete_unacked(stream_id, sequences).await
    }

    async fn list_unacked(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<super::super::persistence::PersistedUnackedStanza>,
        super::super::persistence::SmPersistenceError,
    > {
        self.inner.list_unacked(stream_id).await
    }

    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<
        Vec<super::super::persistence::PersistedSession>,
        super::super::persistence::SmPersistenceError,
    > {
        self.inner.list_expired_sessions(now).await
    }

    async fn list_all_sessions(
        &self,
    ) -> Result<
        Vec<super::super::persistence::PersistedSession>,
        super::super::persistence::SmPersistenceError,
    > {
        if self.fail_reads.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(super::super::persistence::SmPersistenceError::Other(
                "simulated session-list failure".into(),
            ));
        }
        self.inner.list_all_sessions().await
    }

    async fn store_session_atomic(
        &self,
        session: super::super::persistence::PersistedSession,
        unacked: Vec<super::super::persistence::PersistedUnackedStanza>,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        if self
            .fail_snapshots
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(super::super::persistence::SmPersistenceError::Other(
                "simulated snapshot-write failure".into(),
            ));
        }
        self.inner.store_session_atomic(session, unacked).await
    }
}

async fn assert_cross_shard_displacement_preserves_claim(snapshot_fails: bool) {
    let me = crate::ownership::NodeIdentity::new("sm-node", "incarnation");
    let store = std::sync::Arc::new(HangingReleaseClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
        hang_release: std::sync::atomic::AtomicBool::new(false),
        hang_ensure: std::sync::atomic::AtomicBool::new(false),
        commit_then_hang_ensure_once: std::sync::atomic::AtomicBool::new(false),
        poison_fence_cache_after_ensure: std::sync::Mutex::new(None),
    });
    let storage = std::sync::Arc::new(FailingSnapshotPersistence::new());
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(store.clone(), crate::ownership::SharedNodeIdentity::new(me)),
    );
    let old_stream = "cross-shard-displaced";
    let old_lock = registry.stream_lock(old_stream).expect("old stream lock");
    let new_stream = (0..1024)
        .map(|index| format!("cross-shard-replacement-{index}"))
        .find(|candidate| {
            let candidate_lock = registry.stream_lock(candidate).expect("candidate lock");
            !std::sync::Arc::ptr_eq(&old_lock, &candidate_lock)
        })
        .expect("a stream on a different shard");
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    registry
        .store_session(realistic_test_session_for_jid(old_stream, jid.clone()))
        .await
        .expect("seed old detached session");

    let reached = std::sync::Arc::new(tokio::sync::Notify::new());
    let proceed = std::sync::Arc::new(tokio::sync::Notify::new());
    *store
        .poison_fence_cache_after_ensure
        .lock()
        .expect("pause injection lock") = Some(EnsureClaimTestAction::Pause {
        reached: reached.clone(),
        proceed: proceed.clone(),
    });
    let claiming_registry = registry.clone();
    let old_stream_for_claim = old_stream.to_string();
    let claiming = tokio::spawn(async move {
        claiming_registry
            .claim_session_typed(&old_stream_for_claim)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), reached.notified())
        .await
        .expect("claim should pause after its self-ensure");

    storage
        .fail_snapshots
        .store(snapshot_fails, std::sync::atomic::Ordering::SeqCst);
    let replacement = registry
        .store_session(realistic_test_session_for_jid(&new_stream, jid))
        .await;
    if snapshot_fails {
        assert!(replacement.is_err(), "replacement snapshot must fail");
    } else {
        let displaced = replacement.expect("replacement snapshot");
        assert!(displaced
            .iter()
            .any(|session| session.stream_id == old_stream));
    }
    storage
        .fail_snapshots
        .store(false, std::sync::atomic::Ordering::SeqCst);
    proceed.notify_one();

    let outcome = claiming.await.expect("claim task").expect("claim result");
    assert!(matches!(
        outcome,
        super::claims::ClaimSessionOutcome::MissingOrExpired
    ));
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        old_stream.to_string(),
    );
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("old claim lookup")
            .is_some()
    );
    assert!(registry
        .claim_fences
        .read()
        .expect("claim fences")
        .contains_key(old_stream));

    if snapshot_fails {
        let drained = registry.drain_expired().await.expect("drain retry entry");
        assert!(drained
            .iter()
            .any(|session| session.stream_id == old_stream));
    }
    registry.confirm_drained(old_stream).await;
    assert!(
        crate::ownership::ClaimStore::current_claim(store.as_ref(), &entity)
            .await
            .expect("old claim lookup")
            .is_none()
    );
}

#[tokio::test]
async fn cross_shard_displacement_keeps_claim_until_confirmed() {
    assert_cross_shard_displacement_preserves_claim(false).await;
}

#[tokio::test]
async fn cross_shard_failed_snapshot_retry_keeps_claim_until_confirmed() {
    assert_cross_shard_displacement_preserves_claim(true).await;
}

#[tokio::test]
async fn store_session_snapshot_failure_reinserts_displaced_sessions_for_retry() {
    // FINDING E repro: store_session removes displaced sessions from
    // both maps, then writes the NEW session's snapshot. When that
    // write fails, store_session returns Err and the caller drops the
    // displaced vec — the displaced sessions are off-map with durable
    // rows stranded until restart (drain_expired scans memory only).
    // The failure path must re-insert them as expired-for-retry.
    let storage = std::sync::Arc::new(FailingSnapshotPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let jid: FullJid = "alice@example.com/web".parse().unwrap();
    registry
        .store_session(realistic_test_session_for_jid("stream-victim", jid.clone()))
        .await
        .unwrap();

    // Fresh bind for the same JID displaces the victim, but its own
    // snapshot write fails.
    storage
        .fail_snapshots
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let result = registry
        .store_session(realistic_test_session_for_jid("stream-new", jid.clone()))
        .await;
    assert!(
        result.is_err(),
        "snapshot failure must surface to the caller"
    );
    storage
        .fail_snapshots
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // The displaced victim must be back in the map, expired-for-retry:
    // not resumable on the wire...
    assert!(registry
        .peek_session("stream-victim")
        .await
        .unwrap()
        .is_none());
    // ...but drainable by the janitor's next pass with its full queue.
    let drained = registry.drain_expired().await.unwrap();
    let drained_ids: Vec<&str> = drained.iter().map(|s| s.stream_id.as_str()).collect();
    assert!(
        drained_ids.contains(&"stream-victim"),
        "displaced session must be re-inserted for janitor retry when the \
         snapshot write fails (got {drained_ids:?})"
    );
    let victim = drained
        .iter()
        .find(|s| s.stream_id == "stream-victim")
        .expect("victim drained");
    assert_eq!(
        victim.unacked_stanzas.len(),
        2,
        "the displaced session keeps its full unacked queue for promotion"
    );
    // Its durable rows are untouched (persist-until-confirmed).
    assert!(storage
        .get_session(&crate::pending_delivery::SmSessionId::new("stream-victim"))
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn claim_of_expired_session_keeps_it_drainable_for_promotion() {
    // Regression: `claim_session` removed the session from the map and
    // only THEN noticed it was expired, dropping a #1098-hydrated
    // expired session from memory. A client auto-reconnecting with
    // `<resume/>` before the janitor's first tick would make the
    // session vanish — `drain_expired` scans only memory, so its
    // unacked queue was stranded until the next restart.
    let mut expired = make_test_session("stream-claim-expired");
    expired.max_resume_time = Some(0);
    let registry = InMemorySmSessionRegistry::new();
    registry.store_session(expired).await.unwrap();

    // Resume attempt: expired sessions are never claimable.
    let claimed = registry
        .claim_session("stream-claim-expired")
        .await
        .unwrap();
    assert!(claimed.is_none(), "expired session must not be claimable");

    // But the failed claim must NOT strand the queue: the janitor's
    // next drain pass still sees the session for XEP-0198 §5 promotion.
    let drained = registry.drain_expired().await.unwrap();
    assert_eq!(
        drained.len(),
        1,
        "expired session must remain drainable after a rejected claim"
    );
    assert_eq!(drained[0].stream_id, "stream-claim-expired");
    assert_eq!(drained[0].unacked_stanzas.len(), 3);
}

use crate::ownership::ClaimStore as _;

#[tokio::test]
async fn jid_collision_eviction_of_a_claimed_session_releases_its_claim_after_confirm_drained() {
    // `store_session`'s jid-collision retain evicts claimed sessions too
    // (exercised on every connection detach via the cleanup path). The
    // eviction must eventually release the `ClaimStore` entry, or the
    // stream id is stranded as claimed forever (an unbounded leak under
    // the in-process store, a stuck `clustering_claims` row under
    // Postgres) — but FIX 1 (council-adjudicated, ADR-0017 Phase 3 Slice 5
    // corrigenda) moves WHEN that release happens: not eagerly inside
    // `store_session` (which raced a second node's claim-scoped hydration
    // of the same still-undeleted durable row), but only after the
    // caller's XEP-0198 §5 promotion succeeds and `confirm_drained` erases
    // the durable row — the evicted session flows through `store_session`'s
    // `displaced` return value exactly like a plain `sessions` eviction.
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let me = crate::ownership::NodeIdentity::local();
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(crate::ownership::NodeIdentity::local()),
    );

    registry
        .store_session(make_test_session("stream-collide-old"))
        .await
        .expect("store old session");
    registry
        .claim_session("stream-collide-old")
        .await
        .expect("claim")
        .expect("claimable");

    // Same jid, different stream id: displaces the claimed old session.
    let displaced = registry
        .store_session(make_test_session("stream-collide-new"))
        .await
        .expect("store colliding session");
    assert_eq!(
        displaced.len(),
        1,
        "the evicted claimed session must flow through the displaced return value"
    );
    assert_eq!(displaced[0].stream_id, "stream-collide-old");

    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "stream-collide-old".to_string(),
    );
    store.acquire(&entity, &me).await.expect_err(
        "FIX 1: the claim must still be held immediately after store_session — releasing \
         it before the durable row is deleted is exactly the hazard FIX 1 closes",
    );

    // The caller's real contract: promote, then confirm_drained (no
    // persistence attached in this test, so the durable delete is a no-op
    // and confirm_drained proceeds straight to releasing the claim).
    registry.confirm_drained("stream-collide-old").await;

    store
        .acquire(&entity, &me)
        .await
        .expect("evicted claimed session's claim must be released once confirm_drained runs");
}

#[tokio::test]
async fn take_session_of_a_claimed_copy_releases_its_claim() {
    // `take_session` unconditionally removes any claimed copy; that ends
    // the claim and must release the `ClaimStore` entry. The
    // sessions-AND-claimed double residency is not constructible through
    // the public API today (claim_session moves; store_session's retain
    // evicts) — this pins the DEFENSIVE branch by building the state
    // directly, so a future path that reaches it inherits the release.
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let me = crate::ownership::NodeIdentity::local();
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(crate::ownership::NodeIdentity::local()),
    );

    registry
        .store_session(make_test_session("stream-take-claimed"))
        .await
        .expect("store session");
    registry
        .claim_session("stream-take-claimed")
        .await
        .expect("claim")
        .expect("claimable");
    // Re-create the `sessions` copy so take_session's existence peek
    // passes while the claimed copy (and its live claim) still exists.
    registry.sessions.write().expect("sessions lock").insert(
        "stream-take-claimed".to_string(),
        make_test_session("stream-take-claimed"),
    );

    registry
        .take_session("stream-take-claimed")
        .await
        .expect("take");

    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "stream-take-claimed".to_string(),
    );
    store
        .acquire(&entity, &me)
        .await
        .expect("taken claimed session's claim must have been released");
}

// --- FIX 2 (council-adjudicated, ADR-0017 Phase 3 Slice 5 corrigenda):
// `hydrate_reclaimed` targeted-hydration tests -----------------------

#[tokio::test]
async fn hydrate_reclaimed_skips_when_stream_id_already_present_in_memory() {
    // A targeted hydrate must never overwrite a live in-memory copy with a
    // stale durable read — the absence-from-both-maps re-check, taken
    // under the same stream-shard lock as every other mutator, is what
    // prevents the orphan reaper (or the inline post-fence reclaim, FIX 4)
    // from clobbering a session that is already resident, whether because
    // a live detach beat it to the punch or because an overlapping sweep
    // already reclaimed it.
    let claim_store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let me = crate::ownership::NodeIdentity::local();
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(
            claim_store.clone(),
            crate::ownership::SharedNodeIdentity::new(me.clone()),
        );

    // A decoy durable row for the SAME stream id: if `hydrate_reclaimed`
    // wrongly bypassed the present-check, it would load and insert THIS,
    // clobbering the sentinel below.
    storage
        .upsert_session(super::super::persistence::PersistedSession {
            stream_id: crate::pending_delivery::SmSessionId::new("stream-already-present"),
            user_id: "user@example.com".to_string(),
            jid: make_test_jid(),
            inbound_count: 111,
            outbound_count: 111,
            last_acked: 0,
            replay_gap_through: None,
            max_resume_time: Some(300),
            detached_at: chrono::Utc::now(),
            max_resume_duration: Duration::from_secs(300),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        })
        .await
        .expect("seed decoy durable row");

    let mut present = realistic_test_session("stream-already-present");
    present.outbound_count = 999; // sentinel: must survive untouched
    registry
        .store_session(present)
        .await
        .expect("store the already-present session");

    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "stream-already-present".to_string(),
    );
    let epoch = crate::ownership::ClaimEpoch(0);
    let fence = super::super::persistence::SmClaimFence::new(me, epoch);
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaim reservation");
    let hydrated = registry
        .hydrate_reclaimed(&[(entity, fence, reservation)])
        .await
        .expect("hydrate_reclaimed");
    assert_eq!(
        hydrated, 0,
        "an already-present stream id must be skipped, not hydrated"
    );

    let unchanged = registry
        .peek_session("stream-already-present")
        .await
        .expect("peek")
        .expect("session still present");
    assert_eq!(
        unchanged.outbound_count, 999,
        "hydrate_reclaimed must not overwrite the already-present session with the \
         durable (decoy) copy"
    );
}

#[tokio::test]
async fn hydrate_reclaimed_rejects_work_from_a_superseded_epoch() {
    let claim_store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let me = crate::ownership::NodeIdentity::local();
    let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "stream-stale-work".to_string(),
    );
    let current_epoch = claim_store
        .acquire(&entity, &me)
        .await
        .expect("seed current claim");
    storage
        .upsert_session(super::super::persistence::PersistedSession {
            stream_id: crate::pending_delivery::SmSessionId::new("stream-stale-work"),
            user_id: "user@example.com".to_string(),
            jid: make_test_jid(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            max_resume_time: Some(300),
            detached_at: chrono::Utc::now(),
            max_resume_duration: Duration::from_secs(300),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        })
        .await
        .expect("seed durable row");
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(storage)
        .with_claim_store(
            claim_store,
            crate::ownership::SharedNodeIdentity::new(me.clone()),
        );

    let stale_fence = super::super::persistence::SmClaimFence::new(
        me,
        crate::ownership::ClaimEpoch(current_epoch.0 + 1),
    );

    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaim reservation");
    let outcome = registry
        .hydrate_reclaimed_typed(&entity, &stale_fence, reservation)
        .await
        .expect("hydrate stale work");

    assert_eq!(outcome, super::ReclaimedHydrationOutcome::LostClaim);
    assert!(registry
        .peek_session("stream-stale-work")
        .await
        .expect("peek")
        .is_none());
}

/// Persistence wrapper that pauses inside `get_session` for one designated
/// stream id — lets the mid-flight race test below deterministically
/// interleave a concurrent live-path mutator (`store_session`) with
/// `hydrate_reclaimed`'s own in-flight reclaim of the SAME stream id, while
/// `hydrate_reclaimed` still holds that stream's shard lock. Mirrors
/// `GatedSnapshotPersistence` above, one method over.
struct GatedGetSessionPersistence {
    inner: super::super::persistence::InMemorySmPersistence,
    gate_stream: String,
    armed: std::sync::atomic::AtomicBool,
    reached: tokio::sync::Notify,
    proceed: tokio::sync::Notify,
    fail_get_once: std::sync::atomic::AtomicBool,
    corrupt: std::sync::atomic::AtomicBool,
    quarantined: std::sync::atomic::AtomicBool,
}

impl GatedGetSessionPersistence {
    fn new(gate_stream: &str) -> Self {
        Self {
            inner: super::super::persistence::InMemorySmPersistence::new(),
            gate_stream: gate_stream.to_string(),
            armed: std::sync::atomic::AtomicBool::new(false),
            reached: tokio::sync::Notify::new(),
            proceed: tokio::sync::Notify::new(),
            fail_get_once: std::sync::atomic::AtomicBool::new(false),
            corrupt: std::sync::atomic::AtomicBool::new(false),
            quarantined: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl super::super::persistence::SmPersistenceStorage for GatedGetSessionPersistence {
    async fn upsert_session(
        &self,
        session: super::super::persistence::PersistedSession,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        self.inner.upsert_session(session).await
    }

    async fn get_session(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<
        Option<super::super::persistence::PersistedSession>,
        super::super::persistence::SmPersistenceError,
    > {
        if self
            .fail_get_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(super::super::persistence::SmPersistenceError::Other(
                "injected transient get failure".to_string(),
            ));
        }
        if self.corrupt.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(super::super::persistence::SmPersistenceError::Corrupt {
                stream_id: stream_id.clone(),
                detail: "injected corrupt row".to_string(),
            });
        }
        if self.armed.load(std::sync::atomic::Ordering::SeqCst)
            && stream_id.as_str() == self.gate_stream
        {
            self.reached.notify_one();
            self.proceed.notified().await;
        }
        self.inner.get_session(stream_id).await
    }

    async fn delete_session(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        self.inner.delete_session(stream_id).await
    }

    async fn quarantine_session(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
        _expected_fence: &super::super::persistence::SmClaimFence,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        self.quarantined
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.delete_session(stream_id).await
    }

    async fn append_unacked(
        &self,
        stanza: super::super::persistence::PersistedUnackedStanza,
    ) -> Result<(), super::super::persistence::SmPersistenceError> {
        self.inner.append_unacked(stanza).await
    }

    async fn ack_through(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, super::super::persistence::SmPersistenceError> {
        self.inner.ack_through(stream_id, up_to_sequence).await
    }

    async fn delete_unacked(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, super::super::persistence::SmPersistenceError> {
        self.inner.delete_unacked(stream_id, sequences).await
    }

    async fn list_unacked(
        &self,
        stream_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<super::super::persistence::PersistedUnackedStanza>,
        super::super::persistence::SmPersistenceError,
    > {
        self.inner.list_unacked(stream_id).await
    }

    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<
        Vec<super::super::persistence::PersistedSession>,
        super::super::persistence::SmPersistenceError,
    > {
        self.inner.list_expired_sessions(now).await
    }

    async fn list_all_sessions(
        &self,
    ) -> Result<
        Vec<super::super::persistence::PersistedSession>,
        super::super::persistence::SmPersistenceError,
    > {
        self.inner.list_all_sessions().await
    }
}

#[tokio::test]
async fn hydrate_reclaimed_quarantines_corrupt_persistence_before_terminal_outcome() {
    let storage = std::sync::Arc::new(GatedGetSessionPersistence::new("unused"));
    let claim_store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let me = crate::ownership::NodeIdentity::local();
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "stream-poison".to_string(),
    );
    storage
        .inner
        .upsert_session(super::super::persistence::PersistedSession {
            stream_id: crate::pending_delivery::SmSessionId::new("stream-poison"),
            user_id: "poison@example.com".to_string(),
            jid: make_test_jid(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            max_resume_time: Some(300),
            detached_at: chrono::Utc::now(),
            max_resume_duration: Duration::from_secs(300),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        })
        .await
        .expect("seed poison row");
    let epoch = claim_store
        .acquire(&entity, &me)
        .await
        .expect("seed exact claim");
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(
            claim_store,
            crate::ownership::SharedNodeIdentity::new(me.clone()),
        );
    storage
        .corrupt
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaim reservation");
    let outcome = registry
        .hydrate_reclaimed_typed(
            &entity,
            &super::super::persistence::SmClaimFence::new(me, epoch),
            reservation,
        )
        .await
        .expect("hydrate poison");

    assert_eq!(outcome, super::ReclaimedHydrationOutcome::PoisonReleased);
    assert!(storage
        .quarantined
        .load(std::sync::atomic::Ordering::SeqCst));
    storage
        .corrupt
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(storage
        .inner
        .get_session(&crate::pending_delivery::SmSessionId::new("stream-poison"))
        .await
        .expect("read after quarantine")
        .is_none());
}

#[tokio::test]
async fn transient_reclaimed_hydration_is_retained_and_retried() {
    let storage = std::sync::Arc::new(GatedGetSessionPersistence::new("unused"));
    let claim_store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let me = crate::ownership::NodeIdentity::local();
    let stream_id = "stream-transient-hydration";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    storage
        .inner
        .upsert_session(super::super::persistence::PersistedSession {
            stream_id: crate::pending_delivery::SmSessionId::new(stream_id),
            user_id: "retry@example.com".to_string(),
            jid: make_test_jid(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            max_resume_time: Some(300),
            detached_at: chrono::Utc::now(),
            max_resume_duration: Duration::from_secs(300),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        })
        .await
        .expect("seed durable session");
    let epoch = claim_store
        .acquire(&entity, &me)
        .await
        .expect("seed exact claim");
    let fence = super::super::persistence::SmClaimFence::new(me.clone(), epoch);
    let registry = InMemorySmSessionRegistry::with_capacity(1)
        .with_persistence(storage.clone())
        .with_claim_store(claim_store, crate::ownership::SharedNodeIdentity::new(me));
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaim reservation");
    storage
        .fail_get_once
        .store(true, std::sync::atomic::Ordering::SeqCst);

    assert_eq!(
        registry
            .hydrate_reclaimed_typed(&entity, &fence, reservation)
            .await
            .expect("first hydration"),
        super::ReclaimedHydrationOutcome::TransientFailure
    );
    assert_eq!(registry.pending_reclaimed_hydration_count(), 1);
    assert!(!registry
        .claim_fence_reservations
        .read()
        .expect("reservations")
        .contains(stream_id));
    assert_eq!(
        registry
            .claim_fences
            .read()
            .expect("claim fences")
            .get(stream_id),
        Some(&fence),
        "verified hydration must atomically replace the reservation with a counted exact fence before transient storage I/O"
    );
    let another =
        crate::ownership::Entity::new(crate::ownership::EntityType::SmSession, "another-reclaim");
    assert!(
        registry
            .reserve_reclaimed_claim_capacity(&another)
            .is_none(),
        "transient hydration must retain and consume the sole bounded ownership slot"
    );

    assert_eq!(registry.retry_pending_reclaimed_hydrations(1).await, 1);
    assert_eq!(registry.pending_reclaimed_hydration_count(), 0);
    assert!(registry
        .peek_session(stream_id)
        .await
        .expect("peek hydrated session")
        .is_some());
}

#[tokio::test]
async fn cancelled_reclaimed_hydration_waiting_for_shard_remains_retryable() {
    let claim_store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let owner = crate::ownership::NodeIdentity::local();
    let stream_id = "cancelled-reclaimed-hydration-shard-wait";
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        stream_id.to_string(),
    );
    let epoch = claim_store.acquire(&entity, &owner).await.unwrap();
    let fence = super::super::persistence::SmClaimFence::new(owner.clone(), epoch);
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::with_capacity(1).with_claim_store(
            claim_store,
            crate::ownership::SharedNodeIdentity::new(owner),
        ),
    );
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaim reservation");
    let blocker = registry.lock_session_operation(stream_id).await.unwrap();
    let hydrating_registry = registry.clone();
    let hydrating_entity = entity.clone();
    let hydrating_fence = fence.clone();
    let hydration = tokio::spawn(async move {
        hydrating_registry
            .hydrate_reclaimed_typed(&hydrating_entity, &hydrating_fence, reservation)
            .await
    });
    tokio::task::yield_now().await;
    hydration.abort();
    assert!(hydration.await.unwrap_err().is_cancelled());

    assert_eq!(registry.pending_reclaimed_hydration_count(), 1);
    assert!(
        registry
            .reclaimed_claim_reservations
            .read()
            .unwrap()
            .get(stream_id)
            == Some(&reservation)
    );
    drop(blocker);

    assert_eq!(registry.retry_pending_reclaimed_hydrations(1).await, 1);
    assert_eq!(registry.pending_reclaimed_hydration_count(), 0);
    assert!(registry.locally_owned_claim_ids().unwrap().is_empty());
}

#[tokio::test]
async fn hydrate_reclaimed_serializes_against_a_concurrent_live_mutator_for_the_same_stream() {
    // FIX 2's mid-flight race: the orphan reaper's `hydrate_reclaimed` must
    // never overlap a live session's own store/claim/take mutation of the
    // SAME stream id in memory. The stream-shard lock it takes (like every
    // other registry mutator) forces the two to strictly serialize instead
    // of interleave, so there is never a moment with a "ghost" entry (the
    // reaper's hydrated copy briefly coexisting with a fresher live one) or
    // double residency (present under both `sessions` and
    // `claimed_sessions` at once — this test does not construct double
    // residency either, so the existing
    // `take_session_of_a_claimed_copy_releases_its_claim` test's own
    // "double residency is not constructible through the public API today"
    // comment stays true; this test instead proves the LOCK is what
    // prevents it from ever being observable under real concurrency).
    let storage = std::sync::Arc::new(GatedGetSessionPersistence::new("stream-mid-flight"));
    let claim_store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let me = crate::ownership::NodeIdentity::local();
    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "stream-mid-flight".to_string(),
    );
    let jid: FullJid = "race@example.com/reaper".parse().unwrap();

    // Seed a durable row as if a dead node's claim had just been stolen by
    // `me` (the orphan reaper's actual precondition): the row exists, and
    // `me` already holds the `ClaimStore` entry, but nothing is in memory
    // yet.
    storage
        .inner
        .upsert_session(super::super::persistence::PersistedSession {
            stream_id: crate::pending_delivery::SmSessionId::new("stream-mid-flight"),
            user_id: "race@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            max_resume_time: Some(300),
            detached_at: chrono::Utc::now(),
            max_resume_duration: Duration::from_secs(300),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        })
        .await
        .expect("seed durable row");
    let epoch = claim_store
        .acquire(&entity, &me)
        .await
        .expect("seed the claim as if steal_stale just won it");

    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(
                claim_store.clone(),
                crate::ownership::SharedNodeIdentity::new(me.clone()),
            ),
    );

    // Arm the gate, then spawn `hydrate_reclaimed`: it takes the shard
    // lock, passes its absence re-check (nothing in memory yet), confirms
    // the claim, and pauses inside `get_session` — still holding the lock.
    storage
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaim reservation");
    let hydrate_registry = std::sync::Arc::clone(&registry);
    let hydrate_entities = vec![(
        entity,
        super::super::persistence::SmClaimFence::new(me, epoch),
        reservation,
    )];
    let hydrate =
        tokio::spawn(async move { hydrate_registry.hydrate_reclaimed(&hydrate_entities).await });
    storage.reached.notified().await;
    storage
        .armed
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // While `hydrate_reclaimed` is paused HOLDING the shard lock, a
    // concurrent live mutator for the exact same stream id (standing in
    // for "a live session completes/claims") must block rather than
    // interleave: give it ample real-time opportunity to (wrongly)
    // proceed if the lock were not actually held.
    let live_registry = std::sync::Arc::clone(&registry);
    let live_session = realistic_test_session_for_jid("stream-mid-flight", jid.clone());
    let mut live_store =
        tokio::spawn(async move { live_registry.store_session(live_session).await });
    let raced_ahead = tokio::time::timeout(Duration::from_millis(50), &mut live_store).await;
    assert!(
        raced_ahead.is_err(),
        "a concurrent store_session for the same stream id must block on the shard lock \
         hydrate_reclaimed still holds, never interleave with it"
    );

    // Release the gate: `hydrate_reclaimed` finishes (inserts the row it
    // legitimately owns) and drops the lock, unblocking the live mutator.
    storage.proceed.notify_one();
    let hydrated = hydrate
        .await
        .expect("hydrate_reclaimed task")
        .expect("hydrate_reclaimed result");
    assert_eq!(
        hydrated, 1,
        "hydrate_reclaimed must hydrate the row it legitimately owns"
    );

    let live_result = live_store
        .await
        .expect("live store_session task")
        .expect("store_session result");
    assert!(
        live_result.is_empty(),
        "no jid/stream collision to displace — same stream id, same jid"
    );

    // Exactly one in-memory copy for this stream id — no ghost, no double
    // residency — and it reflects the live path's write (the last one to
    // run), proving the two operations serialized rather than corrupted
    // each other's state.
    assert_eq!(registry.session_count().await, 1);
    let final_session = registry
        .peek_session("stream-mid-flight")
        .await
        .expect("peek")
        .expect("exactly one copy present");
    assert_eq!(final_session.jid, jid);
}

#[tokio::test]
async fn invalidate_sessions_for_jid_defers_claim_release_to_confirm_drained() {
    // Mirror of the store_session FIX-1 test for the sibling path the
    // convergence check flagged: `invalidate_sessions_for_jid` (a fresh
    // bind displacing this jid's detached/claimed sessions) must NOT
    // release the `ClaimStore` entry eagerly — the durable row still
    // exists, and an eager release lets another node hydrate a copy that
    // our caller's later `confirm_drained` deletes out from under it. The
    // claim ends only via confirm_drained after the durable delete.
    let store = std::sync::Arc::new(crate::ownership::InProcessClaimStore::new());
    let me = crate::ownership::NodeIdentity::local();
    let registry = InMemorySmSessionRegistry::new().with_claim_store(
        store.clone(),
        crate::ownership::SharedNodeIdentity::new(crate::ownership::NodeIdentity::local()),
    );

    registry
        .store_session(make_test_session("stream-invalidate-claimed"))
        .await
        .expect("store session");
    registry
        .claim_session("stream-invalidate-claimed")
        .await
        .expect("claim")
        .expect("claimable");

    let removed = registry
        .invalidate_sessions_for_jid(&make_test_jid())
        .await
        .expect("invalidate");
    assert_eq!(
        removed.len(),
        1,
        "the invalidated claimed session must flow back to the caller for promotion"
    );
    assert_eq!(removed[0].stream_id, "stream-invalidate-claimed");

    let entity = crate::ownership::Entity::new(
        crate::ownership::EntityType::SmSession,
        "stream-invalidate-claimed".to_string(),
    );
    store.acquire(&entity, &me).await.expect_err(
        "the claim must still be held immediately after invalidate_sessions_for_jid — \
         releasing before the durable row is deleted reopens the double-hydration hazard",
    );

    // The caller's real contract: promote, then confirm_drained.
    registry.confirm_drained("stream-invalidate-claimed").await;
    store
        .acquire(&entity, &me)
        .await
        .expect("invalidated session's claim must be released once confirm_drained runs");
}

/// #1249 cross-node guard: `any_resumable_session_for_full_jid` must
/// see sessions that exist ONLY in the shared durable store (this
/// node's memory has no trace after a cross-node resume-steal), honor
/// row expiry, and fail closed when the durable read errors.
#[tokio::test]
async fn any_resumable_session_probe_covers_durable_rows_and_fails_closed() {
    use super::super::persistence::{InMemorySmPersistence, PersistedSession};

    let storage = std::sync::Arc::new(InMemorySmPersistence::new());
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let jid: FullJid = "roamer@example.com/laptop".parse().expect("jid");

    // No memory, no durable row: not resumable.
    assert!(!registry.any_resumable_session_for_full_jid(&jid).await);

    // Durable-only row (as left behind by a cross-node steal): resumable.
    let durable_row = |stream_id: &str, detached_at| PersistedSession {
        stream_id: crate::pending_delivery::SmSessionId::new(stream_id),
        user_id: jid.to_bare().to_string(),
        jid: jid.clone(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        max_resume_time: Some(120),
        detached_at,
        max_resume_duration: std::time::Duration::from_secs(120),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
    };
    storage
        .upsert_session(durable_row("stream-durable", Utc::now()))
        .await
        .expect("upsert durable row");
    assert!(
        registry.any_resumable_session_for_full_jid(&jid).await,
        "a durable-only row proves the occupancy is still resumable"
    );

    // Expired durable row: no longer resumable.
    storage
        .delete_session(&crate::pending_delivery::SmSessionId::new("stream-durable"))
        .await
        .expect("remove fresh row");
    storage
        .upsert_session(durable_row(
            "stream-expired",
            Utc::now() - chrono::Duration::seconds(600),
        ))
        .await
        .expect("upsert expired row");
    assert!(
        !registry.any_resumable_session_for_full_jid(&jid).await,
        "an expired durable row must not block reconciliation"
    );

    // A clock-skewed row (detached_at in the future → negative elapsed)
    // counts as resumable: fail closed.
    storage
        .delete_session(&crate::pending_delivery::SmSessionId::new("stream-expired"))
        .await
        .expect("remove expired row");
    storage
        .upsert_session(durable_row(
            "stream-skewed",
            Utc::now() + chrono::Duration::seconds(60),
        ))
        .await
        .expect("upsert skewed row");
    assert!(
        registry.any_resumable_session_for_full_jid(&jid).await,
        "clock skew must fail closed (treated as resumable)"
    );

    // Different full JID never matches.
    let other: FullJid = "roamer@example.com/phone".parse().expect("jid");
    storage
        .delete_session(&crate::pending_delivery::SmSessionId::new("stream-skewed"))
        .await
        .expect("remove skewed row");
    storage
        .upsert_session(durable_row("stream-mine", Utc::now()))
        .await
        .expect("upsert row");
    assert!(!registry.any_resumable_session_for_full_jid(&other).await);
}

#[tokio::test]
async fn typed_resumable_session_probe_surfaces_durable_read_failure() {
    let storage = std::sync::Arc::new(FailingSnapshotPersistence::new());
    storage
        .fail_reads
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage);
    let jid: FullJid = "roamer@example.com/laptop".parse().expect("jid");

    assert_eq!(
        registry.probe_resumable_session_for_full_jid(&jid).await,
        super::ResumableSessionProbe::Failed
    );
    assert!(registry.any_resumable_session_for_full_jid(&jid).await);
}
