//! Regression coverage for the real interpreter -> commit -> execute seam.
use super::test_support::IngressFixture;
use super::{commit::commit_submission, execute::execute_effects, *};
use crate::server::routes::interpret::{
    effects::{EffectSink, PlanSink},
    interpret,
};
use jid::{BareJid, FullJid};
use kameo::actor::Spawn;
use std::time::Duration;
use waddle_xmpp::{
    ingress::IngressEffectIntent,
    mam::{MamStorage, SqlxMamStorage},
    muc::{
        pin::{PinChangeRequest, PinPreview, PinStateChange, PinnedEntry},
        room_actor::{ApplyPin, GetPinList, Join},
        room_registry_actor::{CreateRoom, RoomRegistryActor},
    },
    protocol::OutboundEvent,
    registry::ConnectionRegistry,
    xep::xep0421::OccupantIdSecret,
};
use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};

async fn plan_events(
    submission: &mut IngressSubmission,
    deps: &Deps<'_>,
    events: Vec<OutboundEvent>,
) {
    let sink = PlanSink::new();
    sink.set_room_execution(submission.plan.room_execution.clone());
    let capture = crate::ingress::IngressEffectCapture::new();
    sink.observe_sender(
        &submission
            .plan
            .sanitized_message
            .from
            .as_ref()
            .expect("sender")
            .clone()
            .try_into_full()
            .expect("full"),
    );
    let mut deps = deps.clone();
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    interpret(events, &deps).await;
    let (plan, room_execution) = sink.take();
    submission.plan.failure = sink.failure();
    submission.plan.plan = plan;
    submission.plan.room_execution = room_execution;
    submission.plan.intents = capture.snapshot().intents;
}

