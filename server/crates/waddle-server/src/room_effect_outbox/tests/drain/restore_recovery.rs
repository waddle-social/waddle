//! XEP-0045 removal delivery when another owner never observed the live roster.
use super::*;
use kameo::actor::Spawn;
use waddle_xmpp::muc::durable::{
    AdminMutationId, AdminMutationReceipt, DurableRoomState, MucDurableFuture, MucDurableStore,
    RoomClaimFenceContext, RoomCommitFuture, RoomCommitOutcome, RoomCommittedCoordinates,
    RoomDurableMutation,
};
use waddle_xmpp::muc::room_actor::{
    ApplyAdminItems, ChangeAffiliation, GetRoomSnapshot, GetSnapshot, JoinAffiliationGrant,
    JoinWithAffiliation, RestoreDurableRoomState, RoomActor, SetSubject, SetSubjectError,
};
use waddle_xmpp::muc::room_registry_actor::{
    GetOrCreateRoom, GetOrRestoreRoom, WireClusteringClaims,
};
use waddle_xmpp::muc::{AdminItem, MucRoom};
use waddle_xmpp::ownership::SharedNodeIdentity;
use waddle_xmpp::xep::xep0421::OccupantIdSecret;
use waddle_xmpp::{Affiliation, Role, XmppError};

struct RecoveryState {
    room: DurableRoomState,
    receipts: std::collections::HashMap<AdminMutationId, AdminMutationReceipt>,
}

/// State/claims are deterministic test fixtures; removal rows use the real
/// SQL outbox, rendering, local writer acknowledgements, and SFU dispatcher.
struct RecoveryStore {
    state: tokio::sync::Mutex<RecoveryState>,
    claims: Arc<InProcessClaimStore>,
    outbox: Arc<RoomEffectOutboxStore>,
}

impl MucDurableStore for RecoveryStore {
    fn load_room_state_fenced<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        Box::pin(async move {
            assert!(self
                .claims
                .fence(&fence.entity, &fence.owner, fence.epoch)
                .await
                .unwrap());
            Ok(Some(self.state.lock().await.room.clone()))
        })
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        Box::pin(async move {
            self.claims
                .fence(&fence.entity, &fence.owner, fence.epoch)
                .await
                .map_err(|error| XmppError::internal(error.to_string()))
        })
    }

    fn load_admin_mutation_receipt<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        attempt: AdminMutationId,
    ) -> MucDurableFuture<'a, Option<AdminMutationReceipt>> {
        Box::pin(async move { Ok(self.state.lock().await.receipts.get(&attempt).cloned()) })
    }

    fn delete_admin_mutation_receipt<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        attempt: AdminMutationId,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            self.state.lock().await.receipts.remove(&attempt);
            Ok(())
        })
    }

    fn commit_room_mutation<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
        intent: RoomDurableMutation,
        effects: RoomMutationEffects,
    ) -> RoomCommitFuture<'a> {
        Box::pin(async move {
            assert!(self
                .claims
                .fence(&fence.entity, &fence.owner, fence.epoch)
                .await
                .unwrap());
            let mut state = self.state.lock().await;
            if let Some(receipt) = effects
                .admin_mutation_id()
                .and_then(|id| state.receipts.get(&id))
            {
                return Ok(RoomCommitOutcome {
                    coordinates: receipt.coordinates,
                    reservation: None,
                });
            }
            let previous = state.room.coordinates.expect("durable coordinates");
            let coordinates = RoomCommittedCoordinates {
                lifecycle: previous.lifecycle,
                revision: previous.revision.next().expect("next revision"),
            };
            match intent {
                RoomDurableMutation::Affiliation(entry) => {
                    apply_affiliation(&mut state.room, entry)
                }
                RoomDurableMutation::AffiliationBatch(entries) => {
                    for entry in entries {
                        apply_affiliation(&mut state.room, entry);
                    }
                }
                RoomDurableMutation::Subject(subject) => state.room.subject = subject,
                RoomDurableMutation::Projection(_) | RoomDurableMutation::Activate => {}
                other => panic!("unexpected recovery test mutation: {other:?}"),
            }
            let mut tx = self
                .outbox
                .database()
                .begin()
                .await
                .expect("outbox transaction");
            tx.execute(
                "UPDATE clustering_muc_room_lifecycles SET revision = ? WHERE lifecycle_id = ?",
                crate::db_params![
                    coordinates.revision.as_i64(),
                    coordinates.lifecycle.to_string()
                ],
            )
            .await
            .expect("advance lifecycle");
            let reservation = if effects.effects().is_empty() {
                None
            } else {
                Some(
                    self.outbox
                        .enqueue_in_tx(
                            &mut tx,
                            RoomEffectEnqueue {
                                lifecycle: coordinates.lifecycle,
                                revision: coordinates.revision,
                                effects: &effects,
                                origin: &origin(),
                                producing_node: &producing_node(),
                                now_ms: crate::time::now_ms(),
                            },
                        )
                        .await
                        .expect("persist recovery effects"),
                )
            };
            tx.commit().await.expect("commit effects");
            state.room.coordinates = Some(coordinates);
            if let Some(attempt) = effects.admin_mutation_id() {
                state.receipts.insert(
                    attempt,
                    AdminMutationReceipt::from_effects(coordinates, &effects),
                );
            }
            Ok(RoomCommitOutcome {
                coordinates,
                reservation,
            })
        })
    }
}

