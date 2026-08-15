use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use kameo::actor::Spawn;
use tokio::sync::mpsc;
use waddle_xmpp::muc::durable::{MucDurableFuture, MucDurableStore, RoomCommitFuture};
use waddle_xmpp::muc::room_actor::{
    ConfigEffectPlan, EnforceMembersOnlyAffiliations, GetSnapshot, JoinAffiliationGrant,
    JoinWithAffiliation, RestoreDurableRoomState, RollbackConfigIfRevision, RoomActor,
    UpdateConfig,
};
use waddle_xmpp::muc::room_registry_actor::{CreateRoom, GetOrCreateRoom};
use waddle_xmpp::muc::{
    MucRoom, RoomClaimFenceContext, RoomCommitError, RoomCommitOutcome, RoomCommittedCoordinates,
    RoomConfig, RoomDurableMutation, RoomMutationEffects, RoomRevision,
};
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};
use waddle_xmpp::registry::OutboundStanza;
use waddle_xmpp::xep::xep0421::OccupantIdSecret;
use waddle_xmpp::Affiliation;

use super::*;
use crate::room_effect_outbox::drain::drain_due_effects;
use crate::room_effect_outbox::{RoomEffectArmSupervisor, RoomEffectEnqueue};
use crate::server::routes::websocket::tests::{
    create_test_websocket_state, create_test_websocket_state_with_db_pool, register_test_connection,
};
use crate::server::routes::websocket::WebSocketState;

async fn create_owned_room_and_lifecycle(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
) -> waddle_xmpp::muc::RoomLifecycleId {
    let lifecycle = lifecycle();
    state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "room-effect-config-test".to_owned(),
            channel_id: "room-effect-config-test".to_owned(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create owned room");
    let connection = state
        .deps
        .protocol
        .room_effect_outbox
        .database()
        .guard()
        .await
        .expect("connection");
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (lifecycle_id TEXT NOT NULL, room_jid TEXT NOT NULL, revision BIGINT NOT NULL, state TEXT NOT NULL)",
            (),
        )
        .await
        .expect("create lifecycle table");
    connection
        .execute(
            "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
            crate::db_params![
                lifecycle.to_string(),
                room_jid.to_string(),
                initial_revision().as_i64(),
                waddle_xmpp::muc::RoomLifecycleState::Active.as_db_str(),
            ],
        )
        .await
        .expect("insert lifecycle");
    lifecycle
}

async fn receive(rx: &mut mpsc::Receiver<OutboundStanza>) -> OutboundStanza {
    tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("delivery timeout")
        .expect("delivery")
}

fn xml(outbound: &OutboundStanza) -> String {
    crate::server::routes::websocket::stanza_to_xml(&outbound.stanza)
}

struct ActorOutboxStore {
    outbox: Arc<RoomEffectOutboxStore>,
    lifecycle: waddle_xmpp::muc::RoomLifecycleId,
    next_revision: Mutex<i64>,
    expected_fence: RoomClaimFenceContext,
}

impl ActorOutboxStore {
    fn new(
        outbox: Arc<RoomEffectOutboxStore>,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
        room_jid: &jid::BareJid,
    ) -> Arc<Self> {
        Arc::new(Self {
            outbox,
            lifecycle,
            next_revision: Mutex::new(initial_revision().as_i64()),
            expected_fence: RoomClaimFenceContext::new(
                Entity::new(EntityType::RoomActor, room_jid.to_string()),
                NodeIdentity::new("actor-outbox-node", "actor-outbox-epoch"),
                ClaimEpoch(1),
            ),
        })
    }
}

