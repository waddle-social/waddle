use super::*;
use crate::server::routes::websocket::tests::create_test_websocket_state;
use waddle_xmpp::muc::room_registry_actor::CreateRoom;
use waddle_xmpp::muc::{RoomCommittedCoordinates, RoomConfig};
use waddle_xmpp::ownership::NodeIdentity;

struct AmbiguousAdminStore {
    state: std::sync::Mutex<waddle_xmpp::muc::durable::DurableRoomState>,
    commit_admin: bool,
    block_next_load: std::sync::atomic::AtomicBool,
    restore_started: tokio::sync::Notify,
    restore_continue: tokio::sync::Notify,
}

impl waddle_xmpp::muc::durable::MucDurableStore for AmbiguousAdminStore {
    fn load_room_state_fenced<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::durable::MucDurableFuture<
        'a,
        Option<waddle_xmpp::muc::durable::DurableRoomState>,
    > {
        let snapshot = self.state.lock().expect("durable state").clone();
        let blocked = self
            .block_next_load
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            if blocked {
                self.restore_started.notify_one();
                self.restore_continue.notified().await;
            }
            Ok(Some(snapshot))
        })
    }

    fn commit_room_mutation<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        intent: waddle_xmpp::muc::RoomDurableMutation,
        _effects: waddle_xmpp::muc::RoomMutationEffects,
    ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
        if !self.commit_admin
            && matches!(
                intent,
                waddle_xmpp::muc::RoomDurableMutation::AffiliationBatch(_)
            )
        {
            return Box::pin(async {
                Err(waddle_xmpp::muc::RoomCommitError::CommitOutcomeUnknown)
            });
        }
        let mut state = self.state.lock().expect("durable state");
        let previous = state.coordinates.expect("durable coordinates");
        let coordinates = RoomCommittedCoordinates {
            lifecycle: previous.lifecycle,
            revision: previous.revision.next().expect("next revision"),
        };
        state.coordinates = Some(coordinates);
        let ambiguous =
            if let waddle_xmpp::muc::RoomDurableMutation::AffiliationBatch(entries) = intent {
                for entry in entries {
                    state
                        .affiliations
                        .retain(|existing| existing.jid != entry.jid);
                    if let Some(affiliation) = entry.affiliation {
                        state.affiliations.push(
                            waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                                entry.jid,
                                affiliation,
                            ),
                        );
                    }
                }
                true
            } else {
                false
            };
        Box::pin(async move {
            if ambiguous {
                Err(waddle_xmpp::muc::RoomCommitError::CommitOutcomeUnknown)
            } else {
                Ok(waddle_xmpp::muc::RoomCommitOutcome {
                    coordinates,
                    reservation: None,
                })
            }
        })
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::durable::MucDurableFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}

#[tokio::test]
async fn xep0045_admin_recovery_preserves_final_roster_and_departure_retry() {
    assert_admin_recovery_preserves_final_roster(&[Affiliation::Member], false, true).await;
}

#[tokio::test]
async fn xep0045_admin_recovery_does_not_restore_banned_occupant() {
    assert_admin_recovery_preserves_final_roster(&[Affiliation::Outcast], false, true).await;
}

#[tokio::test]
async fn xep0045_admin_recovery_does_not_restore_revoked_member() {
    assert_admin_recovery_preserves_final_roster(&[Affiliation::None], true, true).await;
}

#[tokio::test]
async fn xep0045_admin_recovery_preserves_intermediate_ban_in_duplicate_target_batch() {
    assert_admin_recovery_preserves_final_roster(
        &[Affiliation::Outcast, Affiliation::Member],
        false,
        true,
    )
    .await;
}

#[tokio::test]
async fn xep0045_admin_recovery_keeps_occupant_when_ambiguous_ban_did_not_commit() {
    assert_admin_recovery_preserves_final_roster(&[Affiliation::Outcast], false, false).await;
}