async fn room_pin_seam(mut fixture: IngressFixture, retract: bool, fail_pin: bool, unpin: bool) {
    let metrics = if fail_pin {
        Some(waddle_xmpp::telemetry::test_support::acquire().await)
    } else {
        None
    };
    let room: BareJid = "pins@muc.example.com".parse().expect("room");
    let sender: FullJid = "romeo@example.com/phone".parse().expect("sender");
    let occupant: FullJid = "juliet@example.com/phone".parse().expect("occupant");
    let registry = ConnectionRegistry::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    registry.register(occupant.clone(), tx.clone());
    let users = waddle_xmpp::registry::UserRegistryActor::spawn(
        waddle_xmpp::registry::UserRegistryActor::new(),
    );
    users
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: occupant.clone(),
            entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
        })
        .await
        .expect("register occupant");
    let rooms = RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".into(),
        OccupantIdSecret::new(vec![b'p'; 32]).expect("secret"),
    ));
    let actor = rooms
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "waddle".into(),
            channel_id: "pins".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    actor
        .ask(Join {
            nick: "juliet".into(),
            real_jid: occupant,
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("join");
    let mam: std::sync::Arc<dyn MamStorage> = std::sync::Arc::new(
        SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    let target_id = StanzaId::new("pin-target", room.clone().into());
    let mut target = waddle_xmpp::mam::ArchivedMessage::for_test(
        room.with_resource_str("romeo").expect("nick").into(),
        room.clone().into(),
    );
    target.id = target_id.id.clone();
    target.stanza_id = Some(target_id.clone());
    target.body = Some("target".into());
    target.message_type = xmpp_parsers::message::MessageType::Groupchat;
    mam.store_message(&room, &target)
        .await
        .expect("target archive");
    if retract || unpin {
        actor
            .ask(ApplyPin {
                change: PinStateChange::Pin(PinnedEntry {
                    target_stanza_id: target_id.clone(),
                    pinner_jid: sender.to_bare(),
                    pinned_at: chrono::Utc::now(),
                    preview: PinPreview::new(sender.to_bare(), None, "target", chrono::Utc::now()),
                }),
            })
            .await
            .expect("existing pin");
    }
    let mut deps = Deps::registry_only(&registry);
    deps.room_registry = Some(&rooms);
    deps.user_registry = Some(&users);
    deps.mam_storage = Some(&mam);
    let mut submission = fixture.submission(Some("pin-seam"), "pin request");
    submission.target = waddle_xmpp::ingress::NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    if retract {
        submission.plan.sanitized_message.payloads.push(
            waddle_xmpp::xep::xep0424::build_retract_element(&target_id.id),
        );
    }
    submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &waddle_xmpp::ingress::DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    if fail_pin {
        let synthetic_fence = || {
            waddle_xmpp::muc::RoomClaimFenceContext::new(
                waddle_xmpp::ownership::Entity::new(
                    waddle_xmpp::ownership::EntityType::RoomActor,
                    room.to_string(),
                ),
                waddle_xmpp::ownership::NodeIdentity::local(),
                waddle_xmpp::ownership::ClaimEpoch(1),
            )
        };
        #[cfg(feature = "clustering")]
        let fence = if fixture.db.driver() == crate::db::DatabaseDriver::Postgres {
            fixture.room_fence(&room).await
        } else {
            synthetic_fence()
        };
        #[cfg(not(feature = "clustering"))]
        let fence = synthetic_fence();
        actor
            .ask(waddle_xmpp::muc::room_actor::RestoreDurableRoomState {
                store: std::sync::Arc::new(FailingPinStore),
                claim_fence: fence.clone(),
            })
            .await
            .expect("failing projection store");
        submission.plan.room_execution = RoomExecutionPath::Local {
            room: room.clone(),
            fence: if fixture.db.driver() == crate::db::DatabaseDriver::Postgres {
                effects::room::RoomFenceRequirement::Guarded(fence)
            } else {
                effects::room::RoomFenceRequirement::Unfenced
            },
            snapshot_generation: 0,
        };
    }
    let _ = &mut fixture;
    let events = if retract {
        let mut message = submission.plan.sanitized_message.clone();
        message.id = Some(xmpp_parsers::message::Id("retraction".into()));
        add_stanza_id(
            &mut message,
            &StanzaId::new("retract-archive", room.clone().into()),
        );
        vec![
            OutboundEvent::ArchiveGroupchat {
                room: room.clone(),
                sender: sender.clone(),
                message: Box::new(message.clone()),
                sender_nickname_generation: 0,
                sender_item: None,
            },
            OutboundEvent::ApplyGroupchatRetractionTombstone {
                room: room.clone(),
                target_message_id: target_id.id.clone(),
                retraction_message: Box::new(message),
            },
        ]
    } else if unpin {
        vec![OutboundEvent::ApplyPinChange {
            room: room.clone(),
            request: PinChangeRequest::Unpin {
                target_stanza_id: target_id.clone(),
                pinner_jid: sender.to_bare(),
                pinner_nick: "romeo".into(),
                reason: Some("manual".into()),
            },
        }]
    } else {
        vec![OutboundEvent::ApplyPinChange {
            room: room.clone(),
            request: PinChangeRequest::Pin {
                target_stanza_id: target_id.clone(),
                pinner_jid: sender.to_bare(),
                pinner_nick: "romeo".into(),
                pinned_at: chrono::Utc::now(),
            },
        }]
    };
    plan_events(&mut submission, &deps, events).await;
    assert!(submission.plan.intents.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteMucSystemBroadcast { occupants, .. } if occupants.len() == 1)));
    assert_eq!(
        submission
            .plan
            .intents
            .iter()
            .filter(|intent| matches!(intent, IngressEffectIntent::SystemMessageArchive { .. }))
            .count(),
        1
    );
    if retract {
        assert!(submission
            .plan
            .intents
            .iter()
            .any(|intent| matches!(intent, IngressEffectIntent::ArchiveAuthoritative { .. })));
    }
    assert!(rx.try_recv().is_err(), "planning must not broadcast");
    let decision = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("commit pin plan");
    assert!(decision.class.advances());
    for (effect, receipts) in decision.external.iter().zip(&decision.external_receipts) {
        if matches!(
            effect,
            ExternalEffect::Room(effects::room::ExternalRoomEffect::ArchiveAfterPin { .. })
        ) {
            assert_eq!(
                receipts.len(),
                1,
                "the exact generated archive must map to its receipt"
            );
        }
    }
    assert_eq!(
        fixture.count("mam_messages").await,
        if retract { 2 } else { 1 },
        "system archive waits for pin confirmation"
    );
    assert!(rx.try_recv().is_err(), "Phase B must not broadcast");

    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    if fail_pin {
        assert!(rx.try_recv().is_err());
        let metrics = metrics.as_ref().expect("failure metrics");
        assert!(metrics
            .counter_sum("ingress.effects.unresolved", &[("kind", "room")])
            .is_some_and(|value| value >= 2));
        assert!(metrics
            .counter_sum("ingress.effects.unresolved", &[("kind", "delivery")])
            .is_some_and(|value| value >= 1));
        assert_eq!(fixture.count("mam_messages").await, 1);
        assert!(actor
            .ask(GetPinList)
            .await
            .expect("failed pin list")
            .is_empty());
        assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            0
        );
        assert!(report
            .outcomes
            .iter()
            .all(|(_, outcome)| *outcome == ExternalOutcome::Failed));
    } else {
        assert!(
            rx.try_recv().is_ok(),
            "system broadcast delivered to actual occupant"
        );
        assert_eq!(
            fixture.count("mam_messages").await,
            if retract { 3 } else { 2 }
        );
        assert_eq!(
            actor.ask(GetPinList).await.expect("pins").len(),
            usize::from(!retract && !unpin)
        );
        if retract {
            assert!(fixture
                .optional_text("SELECT body FROM mam_messages WHERE id = 'pin-target'")
                .await
                .is_none());
            assert!(mam
                .get_message("pin-target")
                .await
                .expect("target")
                .expect("row")
                .rich
                .expect("tombstone rich payload")
                .is_tombstoned());
        } else {
            assert_eq!(
                fixture.count("ingress_effect_intents").await,
                fixture.count("ingress_effect_receipts").await
            );
            assert_eq!(
                fixture
                    .count("ingress_messages WHERE terminal_at IS NOT NULL")
                    .await,
                1
            );
            if !unpin {
                let retained = actor.ask(GetPinList).await.expect("retained pin");
                plan_events(
                    &mut submission,
                    &deps,
                    vec![OutboundEvent::ApplyPinChange {
                        room: room.clone(),
                        request: PinChangeRequest::Pin {
                            target_stanza_id: target_id.clone(),
                            pinner_jid: sender.to_bare(),
                            pinner_nick: "new nickname".into(),
                            pinned_at: chrono::Utc::now() + chrono::Duration::seconds(30),
                        },
                    }],
                )
                .await;
                let duplicate = commit_submission(&fixture.uow, &submission, 3)
                    .await
                    .expect("retry with changed pin timestamp");
                assert_eq!(duplicate.message_key, decision.message_key);
                let replay = execute_effects(
                    &fixture.uow,
                    &fixture.db,
                    &duplicate,
                    &ImmediateSink,
                    &deps,
                    Duration::from_secs(5),
                )
                .await;
                assert!(
                    replay
                        .outcomes
                        .iter()
                        .all(|(_, outcome)| *outcome == ExternalOutcome::Done),
                    "restored pin dependencies resolve: {:?}",
                    replay.outcomes
                );
                assert_eq!(
                    actor.ask(GetPinList).await.expect("replayed pins"),
                    retained
                );
                assert!(
                    rx.try_recv().is_err(),
                    "duplicate suppresses non-sender broadcast"
                );
                assert_eq!(fixture.count("mam_messages").await, 2);
            }
        }
    }
    actor.kill();
    rooms.kill();
    users.kill();
    drop(mam);
    fixture.close().await;
}