impl MucDurableStore for ActorOutboxStore {
    fn commit_room_mutation<'a>(
        &'a self,
        _room_jid: &'a jid::BareJid,
        fence: &'a RoomClaimFenceContext,
        intent: RoomDurableMutation,
        effects: RoomMutationEffects,
    ) -> RoomCommitFuture<'a> {
        let exact = fence == &self.expected_fence;
        let lifecycle = self.lifecycle;
        let outbox = Arc::clone(&self.outbox);
        let successor_config_codes = effects.effects().iter().find_map(|effect| match effect {
            waddle_xmpp::muc::RoomEffect::ConfigChanged { status_codes, .. } => {
                Some(status_codes.clone())
            }
            _ => None,
        });
        let is_config_mutation = matches!(intent, RoomDurableMutation::Config { .. });
        let revision = {
            let mut next = self.next_revision.lock().expect("revision lock");
            let revision = RoomRevision::from_stored(*next).expect("positive revision");
            *next += 1;
            revision
        };
        Box::pin(async move {
            if !exact {
                return Err(RoomCommitError::NotOwner);
            }
            let mut tx = outbox.database().begin().await.map_err(|_| {
                RoomCommitError::Database(waddle_xmpp::muc::RoomCommitDatabaseError::sanitized())
            })?;
            if let Some(reservation) = effects.superseding_reservation() {
                outbox
                    .supersede_reservation_in_tx(&mut tx, reservation)
                    .await
                    .map_err(|_| {
                        RoomCommitError::Database(
                            waddle_xmpp::muc::RoomCommitDatabaseError::sanitized(),
                        )
                    })?;
            }
            if is_config_mutation {
                if let Some(successor_codes) = successor_config_codes.as_deref() {
                    outbox
                        .supersede_idempotent_config_in_tx(&mut tx, lifecycle, successor_codes)
                        .await
                        .map_err(|_| {
                            RoomCommitError::Database(
                                waddle_xmpp::muc::RoomCommitDatabaseError::sanitized(),
                            )
                        })?;
                }
            }
            let reservation = if effects.effects().is_empty() {
                None
            } else {
                Some(
                    outbox
                        .enqueue_in_tx(
                            &mut tx,
                            RoomEffectEnqueue {
                                lifecycle,
                                revision,
                                effects: &effects,
                                origin: &origin(),
                                producing_node: &producing_node(),
                                now_ms: 0,
                            },
                        )
                        .await
                        .map_err(|_| {
                            RoomCommitError::Database(
                                waddle_xmpp::muc::RoomCommitDatabaseError::sanitized(),
                            )
                        })?,
                )
            };
            tx.commit().await.map_err(|_| {
                RoomCommitError::Database(waddle_xmpp::muc::RoomCommitDatabaseError::sanitized())
            })?;
            Ok(RoomCommitOutcome {
                coordinates: RoomCommittedCoordinates {
                    lifecycle,
                    revision,
                },
                reservation,
            })
        })
    }

    fn load_room_state_fenced<'a>(
        &'a self,
        _room_jid: &'a jid::BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<waddle_xmpp::muc::DurableRoomState>> {
        let exact = fence == &self.expected_fence;
        Box::pin(async move {
            if exact {
                Ok(None)
            } else {
                Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                })
            }
        })
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        _room_jid: &'a jid::BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        let exact = fence == &self.expected_fence;
        Box::pin(async move { Ok(exact) })
    }
}

async fn actor_with_outbox(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    lifecycle: waddle_xmpp::muc::RoomLifecycleId,
) -> (kameo::actor::ActorRef<RoomActor>, Arc<ActorOutboxStore>) {
    let store = ActorOutboxStore::new(
        Arc::clone(&state.deps.protocol.room_effect_outbox),
        lifecycle,
        room_jid,
    );
    let actor = RoomActor::spawn(RoomActor::new(
        MucRoom::new(
            room_jid.clone(),
            "room-effect-config-test".to_owned(),
            "room-effect-config-test".to_owned(),
            RoomConfig {
                members_only: false,
                enable_logging: false,
                ..RoomConfig::default()
            },
        ),
        OccupantIdSecret::new(b"room-effect-actor-outbox-secret-32b".to_vec())
            .expect("test occupant-id secret"),
    ));
    actor
        .ask(RestoreDurableRoomState {
            store: store.clone(),
            claim_fence: store.expected_fence.clone(),
        })
        .await
        .expect("restore actor");
    (actor, store)
}

