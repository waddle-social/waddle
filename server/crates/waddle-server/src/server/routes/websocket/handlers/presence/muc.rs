use super::{
    build_subject_message, debug, element_to_xml, get_or_create_room_actor, get_room_actor,
    get_xmpp_channel, info, stanza_to_xml, warn, Affiliation, BareJid, CheckPermission,
    DefinedCondition, Element, ErrorType, FullJid, JoinAffiliationGrant, JoinWithAffiliation,
    LeaveByRealJid, Object, ObjectType, Permission, Role, RoomConfig, Session, Stanza, StanzaError,
    Subject, WebSocketState, XmppChannelRecord, DEPLOYMENT_SERVER_ID,
};
#[cfg(feature = "clustering")]
use crate::clustering::ordered_relay::MucProxyOrigin;
#[cfg(feature = "clustering")]
use crate::clustering::ordered_relay::OrderedRelayMucProxyKind;
#[cfg(feature = "clustering")]
use crate::clustering::route_bridge::OrderedRelayMucProxyOutcome;
#[cfg(feature = "clustering")]
use crate::clustering::route_bridge::RegisteredRemoteWriteAcceptedDelivery;
use crate::notification_activity::NotificationPresenceShow;
#[cfg(feature = "clustering")]
use crate::server::routes::interpret::FullJidDeliveryOutcome;
use crate::server::routes::interpret::OrderedRelayRouteOrigin;
use crate::server::routes::websocket::cleanup::get_room_actor_result;
use crate::server::routes::websocket::state::ResolverAffiliationSyncTerminalDisposition;
use crate::server::routes::websocket::state::ResolverAffiliationSyncWorker;
use crate::server::routes::websocket::InFlightLease;
use crate::server::routes::websocket::LeaveAskFailure;
use crate::server::routes::websocket::LocalDepartureItem;
use crate::server::routes::websocket::ResolvedPrincipal;
use crate::server::routes::websocket::ResolverAffiliationSyncSchedule;
use crate::server::routes::websocket::ResolverAffiliationSyncScheduler;
use crate::server::routes::websocket::ResolverAffiliationSyncWork;
use std::collections::HashSet;

mod access;
mod xml;

pub use access::{
    get_managed_channel_for_room, parse_room_jid_context, resolve_managed_channel_entry_access,
    resolve_muc_room_archive_access, RoomArchiveAccess,
};

use access::{resolve_managed_channel_affiliation, server_permission_allowed};
use waddle_xmpp::muc::room_actor::GetSnapshot;
use waddle_xmpp::muc::RoomRegistry;
use xml::{
    build_muc_conflict_presence_xml, build_muc_join_presence_stanza, build_muc_self_unavailable_xml,
};
pub(super) use xml::{build_muc_join_presence_xml, build_muc_presence_error_xml, MucJoinPresence};

#[cfg(test)]
fn sync_resolver_affiliation_on_rejection(
    existing_room_actor: Option<&kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>>,
    scheduler: &std::sync::Arc<crate::server::routes::websocket::ResolverAffiliationSyncScheduler>,
    room_jid: &BareJid,
    jid: BareJid,
    affiliation: Affiliation,
    expected_admission_revision: u64,
) {
    sync_resolver_affiliation_on_rejection_with_registry(
        existing_room_actor,
        scheduler,
        None,
        room_jid,
        jid,
        affiliation,
        expected_admission_revision,
    );
}

#[cfg(all(test, feature = "clustering"))]
mod tests {
    use super::*;

    #[test]
    fn remote_muc_join_decision_suppresses_errors_for_uncertain_commit() {
        assert!(matches!(
            remote_muc_join_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(vec![
                    Stanza::Presence(xmpp_parsers::presence::Presence::new(
                        xmpp_parsers::presence::Type::None,
                    )),
                ]),
            )),
            Some(RemoteMucJoinDecision::Delivered(_))
        ));
        assert!(matches!(
            remote_muc_join_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::MaybeCommitted,
            )),
            Some(RemoteMucJoinDecision::MaybeCommitted)
        ));
        assert!(matches!(
            remote_muc_join_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
            )),
            Some(RemoteMucJoinDecision::MaybeCommitted)
        ));
        assert!(remote_muc_join_decision(Some(
            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Unavailable,
        ))
        .is_none());
        assert!(remote_muc_join_decision(Some(
            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Dropped,
        ))
        .is_none());
        assert!(remote_muc_join_decision(None).is_none());
    }

    #[test]
    fn remote_muc_join_decision_keeps_delivered_replies() {
        let decision = remote_muc_join_decision(Some(
            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(vec![
                Stanza::Presence(xmpp_parsers::presence::Presence::new(
                    xmpp_parsers::presence::Type::None,
                )),
            ]),
        ));
        let Some(RemoteMucJoinDecision::Delivered(replies)) = decision else {
            panic!("expected delivered replies");
        };
        assert_eq!(replies.len(), 1);
    }

    #[test]
    fn remote_muc_leave_decision_preserves_membership_for_uncertain_commit() {
        assert!(matches!(
            remote_muc_leave_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::MaybeCommitted,
            )),
            RemoteMucLeaveDecision::MaybeCommitted
        ));
        assert!(matches!(
            remote_muc_leave_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
            )),
            RemoteMucLeaveDecision::MaybeCommitted
        ));
    }

    #[test]
    fn remote_muc_leave_decision_clears_only_on_delivered() {
        let decision = remote_muc_leave_decision(Some(
            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(vec![
                Stanza::Presence(xmpp_parsers::presence::Presence::new(
                    xmpp_parsers::presence::Type::None,
                )),
            ]),
        ));
        let RemoteMucLeaveDecision::Delivered(replies) = decision else {
            panic!("expected delivered replies");
        };
        assert_eq!(replies.len(), 1);

        assert!(matches!(
            remote_muc_leave_decision(Some(
                crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Unavailable,
            )),
            RemoteMucLeaveDecision::RetryableNoEffect
        ));
        assert!(matches!(
            remote_muc_leave_decision(None),
            RemoteMucLeaveDecision::LocalFallback
        ));
    }
}