async fn assert_admin_recovery_preserves_final_roster(
    affiliation_changes: &[Affiliation],
    members_only: bool,
    commit_admin: bool,
) {
    let new_affiliation = *affiliation_changes
        .last()
        .expect("admin affiliation change");
    use waddle_xmpp::muc::room_actor::{
        Join, LeaveAttemptId, LeaveByRealJid, LeaveDisposition, LeaveOrigin, LeaveSessionSelector,
    };
    use waddle_xmpp::muc::room_registry_actor::{GetOrCreateRoom, GetRoom, WireClusteringClaims};
    use waddle_xmpp::ownership::{InProcessClaimStore, SharedNodeIdentity};

    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "admin-atomic-recovery@muc.example.com"
        .parse()
        .expect("room");
    let owner: FullJid = "owner@example.com/web".parse().expect("owner");
    let departed: FullJid = "departed@example.com/web".parse().expect("departed");
    let later: FullJid = "later@example.com/web".parse().expect("later join");
    let previous_affiliation = if new_affiliation == Affiliation::Member {
        Affiliation::None
    } else {
        Affiliation::Member
    };
    let store = std::sync::Arc::new(AmbiguousAdminStore {
        state: std::sync::Mutex::new(waddle_xmpp::muc::durable::DurableRoomState {
            coordinates: Some(RoomCommittedCoordinates {
                lifecycle: waddle_xmpp::muc::RoomLifecycleId::generate(),
                revision: waddle_xmpp::muc::RoomRevision::initial(),
            }),
            config_coordinates: None,
            waddle_id: "waddle".to_owned(),
            channel_id: "channel".to_owned(),
            config: RoomConfig {
                persistent: true,
                members_only,
                ..Default::default()
            },
            subject: None,
            affiliations: vec![
                waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                    owner.to_bare(),
                    Affiliation::Owner,
                ),
                waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                    departed.to_bare(),
                    Affiliation::Member,
                ),
                waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                    later.to_bare(),
                    previous_affiliation,
                ),
            ],
        }),
        commit_admin,
        block_next_load: std::sync::atomic::AtomicBool::new(false),
        restore_started: tokio::sync::Notify::new(),
        restore_continue: tokio::sync::Notify::new(),
    });
    state
        .deps
        .protocol
        .room_registry
        .ask(WireClusteringClaims {
            claim_store: std::sync::Arc::new(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            durable_store: Some(store.clone()),
            rollout_backoff: None,
        })
        .await
        .expect("wire durable store");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(GetOrCreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "waddle".to_owned(),
            channel_id: "channel".to_owned(),
            config: RoomConfig::default(),
        })
        .await
        .expect("restore room")
        .actor_ref;
    for (nick, jid, affiliation, role) in [
        (
            "owner",
            &owner,
            Affiliation::Owner,
            waddle_xmpp::Role::Moderator,
        ),
        (
            "departed",
            &departed,
            Affiliation::Member,
            waddle_xmpp::Role::Participant,
        ),
    ] {
        actor
            .ask(Join {
                nick: nick.to_owned(),
                real_jid: jid.clone(),
                affiliation,
                role,
            })
            .await
            .expect("join");
    }
    let before = actor.ask(GetSnapshot).await.expect("pre-ask snapshot");
    actor
        .ask(Join {
            nick: "later".to_owned(),
            real_jid: later.clone(),
            affiliation: previous_affiliation,
            role: waddle_xmpp::Role::Participant,
        })
        .await
        .expect("later join");
    let attempt = LeaveAttemptId::generate();
    assert!(matches!(
        actor
            .ask(LeaveByRealJid {
                sender_jid: departed.clone(),
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: LeaveOrigin::Fresh,
            })
            .await
            .expect("departure"),
        LeaveDisposition::Left(_)
    ));
    let items: Vec<_> = affiliation_changes
        .iter()
        .map(|affiliation| AdminItem {
            jid: Some(later.to_bare()),
            nick: None,
            affiliation: Some(*affiliation),
            role: None,
            reason: None,
        })
        .collect();
    assert!(matches!(
        actor
            .ask(ApplyAdminItems {
                sender_jid: owner.clone(),
                sender_affiliation: Affiliation::Owner,
                sender_role: waddle_xmpp::Role::Moderator,
                items: items.clone(),
            })
            .await,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::CommitOutcomeUnknown
        ))
    ));
    let sealed = actor.ask(GetSnapshot).await.expect("sealed snapshot");
    store
        .block_next_load
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let recovery = tokio::spawn({
        let state = state.clone();
        let room = room_jid.clone();
        let before = before.clone();
        let actor = actor.clone();
        let items = items.clone();
        let owner = owner.clone();
        async move {
            recover_admin_result_after_actor_failure(
                state.as_ref(),
                &room,
                &before,
                &actor,
                &items,
                &owner,
            )
            .await
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store.restore_started.notified(),
    )
    .await
    .expect("successor restore started");
    let lookup_room = room_jid.clone();
    let lookup_registry = state.deps.protocol.room_registry.clone();
    let lookup = async move {
        lookup_registry
            .ask(GetRoom {
                room_jid: lookup_room,
            })
            .await
    };
    tokio::pin!(lookup);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut lookup)
            .await
            .is_err(),
        "lookup must wait for the successor instead of observing an absent room"
    );
    store.restore_continue.notify_one();
    let visible = lookup
        .await
        .expect("lookup across handoff")
        .expect("handoff successor");
    let (outcome, _) = recovery.await.expect("recovery task");
    if commit_admin {
        assert!(matches!(outcome, AdminReconciliationOutcome::Committed(_)));
    } else {
        assert!(matches!(outcome, AdminReconciliationOutcome::NotCommitted));
    }
    let successor = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("lookup successor")
        .expect("successor published");
    assert_ne!(successor.id(), actor.id());
    assert_eq!(successor.id(), visible.id());
    let restored = successor.ask(GetSnapshot).await.expect("restored snapshot");
    assert!(restored.room.get_occupant("owner").is_some());
    assert_eq!(
        restored.room.get_occupant("later").is_some(),
        !commit_admin
            || !affiliation_changes
                .iter()
                .any(|affiliation| *affiliation == Affiliation::Outcast
                    || (members_only && *affiliation == Affiliation::None))
    );
    assert!(restored.room.get_occupant("departed").is_none());
    assert_eq!(
        restored.room.get_affiliation(&later.to_bare()),
        if commit_admin {
            new_affiliation
        } else {
            previous_affiliation
        }
    );
    assert_eq!(restored.occupancy_revision, sealed.occupancy_revision);
    assert_eq!(restored.departures.receipts.len(), 1);
    assert!(matches!(
        successor
            .ask(LeaveByRealJid {
                sender_jid: departed,
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: LeaveOrigin::RetainedRetry,
            })
            .await
            .expect("departure retry"),
        LeaveDisposition::Left(_)
    ));
    // A delayed competing recovery must follow the published successor,
    // preserving a join that was never in either predecessor snapshot.
    let newest: FullJid = "newest@example.com/web".parse().expect("newest");
    successor
        .ask(Join {
            nick: "newest".to_owned(),
            real_jid: newest.clone(),
            affiliation: Affiliation::Member,
            role: waddle_xmpp::Role::Participant,
        })
        .await
        .expect("join on successor");
    let _ = recover_admin_result_after_actor_failure(
        state.as_ref(),
        &room_jid,
        &before,
        &actor,
        &items,
        &owner,
    )
    .await;
    let current = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom { room_jid })
        .await
        .expect("current lookup")
        .expect("current actor");
    assert_eq!(current.id(), successor.id());
    assert!(current
        .ask(GetSnapshot)
        .await
        .expect("current snapshot")
        .room
        .get_occupant("newest")
        .is_some());
}

#[tokio::test]
async fn xep0045_admin_recovery_does_not_create_missing_room() {
    use waddle_xmpp::muc::room_registry_actor::{DemoteRoomIfExactActor, GetRoom};
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "admin-gone@muc.example.com".parse().expect("room");
    let owner: FullJid = "owner@example.com/web".parse().expect("owner");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "waddle".to_owned(),
            channel_id: "channel".to_owned(),
            config: RoomConfig::default(),
        })
        .await
        .expect("room");
    let before = actor.ask(GetSnapshot).await.expect("snapshot");
    state
        .deps
        .protocol
        .room_registry
        .ask(DemoteRoomIfExactActor {
            room_jid: room_jid.clone(),
            actor_ref: actor.clone(),
        })
        .await
        .expect("retire actor");
    let (outcome, _) = recover_admin_result_after_actor_failure(
        state.as_ref(),
        &room_jid,
        &before,
        &actor,
        &[],
        &owner,
    )
    .await;
    assert!(matches!(outcome, AdminReconciliationOutcome::Inconclusive));
    assert!(state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom { room_jid })
        .await
        .expect("lookup")
        .is_none());
}