#[tokio::test]
async fn uncommitted_members_only_enforcement_arms_and_delivers_its_staged_fallback_late() {
    let state = create_test_websocket_state().await;
    let room_jid = room_jid();
    let recipient = full_jid("alice@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref(), &room_jid).await;
    let (actor, _) = actor_with_outbox(state.as_ref(), &room_jid, lifecycle).await;
    actor
        .ask(JoinWithAffiliation {
            sender_jid: recipient.clone(),
            nick: "alice".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.test".to_owned(),
            admission_revision: 0,
        })
        .await
        .expect("qualified occupant joins");
    let fallback = actor
        .ask(UpdateConfig {
            config: RoomConfig {
                members_only: true,
                enable_logging: false,
                ..RoomConfig::default()
            },
            effect_plan: ConfigEffectPlan::ManagedMembersOnlyFallback,
        })
        .await
        .expect("actor stages managed members-only fallback")
        .reservation
        .expect("config commit reserves fallback");
    let store = Arc::clone(&state.deps.protocol.room_effect_outbox);
    let key = RoomEffectKey {
        lifecycle,
        revision: fallback.revision,
        ordinal: fallback.ordinals[0],
    };
    assert_eq!(
        store
            .find(&key)
            .await
            .expect("staged fallback")
            .expect("fallback row")
            .available_at_ms,
        i64::MAX,
        "the fallback remains inert until the failed enforcement is recovered"
    );
    let (sender, mut receiver) = mpsc::channel(2);
    register_test_connection(state.as_ref(), &recipient, sender).await;
    let supervisor =
        RoomEffectArmSupervisor::new(Arc::clone(&store), tokio::runtime::Handle::current());
    supervisor.attach_drain_state(&state);

    supervisor.arm(fallback);
    let delivery = receive(&mut receiver).await;
    assert!(xml(&delivery).contains("code='104'"));
    delivery
        .write_acceptance
        .as_ref()
        .expect("fallback write acceptance")
        .acknowledge();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if store.find(&key).await.expect("find fallback").is_none() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("supervisor arms and drains fallback after enforcement abort");
}

#[tokio::test]
async fn zero_delta_members_only_enforcement_supersedes_fallback_and_delivers_one_config() {
    let state = create_test_websocket_state().await;
    let room_jid = room_jid();
    let recipient = full_jid("alice@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref(), &room_jid).await;
    let (actor, _) = actor_with_outbox(state.as_ref(), &room_jid, lifecycle).await;
    actor
        .ask(JoinWithAffiliation {
            sender_jid: recipient.clone(),
            nick: "alice".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.test".to_owned(),
            admission_revision: 0,
        })
        .await
        .expect("already-qualified occupant joins");
    let config_update = actor
        .ask(UpdateConfig {
            config: RoomConfig {
                members_only: true,
                ..RoomConfig::default()
            },
            effect_plan: ConfigEffectPlan::ManagedMembersOnlyFallback,
        })
        .await
        .expect("actor stages managed members-only fallback");
    let fallback = config_update
        .reservation
        .expect("config commit reserves fallback");
    let config_status_codes = config_update
        .notification
        .expect("members-only change is notified")
        .status_codes;
    let store = state.deps.protocol.room_effect_outbox.as_ref();
    let enforcement = actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: vec![(recipient.to_bare(), Affiliation::Member)],
            fallback_reservation: Some(fallback.clone()),
            config_status_codes,
        })
        .await
        .expect("zero-delta enforcement commits")
        .outbox_reservation
        .expect("enforcement reserves fused effects");
    assert!(
        store
            .find(&RoomEffectKey {
                lifecycle,
                revision: fallback.revision,
                ordinal: fallback.ordinals[0],
            })
            .await
            .expect("find fallback")
            .is_none(),
        "the staged fallback is gone before a supervisor can arm it"
    );
    assert_eq!(
        enforcement.ordinals.len(),
        3,
        "self-notify, remaining-broadcast, then config effect"
    );
    let (sender, mut receiver) = mpsc::channel(2);
    register_test_connection(state.as_ref(), &recipient, sender).await;

    assert_eq!(
        drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8,)
            .await
            .expect("drain empty self-notify"),
        crate::room_effect_outbox::drain::RoomEffectDrainSummary {
            drained: 1,
            ..Default::default()
        },
        "the empty self-notify remains the FIFO head of the fused pair"
    );
    assert_eq!(
        drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8,)
            .await
            .expect("drain empty remaining-broadcast"),
        crate::room_effect_outbox::drain::RoomEffectDrainSummary {
            drained: 1,
            ..Default::default()
        },
        "the empty remaining-broadcast must also preserve FIFO before config"
    );
    let config_drain = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8).await
        })
    };
    let delivery = receive(&mut receiver).await;
    assert!(xml(&delivery).contains("code='104'"));
    delivery
        .write_acceptance
        .as_ref()
        .expect("fused config write acceptance")
        .acknowledge();
    assert_eq!(
        config_drain
            .await
            .expect("fused config drain join")
            .expect("fused config drain")
            .drained,
        1,
        "the fused config effect drains once after its FIFO self-notify"
    );
    assert!(
        receiver.try_recv().is_err(),
        "no fallback duplicate is delivered"
    );
    assert_eq!(store.queue_depth().await.expect("queue depth"), 0);
}

