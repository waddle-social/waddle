use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jid::{BareJid, FullJid};
use waddle_sfu::{CallId, Identity, LiveParticipant, ParticipantSid, RoomOccupancy, SfuService};
use waddle_xmpp::muc::room_actor::{GetSnapshot, RestoreDurableRoomState};
use waddle_xmpp::muc::{
    DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext, RoomCommitError,
    RoomCommitFuture, RoomDurableMutation, RoomMutationEffects,
};
use waddle_xmpp::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    StalePredicate,
};

use super::{
    create_test_server_owner_session, create_test_websocket_state_with_clustering, drain_due,
    fixture_config, get_room_actor, handle_muc_join_with_occupancy_session, occupant_generation,
    CallTeardownIntent, CallTeardownStatus, RecordingAdmin, TeardownTarget,
};

/// The actor's exact restore fence comes from the real claim store. Mutations
/// are not part of these read-authority tests and fail closed at this seam.
struct FenceRestoreStore(Arc<dyn ClaimStore>);

impl MucDurableStore for FenceRestoreStore {
    fn check_exact_claim_fence<'a>(
        &'a self,
        room: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        Box::pin(async move {
            if fence.entity != Entity::new(EntityType::RoomActor, room.to_string()) {
                return Ok(false);
            }
            self.0
                .fence(&fence.entity, &fence.owner, fence.epoch)
                .await
                .map_err(|error| waddle_xmpp::XmppError::internal(error.to_string()))
        })
    }

    fn load_room_state_fenced<'a>(
        &'a self,
        _room: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        Box::pin(async move {
            if self
                .0
                .fence(&fence.entity, &fence.owner, fence.epoch)
                .await
                .map_err(|error| waddle_xmpp::XmppError::internal(error.to_string()))?
            {
                Ok(None)
            } else {
                Err(waddle_xmpp::XmppError::internal("restore fence is stale"))
            }
        })
    }

    fn commit_room_mutation<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a RoomClaimFenceContext,
        _mutation: RoomDurableMutation,
        _effects: RoomMutationEffects,
    ) -> RoomCommitFuture<'a> {
        Box::pin(async { Err(RoomCommitError::OwnershipUnavailable) })
    }
}

async fn assert_lost_claim_prevents_removal(during_probe: bool, same_node: bool) {
    let claims = Arc::new(InProcessClaimStore::new());
    let me = NodeIdentity::new("retirement-node", "old-epoch");
    let successor_node = if same_node {
        me.clone()
    } else {
        NodeIdentity::new("retirement-node", "new-epoch")
    };
    let candidate = occupant_generation();
    let successor = occupant_generation();
    let jid: FullJid = "alice@example.com/web".parse().expect("participant JID");
    let room_jid: BareJid = "claim-fenced-retirement@muc.example.com"
        .parse()
        .expect("room JID");
    let call_id = CallId::try_new(room_jid.to_string()).expect("call ID");
    let identity = Identity::from_jid(jid.clone());
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let admin = Arc::new(RecordingAdmin {
        occupancy: Mutex::new(RoomOccupancy {
            waddle: vec![LiveParticipant {
                identity: identity.clone(),
                participant_sid: Some(ParticipantSid::try_new("PA_old").expect("SID")),
                media_generation: Some(candidate),
            }],
            foreign: 0,
        }),
        occupancy_entered: during_probe.then(|| entered.clone()),
        occupancy_gate: during_probe.then(|| gate.clone()),
        ..RecordingAdmin::default()
    });
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        admin.clone() as Arc<_>,
    ));
    let mut state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles {
            claim_store: Some(claims.clone() as Arc<_>),
            node_identity: Some(SharedNodeIdentity::new(me.clone())),
            ..crate::clustering::ClusteringHandles::default()
        },
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
    )
    .await;
    let protocol = &mut Arc::get_mut(&mut state)
        .expect("unique state")
        .deps
        .protocol;
    protocol.sfu = Some(sfu.clone() as Arc<dyn SfuService>);
    protocol.call_teardown_executor = Some(sfu.teardown_executor());
    let owner = create_test_server_owner_session(state.as_ref(), "alice").await;
    handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        (successor, Some(&owner)),
    )
    .await;
    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("cached live actor");
    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    let epoch = claims.acquire(&entity, &me).await.expect("room claim");
    actor
        .ask(RestoreDurableRoomState {
            store: Arc::new(FenceRestoreStore(claims.clone() as Arc<_>)),
            claim_fence: RoomClaimFenceContext::new(entity.clone(), me, epoch),
        })
        .await
        .expect("install exact actor fence");
    let store = &state.deps.protocol.call_teardown_outbox;
    let id = store
        .enqueue(CallTeardownIntent {
            call_id,
            target: TeardownTarget::DisplacedParticipant {
                identity: jid.clone(),
                participant_sid: None,
            },
            generation: None,
            occupant: Some(candidate),
            unbound_occupant: waddle_sfu::UnboundOccupantPolicy::Keep,
            room_sid: None,
            session: None,
        })
        .await
        .expect("retirement");
    if !during_probe {
        claims
            .steal_stale(&entity, epoch, StalePredicate::OwnerStale, &successor_node)
            .await
            .expect("replace claim");
    }
    let drain = tokio::spawn({
        let state = state.clone();
        async move { drain_due(&state, 8).await }
    });
    if during_probe {
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("provider probe started under valid authority");
        claims
            .steal_stale(&entity, epoch, StalePredicate::OwnerStale, &successor_node)
            .await
            .expect("replace claim during provider await");
        gate.add_permits(1);
    }
    let summary = drain.await.expect("drain task").expect("drain");
    assert_eq!(summary.requeued, 1);
    assert_eq!(
        admin.occupancy_calls.load(Ordering::SeqCst),
        usize::from(during_probe)
    );
    assert!(admin.remove_calls.lock().expect("calls").is_empty());
    assert_eq!(
        store.find(&id).await.expect("find").expect("queued").status,
        CallTeardownStatus::Queued
    );
    let snapshot = actor
        .ask(GetSnapshot)
        .await
        .expect("stale actor is still alive");
    assert_eq!(snapshot.room.session_generation(&jid), Some(successor));
    assert_eq!(snapshot.claim_fence.expect("actor fence").epoch, epoch);
}

#[tokio::test]
async fn cached_actor_cannot_authorize_retirement_after_its_node_epoch_loses_the_claim() {
    assert_lost_claim_prevents_removal(false, false).await;
}

#[tokio::test]
async fn cached_actor_cannot_authorize_retirement_after_same_node_claim_epoch_changes() {
    assert_lost_claim_prevents_removal(false, true).await;
}

#[tokio::test]
async fn retirement_rechecks_the_exact_room_claim_after_a_paused_provider_probe() {
    assert_lost_claim_prevents_removal(true, false).await;
}
