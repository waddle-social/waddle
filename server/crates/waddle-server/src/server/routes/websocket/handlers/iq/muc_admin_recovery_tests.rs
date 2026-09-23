use super::*;
use crate::server::routes::websocket::tests::create_test_websocket_state;
use waddle_xmpp::muc::room_registry_actor::CreateRoom;
use waddle_xmpp::muc::{RoomCommittedCoordinates, RoomConfig};
use waddle_xmpp::ownership::NodeIdentity;

struct AmbiguousAdminStore {
    state: std::sync::Mutex<waddle_xmpp::muc::durable::DurableRoomState>,
    commit_admin: bool,
    receipts: std::sync::Mutex<
        std::collections::HashMap<
            (BareJid, waddle_xmpp::muc::AdminMutationId),
            RoomCommittedCoordinates,
        >,
    >,
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
        room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        intent: waddle_xmpp::muc::RoomDurableMutation,
        effects: waddle_xmpp::muc::RoomMutationEffects,
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
        if let Some(attempt) = effects.admin_mutation_id() {
            self.receipts
                .lock()
                .expect("admin receipts")
                .insert((room.clone(), attempt), coordinates);
        }
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

    fn load_admin_mutation_receipt<'a>(
        &'a self,
        room: &'a BareJid,
        attempt: waddle_xmpp::muc::AdminMutationId,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, Option<RoomCommittedCoordinates>> {
        let receipt = self
            .receipts
            .lock()
            .expect("admin receipts")
            .get(&(room.clone(), attempt))
            .copied();
        Box::pin(async move { Ok(receipt) })
    }

    fn delete_admin_mutation_receipt<'a>(
        &'a self,
        room: &'a BareJid,
        attempt: waddle_xmpp::muc::AdminMutationId,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
        self.receipts
            .lock()
            .expect("admin receipts")
            .remove(&(room.clone(), attempt));
        Box::pin(async { Ok(()) })
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
    assert_admin_recovery_preserves_final_roster(&[Affiliation::Member], false, true, false, None)
        .await;
}

#[tokio::test]
async fn xep0045_admin_recovery_does_not_restore_banned_occupant() {
    assert_admin_recovery_preserves_final_roster(&[Affiliation::Outcast], false, true, false, None)
        .await;
}

#[tokio::test]
async fn xep0045_admin_recovery_does_not_restore_revoked_member() {
    assert_admin_recovery_preserves_final_roster(&[Affiliation::None], true, true, false, None)
        .await;
}

#[tokio::test]
async fn xep0045_admin_recovery_preserves_intermediate_ban_in_duplicate_target_batch() {
    assert_admin_recovery_preserves_final_roster(
        &[Affiliation::Outcast, Affiliation::Member],
        false,
        true,
        false,
        None,
    )
    .await;
}

#[tokio::test]
async fn xep0045_admin_recovery_keeps_occupant_when_ambiguous_ban_did_not_commit() {
    assert_admin_recovery_preserves_final_roster(
        &[Affiliation::Outcast],
        false,
        false,
        false,
        None,
    )
    .await;
}

#[tokio::test]
async fn xep0045_admin_recovery_timeout_retires_unresponsive_exact_actor() {
    assert_admin_recovery_preserves_final_roster(&[Affiliation::Member], false, true, true, None)
        .await;
}

#[tokio::test]
async fn xep0045_admin_recovery_foreign_matching_affiliation_does_not_prove_rollback_committed() {
    assert_admin_recovery_preserves_final_roster(
        &[Affiliation::Outcast, Affiliation::Member],
        false,
        false,
        false,
        Some(Affiliation::Member),
    )
    .await;
}

#[tokio::test]
async fn xep0045_admin_recovery_foreign_unban_does_not_disprove_original_commit() {
    assert_admin_recovery_preserves_final_roster(
        &[Affiliation::Outcast],
        false,
        true,
        false,
        Some(Affiliation::Member),
    )
    .await;
}