#[cfg(test)]
mod resolver_sync_retry_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use kameo::actor::Spawn;
    use waddle_xmpp::muc::durable::{
        DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext,
    };
    use waddle_xmpp::muc::room_actor::{
        ChangeAffiliation, GetAffiliation, RestoreDurableRoomState, RoomActor,
    };
    use waddle_xmpp::muc::{MucRoom, RoomConfig};
    use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OCCUPANT_ID_SECRET_MIN_BYTES};

    use super::*;

    struct SequencedOwnershipStore {
        ownership_held: bool,
        failures_remaining: AtomicUsize,
        blocks_remaining: AtomicUsize,
        block_at_check: AtomicUsize,
        block_every_check: bool,
        checks: AtomicUsize,
        check_started: tokio::sync::Notify,
        release_blocked_check: tokio::sync::Notify,
    }

    impl SequencedOwnershipStore {
        fn new(failures: usize) -> Arc<Self> {
            Arc::new(Self {
                ownership_held: true,
                failures_remaining: AtomicUsize::new(failures),
                blocks_remaining: AtomicUsize::new(0),
                block_at_check: AtomicUsize::new(0),
                block_every_check: false,
                checks: AtomicUsize::new(0),
                check_started: tokio::sync::Notify::new(),
                release_blocked_check: tokio::sync::Notify::new(),
            })
        }

        fn hanging() -> Arc<Self> {
            Arc::new(Self {
                ownership_held: true,
                failures_remaining: AtomicUsize::new(0),
                blocks_remaining: AtomicUsize::new(1),
                block_at_check: AtomicUsize::new(0),
                block_every_check: false,
                checks: AtomicUsize::new(0),
                check_started: tokio::sync::Notify::new(),
                release_blocked_check: tokio::sync::Notify::new(),
            })
        }

        fn always_hanging() -> Arc<Self> {
            Arc::new(Self {
                ownership_held: true,
                failures_remaining: AtomicUsize::new(0),
                blocks_remaining: AtomicUsize::new(0),
                block_at_check: AtomicUsize::new(0),
                block_every_check: true,
                checks: AtomicUsize::new(0),
                check_started: tokio::sync::Notify::new(),
                release_blocked_check: tokio::sync::Notify::new(),
            })
        }

        fn not_owner() -> Arc<Self> {
            let mut store = Self::new(0);
            Arc::get_mut(&mut store)
                .expect("new ownership store is uniquely held")
                .ownership_held = false;
            store
        }

        fn checks(&self) -> usize {
            self.checks.load(Ordering::SeqCst)
        }

        fn fail_next(&self, failures: usize) {
            self.failures_remaining.store(failures, Ordering::SeqCst);
        }

        fn block_check(&self, check: usize) {
            self.block_at_check.store(check, Ordering::SeqCst);
        }
    }

    impl MucDurableStore for SequencedOwnershipStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            let validation = validate_test_claim_fence(room_jid, fence);
            Box::pin(async move {
                validation?;
                Ok(None)
            })
        }

        fn commit_room_mutation<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
            _intent: waddle_xmpp::muc::RoomDurableMutation,
            _effects: waddle_xmpp::muc::RoomMutationEffects,
        ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
            if let Err(error) = validate_test_claim_fence(room_jid, fence) {
                return Box::pin(async move {
                    let _ = error;
                    Err(waddle_xmpp::muc::RoomCommitError::NotOwner)
                });
            }
            Box::pin(async move {
                Ok(waddle_xmpp::muc::RoomCommitOutcome {
                    coordinates: waddle_xmpp::muc::RoomCommittedCoordinates {
                        lifecycle: waddle_xmpp::muc::RoomLifecycleId::generate(),
                        revision: waddle_xmpp::muc::RoomRevision::initial(),
                    },
                    reservation: None,
                })
            })
        }

        fn check_fenced_fanout<'a>(&'a self, _room_jid: &'a BareJid) -> MucDurableFuture<'a, bool> {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            let should_block = self.block_every_check
                || self.block_at_check.load(Ordering::SeqCst) == check
                || self
                    .blocks_remaining
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_ok();
            let should_fail = self
                .failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok();
            Box::pin(async move {
                self.check_started.notify_one();
                if should_block {
                    self.release_blocked_check.notified().await;
                }
                if should_fail {
                    Err(waddle_xmpp::XmppError::internal(
                        "transient ownership check failure",
                    ))
                } else {
                    Ok(self.ownership_held)
                }
            })
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, bool> {
            if fence != &test_claim_fence(room_jid) {
                return Box::pin(async { Ok(false) });
            }
            self.check_fenced_fanout(room_jid)
        }
    }

    fn test_claim_fence(room_jid: &BareJid) -> RoomClaimFenceContext {
        RoomClaimFenceContext::new(
            waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                room_jid.to_string(),
            ),
            waddle_xmpp::ownership::NodeIdentity::local(),
            waddle_xmpp::ownership::ClaimEpoch(1),
        )
    }

    fn validate_test_claim_fence(
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), waddle_xmpp::XmppError> {
        if fence == &test_claim_fence(room_jid) {
            Ok(())
        } else {
            Err(waddle_xmpp::XmppError::internal(
                "test store received an unexpected room claim fence",
            ))
        }
    }

    async fn spawn_sync_test_room(
        store: Arc<SequencedOwnershipStore>,
    ) -> (kameo::actor::ActorRef<RoomActor>, BareJid) {
        spawn_sync_test_room_with_seed(store, None).await
    }

    async fn spawn_sync_test_room_with_seed(
        store: Arc<SequencedOwnershipStore>,
        seed: Option<(BareJid, Affiliation)>,
    ) -> (kameo::actor::ActorRef<RoomActor>, BareJid) {
        let room_jid: BareJid = "resolver-sync@muc.example.com".parse().expect("room JID");
        let mut room = MucRoom::new(
            room_jid.clone(),
            "waddle-1".to_string(),
            "channel-1".to_string(),
            RoomConfig::default(),
        );
        if let Some((jid, affiliation)) = seed {
            room.update_affiliation_from_resolver(jid, affiliation);
        }
        let secret = OccupantIdSecret::try_new(vec![7; OCCUPANT_ID_SECRET_MIN_BYTES])
            .expect("occupant-id secret");
        let actor = RoomActor::spawn(RoomActor::new(room, secret));
        actor
            .ask(RestoreDurableRoomState {
                store: Arc::clone(&store) as Arc<dyn MucDurableStore>,
                claim_fence: test_claim_fence(&room_jid),
            })
            .await
            .expect("install durable store");
        (actor, room_jid)
    }

    fn spawn_sync_worker(
        actor: kameo::actor::ActorRef<RoomActor>,
        room_jid: BareJid,
        member: BareJid,
        affiliation: Affiliation,
        expected_admission_revision: u64,
    ) -> tokio::task::JoinHandle<()> {
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());
        spawn_sync_worker_with_scheduler(
            actor,
            room_jid,
            member,
            affiliation,
            expected_admission_revision,
            scheduler,
        )
    }

    fn spawn_sync_worker_with_scheduler(
        actor: kameo::actor::ActorRef<RoomActor>,
        room_jid: BareJid,
        member: BareJid,
        affiliation: Affiliation,
        expected_admission_revision: u64,
        scheduler: Arc<crate::server::routes::websocket::ResolverAffiliationSyncScheduler>,
    ) -> tokio::task::JoinHandle<()> {
        spawn_sync_worker_with_scheduler_and_registry(
            actor,
            room_jid,
            member,
            affiliation,
            expected_admission_revision,
            scheduler,
            None,
        )
    }

    fn spawn_sync_worker_with_scheduler_and_registry(
        actor: kameo::actor::ActorRef<RoomActor>,
        room_jid: BareJid,
        member: BareJid,
        affiliation: Affiliation,
        expected_admission_revision: u64,
        scheduler: Arc<crate::server::routes::websocket::ResolverAffiliationSyncScheduler>,
        room_registry: Option<RoomRegistry>,
    ) -> tokio::task::JoinHandle<()> {
        let work = crate::server::routes::websocket::ResolverAffiliationSyncWork {
            affiliation,
            expected_admission_revision,
        };
        let crate::server::routes::websocket::ResolverAffiliationSyncSchedule::Started(worker) =
            scheduler.schedule(&room_jid, &member, actor.id(), work)
        else {
            panic!("new scheduler starts one worker");
        };
        tokio::spawn(run_resolver_affiliation_sync_worker(
            actor,
            room_registry,
            room_jid,
            member,
            *worker,
        ))
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_retries_transient_ownership_then_applies() {
        let store = SequencedOwnershipStore::new(1);
        let (actor, room_jid) = spawn_sync_test_room(Arc::clone(&store)).await;
        let member: BareJid = "member@example.com".parse().expect("member JID");
        let retry = spawn_sync_worker(
            actor.clone(),
            room_jid,
            member.clone(),
            Affiliation::Member,
            0,
        );

        tokio::task::yield_now().await;
        assert_eq!(store.checks(), 1, "first ownership check must fail once");
        tokio::time::advance(RESOLVER_SYNC_RETRY_BACKOFF).await;
        retry.await.expect("bounded retry task");

        assert_eq!(store.checks(), 2, "second ownership check must retry");
        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("affiliation"),
            Affiliation::Member,
        );
    }

    #[tokio::test]
    async fn resolver_sync_reaps_actor_after_ownership_loss() {
        let room_jid: BareJid = "resolver-sync@muc.example.com".parse().expect("room JID");
        let member: BareJid = "deposed@example.com".parse().expect("member JID");
        let secret = OccupantIdSecret::try_new(vec![7; OCCUPANT_ID_SECRET_MIN_BYTES])
            .expect("occupant-id secret");
        let registry = RoomRegistry::spawn(
            "muc.example.com".to_string(),
            secret,
            waddle_xmpp::xep::xep0317::ServerHats::default(),
            None,
        );
        let actor = registry
            .get_or_create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                RoomConfig::default(),
            )
            .await
            .expect("create registered room")
            .actor_ref;
        let store = SequencedOwnershipStore::not_owner();
        actor
            .ask(RestoreDurableRoomState {
                store: Arc::clone(&store) as Arc<dyn MucDurableStore>,
                claim_fence: test_claim_fence(&room_jid),
            })
            .await
            .expect("install deposed ownership store");
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());

        spawn_sync_worker_with_scheduler_and_registry(
            actor,
            room_jid.clone(),
            member,
            Affiliation::None,
            0,
            scheduler,
            Some(registry.clone()),
        )
        .await
        .expect("sealed repair worker");

        assert!(
            registry
                .get_room(room_jid)
                .await
                .expect("inspect registry")
                .is_none(),
            "the rejected-join repair must evict the deposed actor"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_stops_after_bounded_persistent_ownership_errors() {
        let store = SequencedOwnershipStore::new(RESOLVER_SYNC_MAX_ATTEMPTS);
        let (actor, room_jid) = spawn_sync_test_room(Arc::clone(&store)).await;
        let member: BareJid = "member@example.com".parse().expect("member JID");
        let retry = spawn_sync_worker(
            actor.clone(),
            room_jid,
            member.clone(),
            Affiliation::Member,
            0,
        );

        for expected_checks in 1..=RESOLVER_SYNC_MAX_ATTEMPTS {
            tokio::task::yield_now().await;
            assert_eq!(store.checks(), expected_checks);
            if expected_checks < RESOLVER_SYNC_MAX_ATTEMPTS {
                tokio::time::advance(RESOLVER_SYNC_RETRY_BACKOFF).await;
            }
        }
        retry.await.expect("bounded retry task");

        assert_eq!(store.checks(), RESOLVER_SYNC_MAX_ATTEMPTS);
        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("affiliation"),
            Affiliation::None,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_waits_for_the_intrinsically_bounded_handler() {
        let store = SequencedOwnershipStore::hanging();
        let (actor, room_jid) = spawn_sync_test_room(Arc::clone(&store)).await;
        let member: BareJid = "member@example.com".parse().expect("member JID");
        let retry = spawn_sync_worker(
            actor.clone(),
            room_jid,
            member.clone(),
            Affiliation::Member,
            0,
        );

        store.check_started.notified().await;
        tokio::time::advance(RESOLVER_SYNC_MAILBOX_TIMEOUT * 4).await;
        tokio::task::yield_now().await;
        assert!(
            !retry.is_finished(),
            "delivery transfers completion ownership to the actor handler"
        );
        store.release_blocked_check.notify_one();
        tokio::time::advance(RESOLVER_SYNC_RETRY_BACKOFF).await;
        retry.await.expect("delivered repair completes");

        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("affiliation after the delivered repair completes"),
            Affiliation::Member,
        );
        assert_eq!(
            store.checks(),
            2,
            "the timed-out handler is retried once without an independent worker"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_scheduler_coalesces_identical_work_past_the_mailbox_timeout() {
        let store = SequencedOwnershipStore::hanging();
        let (actor, room_jid) = spawn_sync_test_room(Arc::clone(&store)).await;
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());
        let member: BareJid = "coalesced@example.com".parse().expect("member JID");

        sync_resolver_affiliation_on_rejection(
            Some(&actor),
            &scheduler,
            &room_jid,
            member.clone(),
            Affiliation::Member,
            0,
        );
        store.check_started.notified().await;
        sync_resolver_affiliation_on_rejection(
            Some(&actor),
            &scheduler,
            &room_jid,
            member.clone(),
            Affiliation::Member,
            0,
        );

        tokio::time::advance(RESOLVER_SYNC_MAILBOX_TIMEOUT * 4).await;
        tokio::task::yield_now().await;
        sync_resolver_affiliation_on_rejection(
            Some(&actor),
            &scheduler,
            &room_jid,
            member.clone(),
            Affiliation::Member,
            0,
        );
        store.release_blocked_check.notify_one();
        tokio::time::advance(RESOLVER_SYNC_RETRY_BACKOFF).await;
        tokio::task::yield_now().await;
        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("coalesced repair completes"),
            Affiliation::Member,
        );
        assert_eq!(
            store.checks(),
            2,
            "duplicates must stay coalesced after the old reply-timeout boundary"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_scheduler_applies_the_latest_same_revision_verdict() {
        let store = SequencedOwnershipStore::hanging();
        let member: BareJid = "revision@example.com".parse().expect("member JID");
        let (actor, room_jid) = spawn_sync_test_room_with_seed(
            Arc::clone(&store),
            Some((member.clone(), Affiliation::Member)),
        )
        .await;
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());

        sync_resolver_affiliation_on_rejection(
            Some(&actor),
            &scheduler,
            &room_jid,
            member.clone(),
            Affiliation::None,
            0,
        );
        store.check_started.notified().await;
        sync_resolver_affiliation_on_rejection(
            Some(&actor),
            &scheduler,
            &room_jid,
            member.clone(),
            Affiliation::Outcast,
            0,
        );
        tokio::task::yield_now().await;

        store.release_blocked_check.notify_one();
        store.check_started.notified().await;
        assert_eq!(store.checks(), 2, "both distinct states must be delivered");
        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("both distinct repairs drain"),
            Affiliation::Outcast,
            "the newer same-snapshot verdict must supersede the first repair's own revision bump"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_chained_update_receives_the_full_retry_budget() {
        let store = SequencedOwnershipStore::hanging();
        let member: BareJid = "chained-retry@example.com".parse().expect("member JID");
        let (actor, room_jid) = spawn_sync_test_room_with_seed(
            Arc::clone(&store),
            Some((member.clone(), Affiliation::Member)),
        )
        .await;
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());

        let worker = spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid.clone(),
            member.clone(),
            Affiliation::None,
            0,
            Arc::clone(&scheduler),
        );
        store.check_started.notified().await;
        sync_resolver_affiliation_on_rejection(
            Some(&actor),
            &scheduler,
            &room_jid,
            member,
            Affiliation::Outcast,
            0,
        );
        store.fail_next(RESOLVER_SYNC_MAX_ATTEMPTS);
        store.release_blocked_check.notify_one();

        for _ in 0..=RESOLVER_SYNC_MAX_ATTEMPTS {
            tokio::task::yield_now().await;
            tokio::time::advance(RESOLVER_SYNC_RETRY_BACKOFF).await;
        }
        worker.await.expect("bounded chained worker");

        assert_eq!(
            store.checks(),
            1 + RESOLVER_SYNC_MAX_ATTEMPTS,
            "queued work adopted after a successful repair must receive its own full retry budget"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_post_close_same_base_later_verdict_wins() {
        let store = SequencedOwnershipStore::new(0);
        let member: BareJid = "post-close@example.com".parse().expect("member JID");
        let (actor, room_jid) = spawn_sync_test_room_with_seed(
            Arc::clone(&store),
            Some((member.clone(), Affiliation::Member)),
        )
        .await;
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());

        spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid.clone(),
            member.clone(),
            Affiliation::None,
            0,
            Arc::clone(&scheduler),
        )
        .await
        .expect("first worker closes after applying its repair");
        spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid,
            member.clone(),
            Affiliation::Outcast,
            0,
            scheduler,
        )
        .await
        .expect("later same-snapshot worker drains");

        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("post-close repair result"),
            Affiliation::Outcast,
            "the later verdict must chain from the exact revision produced before worker closure"
        );
        assert_eq!(store.checks(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_unrelated_mutation_preserves_post_close_chain() {
        let store = SequencedOwnershipStore::new(0);
        let member: BareJid = "stale-chain@example.com".parse().expect("member JID");
        let unrelated: BareJid = "unrelated@example.com".parse().expect("member JID");
        let (actor, room_jid) = spawn_sync_test_room_with_seed(
            Arc::clone(&store),
            Some((member.clone(), Affiliation::Member)),
        )
        .await;
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());

        spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid.clone(),
            member.clone(),
            Affiliation::None,
            0,
            Arc::clone(&scheduler),
        )
        .await
        .expect("first worker closes after applying its repair");
        actor
            .ask(ChangeAffiliation {
                jid: unrelated,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("unrelated mutation succeeds");
        spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid,
            member.clone(),
            Affiliation::Outcast,
            0,
            scheduler,
        )
        .await
        .expect("stale chained worker terminates");

        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("affiliation after stale chained repair"),
            Affiliation::Outcast,
            "an unrelated member mutation must not invalidate the carried revision"
        );
        // Two sync workers check ownership once each; the interleaved
        // `ChangeAffiliation` carries a durable delta, so its ownership
        // authority is the in-transaction fence assert inside
        // `commit_room_mutation` (#1645) — the separate pre-commit probe
        // this count used to include no longer exists.
        assert_eq!(store.checks(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_out_of_order_stale_work_preserves_newer_completion_chain() {
        let store = SequencedOwnershipStore::new(0);
        let member: BareJid = "out-of-order@example.com".parse().expect("member JID");
        let unrelated: BareJid = "revision-source@example.com".parse().expect("member JID");
        let (actor, room_jid) = spawn_sync_test_room_with_seed(
            Arc::clone(&store),
            Some((member.clone(), Affiliation::Member)),
        )
        .await;
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());

        actor
            .ask(ChangeAffiliation {
                jid: unrelated,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("establish source revision one");
        spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid.clone(),
            member.clone(),
            Affiliation::None,
            1,
            Arc::clone(&scheduler),
        )
        .await
        .expect("newer-source worker records its completion chain");
        assert!(matches!(
            scheduler.schedule(
                &room_jid,
                &member,
                actor.id(),
                crate::server::routes::websocket::ResolverAffiliationSyncWork {
                    affiliation: Affiliation::Member,
                    expected_admission_revision: 0,
                },
            ),
            crate::server::routes::websocket::ResolverAffiliationSyncSchedule::Stale
        ));
        spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid,
            member.clone(),
            Affiliation::Outcast,
            1,
            scheduler,
        )
        .await
        .expect("matching newer-source worker consumes the preserved chain");

        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("affiliation after out-of-order repair"),
            Affiliation::Outcast,
            "stale-source work must not erase a newer completion chain"
        );
        // The leading `ChangeAffiliation` commits its durable delta under
        // the in-transaction fence assert (#1645) instead of a counted
        // pre-commit probe, leaving one counted check per sync worker.
        assert_eq!(
            store.checks(),
            2,
            "stale work must not consume ownership-check or worker capacity"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_retry_update_preserves_post_close_chain_revision() {
        let store = SequencedOwnershipStore::new(0);
        let member: BareJid = "retry-chain@example.com".parse().expect("member JID");
        let (actor, room_jid) = spawn_sync_test_room_with_seed(
            Arc::clone(&store),
            Some((member.clone(), Affiliation::Member)),
        )
        .await;
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());

        spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid.clone(),
            member.clone(),
            Affiliation::None,
            0,
            Arc::clone(&scheduler),
        )
        .await
        .expect("first worker closes after recording its revision chain");
        store.check_started.notified().await;
        store.fail_next(1);

        let retry = spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid.clone(),
            member.clone(),
            Affiliation::Member,
            0,
            Arc::clone(&scheduler),
        );
        store.check_started.notified().await;
        sync_resolver_affiliation_on_rejection(
            Some(&actor),
            &scheduler,
            &room_jid,
            member.clone(),
            Affiliation::Outcast,
            0,
        );
        tokio::task::yield_now().await;
        tokio::time::advance(RESOLVER_SYNC_RETRY_BACKOFF).await;
        retry.await.expect("same-source retry update drains");

        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("affiliation after retry update"),
            Affiliation::Outcast,
            "a transient retry must retain the completion-chain revision for a newer same-source verdict"
        );
        assert_eq!(store.checks(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_exhaustion_preserves_chain_for_pending_same_source_update() {
        let store = SequencedOwnershipStore::new(0);
        let member: BareJid = "exhausted-chain@example.com".parse().expect("member JID");
        let (actor, room_jid) = spawn_sync_test_room_with_seed(
            Arc::clone(&store),
            Some((member.clone(), Affiliation::Member)),
        )
        .await;
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());

        spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid.clone(),
            member.clone(),
            Affiliation::None,
            0,
            Arc::clone(&scheduler),
        )
        .await
        .expect("first worker records source-zero to revision-one chain");
        store.check_started.notified().await;
        store.fail_next(RESOLVER_SYNC_MAX_ATTEMPTS);
        store.block_check(4);

        let exhausted = spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid.clone(),
            member.clone(),
            Affiliation::Member,
            0,
            Arc::clone(&scheduler),
        );
        for expected_check in 2..=3 {
            store.check_started.notified().await;
            assert_eq!(store.checks(), expected_check);
            tokio::task::yield_now().await;
            tokio::time::advance(RESOLVER_SYNC_RETRY_BACKOFF).await;
        }
        store.check_started.notified().await;
        assert_eq!(store.checks(), 4, "third failed ownership check is blocked");
        sync_resolver_affiliation_on_rejection(
            Some(&actor),
            &scheduler,
            &room_jid,
            member.clone(),
            Affiliation::Outcast,
            0,
        );
        store.release_blocked_check.notify_one();
        exhausted
            .await
            .expect("pending same-source verdict drains after exhaustion");

        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("affiliation after ownership exhaustion"),
            Affiliation::Outcast,
            "non-mutating exhaustion must preserve the effective revision for pending same-source work"
        );
        assert_eq!(store.checks(), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_exhausted_worker_retains_chain_for_later_same_source_work() {
        let store = SequencedOwnershipStore::new(0);
        let member: BareJid = "closed-exhaustion@example.com".parse().expect("member JID");
        let (actor, room_jid) = spawn_sync_test_room_with_seed(
            Arc::clone(&store),
            Some((member.clone(), Affiliation::Member)),
        )
        .await;
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());

        spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid.clone(),
            member.clone(),
            Affiliation::None,
            0,
            Arc::clone(&scheduler),
        )
        .await
        .expect("first worker records source-zero to revision-one chain");
        store.check_started.notified().await;
        store.fail_next(RESOLVER_SYNC_MAX_ATTEMPTS);

        let exhausted = spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid.clone(),
            member.clone(),
            Affiliation::Member,
            0,
            Arc::clone(&scheduler),
        );
        for expected_check in 2..=4 {
            store.check_started.notified().await;
            assert_eq!(store.checks(), expected_check);
            if expected_check < 4 {
                tokio::task::yield_now().await;
                tokio::time::advance(RESOLVER_SYNC_RETRY_BACKOFF).await;
            }
        }
        exhausted
            .await
            .expect("exhausted worker closes without pending work");

        spawn_sync_worker_with_scheduler(
            actor.clone(),
            room_jid,
            member.clone(),
            Affiliation::Outcast,
            0,
            scheduler,
        )
        .await
        .expect("later same-source work consumes the retained chain");

        assert_eq!(
            actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("affiliation after exhausted worker closure"),
            Affiliation::Outcast,
            "non-mutating exhaustion must retain authorization after the worker closes"
        );
        assert_eq!(store.checks(), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_intrinsic_timeout_releases_global_worker_capacity() {
        let blocked_store = SequencedOwnershipStore::always_hanging();
        let (blocked_actor, blocked_room_jid) =
            spawn_sync_test_room(Arc::clone(&blocked_store)).await;
        let ready_store = SequencedOwnershipStore::new(0);
        let (ready_actor, ready_room_jid) = spawn_sync_test_room(Arc::clone(&ready_store)).await;
        let scheduler = Arc::new(
            crate::server::routes::websocket::ResolverAffiliationSyncScheduler::with_capacity(1),
        );
        let blocked_member: BareJid = "blocked@example.com".parse().expect("member JID");
        let ready_member: BareJid = "ready@example.com".parse().expect("member JID");

        sync_resolver_affiliation_on_rejection(
            Some(&blocked_actor),
            &scheduler,
            &blocked_room_jid,
            blocked_member,
            Affiliation::Member,
            0,
        );
        blocked_store.check_started.notified().await;
        sync_resolver_affiliation_on_rejection(
            Some(&ready_actor),
            &scheduler,
            &ready_room_jid,
            ready_member.clone(),
            Affiliation::Member,
            0,
        );
        tokio::task::yield_now().await;
        assert_eq!(
            ready_actor
                .ask(GetAffiliation {
                    jid: ready_member.clone(),
                })
                .await
                .expect("capacity-rejected actor remains responsive"),
            Affiliation::None,
        );

        for attempt in 1..=RESOLVER_SYNC_MAX_ATTEMPTS {
            tokio::time::advance(std::time::Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            if attempt < RESOLVER_SYNC_MAX_ATTEMPTS {
                tokio::time::advance(RESOLVER_SYNC_RETRY_BACKOFF).await;
                tokio::task::yield_now().await;
            }
        }
        assert_eq!(blocked_store.checks(), RESOLVER_SYNC_MAX_ATTEMPTS);
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }

        sync_resolver_affiliation_on_rejection(
            Some(&ready_actor),
            &scheduler,
            &ready_room_jid,
            ready_member.clone(),
            Affiliation::Member,
            0,
        );
        tokio::task::yield_now().await;
        assert_eq!(
            ready_actor
                .ask(GetAffiliation { jid: ready_member })
                .await
                .expect("released capacity admits the next worker"),
            Affiliation::Member,
        );
        assert_eq!(ready_store.checks(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn resolver_sync_scheduler_isolates_replacement_actor_incarnations() {
        let old_store = SequencedOwnershipStore::hanging();
        let (old_actor, room_jid) = spawn_sync_test_room(Arc::clone(&old_store)).await;
        let new_store = SequencedOwnershipStore::new(0);
        let (new_actor, new_room_jid) = spawn_sync_test_room(Arc::clone(&new_store)).await;
        assert_eq!(room_jid, new_room_jid);
        let scheduler =
            Arc::new(crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default());
        let member: BareJid = "replacement@example.com".parse().expect("member JID");

        sync_resolver_affiliation_on_rejection(
            Some(&old_actor),
            &scheduler,
            &room_jid,
            member.clone(),
            Affiliation::Member,
            0,
        );
        old_store.check_started.notified().await;
        sync_resolver_affiliation_on_rejection(
            Some(&new_actor),
            &scheduler,
            &room_jid,
            member.clone(),
            Affiliation::Member,
            0,
        );
        tokio::task::yield_now().await;

        assert_eq!(
            new_actor
                .ask(GetAffiliation {
                    jid: member.clone(),
                })
                .await
                .expect("replacement actor repair drains"),
            Affiliation::Member,
        );
        assert_eq!(new_store.checks(), 1);

        old_store.release_blocked_check.notify_one();
        assert_eq!(
            old_actor
                .ask(GetAffiliation { jid: member })
                .await
                .expect("old actor repair drains independently"),
            Affiliation::Member,
        );
        assert_eq!(old_store.checks(), 1);
    }
}

#[cfg(test)]
mod occupancy_projection_handler_tests {
    use super::*;
    use crate::server::routes::websocket::tests::{
        create_test_server_owner_session, create_test_websocket_state,
        create_test_websocket_state_with_sfu, register_test_connection, snapshot_room,
        RecordingSfu,
    };
    use kameo::actor::ActorRef;
    use std::str::FromStr;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;
    use waddle_xmpp::muc::durable::{
        MucDurableFuture, MucDurableStore, RoomClaimFenceContext, RoomLifecycleId, RoomProjection,
        RoomRevision,
    };
    use waddle_xmpp::muc::room_actor::{
        GetSnapshot, RestoreDurableRoomState, RoomActor, SealForDestroy, SyncResolverAffiliation,
    };
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};

    use crate::server::routes::websocket::tests::{
        current_test_occupancy_session as current_occupancy_session,
        record_test_occupancy_session as record_occupancy_session,
    };

    async fn handle_muc_join(
        state: &WebSocketState,
        domain: &str,
        room_jid: &BareJid,
        sender_jid: &FullJid,
        nick: &str,
        presence_show: Option<crate::notification_activity::NotificationPresenceShow>,
        authenticated_session: Option<&Session>,
    ) -> Vec<String> {
        super::handle_muc_join(
            state,
            domain,
            room_jid,
            sender_jid,
            nick,
            presence_show,
            MucJoinConnectionContext {
                registry_owner: None,
                occupancy_session: record_occupancy_session(state, sender_jid),
                authenticated_session,
            },
        )
        .await
    }

    async fn handle_muc_leave(
        state: &WebSocketState,
        room_jid: &BareJid,
        sender_jid: &FullJid,
        nick: &str,
        ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
    ) -> Vec<String> {
        super::handle_muc_leave(
            state,
            room_jid,
            sender_jid,
            nick,
            current_occupancy_session(state, sender_jid),
            ordered_relay_origin,
        )
        .await
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ProjectionFailureMode {
        Succeed,
        Join,
        Leave,
        /// The leave projection's durable commit outcome is ambiguous: the
        /// actor seals itself (the mutation may have committed) and answers
        /// `RoomActorError::RoomSealed`.
        LeaveAmbiguous,
        LeaveDelay(std::time::Duration),
    }

    struct ProjectionFailingStore {
        failure: Mutex<ProjectionFailureMode>,
        lifecycle: RoomLifecycleId,
        next_revision: Mutex<RoomRevision>,
        allowed_join_projections: Mutex<usize>,
    }

    impl ProjectionFailingStore {
        fn new(failure: ProjectionFailureMode) -> Arc<Self> {
            Arc::new(Self {
                failure: Mutex::new(failure),
                lifecycle: RoomLifecycleId::generate(),
                next_revision: Mutex::new(RoomRevision::initial()),
                allowed_join_projections: Mutex::new(0),
            })
        }

        fn with_join_failure_after(successful_join_projections: usize) -> Arc<Self> {
            Arc::new(Self {
                failure: Mutex::new(ProjectionFailureMode::Join),
                lifecycle: RoomLifecycleId::generate(),
                next_revision: Mutex::new(RoomRevision::initial()),
                allowed_join_projections: Mutex::new(successful_join_projections),
            })
        }

        fn set_failure(&self, failure: ProjectionFailureMode) {
            *self.failure.lock().expect("projection failure lock") = failure;
        }

        fn next_coordinates(&self) -> waddle_xmpp::muc::RoomCommittedCoordinates {
            let mut revision = self.next_revision.lock().expect("revision lock");
            let current = *revision;
            *revision = revision.next().expect("test revisions must not overflow");
            waddle_xmpp::muc::RoomCommittedCoordinates {
                lifecycle: self.lifecycle,
                revision: current,
            }
        }
    }

    impl MucDurableStore for ProjectionFailingStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, Option<waddle_xmpp::muc::durable::DurableRoomState>> {
            let validation = validate_projection_test_claim_fence(room_jid, fence);
            Box::pin(async move {
                validation?;
                Ok(None)
            })
        }

        fn commit_room_mutation<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
            intent: waddle_xmpp::muc::RoomDurableMutation,
            _effects: waddle_xmpp::muc::RoomMutationEffects,
        ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
            if let Err(error) = validate_projection_test_claim_fence(room_jid, fence) {
                return Box::pin(async move {
                    let _ = error;
                    Err(waddle_xmpp::muc::RoomCommitError::NotOwner)
                });
            }
            let failure = *self.failure.lock().expect("projection failure lock");
            let coordinates = self.next_coordinates();
            let allow_join_projection = {
                let mut remaining = self
                    .allowed_join_projections
                    .lock()
                    .expect("join projection allowance lock");
                if *remaining > 0 {
                    *remaining -= 1;
                    true
                } else {
                    false
                }
            };
            Box::pin(async move {
                match &intent {
                    waddle_xmpp::muc::RoomDurableMutation::Projection(
                        RoomProjection::OccupancyJoin { .. },
                    ) if failure == ProjectionFailureMode::Join && !allow_join_projection => {
                        return Err(waddle_xmpp::muc::RoomCommitError::OwnershipUnavailable);
                    }
                    waddle_xmpp::muc::RoomDurableMutation::Projection(
                        RoomProjection::OccupancyLeave { .. },
                    ) if failure == ProjectionFailureMode::Leave => {
                        return Err(waddle_xmpp::muc::RoomCommitError::OwnershipUnavailable);
                    }
                    waddle_xmpp::muc::RoomDurableMutation::Projection(
                        RoomProjection::OccupancyLeave { .. },
                    ) if failure == ProjectionFailureMode::LeaveAmbiguous => {
                        return Err(waddle_xmpp::muc::RoomCommitError::CommitOutcomeUnknown);
                    }
                    waddle_xmpp::muc::RoomDurableMutation::Projection(
                        RoomProjection::OccupancyLeave { .. },
                    ) => {
                        if let ProjectionFailureMode::LeaveDelay(delay) = failure {
                            tokio::time::sleep(delay).await;
                        }
                    }
                    _ if failure == ProjectionFailureMode::Succeed => {}
                    _ => {}
                }
                Ok(waddle_xmpp::muc::RoomCommitOutcome {
                    coordinates,
                    reservation: None,
                })
            })
        }

        fn check_fenced_fanout<'a>(&'a self, _room_jid: &'a BareJid) -> MucDurableFuture<'a, bool> {
            Box::pin(async { Ok(true) })
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, bool> {
            let matches = fence == &projection_test_claim_fence(room_jid);
            Box::pin(async move { Ok(matches) })
        }
    }

    fn projection_test_claim_fence(room_jid: &BareJid) -> RoomClaimFenceContext {
        RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, room_jid.to_string()),
            NodeIdentity::local(),
            ClaimEpoch(1),
        )
    }

    fn validate_projection_test_claim_fence(
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), waddle_xmpp::XmppError> {
        if fence == &projection_test_claim_fence(room_jid) {
            Ok(())
        } else {
            Err(waddle_xmpp::XmppError::internal(
                "unexpected room claim fence in occupancy projection handler test",
            ))
        }
    }

    async fn create_room_with_projection_store(
        state: &WebSocketState,
        room_jid: &BareJid,
        store: Arc<ProjectionFailingStore>,
    ) -> ActorRef<RoomActor> {
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "test-waddle".to_string(),
                channel_id: "test-channel".to_string(),
                config: waddle_xmpp::muc::RoomConfig {
                    members_only: false,
                    ..Default::default()
                },
            })
            .await
            .expect("create room");
        actor
            .ask(RestoreDurableRoomState {
                store: store as Arc<dyn MucDurableStore>,
                claim_fence: projection_test_claim_fence(room_jid),
            })
            .await
            .expect("install projection test durable store");
        actor
    }

    #[tokio::test]
    async fn join_projection_commit_failure_bounces_resource_constraint_without_broadcast() {
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "owner").await;
        let room_jid: BareJid = "join-projection-failure@muc.example.com"
            .parse()
            .expect("room jid");
        let owner: FullJid = "owner@example.com/web".parse().expect("owner jid");
        let bob: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let (bob_tx, mut bob_rx) = mpsc::channel(8);

        create_room_with_projection_store(
            state.as_ref(),
            &room_jid,
            ProjectionFailingStore::with_join_failure_after(2),
        )
        .await;
        register_test_connection(state.as_ref(), &bob, bob_tx).await;

        let owner_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &owner,
            "owner",
            None,
            Some(&owner_session),
        )
        .await;
        assert!(
            owner_join
                .iter()
                .any(|frame| frame.contains("status code='110'")),
            "owner join must succeed so the room exists for the failed admission: {owner_join:?}"
        );
        let bob_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &bob,
            "bob",
            None,
            None,
        )
        .await;
        assert!(
            bob_join
                .iter()
                .any(|frame| frame.contains("status code='110'")),
            "bob join must succeed so another occupant could observe a stray broadcast"
        );
        while bob_rx.try_recv().is_ok() {}

        let responses = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            None,
        )
        .await;

        assert_eq!(responses.len(), 1);
        let denial = Element::from_str(&responses[0]).expect("join denial XML");
        assert_eq!(denial.name(), "presence");
        assert_eq!(denial.attr("type"), Some("error"));
        assert!(
            responses[0].contains("resource-constraint"),
            "projection refusal must surface the generic wait/resource-constraint bounce"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "other occupants must not see a rejected join projection"
        );
        let room = snapshot_room(state.as_ref(), &room_jid).await.room;
        assert!(
            room.find_nick_by_real_jid(&alice).is_none(),
            "the failed join must not materialize the rejected occupant"
        );
    }

    #[tokio::test]
    async fn explicit_leave_deferred_bounces_resource_constraint_and_keeps_occupancy() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "leave-projection-deferred@muc.example.com"
            .parse()
            .expect("room jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let (bob_tx, mut bob_rx) = mpsc::channel(8);

        create_room_with_projection_store(
            state.as_ref(),
            &room_jid,
            ProjectionFailingStore::new(ProjectionFailureMode::Leave),
        )
        .await;
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        let alice_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            Some(&alice_session),
        )
        .await;
        assert!(
            alice_join
                .iter()
                .any(|frame| frame.contains("status code='110'")),
            "alice join must succeed before the deferred leave: {alice_join:?}"
        );
        let bob_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &bob,
            "bob",
            None,
            None,
        )
        .await;
        assert!(
            bob_join
                .iter()
                .any(|frame| frame.contains("status code='110'")),
            "bob join must succeed before observing the failed leave"
        );
        while bob_rx.try_recv().is_ok() {}

        let responses = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice", None).await;

        assert_eq!(responses.len(), 1);
        let denial = Element::from_str(&responses[0]).expect("leave denial XML");
        assert_eq!(denial.name(), "presence");
        assert_eq!(denial.attr("type"), Some("error"));
        assert!(
            responses[0].contains("resource-constraint"),
            "a deferred explicit leave must bounce with wait/resource-constraint"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "remaining occupants must not see unavailable when projection commit defers"
        );
        assert!(
            recorder.snapshot().is_empty(),
            "the explicit-leave failure path must not unregister SFU participation"
        );
        let room = snapshot_room(state.as_ref(), &room_jid).await.room;
        assert_eq!(
            room.find_nick_by_real_jid(&alice),
            Some("alice"),
            "deferred leave keeps the occupant in the room"
        );
    }

    #[tokio::test]
    async fn explicit_leave_not_occupant_keeps_todays_self_unavailable() {
        let state = create_test_websocket_state().await;
        let room_jid: BareJid = "leave-not-occupant@muc.example.com"
            .parse()
            .expect("room jid");
        let bob: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "test-waddle".to_string(),
                channel_id: "test-channel".to_string(),
                config: waddle_xmpp::muc::RoomConfig {
                    members_only: false,
                    ..Default::default()
                },
            })
            .await
            .expect("create room");
        let bob_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &bob,
            "bob",
            None,
            None,
        )
        .await;
        assert!(
            bob_join
                .iter()
                .any(|frame| frame.contains("status code='110'")),
            "seed occupant join must succeed: {bob_join:?}"
        );

        let responses = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice", None).await;

        assert_eq!(responses.len(), 1);
        let unavailable = Element::from_str(&responses[0]).expect("self unavailable XML");
        assert_eq!(unavailable.name(), "presence");
        assert_eq!(unavailable.attr("type"), Some("unavailable"));
        assert!(unavailable
            .get_child("error", waddle_xmpp::ns::JABBER_CLIENT)
            .is_none());
        let snapshot = actor.ask(GetSnapshot).await.expect("room snapshot");
        assert_eq!(snapshot.room.find_nick_by_real_jid(&bob), Some("bob"));
        assert!(snapshot.room.find_nick_by_real_jid(&alice).is_none());
    }

    #[tokio::test]
    async fn suppressed_leave_is_acknowledged_and_leaves_no_receipt() {
        let state = create_test_websocket_state().await;
        let room_jid: BareJid = "suppressed-store-less@muc.example.com"
            .parse()
            .expect("room jid");
        let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "test-waddle".to_string(),
                channel_id: "test-channel".to_string(),
                config: waddle_xmpp::muc::RoomConfig {
                    members_only: false,
                    ..Default::default()
                },
            })
            .await
            .expect("create store-less room");
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        let alice_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            Some(&alice_session),
        )
        .await;
        assert!(alice_join
            .iter()
            .any(|frame| frame.contains("status code='110'")));
        actor
            .ask(SyncResolverAffiliation {
                jid: alice.to_bare(),
                affiliation: Affiliation::Owner,
                expected_admission_revision: actor
                    .ask(GetSnapshot)
                    .await
                    .expect("snapshot before owner grant")
                    .admission_revision,
            })
            .await
            .expect("grant creator affiliation");
        let bob_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &bob,
            "bob",
            None,
            None,
        )
        .await;
        assert!(bob_join
            .iter()
            .any(|frame| frame.contains("status code='110'")));
        while bob_rx.try_recv().is_ok() {}
        actor
            .ask(SealForDestroy {
                attempt: waddle_xmpp::muc::DestroyAttemptId::generate(),
            })
            .await
            .expect("seal destroying");

        let frames = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice", None).await;

        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("type='unavailable'"), "{frames:?}");
        assert!(frames[0].contains("status code='110'"), "{frames:?}");
        assert!(frames[0].contains("affiliation='owner'"), "{frames:?}");
        assert!(
            bob_rx.try_recv().is_err(),
            "suppressed leave must not fan out"
        );
        // The acknowledgement is delivered by a detached task after the handler
        // returned: wait for it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let receipts = actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot after suppressed leave")
                .departures
                .receipts
                .len();
            if receipts == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the explicit caller acknowledges the suppressed receipt"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// #1647 (codex round 22): a sealed answer can follow an ENQUEUED leave
    /// whose durable projection was ambiguous — the actor self-seals because
    /// the mutation may have committed. The handler must retain a
    /// `ConfirmRetired` replay responsibility instead of dropping the only
    /// owner of the departure's effects.
    #[tokio::test]
    async fn explicit_leave_sealed_after_ambiguous_commit_retains_confirm_retired() {
        let state = create_test_websocket_state().await;
        let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "leave-ambiguous-sealed@muc.example.com"
            .parse()
            .expect("room jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        create_room_with_projection_store(
            state.as_ref(),
            &room_jid,
            ProjectionFailingStore::new(ProjectionFailureMode::LeaveAmbiguous),
        )
        .await;
        let alice_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            Some(&alice_session),
        )
        .await;
        assert!(alice_join
            .iter()
            .any(|frame| frame.contains("status code='110'")));

        let responses = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice", None).await;
        assert_eq!(responses.len(), 1);
        assert!(
            responses[0].contains("type='error'"),
            "the ambiguous-sealed leave bounces wait-class: {responses:?}"
        );

        let retained = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_due(std::time::Instant::now() + std::time::Duration::from_secs(600));
        assert_eq!(
            retained.len(),
            1,
            "the sealed leave must leave exactly one replay responsibility"
        );
        assert!(
            matches!(
                &retained[0].item,
                crate::server::routes::websocket::LocalDepartureItem::ConfirmRetired {
                    room,
                    jid,
                    cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
                    ..
                } if room == &room_jid && jid == &alice
            ),
            "the retained item follows THIS actor's retirement: {:?}",
            retained[0].item
        );
    }

    /// #1647 (codex round 22): the suppressed-leave self-echo must use the
    /// actor's authoritative nickname, not whatever nick the request's `to`
    /// JID carried.
    #[tokio::test]
    async fn suppressed_leave_echo_uses_the_actor_nick_not_the_request_nick() {
        let state = create_test_websocket_state().await;
        let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "suppressed-wrong-nick@muc.example.com"
            .parse()
            .expect("room jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "test-waddle".to_string(),
                channel_id: "test-channel".to_string(),
                config: waddle_xmpp::muc::RoomConfig {
                    members_only: false,
                    ..Default::default()
                },
            })
            .await
            .expect("create store-less room");
        let alice_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            Some(&alice_session),
        )
        .await;
        assert!(alice_join
            .iter()
            .any(|frame| frame.contains("status code='110'")));
        actor
            .ask(SealForDestroy {
                attempt: waddle_xmpp::muc::DestroyAttemptId::generate(),
            })
            .await
            .expect("seal destroying");

        let frames = handle_muc_leave(state.as_ref(), &room_jid, &alice, "impostor", None).await;
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("status code='110'"), "{frames:?}");
        assert!(
            frames[0].contains(&format!("{room_jid}/alice")),
            "the echo is from the nick alice actually held: {frames:?}"
        );
        assert!(
            !frames[0].contains("impostor"),
            "the request's bogus nick never reaches the wire: {frames:?}"
        );
    }

    #[tokio::test]
    async fn explicit_leave_reply_timeout_bounces_then_janitor_echoes_self_unavailable_once() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "leave-reply-timeout@muc.example.com"
            .parse()
            .expect("room jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let (alice_tx, mut alice_rx) = mpsc::channel(8);
        let (bob_tx, mut bob_rx) = mpsc::channel(8);

        create_room_with_projection_store(
            state.as_ref(),
            &room_jid,
            ProjectionFailingStore::new(ProjectionFailureMode::LeaveDelay(
                std::time::Duration::from_secs(6),
            )),
        )
        .await;
        register_test_connection(state.as_ref(), &alice, alice_tx).await;
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        let alice_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            Some(&alice_session),
        )
        .await;
        assert!(alice_join
            .iter()
            .any(|frame| frame.contains("status code='110'")));
        waddle_sfu::SfuService::register_call_participant_with_session(
            recorder.as_ref(),
            &waddle_sfu::CallId::try_new(room_jid.to_string()).expect("call id"),
            &waddle_sfu::Identity::from_jid(alice.clone()),
            &waddle_sfu::SessionBinding::try_new("leave-timeout-session").expect("session binding"),
            current_occupancy_session(state.as_ref(), &alice),
        );
        let bob_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &bob,
            "bob",
            None,
            None,
        )
        .await;
        assert!(bob_join
            .iter()
            .any(|frame| frame.contains("status code='110'")));
        while alice_rx.try_recv().is_ok() {}
        while bob_rx.try_recv().is_ok() {}

        let responses = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice", None).await;

        assert_eq!(responses.len(), 1);
        assert!(
            responses[0].contains("resource-constraint"),
            "the timed-out explicit leave must bounce with wait/resource-constraint"
        );
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 1);
        assert!(alice_rx.try_recv().is_err());
        assert!(bob_rx.try_recv().is_err());

        tokio::time::sleep(std::time::Duration::from_secs(7)).await;
        crate::server::session_janitors::run_local_muc_departure_sweep(state.as_ref()).await;

        let alice_unavailable = alice_rx.try_recv().expect("self unavailable");
        let waddle_xmpp::Stanza::Presence(alice_presence) = alice_unavailable.stanza else {
            panic!("expected self unavailable presence");
        };
        assert_eq!(
            alice_presence.type_,
            xmpp_parsers::presence::Type::Unavailable
        );
        assert!(
            alice_presence.payloads.iter().any(|payload| {
                payload
                    .children()
                    .any(|child| child.name() == "status" && child.attr("code") == Some("110"))
            }),
            "self unavailable must carry MUC self-presence status 110"
        );

        let bob_unavailable = bob_rx.try_recv().expect("remaining occupant unavailable");
        let waddle_xmpp::Stanza::Presence(bob_presence) = bob_unavailable.stanza else {
            panic!("expected fan-out unavailable presence");
        };
        assert_eq!(
            bob_presence.type_,
            xmpp_parsers::presence::Type::Unavailable
        );
        assert!(
            alice_rx.try_recv().is_err(),
            "self echo must happen exactly once"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "fan-out must happen exactly once"
        );
        assert_eq!(
            recorder.snapshot().len(),
            1,
            "SFU unregister must happen exactly once"
        );
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 0);
    }

    #[tokio::test]
    async fn explicit_leave_retry_after_rejoin_does_not_evict_the_new_session() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "leave-timeout-rejoin@muc.example.com"
            .parse()
            .expect("room jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let (alice_tx, mut alice_rx) = mpsc::channel(8);
        let (bob_tx, mut bob_rx) = mpsc::channel(8);

        create_room_with_projection_store(
            state.as_ref(),
            &room_jid,
            ProjectionFailingStore::new(ProjectionFailureMode::LeaveDelay(
                std::time::Duration::from_secs(6),
            )),
        )
        .await;
        register_test_connection(state.as_ref(), &alice, alice_tx).await;
        register_test_connection(state.as_ref(), &bob, bob_tx).await;

        let alice_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            Some(&alice_session.clone()),
        )
        .await;
        assert!(alice_join
            .iter()
            .any(|frame| frame.contains("status code='110'")));
        let bob_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &bob,
            "bob",
            None,
            None,
        )
        .await;
        assert!(bob_join
            .iter()
            .any(|frame| frame.contains("status code='110'")));
        while alice_rx.try_recv().is_ok() {}
        while bob_rx.try_recv().is_ok() {}

        let leave_frames = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice", None).await;
        assert_eq!(leave_frames.len(), 1);
        assert!(
            leave_frames[0].contains("resource-constraint"),
            "the timed-out explicit leave must bounce first: {leave_frames:?}"
        );
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 1);
        assert!(alice_rx.try_recv().is_err());
        assert!(bob_rx.try_recv().is_err());
        assert!(
            recorder.snapshot().is_empty(),
            "the timed-out first attempt must not unregister SFU participation"
        );

        tokio::time::sleep(std::time::Duration::from_secs(7)).await;

        let rejoin = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            Some(&alice_session),
        )
        .await;
        assert!(
            rejoin
                .iter()
                .any(|frame| frame.contains("status code='110'")),
            "alice must be able to rejoin before the janitor retry: {rejoin:?}"
        );
        while alice_rx.try_recv().is_ok() {}
        while bob_rx.try_recv().is_ok() {}

        crate::server::session_janitors::run_local_muc_departure_sweep(state.as_ref()).await;

        assert!(
            alice_rx.try_recv().is_err(),
            "the superseded retry must not self-echo unavailable to the new session"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "the superseded retry must not fan out unavailable to other occupants"
        );
        assert!(
            recorder.snapshot().is_empty(),
            "the superseded retry must not unregister the rejoined SFU participant"
        );
        let room = snapshot_room(state.as_ref(), &room_jid).await.room;
        assert_eq!(
            room.find_nick_by_real_jid(&alice),
            Some("alice"),
            "alice must remain an occupant after the superseded retry"
        );
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 0);
    }

    #[tokio::test]
    async fn explicit_leave_timeout_then_rejoin_then_leave_then_old_retry_emits_no_stale_presence()
    {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "leave-timeout-rejoin-leave@muc.example.com"
            .parse()
            .expect("room jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob: FullJid = "bob@example.com/web".parse().expect("bob jid");
        let (alice_tx, mut alice_rx) = mpsc::channel(8);
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        let store = ProjectionFailingStore::new(ProjectionFailureMode::LeaveDelay(
            std::time::Duration::from_secs(6),
        ));

        create_room_with_projection_store(state.as_ref(), &room_jid, store.clone()).await;
        register_test_connection(state.as_ref(), &alice, alice_tx).await;
        register_test_connection(state.as_ref(), &bob, bob_tx).await;

        let alice_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            Some(&alice_session.clone()),
        )
        .await;
        assert!(alice_join
            .iter()
            .any(|frame| frame.contains("status code='110'")));
        let bob_join = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &bob,
            "bob",
            None,
            None,
        )
        .await;
        assert!(bob_join
            .iter()
            .any(|frame| frame.contains("status code='110'")));
        while alice_rx.try_recv().is_ok() {}
        while bob_rx.try_recv().is_ok() {}

        let first_leave = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice", None).await;
        assert_eq!(first_leave.len(), 1);
        assert!(
            first_leave[0].contains("resource-constraint"),
            "the timed-out first leave must bounce first: {first_leave:?}"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            1,
            "the timed-out first leave must stay queued for retry"
        );
        assert!(alice_rx.try_recv().is_err());
        assert!(bob_rx.try_recv().is_err());
        assert!(
            recorder.snapshot().is_empty(),
            "the timed-out first leave must not unregister SFU participation"
        );

        tokio::time::sleep(std::time::Duration::from_secs(7)).await;

        let rejoin = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            Some(&alice_session),
        )
        .await;
        assert!(
            rejoin
                .iter()
                .any(|frame| frame.contains("status code='110'")),
            "alice must be able to rejoin before replaying the old leave: {rejoin:?}"
        );
        waddle_sfu::SfuService::register_call_participant_with_session(
            recorder.as_ref(),
            &waddle_sfu::CallId::try_new(room_jid.to_string()).expect("call id"),
            &waddle_sfu::Identity::from_jid(alice.clone()),
            &waddle_sfu::SessionBinding::try_new("replacement-leave-session")
                .expect("session binding"),
            current_occupancy_session(state.as_ref(), &alice),
        );
        while alice_rx.try_recv().is_ok() {}
        while bob_rx.try_recv().is_ok() {}

        store.set_failure(ProjectionFailureMode::Succeed);
        let second_leave = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice", None).await;
        assert_eq!(second_leave.len(), 1);
        assert!(
            second_leave[0].contains("type='unavailable'"),
            "the second leave must complete normally: {second_leave:?}"
        );
        assert!(
            second_leave[0].contains("status code='110'"),
            "the second leave must self-echo exactly once via the direct reply: {second_leave:?}"
        );

        let bob_unavailable = bob_rx.try_recv().expect("second leave fan-out");
        let waddle_xmpp::Stanza::Presence(bob_presence) = bob_unavailable.stanza else {
            panic!("expected fan-out unavailable presence");
        };
        assert_eq!(
            bob_presence.type_,
            xmpp_parsers::presence::Type::Unavailable
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "bob must receive exactly one unavailable from the second leave"
        );
        assert!(
            alice_rx.try_recv().is_err(),
            "the second leave must not enqueue an extra websocket self echo"
        );
        assert_eq!(
            recorder.snapshot().len(),
            1,
            "the successful second leave must unregister SFU participation exactly once"
        );
        // The newer leave's acknowledgement is delivered by a detached task:
        // wait until only the older timed-out leave remains queued.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.deps.protocol.pending_local_muc_departures.len() != 1 {
            assert!(
                std::time::Instant::now() < deadline,
                "only the older timed-out leave should remain queued before the janitor retry, \
                 got {}",
                state.deps.protocol.pending_local_muc_departures.len()
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        crate::server::session_janitors::run_local_muc_departure_sweep(state.as_ref()).await;

        assert!(
            alice_rx.try_recv().is_err(),
            "the old retry must not emit a stale self unavailable after the later successful leave"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "the old retry must not emit a stale unavailable to remaining occupants"
        );
        assert_eq!(
            recorder.snapshot().len(),
            1,
            "the old retry must not unregister the same SFU participant twice"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the stale retry must converge and drop the retained item"
        );
        let room = snapshot_room(state.as_ref(), &room_jid).await.room;
        assert!(
            room.find_nick_by_real_jid(&alice).is_none(),
            "alice must stay absent after the later successful leave and stale retry suppression"
        );
        assert_eq!(
            room.find_nick_by_real_jid(&bob),
            Some("bob"),
            "bob must remain present after suppressing the stale retry"
        );
    }

    #[tokio::test]
    async fn join_and_leave_requests_record_ws_boundary_duration() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state().await;
        let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
        let room_jid: BareJid = "ws-boundary-duration@muc.example.com"
            .parse()
            .expect("room jid");
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

        let joined = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice,
            "alice",
            None,
            Some(&owner_session),
        )
        .await;
        assert!(joined
            .iter()
            .any(|frame| frame.contains("status code='110'")));

        let left = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice", None).await;
        assert!(left
            .iter()
            .any(|frame| frame.contains("type='unavailable'")));

        assert!(metrics
            .histogram_count(
                "waddle.muc.occupancy.handler.duration",
                &[("op", "join_request")]
            )
            .is_some_and(|count| count >= 1));
        assert!(metrics
            .histogram_count(
                "waddle.muc.occupancy.handler.duration",
                &[("op", "leave_request")]
            )
            .is_some_and(|count| count >= 1));
    }
}