#[tokio::test]
async fn managed_members_only_enforcement_commits_after_interleaved_config_consumes_fallback() {
    let state = create_test_websocket_state().await;
    let room_jid = room_jid();
    let removed = full_jid("alice@example.test/device");
    let remaining = full_jid("bob@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref(), &room_jid).await;
    let (actor, _) = actor_with_outbox(state.as_ref(), &room_jid, lifecycle).await;
    for (sender_jid, nick) in [(removed.clone(), "alice"), (remaining.clone(), "bob")] {
        actor
            .ask(JoinWithAffiliation {
                sender_jid,
                nick: nick.to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.test".to_owned(),
                admission_revision: actor
                    .ask(GetSnapshot)
                    .await
                    .expect("current admission revision")
                    .admission_revision,
            })
            .await
            .expect("occupant joins with managed membership");
    }
    let config_update = actor
        .ask(UpdateConfig {
            config: RoomConfig {
                members_only: true,
                enable_logging: false,
                ..RoomConfig::default()
            },
            effect_plan: ConfigEffectPlan::ManagedMembersOnlyFallback,
        })
        .await
        .expect("actor stages managed fallback");
    let fallback = config_update
        .reservation
        .expect("config update reserves fallback");
    let status_codes = config_update
        .notification
        .expect("members-only change is notified")
        .status_codes;
    let (removed_sender, mut removed_receiver) = mpsc::channel(2);
    let (remaining_sender, mut remaining_receiver) = mpsc::channel(2);
    register_test_connection(state.as_ref(), &removed, removed_sender).await;
    register_test_connection(state.as_ref(), &remaining, remaining_sender).await;
    let interleaved = actor
        .ask(UpdateConfig {
            config: RoomConfig {
                members_only: true,
                enable_logging: false,
                name: "interleaved config".to_owned(),
                ..RoomConfig::default()
            },
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("interleaved pure-104 config commits");
    assert!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .staged_reservation_for(lifecycle, fallback.revision)
            .await
            .expect("lookup consumed fallback")
            .is_none(),
        "the interleaved pure-104 config commit consumes the staged fallback"
    );
    let interleaved_reservation = interleaved
        .reservation
        .expect("pure-104 config reserves its own effect");
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&interleaved_reservation, 0)
        .await
        .expect("arm interleaved config");
    let interleaved_drain = {
        let state = Arc::clone(&state);
        tokio::spawn(async move { drain_due_effects(state.as_ref(), 0, 8).await })
    };
    for receiver in [&mut removed_receiver, &mut remaining_receiver] {
        let delivery = receive(receiver).await;
        assert!(xml(&delivery).contains("code='104'"));
        delivery
            .write_acceptance
            .as_ref()
            .expect("interleaved config write acceptance")
            .acknowledge();
    }
    assert_eq!(
        interleaved_drain
            .await
            .expect("interleaved drain join")
            .expect("interleaved drain")
            .drained,
        1
    );
    let applied = actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: vec![
                (removed.to_bare(), Affiliation::None),
                (remaining.to_bare(), Affiliation::Member),
            ],
            fallback_reservation: Some(fallback.clone()),
            config_status_codes: status_codes,
        })
        .await
        .expect("enforcement still commits after the fallback was already consumed");
    let enforcement = applied
        .outbox_reservation
        .expect("enforcement reserves the fused durable effects");
    assert_eq!(enforcement.ordinals.len(), 3);

    let removal_drain = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8).await
        })
    };
    let removal = receive(&mut removed_receiver).await;
    assert!(xml(&removal).contains("code='322'"));
    removal
        .write_acceptance
        .as_ref()
        .expect("322 write acceptance")
        .acknowledge();
    assert_eq!(
        removal_drain
            .await
            .expect("322 drain join")
            .expect("322 drain")
            .drained,
        1
    );

    let remaining_broadcast_drain = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8).await
        })
    };
    let remaining_322 = receive(&mut remaining_receiver).await;
    assert!(xml(&remaining_322).contains("code='322'"));
    remaining_322
        .write_acceptance
        .as_ref()
        .expect("remaining 322 write acceptance")
        .acknowledge();
    assert_eq!(
        remaining_broadcast_drain
            .await
            .expect("remaining 322 drain join")
            .expect("remaining 322 drain")
            .drained,
        1,
        "the remaining-occupant 322 must still drain after the missing-row supersession path"
    );

    let config_drain = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8).await
        })
    };
    let config = receive(&mut remaining_receiver).await;
    assert!(xml(&config).contains("code='104'"));
    config
        .write_acceptance
        .as_ref()
        .expect("config write acceptance")
        .acknowledge();
    assert_eq!(
        config_drain
            .await
            .expect("config drain join")
            .expect("config drain")
            .drained,
        1,
        "the fused config effect must still drain after the missing-row supersession path"
    );
}