fn apply_affiliation(
    room: &mut DurableRoomState,
    entry: waddle_xmpp::muc::durable::AffiliationEntry,
) {
    room.affiliations
        .retain(|existing| existing.jid != entry.jid);
    if let Some(affiliation) = entry.affiliation {
        room.affiliations
            .push(waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                entry.jid,
                affiliation,
            ));
    }
}

#[tokio::test]
async fn xep0045_foreign_ban_recovery_delivers_301_and_sfu_removal() {
    assert_foreign_removal_delivery(Affiliation::Outcast, "301").await;
}

#[tokio::test]
async fn xep0045_foreign_membership_revocation_recovery_delivers_321_and_sfu_removal() {
    assert_foreign_removal_delivery(Affiliation::None, "321").await;
}

async fn assert_foreign_removal_delivery(affiliation: Affiliation, status: &str) {
    let recorder = Arc::new(crate::server::routes::websocket::tests::RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let room = drain_room_jid();
    let alice = full_jid("alice@example.test/web");
    let bob = full_jid("bob@example.test/web");
    let bob_phone = full_jid("bob@example.test/phone");
    let lifecycle = lifecycle();
    insert_lifecycle_row(
        &state,
        &room,
        lifecycle,
        initial_revision(),
        RoomLifecycleState::Active,
    )
    .await;
    let claims = Arc::new(InProcessClaimStore::new());
    let config = RoomConfig {
        members_only: true,
        persistent: true,
        ..Default::default()
    };
    let store = Arc::new(RecoveryStore {
        state: tokio::sync::Mutex::new(RecoveryState {
            room: DurableRoomState {
                coordinates: Some(RoomCommittedCoordinates {
                    lifecycle,
                    revision: initial_revision(),
                }),
                config_coordinates: None,
                waddle_id: "w".to_owned(),
                channel_id: "c".to_owned(),
                config: config.clone(),
                subject: None,
                affiliations: vec![
                    waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                        alice.to_bare(),
                        Affiliation::Owner,
                    ),
                    waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                        bob.to_bare(),
                        Affiliation::Member,
                    ),
                ],
            },
            receipts: Default::default(),
        }),
        claims: claims.clone(),
        outbox: state.deps.protocol.room_effect_outbox.clone(),
    });
    let registry = &state.deps.protocol.room_registry;
    registry
        .ask(WireClusteringClaims {
            claim_store: claims.clone(),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            durable_store: Some(store.clone()),
            rollout_backoff: None,
        })
        .await
        .expect("wire test durable store");
    let actor = registry
        .ask(GetOrCreateRoom {
            room_jid: room.clone(),
            waddle_id: "w".to_owned(),
            channel_id: "c".to_owned(),
            config: config.clone(),
        })
        .await
        .expect("hydrate predecessor")
        .actor_ref;
    for (jid, nick, affiliation) in [
        (&alice, "alice", Affiliation::Owner),
        (&bob, "bob", Affiliation::Member),
        (&bob_phone, "bob", Affiliation::Member),
    ] {
        let snapshot = actor.ask(GetSnapshot).await.expect("admission snapshot");
        actor
            .ask(JoinWithAffiliation {
                sender_jid: jid.clone(),
                nick: nick.to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(affiliation),
                local_domain: "example.test".to_owned(),
                admission_revision: snapshot.admission_revision,
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("join predecessor");
    }
    let previous = actor.ask(GetSnapshot).await.expect("predecessor snapshot");
    let old_fence = previous.claim_fence.clone().expect("claim fence");
    claims
        .release_exact(&old_fence.entity, &old_fence.owner, old_fence.epoch)
        .await
        .expect("release original claim");
    let foreign_owner = NodeIdentity::new("foreign", "foreign-epoch");
    let foreign_epoch = claims
        .acquire(&old_fence.entity, &foreign_owner)
        .await
        .expect("foreign owner claims room");
    let foreign_fence =
        RoomClaimFenceContext::new(old_fence.entity.clone(), foreign_owner, foreign_epoch);
    let foreign_actor = RoomActor::spawn(RoomActor::new(
        MucRoom::new(room.clone(), "w".to_owned(), "c".to_owned(), config),
        OccupantIdSecret::new(vec![7; 32]).expect("secret"),
    ));
    foreign_actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
            claim_fence: foreign_fence.clone(),
        })
        .await
        .expect("hydrate foreign owner");
    assert!(
        foreign_actor
            .ask(GetSnapshot)
            .await
            .unwrap()
            .room
            .occupants
            .is_empty(),
        "foreign owner has no audience or local SFU sessions"
    );
    if affiliation == Affiliation::Outcast {
        let applied = foreign_actor
            .ask(ApplyAdminItems {
                attempt: AdminMutationId::generate(),
                sender_jid: alice.clone(),
                sender_affiliation: Affiliation::Owner,
                sender_role: Role::Moderator,
                items: vec![AdminItem {
                    jid: Some(bob.to_bare()),
                    nick: None,
                    affiliation: Some(affiliation),
                    role: None,
                    reason: None,
                }],
            })
            .await
            .expect("foreign actor bans Bob");
        assert!(applied.removed_by_moderation.is_empty());
        assert!(applied.presence_updates.is_empty());
    } else {
        foreign_actor
            .ask(ChangeAffiliation {
                jid: bob.to_bare(),
                affiliation,
            })
            .await
            .expect("foreign actor revokes Bob's membership");
    }
    assert!(matches!(
        actor
            .ask(SetSubject {
                texts: waddle_xmpp::muc::RoomSubjectTexts::from_iter([(
                    String::new(),
                    "new subject".to_owned()
                )]),
                setter: alice.to_bare(),
                setter_nick: "alice".to_owned(),
                set_at: chrono::Utc::now(),
            })
            .await,
        Err(kameo::error::SendError::HandlerError(
            SetSubjectError::NotOwner
        ))
    ));
    claims
        .release_exact(
            &foreign_fence.entity,
            &foreign_fence.owner,
            foreign_fence.epoch,
        )
        .await
        .expect("foreign owner releases");
    foreign_actor.kill();
    let sealed_snapshot = actor.ask(GetSnapshot).await.expect("sealed final roster");
    let recovered = registry
        .ask(GetOrRestoreRoom {
            room_jid: room.clone(),
            previous_snapshot: previous,
            stale_actor: actor,
            live_restore: Some(sealed_snapshot),
        })
        .await
        .expect("recover original live roster")
        .expect("successor");
    let dispatch = recovered
        .ask(GetRoomSnapshot {
            sender_jid: bob.clone(),
        })
        .await
        .expect("recovered dispatch snapshot");
    assert!(dispatch.sender_nick.is_none(), "removed member cannot send");
    assert!(
        dispatch
            .occupants
            .iter()
            .all(|occupant| occupant.nick != "bob"),
        "removed member cannot receive"
    );

    let mut deliveries = Vec::new();
    for (jid, is_self) in [(&alice, false), (&bob, true), (&bob_phone, true)] {
        let (tx, mut rx) = mpsc::channel(8);
        register_test_connection(&state, jid, tx).await;
        let status = status.to_owned();
        let room = room.clone();
        deliveries.push(tokio::spawn(async move {
            let outbound = recv_outbound(&mut rx).await;
            let xml = stanza_to_xml(&outbound.stanza);
            let presence = xml.parse::<Element>().expect("removal presence");
            assert_eq!(presence.name(), "presence");
            assert_eq!(presence.attr("type"), Some("unavailable"));
            assert_eq!(
                presence.attr("from"),
                Some(room.with_resource_str("bob").unwrap().as_str())
            );
            let codes = muc_status_codes(&outbound);
            assert!(codes.contains(&status), "missing removal code: {xml}");
            assert_eq!(
                codes.contains(&"110".to_owned()),
                is_self,
                "self marker: {xml}"
            );
            outbound
                .write_acceptance
                .as_ref()
                .expect("local write acknowledgement")
                .acknowledge();
            rx
        }));
    }
    // HandlerWindow rows become janitor-eligible without an inline handler
    // returning a reservation: exercise that production recovery path.
    let due = crate::time::now_ms() + 60_000;
    let mut drained = 0;
    for _ in 0..3 {
        drained += drain_due_effects(&state, due, 8)
            .await
            .expect("drain recovery effects")
            .drained;
    }
    assert_eq!(
        drained, 2,
        "self and remaining removal rows must both deliver"
    );
    for delivery in deliveries {
        let mut rx = delivery.await.expect("writer task");
        assert!(
            rx.try_recv().is_err(),
            "removal must not be delivered twice"
        );
    }
    let unregistered = recorder.snapshot();
    let sessions: HashSet<_> = unregistered
        .iter()
        .map(|(call, identity)| {
            assert_eq!(call.as_str(), room.as_str());
            identity.as_livekit_identity()
        })
        .collect();
    assert_eq!(
        sessions,
        HashSet::from([bob.to_string(), bob_phone.to_string()])
    );
    assert_eq!(
        unregistered.len(),
        2,
        "each resource loses SFU membership exactly once"
    );
}