#[cfg(test)]
mod join_denial_telemetry_tests {
    use super::*;
    use waddle_xmpp::telemetry::attributes::{MucJoinDenyReason, StanzaErrorCondition};

    fn resource_constraint_denial(deny_reason: MucJoinDenyReason) -> JoinAdmissionDenial {
        JoinAdmissionDenial {
            condition: StanzaErrorCondition::ResourceConstraint,
            deny_reason,
            error_type: ErrorType::Wait,
            resolver_outcome: ManagedAdmissionResolverOutcome::NotConsulted,
            message: "This room's ownership is currently held by another node; please retry.",
        }
    }

    /// #1440: the wait-type ownership bounces used to answer the client
    /// with `<resource-constraint/>` and record nothing at all. Assert
    /// through the metric-reader seam that the denial is now counted
    /// under both its condition and its refusal site, so the distinct
    /// wait reasons stay distinguishable.
    #[tokio::test(flavor = "current_thread")]
    async fn resource_constraint_join_denial_is_counted_with_its_deny_reason() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let room_jid: BareJid = "wait-room@muc.example.com".parse().expect("room jid");
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

        let frames = deny_join_admission(
            &room_jid,
            &sender_jid,
            "alice",
            false,
            resource_constraint_denial(MucJoinDenyReason::OwnershipHeldByAnotherNode),
        );

        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("resource-constraint"), "{frames:?}");
        assert_eq!(
            metrics.counter_sum(
                "waddle.muc.admission.denied",
                &[
                    ("condition", "resource-constraint"),
                    ("deny_reason", "ownership_held_by_another_node"),
                ]
            ),
            Some(1),
            "the wait denial must increment the admission counter exactly once"
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.muc.admission.denied",
                &[("deny_reason", "ownership_reconciling")]
            ),
            Some(0),
            "a different wait reason must not be conflated with this one"
        );
        assert_eq!(
            metrics.metric_unit("waddle.muc.admission.denied"),
            Some("1".to_string()),
            "the admission denial counter must use the dimensionless unit"
        );
    }

    /// No actor runs inside this span, so it closes and exports
    /// deterministically with the condition attribute and ERROR status.
    #[tokio::test(flavor = "current_thread")]
    async fn internal_join_denial_exports_error_dispatch_span() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
        let room_jid: BareJid = "fail-room@muc.example.com".parse().expect("room jid");
        let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
        let dispatch_span =
            tracing::info_span!("xmpp.stanza.dispatch", condition = tracing::field::Empty);

        let frames = dispatch_span.in_scope(|| {
            deny_join_admission(
                &room_jid,
                &sender_jid,
                "alice",
                false,
                JoinAdmissionDenial {
                    condition: StanzaErrorCondition::InternalServerError,
                    deny_reason: MucJoinDenyReason::RoomLookup,
                    error_type: ErrorType::Wait,
                    resolver_outcome: ManagedAdmissionResolverOutcome::NotConsulted,
                    message: "Failed to look up room before join.",
                },
            )
        });
        drop(dispatch_span);

        assert!(frames[0].contains("internal-server-error"), "{frames:?}");
        assert_eq!(
            spans.attribute_of("xmpp.stanza.dispatch", "condition"),
            Some("internal-server-error".to_string())
        );
        assert!(spans.has_error_status("xmpp.stanza.dispatch"));
    }

    /// The wait-type infrastructure faults stay failed operations even
    /// though their condition is not `internal-server-error`; policy
    /// denials must not be promoted to span errors.
    #[test]
    fn only_server_side_faults_mark_the_join_span_as_failed() {
        assert_eq!(
            internal_join_failure_description(&resource_constraint_denial(
                MucJoinDenyReason::DurableRestorePending
            )),
            Some("MUC durable restore remained pending")
        );
        assert_eq!(
            internal_join_failure_description(&resource_constraint_denial(
                MucJoinDenyReason::OwnershipUnavailable
            )),
            Some("MUC ownership reconciliation unavailable")
        );
        assert_eq!(
            internal_join_failure_description(&resource_constraint_denial(
                MucJoinDenyReason::OwnershipHeldByAnotherNode
            )),
            None
        );
        assert_eq!(
            internal_join_failure_description(&JoinAdmissionDenial {
                condition: StanzaErrorCondition::RegistrationRequired,
                deny_reason: MucJoinDenyReason::MembershipRequired,
                error_type: ErrorType::Auth,
                resolver_outcome: ManagedAdmissionResolverOutcome::NoAffiliation,
                message: "Membership required to join this room.",
            }),
            None
        );
        assert_eq!(
            internal_join_failure_description(&JoinAdmissionDenial {
                condition: StanzaErrorCondition::InternalServerError,
                deny_reason: MucJoinDenyReason::RoomLookup,
                error_type: ErrorType::Wait,
                resolver_outcome: ManagedAdmissionResolverOutcome::NotConsulted,
                message: "Failed to look up room before join.",
            }),
            Some("MUC join admission failed internally")
        );
    }
}