#[tokio::test]
async fn managed_members_only_removal_drains_322_before_full_config_to_remaining_member() {
    let state = create_test_websocket_state().await;
    let room_jid = room_jid();
    let removed = full_jid("alice@example.test/device");
    let remaining = full_jid("bob@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref(), &room_jid).await;
    let (actor, _) = actor_with_outbox(state.as_ref(), &room_jid, lifecycle).await;
    for (sender_jid, nick) in [(removed.clone(), "alice"), (remaining.clone(), "bob")] {
        actor
            .ask(JoinWithAffiliation {
                sender_jid,
                nick: nick.to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.test".to_owned(),
                admission_revision: actor
                    .ask(GetSnapshot)
                    .await
                    .expect("current admission revision")
                    .admission_revision,
            })
            .await
            .expect("occupant joins with managed membership");
    }
    let config_update = actor
        .ask(UpdateConfig {
            config: RoomConfig {
                members_only: true,
                enable_logging: true,
                ..RoomConfig::default()
            },
            effect_plan: ConfigEffectPlan::ManagedMembersOnlyFallback,
        })
        .await
        .expect("actor stages managed fallback");
    let fallback = config_update
        .reservation
        .expect("config update reserves fallback");
    let status_codes = config_update
        .notification
        .expect("combined config notification")
        .status_codes;
    let store = state.deps.protocol.room_effect_outbox.as_ref();
    let (removed_sender, mut removed_receiver) = mpsc::channel(2);
    let (remaining_sender, mut remaining_receiver) = mpsc::channel(2);
    register_test_connection(state.as_ref(), &removed, removed_sender).await;
    register_test_connection(state.as_ref(), &remaining, remaining_sender).await;
    let applied = actor
        .ask(EnforceMembersOnlyAffiliations {
            affiliations: vec![
                (removed.to_bare(), Affiliation::None),
                (remaining.to_bare(), Affiliation::Member),
            ],
            fallback_reservation: Some(fallback.clone()),
            config_status_codes: status_codes,
        })
        .await
        .expect("non-zero-delta enforcement commits");
    let enforcement = applied
        .outbox_reservation
        .expect("enforcement reserves ordered effects");
    assert!(
        store
            .find(&RoomEffectKey {
                lifecycle,
                revision: fallback.revision,
                ordinal: fallback.ordinals[0],
            })
            .await
            .expect("find fallback")
            .is_none(),
        "the actor commit supersedes the exact fallback"
    );
    assert_eq!(enforcement.ordinals.len(), 3);
    let removal_drain = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8).await
        })
    };
    let removal = receive(&mut removed_receiver).await;
    assert!(xml(&removal).contains("code='322'"));
    removal
        .write_acceptance
        .as_ref()
        .expect("322 write acceptance")
        .acknowledge();
    assert_eq!(
        removal_drain
            .await
            .expect("322 drain join")
            .expect("322 drain")
            .drained,
        1,
        "the removed occupant self-notify is the fused effect's FIFO head"
    );

    let remaining_broadcast_drain = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8).await
        })
    };
    let remaining_322 = receive(&mut remaining_receiver).await;
    assert!(xml(&remaining_322).contains("code='322'"));
    remaining_322
        .write_acceptance
        .as_ref()
        .expect("remaining 322 write acceptance")
        .acknowledge();
    assert_eq!(
        remaining_broadcast_drain
            .await
            .expect("remaining 322 drain join")
            .expect("remaining 322 drain")
            .drained,
        1,
        "the remaining-occupant 322 must drain as its own FIFO effect"
    );

    let config_drain = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8).await
        })
    };
    let config = receive(&mut remaining_receiver).await;
    let config_xml = xml(&config);
    assert_eq!(config_xml.matches("code='170'").count(), 1);
    assert_eq!(config_xml.matches("code='104'").count(), 1);
    assert!(
        !config_xml.contains("code='171'"),
        "the transferred full set must not collapse or invent transitions"
    );
    config
        .write_acceptance
        .as_ref()
        .expect("config write acceptance")
        .acknowledge();
    assert_eq!(
        config_drain
            .await
            .expect("config drain join")
            .expect("config drain")
            .drained,
        1
    );
    assert!(
        removed_receiver.try_recv().is_err(),
        "the removed occupant is excluded from the config audience"
    );
    assert_eq!(store.queue_depth().await.expect("queue depth"), 0);
}