async fn assert_admin_recovery_preserves_final_roster(
    affiliation_changes: &[Affiliation],
    members_only: bool,
    commit_admin: bool,
    retire_on_timeout: bool,
    foreign_affiliation: Option<Affiliation>,
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
        receipts: std::sync::Mutex::new(std::collections::HashMap::new()),
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
    let mutation_attempt = waddle_xmpp::muc::AdminMutationId::generate();
    assert!(matches!(
        actor
            .ask(ApplyAdminItems {
                attempt: mutation_attempt,
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
    if retire_on_timeout {
        store
            .block_next_load
            .store(true, std::sync::atomic::Ordering::SeqCst);
        actor
            .tell(waddle_xmpp::muc::room_actor::RestoreDurableRoomState {
                store: store.clone(),
                claim_fence: before.claim_fence.clone().expect("exact claim"),
            })
            .await
            .expect("queue stalled actor operation");
        store.restore_started.notified().await;
        let (outcome, _) = recover_admin_result_after_actor_failure(
            state.as_ref(),
            &room_jid,
            &before,
            &actor,
            mutation_attempt,
            &items,
            &owner,
        )
        .await;
        assert!(matches!(outcome, AdminReconciliationOutcome::Inconclusive));
        assert!(
            matches!(state.deps.protocol.room_registry.ask(GetRoom { room_jid: room_jid.clone() }).await,
            Err(kameo::error::SendError::HandlerError(waddle_xmpp::muc::room_registry_actor::RoomRegistryError::RoomActorStateLost(jid))) if jid == room_jid)
        );
        return;
    }
    let committed_coordinates = store
        .receipts
        .lock()
        .expect("admin receipts")
        .get(&(room_jid.clone(), mutation_attempt))
        .copied();
    let expected_reservation = if let Some(coordinates) = committed_coordinates {
        let pre = before.durable_coordinates.expect("pre-ask coordinates");
        assert!(coordinates.revision > pre.revision.next().expect("next revision"));
        // A projection committed after the handler snapshot. Recovery must
        // select the attempt receipt's revision, not simply pre-ask + 1.
        let _unrelated_reservation = super::tests::enqueue_recovered_admin_reservation(
            state.as_ref(),
            &room_jid,
            RoomCommittedCoordinates {
                lifecycle: pre.lifecycle,
                revision: pre.revision.next().expect("next revision"),
            },
            &owner,
        )
        .await;
        Some(
            super::tests::enqueue_recovered_admin_reservation(
                state.as_ref(),
                &room_jid,
                coordinates,
                &owner,
            )
            .await,
        )
    } else {
        None
    };
    if let Some(affiliation) = foreign_affiliation {
        let mut durable = store.state.lock().expect("durable state");
        durable
            .affiliations
            .retain(|entry| entry.jid != later.to_bare());
        durable
            .affiliations
            .push(waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                later.to_bare(),
                affiliation,
            ));
        let previous = durable.coordinates.expect("durable coordinates");
        durable.coordinates = Some(RoomCommittedCoordinates {
            lifecycle: previous.lifecycle,
            revision: previous.revision.next().expect("foreign revision"),
        });
    }
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
                mutation_attempt,
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
    let (outcome, suppress_direct) = recovery.await.expect("recovery task");
    if commit_admin {
        let AdminReconciliationOutcome::Committed(applied) = outcome else {
            panic!("exact original receipt must prove commitment after later mutations");
        };
        assert!(
            suppress_direct,
            "durable receipts must never replay effects directly"
        );
        assert_eq!(applied.outbox_reservation, expected_reservation);
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
    let (lost_reply_result, rollback_required, lost_reply_suppresses_direct) =
        reconcile_ambiguous_admin_result(
            state.as_ref(),
            &successor,
            &room_jid,
            Some(&before),
            &items,
            &owner,
            mutation_attempt,
        )
        .await;
    assert_eq!(rollback_required, !commit_admin);
    if commit_admin {
        assert!(lost_reply_suppresses_direct);
        assert_eq!(
            lost_reply_result
                .expect("exact proof survives a lost handler reply")
                .outbox_reservation,
            expected_reservation
        );
    } else {
        assert!(
            lost_reply_result.is_none(),
            "foreign matching affiliations cannot prove the original reply"
        );
    }
    let (unrelated_reply, unrelated_rollback, _) = reconcile_ambiguous_admin_result(
        state.as_ref(),
        &successor,
        &room_jid,
        Some(&before),
        &items,
        &owner,
        waddle_xmpp::muc::AdminMutationId::generate(),
    )
    .await;
    assert!(unrelated_reply.is_none());
    assert!(
        !unrelated_rollback,
        "missing exact verdict cannot authorize managed affiliation rollback"
    );
    let (unrelated_outcome, _) = recover_admin_result_after_actor_failure(
        state.as_ref(),
        &room_jid,
        &before,
        &actor,
        waddle_xmpp::muc::AdminMutationId::generate(),
        &items,
        &owner,
    )
    .await;
    assert!(matches!(
        unrelated_outcome,
        AdminReconciliationOutcome::Inconclusive
    ));
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
        foreign_affiliation.unwrap_or(if commit_admin {
            new_affiliation
        } else {
            previous_affiliation
        })
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
        mutation_attempt,
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
        waddle_xmpp::muc::AdminMutationId::generate(),
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