mod implementation;
pub(crate) use implementation::deliver_room_presence_locally;
#[cfg(test)]
use implementation::deny_join_admission;
#[cfg(any(test, feature = "clustering"))]
pub use implementation::handle_muc_join;
pub use implementation::handle_muc_join_with_ordered_relay;
pub use implementation::handle_muc_leave;
#[cfg(test)]
use implementation::internal_join_failure_description;
pub(crate) use implementation::registered_remote_resource_delivery;
pub(crate) use implementation::registered_remote_resource_write_accepted_delivery;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::remote_muc_join_decision;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::remote_muc_leave_decision;
pub(crate) use implementation::route_room_presence_to_occupant;
#[cfg(test)]
use implementation::run_resolver_affiliation_sync_worker;
#[cfg(test)]
use implementation::sync_resolver_affiliation_on_rejection_with_registry;
pub(crate) use implementation::try_deliver_registered_remote_resource;
#[cfg(test)]
use implementation::JoinAdmissionDenial;
#[cfg(test)]
use implementation::ManagedAdmissionResolverOutcome;
#[cfg(any(test, feature = "clustering"))]
pub use implementation::MucJoinConnectionContext;
pub use implementation::MucJoinRequest;
pub(crate) use implementation::RegisteredRemoteDelivery;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::RemoteMucJoinDecision;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::RemoteMucLeaveDecision;
#[cfg(test)]
use implementation::RESOLVER_SYNC_MAILBOX_TIMEOUT;
#[cfg(test)]
use implementation::RESOLVER_SYNC_MAX_ATTEMPTS;
#[cfg(test)]
use implementation::RESOLVER_SYNC_RETRY_BACKOFF;