#[tokio::test]
async fn rollback_config_proceeds_after_staged_row_was_already_drained() {
    let state = create_test_websocket_state().await;
    let room_jid = room_jid();
    let recipient = full_jid("alice@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref(), &room_jid).await;
    let (actor, _) = actor_with_outbox(state.as_ref(), &room_jid, lifecycle).await;
    actor
        .ask(JoinWithAffiliation {
            sender_jid: recipient.clone(),
            nick: "alice".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.test".to_owned(),
            admission_revision: 0,
        })
        .await
        .expect("recipient joins before the config mutation");
    let committed = actor
        .ask(UpdateConfig {
            config: RoomConfig {
                enable_logging: true,
                ..RoomConfig::default()
            },
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("config update commits");
    let expected_revision = committed.revision;
    let reservation = committed
        .reservation
        .expect("config update reserves one staged effect");
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm staged row");
    let (sender, mut receiver) = mpsc::channel(2);
    register_test_connection(state.as_ref(), &recipient, sender).await;
    let drain = {
        let state = Arc::clone(&state);
        tokio::spawn(async move { drain_due_effects(state.as_ref(), 0, 8).await })
    };
    let delivery = receive(&mut receiver).await;
    assert!(xml(&delivery).contains("code='170'"));
    delivery
        .write_acceptance
        .as_ref()
        .expect("write acceptance")
        .acknowledge();
    assert_eq!(
        drain.await.expect("drain join").expect("drain").drained,
        1,
        "the staged row must be gone before rollback runs"
    );
    assert!(actor
        .ask(RollbackConfigIfRevision {
            expected_revision,
            config: RoomConfig {
                enable_logging: false,
                ..RoomConfig::default()
            },
            reservation: Some(reservation),
        })
        .await
        .expect("rollback after drained row"));
    let snapshot = actor
        .ask(GetSnapshot)
        .await
        .expect("snapshot after rollback");
    assert!(
        !snapshot.room.config.enable_logging,
        "the rollback should still revert the in-memory config after the row was already drained"
    );
}

#[tokio::test]
async fn armed_config_effect_preserves_original_recipients_across_dormancy_and_store_reopen() {
    let state = create_test_websocket_state().await;
    let room_jid = room_jid();
    let recipient = full_jid("alice@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref(), &room_jid).await;
    let (actor, durable_store) = actor_with_outbox(state.as_ref(), &room_jid, lifecycle).await;
    actor
        .ask(JoinWithAffiliation {
            sender_jid: recipient.clone(),
            nick: "alice".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.test".to_owned(),
            admission_revision: 0,
        })
        .await
        .expect("original recipient joins before the config mutation");

    let committed = actor
        .ask(UpdateConfig {
            config: RoomConfig {
                enable_logging: true,
                ..RoomConfig::default()
            },
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("actor commits config change with direct audience");
    let reservation = committed
        .reservation
        .expect("config change reserves one durable effect");
    let key = RoomEffectKey {
        lifecycle,
        revision: reservation.revision,
        ordinal: reservation.ordinals[0],
    };
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm committed config effect");
    assert_eq!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .queue_depth()
            .await
            .expect("queue depth after config commit"),
        1,
        "the armed config effect must be the only queued row before dormancy"
    );

    let dormancy = durable_store
        .commit_room_mutation(
            &room_jid,
            &durable_store.expected_fence,
            RoomDurableMutation::Dormancy,
            RoomMutationEffects::none(),
        )
        .await
        .expect("dormancy commit");
    assert!(
        dormancy.reservation.is_none(),
        "dormancy itself must not enqueue any new effect rows"
    );
    assert_eq!(
        state
            .deps
            .protocol
            .room_effect_outbox
            .queue_depth()
            .await
            .expect("queue depth after dormancy"),
        1,
        "the original armed effect must survive a no-op dormancy commit"
    );

    actor.kill();
    let fresh_state =
        create_test_websocket_state_with_db_pool(Arc::clone(&state.deps.app_state.db_pool)).await;
    fresh_state
        .deps
        .protocol
        .room_registry
        .ask(GetOrCreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "room-effect-config-test".to_owned(),
            channel_id: "room-effect-config-test".to_owned(),
            config: RoomConfig {
                members_only: false,
                enable_logging: false,
                ..RoomConfig::default()
            },
        })
        .await
        .expect("fresh registry reacquires the room");
    let reopened_store =
        RoomEffectOutboxStore::new(fresh_state.deps.app_state.db_pool.global().clone())
            .await
            .expect("reopen room effect outbox store");
    assert!(
        reopened_store
            .find(&key)
            .await
            .expect("find effect after store reopen")
            .is_some(),
        "reopening the store over the same DB pool must retain the armed effect row"
    );
    let reopened_row = reopened_store
        .find(&key)
        .await
        .expect("reopen find row")
        .expect("reopened row");
    match reopened_row.effect {
        waddle_xmpp::muc::RoomEffect::ConfigChanged { recipients, .. } => {
            assert_eq!(
                recipients,
                vec![recipient.clone()],
                "the reopened row must preserve the original mutation-time recipient set"
            );
        }
        other => panic!("expected config-changed effect, got {other:?}"),
    }

    let (sender, mut receiver) = mpsc::channel(2);
    register_test_connection(fresh_state.as_ref(), &recipient, sender).await;
    let drain = {
        let state = Arc::clone(&fresh_state);
        tokio::spawn(async move {
            drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8).await
        })
    };
    let delivery = receive(&mut receiver).await;
    assert!(
        xml(&delivery).contains("code='170'"),
        "the drained stanza must still target the original mutation-time recipient"
    );
    delivery
        .write_acceptance
        .as_ref()
        .expect("config write acceptance")
        .acknowledge();
    assert_eq!(
        drain
            .await
            .expect("dormancy drain join")
            .expect("dormancy drain")
            .drained,
        1
    );
    assert_eq!(
        fresh_state
            .deps
            .protocol
            .room_effect_outbox
            .queue_depth()
            .await
            .expect("queue depth after drain"),
        0
    );
}

#[tokio::test]
async fn join_processed_immediately_before_config_is_included_in_durable_config_audience() {
    let state = create_test_websocket_state().await;
    let room_jid = room_jid();
    let alice = full_jid("alice@example.test/device");
    let bob = full_jid("bob@example.test/device");
    let lifecycle = create_owned_room_and_lifecycle(state.as_ref(), &room_jid).await;
    let (actor, _) = actor_with_outbox(state.as_ref(), &room_jid, lifecycle).await;

    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.test".to_owned(),
            admission_revision: 0,
        })
        .await
        .expect("first occupant joins");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: bob.clone(),
            nick: "bob".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.test".to_owned(),
            admission_revision: actor
                .ask(GetSnapshot)
                .await
                .expect("admission revision before second join")
                .admission_revision,
        })
        .await
        .expect("second occupant joins immediately before config");
    let committed = actor
        .ask(UpdateConfig {
            config: RoomConfig {
                enable_logging: true,
                ..RoomConfig::default()
            },
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("actor commits config change");
    let reservation = committed
        .reservation
        .expect("config change reserves one durable effect");
    assert_eq!(
        reservation.ordinals.len(),
        1,
        "the config change should enqueue a single durable effect row"
    );
    state
        .deps
        .protocol
        .room_effect_outbox
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm committed config effect");

    let (alice_sender, mut alice_receiver) = mpsc::channel(2);
    let (bob_sender, mut bob_receiver) = mpsc::channel(2);
    register_test_connection(state.as_ref(), &alice, alice_sender).await;
    register_test_connection(state.as_ref(), &bob, bob_sender).await;

    let drain = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            drain_due_effects(state.as_ref(), super::super::store::HANDLER_GRACE_MS, 8).await
        })
    };
    let alice_delivery = receive(&mut alice_receiver).await;
    let bob_delivery = receive(&mut bob_receiver).await;
    assert!(xml(&alice_delivery).contains("code='170'"));
    assert!(
        xml(&bob_delivery).contains("code='170'"),
        "the join that landed immediately before the config mutation must be captured in the durable audience"
    );
    alice_delivery
        .write_acceptance
        .as_ref()
        .expect("alice config write acceptance")
        .acknowledge();
    bob_delivery
        .write_acceptance
        .as_ref()
        .expect("bob config write acceptance")
        .acknowledge();
    assert_eq!(
        drain
            .await
            .expect("serialized-audience drain join")
            .expect("serialized-audience drain")
            .drained,
        1
    );
}