#[tokio::test]
async fn pinned_retraction_plan_commit_execute_sqlite() {
    room_pin_seam(IngressFixture::sqlite().await, true, false, false).await;
}
#[tokio::test]
async fn pinned_retraction_plan_commit_execute_postgres() {
    if let Some(fixture) = IngressFixture::postgres("pinned_retraction").await {
        room_pin_seam(fixture, true, false, false).await;
    }
}
#[tokio::test]
async fn pin_system_broadcast_plan_commit_execute_sqlite() {
    room_pin_seam(IngressFixture::sqlite().await, false, false, false).await;
}
#[tokio::test]
async fn pin_system_broadcast_plan_commit_execute_postgres() {
    if let Some(fixture) = IngressFixture::postgres("pin_broadcast").await {
        room_pin_seam(fixture, false, false, false).await;
    }
}
#[tokio::test]
async fn pin_projection_commit_failure_plan_commit_execute_sqlite() {
    room_pin_seam(IngressFixture::sqlite().await, false, true, false).await;
}
#[tokio::test]
async fn pin_projection_commit_failure_plan_commit_execute_postgres() {
    if let Some(fixture) = IngressFixture::postgres("pin_failure").await {
        room_pin_seam(fixture, false, true, false).await;
    }
}

struct FailingPinStore;
impl waddle_xmpp::muc::durable::MucDurableStore for FailingPinStore {
    fn load_room_state_fenced<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::durable::MucDurableFuture<
        'a,
        Option<waddle_xmpp::muc::durable::DurableRoomState>,
    > {
        Box::pin(async { Ok(None) })
    }
    fn commit_room_mutation<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        intent: waddle_xmpp::muc::RoomDurableMutation,
        _effects: waddle_xmpp::muc::RoomMutationEffects,
    ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
        Box::pin(async move {
            assert!(
                matches!(
                    intent,
                    waddle_xmpp::muc::RoomDurableMutation::Projection(
                        waddle_xmpp::muc::durable::RoomProjection::Pin(_)
                    )
                ),
                "only pin projection attempted"
            );
            Err(waddle_xmpp::muc::RoomCommitError::OwnershipUnavailable)
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
async fn unpin_system_broadcast_plan_commit_execute_sqlite() {
    room_pin_seam(IngressFixture::sqlite().await, false, false, true).await;
}
#[tokio::test]
async fn unpin_system_broadcast_plan_commit_execute_postgres() {
    if let Some(fixture) = IngressFixture::postgres("unpin_broadcast").await {
        room_pin_seam(fixture, false, false, true).await;
    }
}

#[derive(Default)]
struct BlockedRoomRestore {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl waddle_xmpp::muc::durable::MucDurableStore for BlockedRoomRestore {
    fn load_room_state_fenced<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::durable::MucDurableFuture<
        'a,
        Option<waddle_xmpp::muc::durable::DurableRoomState>,
    > {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(None)
        })
    }

    fn commit_room_mutation<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        _intent: waddle_xmpp::muc::RoomDurableMutation,
        _effects: waddle_xmpp::muc::RoomMutationEffects,
    ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
        Box::pin(async { panic!("an incomplete pin plan must not mutate the room") })
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::durable::MucDurableFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}

async fn room_pin_snapshot_failure(fixture: IngressFixture, unpin: bool) {
    let room: BareJid = "snapshot-failure@muc.example.com".parse().expect("room");
    let sender: FullJid = "romeo@example.com/phone".parse().expect("sender");
    let registry = ConnectionRegistry::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    registry.register(sender.clone(), tx);
    let rooms = RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".into(),
        OccupantIdSecret::new(vec![b'p'; 32]).expect("secret"),
    ));
    let actor = rooms
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "waddle".into(),
            channel_id: "pins".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    let target_stanza_id = StanzaId::new("pin-target", room.clone().into());
    let entry = PinnedEntry {
        target_stanza_id: target_stanza_id.clone(),
        pinner_jid: sender.to_bare(),
        pinned_at: chrono::Utc::now(),
        preview: PinPreview::new(sender.to_bare(), None, "target", chrono::Utc::now()),
    };
    if unpin {
        actor
            .ask(ApplyPin {
                change: PinStateChange::Pin(entry.clone()),
            })
            .await
            .expect("existing pin");
    }
    let store = std::sync::Arc::new(BlockedRoomRestore::default());
    let blocked_actor = actor.clone();
    let blocked_store = store.clone();
    let fence = waddle_xmpp::muc::RoomClaimFenceContext::new(
        waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::RoomActor,
            room.to_string(),
        ),
        waddle_xmpp::ownership::NodeIdentity::local(),
        waddle_xmpp::ownership::ClaimEpoch(1),
    );
    let restore = tokio::spawn(async move {
        blocked_actor
            .ask(waddle_xmpp::muc::room_actor::RestoreDurableRoomState {
                store: blocked_store,
                claim_fence: fence,
            })
            .await
            .expect("release blocked restore");
    });
    store.entered.notified().await;
    // The registry can resolve this live actor, but GetRoomSnapshot really
    // times out behind the blocked restore in the actor's mailbox.
    let mut deps = Deps::registry_only(&registry);
    deps.room_registry = Some(&rooms);
    let mut submission = fixture.submission(Some("pin-snapshot-failure"), "pin request");
    let request = if unpin {
        PinChangeRequest::Unpin {
            target_stanza_id,
            pinner_jid: sender.to_bare(),
            pinner_nick: "romeo".into(),
            reason: Some("manual".into()),
        }
    } else {
        PinChangeRequest::Pin {
            target_stanza_id,
            pinner_jid: sender.to_bare(),
            pinner_nick: "romeo".into(),
            pinned_at: chrono::Utc::now(),
        }
    };
    plan_events(
        &mut submission,
        &deps,
        vec![OutboundEvent::ApplyPinChange { room, request }],
    )
    .await;
    assert_eq!(
        submission.plan.failure,
        Some(effects::PlanFailure::RoomSnapshotUnavailable)
    );
    assert!(submission.plan.plan.is_empty());
    assert!(submission.plan.intents.is_empty());
    let failure = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect_err("incomplete pin plan cannot commit");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    assert!(!failure.class().advances());
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_sm_streams",
        "ingress_sm_refs",
        "ingress_deliveries",
        "mam_messages",
        "inbox_entries",
    ] {
        assert_eq!(fixture.count(table).await, 0, "no writes to {table}");
    }
    assert!(rx.try_recv().is_err(), "no reply or broadcast");
    store.release.notify_one();
    restore.await.expect("restore task");
    assert_eq!(
        actor.ask(GetPinList).await.expect("unchanged pins"),
        if unpin { vec![entry] } else { vec![] }
    );
    actor.kill();
    rooms.kill();
    fixture.close().await;
}

#[tokio::test]
async fn pin_snapshot_failure_plan_is_nonadvancing_sqlite() {
    room_pin_snapshot_failure(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn pin_snapshot_failure_plan_is_nonadvancing_postgres() {
    if let Some(fixture) = IngressFixture::postgres("pin_snapshot_failure").await {
        room_pin_snapshot_failure(fixture, false).await;
    }
}

#[tokio::test]
async fn unpin_snapshot_failure_plan_is_nonadvancing_sqlite() {
    room_pin_snapshot_failure(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn unpin_snapshot_failure_plan_is_nonadvancing_postgres() {
    if let Some(fixture) = IngressFixture::postgres("unpin_snapshot_failure").await {
        room_pin_snapshot_failure(fixture, true).await;
    }
}
