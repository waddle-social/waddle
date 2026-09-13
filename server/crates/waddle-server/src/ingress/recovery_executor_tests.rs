//! Maintenance regressions against committed obligations and their actual sinks.
use crate::{
    ingress::{
        commit::commit_submission,
        effects::{delivery::ExternalDeliveryEffect, Effect, PlanSink},
        execute::{execute_effects, test_hooks},
        execute_uow::STALL_DELIVERY_RESOURCE,
        maintenance::{
            run_maintenance_pass_with_cursor, MaintenanceBudget, MaintenanceCursor,
            MaintenanceOutcome,
        },
        test_support::IngressFixture,
        Deps, ExternalEffect, ImmediateSink, IngressSubmission, RecoveryEnvironment,
    },
    ingress_uow::{CanonicalMessageRepository, EffectReceiptRepository},
    server::routes::websocket::{
        interpret_loop::build_interpret_deps, tests as socket_tests, WebSocketState,
    },
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use waddle_xmpp::{
    ingress::{
        DigestContext, DigestInput, EffectMessageIdentity, IngressEffectIntent, MessageKey,
        NormalizedTarget,
    },
    stream_management::{DetachedSession, InMemorySmSessionRegistry, SmSessionRegistry},
    Stanza,
};

struct StateEnvironment(Arc<WebSocketState>);
impl RecoveryEnvironment for StateEnvironment {
    fn recovery_deps(&self) -> Deps<'_> {
        build_interpret_deps(&self.0, None)
    }
}

// TestStateOverrides is private to websocket tests. Substitute the SM registry on
// the uniquely owned state produced by the shared database/authority constructor.
async fn state_for(
    fixture: &IngressFixture,
    sm: Arc<InMemorySmSessionRegistry>,
) -> Arc<WebSocketState> {
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared database");
    let state = socket_tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool),
        Arc::new(fixture.authority().await),
    )
    .await;
    let mut state = Arc::try_unwrap(state).unwrap_or_else(|_| panic!("unique state"));
    state.deps.protocol.sm_session_registry = sm;
    Arc::new(state)
}
fn immediate_recovery_budget() -> MaintenanceBudget {
    MaintenanceBudget {
        grace: chrono::Duration::zero(),
        recovery: Duration::from_secs(20),
        recovery_row: Duration::from_secs(5),
        hard_deadline: Duration::from_secs(30),
        ..MaintenanceBudget::DEFAULT
    }
}
async fn pass(
    f: &IngressFixture,
    env: &Arc<dyn RecoveryEnvironment>,
    cursor: &MaintenanceCursor,
) -> MaintenanceOutcome {
    run_maintenance_pass_with_cursor(
        &f.db,
        &f.uow,
        immediate_recovery_budget(),
        cursor,
        Some(env.clone()),
    )
    .await
}
async fn assert_recovered(f: &IngressFixture, key: MessageKey, receipts: i64) {
    let mut tx = f.uow.begin().await.expect("inspect recovery");
    assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("receipts complete"));
    assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal"));
    tx.commit().await.expect("inspection commit");
    assert_eq!(
        f.count(&format!(
            "ingress_effect_receipts WHERE message_key = '{}'",
            key.to_storage()
        ))
        .await,
        receipts
    );
}
async fn assert_pending(f: &IngressFixture, key: MessageKey) {
    let mut tx = f.uow.begin().await.expect("inspect pending");
    assert!(!CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("nonterminal"));
    assert!(!EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("missing receipts"));
    tx.commit().await.expect("inspection commit");
}
async fn persistent_sm(f: &IngressFixture) -> Arc<InMemorySmSessionRegistry> {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(f.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence))
}
async fn store_detached(sm: &InMemorySmSessionRegistry, resource: &jid::FullJid) {
    sm.store_session(DetachedSession {
        stream_id: resource.to_string(),
        user_id: resource.to_bare().to_string(),
        jid: resource.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    })
    .await
    .expect("store detached session");
}

async fn append_count(sm: &InMemorySmSessionRegistry, resource: &jid::FullJid) -> usize {
    sm.peek_session(&resource.to_string())
        .await
        .expect("peek session")
        .expect("retained session")
        .unacked_stanzas
        .len()
}

fn direct_submission(
    f: &IngressFixture,
    origin: &str,
    resources: &[jid::FullJid],
) -> IngressSubmission {
    let mut submission = f.submission(Some(origin), "lost canonical delivery");
    let identity = EffectMessageIdentity::capture_ordinal(0);
    submission.plan.intents = vec![IngressEffectIntent::RouteDirect {
        recipient: resources[0].to_bare(),
        fanout: resources.to_vec(),
        route_identity: identity.clone(),
    }];
    let sink = PlanSink::new();
    let registry = waddle_xmpp::registry::ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.effects = &sink;
    crate::server::routes::interpret::effects::delivery::record(
        &deps,
        ExternalDeliveryEffect::QueueDetached {
            route_identity: Some(identity),
            call_setup: None,
            bare: resources[0].to_bare(),
            resources: resources.to_vec(),
            stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
        },
    );
    submission.plan.plan = sink.take().0;
    submission
}
fn retarget(
    submission: &mut IngressSubmission,
    target: NormalizedTarget,
    type_: xmpp_parsers::message::MessageType,
) {
    submission.target = target;
    submission.plan.sanitized_message.type_ = type_;
    submission.plan.sanitized_message.to = Some(match &submission.target {
        NormalizedTarget::Bare(bare) => bare.clone().into(),
        NormalizedTarget::Full(full) => full.clone().into(),
        _ => panic!("direct target"),
    });
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![submission.sender.to_bare()],
            stanza_lang: None,
        },
    )
    .expect("retarget digest");
    for planned in &mut submission.plan.plan {
        if let Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            stanza,
            ..
        })) = &mut planned.effect
        {
            **stanza = Stanza::Message(submission.plan.sanitized_message.clone());
        }
    }
}
async fn detached_recovery(f: IngressFixture, stall_original: bool) {
    let sm = persistent_sm(&f).await;
    let resources = [
        "juliet@example.com/phone".parse().expect("phone"),
        "juliet@example.com/laptop".parse().expect("laptop"),
    ];
    for resource in &resources {
        store_detached(&sm, resource).await;
    }
    let state = state_for(&f, sm.clone()).await;
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    let submission = direct_submission(&f, "detached-recovery", &resources);
    let decision = commit_submission(&f.uow, &submission, 5)
        .await
        .expect("Phase B");
    let key = decision.message_key.expect("key");
    if stall_original {
        let entered = Arc::new(AtomicBool::new(false));
        let deps = env.recovery_deps();
        assert!(tokio::time::timeout(
            Duration::from_millis(500),
            STALL_DELIVERY_RESOURCE.scope(
                (resources[1].clone(), entered.clone()),
                execute_effects(
                    &f.uow,
                    &f.db,
                    &decision,
                    &ImmediateSink,
                    &deps,
                    Duration::from_secs(10)
                )
            )
        )
        .await
        .is_err());
        assert!(entered.load(Ordering::SeqCst));
        assert_eq!(append_count(&sm, &resources[0]).await, 1);
        assert_eq!(append_count(&sm, &resources[1]).await, 0);
    }
    assert_pending(&f, key).await;
    let cursor = MaintenanceCursor::default();
    for _ in 0..2 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
        for resource in &resources {
            assert_eq!(append_count(&sm, resource).await, 1);
        }
        assert_eq!(f.count("sm_ingress_appends").await, 2);
        assert_recovered(&f, key, 1).await;
    }
    f.close().await;
}

async fn live_route(f: IngressFixture, full: bool, detached_no_store: bool, headline: bool) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let sm = persistent_sm(&f).await;
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    let state = state_for(&f, sm.clone()).await;
    let (sender, mut rx) = tokio::sync::mpsc::channel(8);
    if detached_no_store {
        store_detached(&sm, &resource).await;
    } else {
        state
            .deps
            .protocol
            .connection_registry
            .register_with_carbons(resource.clone(), sender, false);
        assert!(
            crate::server::dual_registration::mirror_register(
                &state.deps.protocol.user_registry,
                resource.clone(),
                state
                    .deps
                    .protocol
                    .connection_registry
                    .get_entry(&resource)
                    .expect("live entry")
            )
            .await
        );
    }
    let mut submission = direct_submission(&f, "live-recovery", std::slice::from_ref(&resource));
    if detached_no_store {
        waddle_xmpp::xep::xep0334::add_hint(
            &mut submission.plan.sanitized_message,
            waddle_xmpp::xep::xep0334::Hint::NoStore,
        );
    }
    retarget(
        &mut submission,
        if full {
            NormalizedTarget::Full(resource.clone())
        } else {
            NormalizedTarget::Bare(resource.to_bare())
        },
        if headline {
            xmpp_parsers::message::MessageType::Headline
        } else {
            xmpp_parsers::message::MessageType::Chat
        },
    );
    let decision = commit_submission(&f.uow, &submission, 5)
        .await
        .expect("Phase B");
    let key = decision.message_key.expect("key");
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    let cursor = MaintenanceCursor::default();
    let before = metrics
        .counter_sum(
            "ingress.maintenance.unrecoverable_obligations",
            &[("kind", "route_direct")],
        )
        .unwrap_or(0);
    let deferred = full || headline;
    for i in 0..2 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
        if deferred {
            assert_pending(&f, key).await;
            assert_eq!(
                super::attempt_count(key),
                1,
                "cached unsupported rows are not attempted twice"
            );
            assert_eq!(
                metrics
                    .counter_sum(
                        "ingress.maintenance.unrecoverable_obligations",
                        &[("kind", "route_direct")]
                    )
                    .unwrap_or(0),
                before + 1
            );
            if detached_no_store {
                assert_eq!(append_count(&sm, &resource).await, 0);
            }
        } else {
            if i == 0 {
                let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                    .await
                    .expect("actual delivery")
                    .expect("frame");
                assert!(matches!(frame.stanza, Stanza::Message(_)));
            }
            assert_recovered(&f, key, 1).await;
        }
        assert!(rx.try_recv().is_err());
    }
    f.close().await;
}
mod family_tests {
    use crate::{
        ingress::{
            commit::commit_submission,
            effects::{
                delivery::{ExternalDeliveryEffect, PreparedOfflineNotification},
                room::ExternalRoomEffect,
                Effect, PlanSuppressionPolicy,
            },
            execute::{execute_effects, test_hooks},
            maintenance::{
                run_maintenance_pass_with_cursor, MaintenanceBudget, MaintenanceCursor,
                MaintenanceOutcome,
            },
            test_support::IngressFixture,
            ExternalEffect, ImmediateSink, IngressSubmission, PlannedEffect, RecoveryEnvironment,
        },
        ingress_uow::{CanonicalMessageRepository, EffectReceiptRepository},
        notification_outbox::{NotificationCandidate, NotificationOutboxStore},
        pending_delivery::DatabasePendingDeliveryStorage,
        server::routes::websocket::{interpret_loop::build_interpret_deps, WebSocketState},
    };
    use std::{sync::Arc, time::Duration};
    use waddle_xmpp::{
        ingress::{
            IngressEffectIntent, MessageKey, NotificationActivityMutation,
            NotificationCandidateOutcome, PendingDeliveryMutation,
        },
        pending_delivery::{PendingPayload, PendingRow, PendingRowId, QuotaPolicy},
    };

    struct FamilyEnvironment(Arc<WebSocketState>);
    impl RecoveryEnvironment for FamilyEnvironment {
        fn recovery_deps(&self) -> crate::server::routes::interpret::Deps<'_> {
            build_interpret_deps(self.0.as_ref(), None)
        }
    }
    pub(super) async fn family_state(fixture: &IngressFixture) -> Arc<WebSocketState> {
        let pool = crate::db::DatabasePool::new(
            crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
            crate::db::PoolConfig,
        )
        .await
        .expect("shared database pool");
        let mut state = crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
            Arc::new(pool), Arc::new(fixture.authority().await),
        ).await;
        let protocol = &mut Arc::get_mut(&mut state)
            .expect("unique fixture state")
            .deps
            .protocol;
        protocol.notification_settings_projection = Arc::new(
            crate::notification_settings_projection::NotificationSettingsProjectionStore::new(
                fixture.db.clone(),
            ),
        );
        protocol.pending_delivery_storage = Arc::new(
            DatabasePendingDeliveryStorage::from_database(
                fixture.db.clone(),
                QuotaPolicy::Unlimited,
            )
            .await
            .expect("pending storage"),
        );
        NotificationOutboxStore::new(fixture.db.clone())
            .await
            .expect("notification schema");
        state
    }
    pub(super) fn family_budget() -> MaintenanceBudget {
        MaintenanceBudget {
            grace: chrono::Duration::zero(),
            recovery: Duration::from_secs(10),
            recovery_row: Duration::from_secs(3),
            hard_deadline: Duration::from_secs(20),
            ..MaintenanceBudget::DEFAULT
        }
    }
    pub(super) async fn family_pass(
        fixture: &IngressFixture,
        state: &Arc<WebSocketState>,
        cursor: &MaintenanceCursor,
    ) -> MaintenanceOutcome {
        run_maintenance_pass_with_cursor(
            &fixture.db,
            &fixture.uow,
            family_budget(),
            cursor,
            Some(Arc::new(FamilyEnvironment(state.clone()))),
        )
        .await
    }
    pub(super) async fn family_recovered(fixture: &IngressFixture, key: MessageKey, receipts: i64) {
        let mut tx = fixture.uow.begin().await.expect("inspect recovery");
        assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
            .await
            .expect("complete receipts"));
        assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal row"));
        tx.commit().await.expect("inspection commit");
        assert_eq!(fixture.count("ingress_effect_receipts").await, receipts);
    }
    fn offline_plan(fixture: &IngressFixture, origin: &str) -> IngressSubmission {
        let mut submission = fixture.submission(Some(origin), "canonical offline body");
        let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
        let stamp = waddle_xmpp_core::xep0359::StanzaId::new(origin, recipient.clone().into());
        let row = PendingRow {
            id: PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Archived(stamp.clone()),
            flushed_in_session: None,
            outbound_sequence: None,
        };
        let candidate = NotificationCandidate::direct_message(
            recipient.clone(),
            submission.sender.clone().into(),
            stamp.clone(),
            false,
        )
        .expect("direct candidate");
        submission.plan.intents.extend([
            IngressEffectIntent::PendingDelivery {
                mutation: PendingDeliveryMutation::Archived {
                    recipient: recipient.clone(),
                    row_id: row.id.clone(),
                    archive_stanza_id: stamp.clone(),
                },
            },
            IngressEffectIntent::NotificationActivityPreview {
                owner: recipient.clone(),
                mutation: NotificationActivityMutation::NotificationCandidate {
                    conversation: recipient.clone(),
                    archive_stanza_id: stamp.clone(),
                    outcome: NotificationCandidateOutcome::Inserted,
                },
            },
            IngressEffectIntent::NotificationActivityPreview {
                owner: recipient.clone(),
                mutation: NotificationActivityMutation::OfflineDelivery {
                    conversation: recipient,
                    archive_stanza_id: stamp,
                },
            },
        ]);
        submission
            .plan
            .plan
            .push(PlannedEffect::new(Effect::External(
                ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
                    row,
                    prepared_notification: PreparedOfflineNotification::Prepared(Box::new(
                        candidate,
                    )),
                    original_message: Box::new(submission.plan.sanitized_message.clone()),
                }),
            )));
        submission
    }
    async fn offline_pending_row_and_candidate_recover_once(fixture: IngressFixture) {
        let state = family_state(&fixture).await;
        let decision = commit_submission(
            &fixture.uow,
            &offline_plan(&fixture, "offline-lost-phase-c"),
            5,
        )
        .await
        .expect("commit offline plan");
        let cursor = MaintenanceCursor::default();
        assert_eq!(fixture.count("pending_delivery").await, 0);
        assert_eq!(fixture.count("notification_candidates").await, 0);
        for _ in 0..2 {
            assert_eq!(
                family_pass(&fixture, &state, &cursor).await,
                MaintenanceOutcome::Complete
            );
            assert_eq!(fixture.count("pending_delivery").await, 1);
            assert_eq!(fixture.count("notification_candidates").await, 1);
            family_recovered(&fixture, decision.message_key.expect("key"), 3).await;
        }
        drop(state);
        fixture.close().await;
    }
    async fn observer_plugin_recovers_once(fixture: IngressFixture) {
        observer_recovery(fixture, false).await;
    }

    /// `warning == true` models a plugin whose only outcome is an error reply
    /// to the original sender: recovery has no such socket, so the row is
    /// evaluated once, cached as unsupported and the plugin is not re-invoked.
    async fn observer_recovery(fixture: IngressFixture, warning: bool) {
        use waddle_extensions::{
            observer_test_support::{ObserverTestBehavior, ObserverTestPlugin},
            ExtensionManager, PluginId,
        };
        let plugin_id = PluginId::new("recovery-observer").expect("plugin id");
        let behavior = if warning {
            ObserverTestBehavior::Warning
        } else {
            ObserverTestBehavior::Success
        };
        let plugin = ObserverTestPlugin::new(plugin_id.clone(), behavior);
        // A sibling plugin that succeeds: its receipt commits during the same
        // attempt, and the cached evidence must reflect that so the warning
        // plugin is not re-invoked on the next scan.
        let sibling_id = PluginId::new("recovery-observer-sibling").expect("sibling id");
        let sibling = ObserverTestPlugin::new(sibling_id.clone(), ObserverTestBehavior::Success);
        let mut plugins = vec![plugin.clone()];
        if warning {
            plugins.push(sibling.clone());
        }
        let manager = ExtensionManager::with_observer_test_plugins(plugins).await;
        let mut state = family_state(&fixture).await;
        Arc::get_mut(&mut state)
            .expect("unique state")
            .deps
            .protocol
            .extension_manager = Arc::new(manager);
        let mut submission =
            fixture.submission(Some("observer-lost-phase-c"), "frozen observer body");
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        submission
            .plan
            .intents
            .push(IngressEffectIntent::RoomObserver {
                room: room.clone(),
                plugin: plugin_id.clone(),
                requester: submission.sender.to_bare(),
                sender: submission.sender.clone(),
            });
        if warning {
            submission
                .plan
                .intents
                .push(IngressEffectIntent::RoomObserver {
                    room: room.clone(),
                    plugin: sibling_id.clone(),
                    requester: submission.sender.to_bare(),
                    sender: submission.sender.clone(),
                });
            submission.plan.plan.push(
                PlannedEffect::new(Effect::External(ExternalEffect::Room(
                    ExternalRoomEffect::ObserveRoomMessage {
                        room: room.clone(),
                        plugin: sibling_id,
                        requester: submission.sender.to_bare(),
                        sender: submission.sender.clone(),
                        message: Box::new(submission.plan.sanitized_message.clone()),
                        error_request: Box::new(submission.plan.sanitized_message.clone()),
                    },
                )))
                .with_suppression(PlanSuppressionPolicy::Always),
            );
            // A production groupchat row also carries the occupant fan-out,
            // which recovery cannot rebuild. The warning-only observer must
            // still be cached alongside that permanently pending sibling.
            submission
                .plan
                .intents
                .push(IngressEffectIntent::RouteMucGroupchat {
                    room: room.clone(),
                    occupants: vec!["occupant@example.com/phone".parse().expect("occupant")],
                    reflection: "room@muc.example.com/romeo".parse().expect("reflection"),
                    room_generation: waddle_xmpp::ingress::EntityGeneration::INITIAL,
                    route_identity: waddle_xmpp::ingress::EffectMessageIdentity::capture_ordinal(7),
                });
        }
        submission.plan.plan.push(
            PlannedEffect::new(Effect::External(ExternalEffect::Room(
                ExternalRoomEffect::ObserveRoomMessage {
                    room,
                    plugin: plugin_id,
                    requester: submission.sender.to_bare(),
                    sender: submission.sender.clone(),
                    message: Box::new(submission.plan.sanitized_message.clone()),
                    error_request: Box::new(submission.plan.sanitized_message.clone()),
                },
            )))
            .with_suppression(PlanSuppressionPolicy::Always),
        );
        let decision = commit_submission(&fixture.uow, &submission, 5)
            .await
            .expect("commit observer plan");
        assert!(plugin.invocations().is_empty());
        let key = decision.message_key.expect("key");
        let cursor = MaintenanceCursor::default();
        for _ in 0..2 {
            assert_eq!(
                family_pass(&fixture, &state, &cursor).await,
                MaintenanceOutcome::Complete
            );
            assert_eq!(plugin.invocations().len(), 1);
            assert_eq!(
                plugin.invocations()[0].body.as_str(),
                "frozen observer body"
            );
            if warning {
                super::assert_pending(&fixture, key).await;
                assert_eq!(
                    crate::ingress::recovery_executor::attempt_count(key),
                    1,
                    "a warning-only observer is cached as unsupported, not re-invoked"
                );
                assert_eq!(
                    sibling.invocations().len(),
                    1,
                    "the successful sibling ran once and its receipt is part of the cached evidence"
                );
                assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
            } else {
                family_recovered(&fixture, key, 1).await;
            }
        }
        drop(state);
        fixture.close().await;
    }
    async fn groupchat_notification_recovery_completes_via_maintenance(fixture: IngressFixture) {
        crate::pubsub::DatabasePubSubStorage::open(Some(fixture.db.database_url()))
            .await
            .expect("projection schema");
        let state = family_state(&fixture).await;
        let decision = commit_submission(
            &fixture.uow,
            &crate::ingress::recovery_tests::recovery_plan(&fixture),
            5,
        )
        .await
        .expect("commit groupchat plan");
        let key = decision.message_key.expect("key");
        let mut blocker = fixture.uow.begin().await.expect("blocker");
        assert!(CanonicalMessageRepository::lock(&mut blocker, key)
            .await
            .expect("canonical lock"));
        let cursor = MaintenanceCursor::default();
        let pass = family_pass(&fixture, &state, &cursor);
        let sweep =
            crate::server::routes::interpret::reconcile_groupchat_notification_candidates_for_sweep(
                &state, 10,
            );
        let release = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            blocker.commit().await.expect("release canonical lock");
        };
        let (outcome, swept, ()) = tokio::join!(pass, sweep, release);
        assert_eq!(outcome, MaintenanceOutcome::Complete);
        assert!(!swept.had_failure);
        assert_eq!(fixture.count("notification_candidates").await, 1);
        assert_eq!(
            fixture
                .count("groupchat_notification_recovery WHERE completed_at_ms IS NOT NULL")
                .await,
            1
        );
        family_recovered(&fixture, key, 4).await;
        assert_eq!(
            family_pass(&fixture, &state, &cursor).await,
            MaintenanceOutcome::Complete
        );
        let swept = crate::server::routes::interpret::reconcile_groupchat_notification_candidates_for_sweep(&state, 10).await;
        assert_eq!(swept.completed, 0);
        assert_eq!(fixture.count("notification_candidates").await, 1);
        assert_eq!(
            fixture
                .count("groupchat_notification_recovery WHERE completed_at_ms IS NOT NULL")
                .await,
            1
        );
        family_recovered(&fixture, key, 4).await;
        drop(state);
        fixture.close().await;
    }
    async fn live_duplicate_and_recovery_serialize_offline_on_the_canonical_lock(
        fixture: IngressFixture,
    ) {
        let state = family_state(&fixture).await;
        let submission = offline_plan(&fixture, "offline-duplicate-recovery");
        let initial = commit_submission(&fixture.uow, &submission, 5)
            .await
            .expect("commit offline");
        let duplicate = commit_submission(&fixture.uow, &submission, 5)
            .await
            .expect("duplicate offline");
        let key = initial.message_key.expect("key");
        assert_eq!(duplicate.message_key, Some(key));
        let mut blocker = fixture.uow.begin().await.expect("blocker");
        assert!(CanonicalMessageRepository::lock(&mut blocker, key)
            .await
            .expect("canonical lock"));
        let cursor = MaintenanceCursor::default();
        let deps = build_interpret_deps(state.as_ref(), None);
        let pass = family_pass(&fixture, &state, &cursor);
        let foreground = execute_effects(
            &fixture.uow,
            &fixture.db,
            &duplicate,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        );
        let release = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            blocker.commit().await.expect("release canonical lock");
        };
        let (outcome, report, ()) = tokio::join!(pass, foreground, release);
        assert_eq!(outcome, MaintenanceOutcome::Complete);
        assert!(report.receipt_failures.is_empty());
        assert!(report.terminalization_failure.is_none());
        assert_eq!(fixture.count("pending_delivery").await, 1);
        assert_eq!(fixture.count("notification_candidates").await, 1);
        family_recovered(&fixture, key, 3).await;
        assert_eq!(
            family_pass(&fixture, &state, &cursor).await,
            MaintenanceOutcome::Complete
        );
        assert_eq!(fixture.count("pending_delivery").await, 1);
        assert_eq!(fixture.count("notification_candidates").await, 1);
        family_recovered(&fixture, key, 3).await;
        drop(deps);
        drop(state);
        fixture.close().await;
    }
    async fn row_deadline_bounds_a_stalled_groupchat_delegate_and_later_rows_still_run(
        fixture: IngressFixture,
    ) {
        crate::pubsub::DatabasePubSubStorage::open(Some(fixture.db.database_url()))
            .await
            .expect("projection schema");
        let state = family_state(&fixture).await;
        let first = commit_submission(
            &fixture.uow,
            &crate::ingress::recovery_tests::recovery_plan(&fixture),
            5,
        )
        .await
        .expect("commit delegate");
        let key = first.message_key.expect("delegate key");
        let backdate = match fixture.db.driver() {
            crate::db::DatabaseDriver::Postgres => "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE message_key = ?::uuid",
            crate::db::DatabaseDriver::Sqlite => "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE message_key = ?",
        };
        fixture
            .execute(
                backdate,
                crate::db_params![
                    (chrono::Utc::now() - chrono::Duration::seconds(2)).to_rfc3339(),
                    key.to_storage().to_string()
                ],
            )
            .await;
        let later = commit_submission(
            &fixture.uow,
            &offline_plan(&fixture, "later-offline-recovery"),
            5,
        )
        .await
        .expect("commit later row");
        let later_key = later.message_key.expect("later key");
        let gate = test_hooks::pause_after_recovery_freeze(key);
        let cursor = MaintenanceCursor::default();
        let pass = family_pass(&fixture, &state, &cursor);
        let block_delegate = async {
            tokio::time::timeout(Duration::from_secs(5), gate.wait_until_reached())
                .await
                .expect("freeze gate");
            let mut blocker = fixture.uow.begin().await.expect("delegate blocker");
            assert!(CanonicalMessageRepository::lock(&mut blocker, key)
                .await
                .expect("delegate canonical lock"));
            gate.release();
            // Wait until A's delegate has failed and the same pass enters B,
            // rather than racing a wall-clock sleep against delegate startup.
            tokio::time::timeout(Duration::from_millis(750), async {
                while crate::ingress::recovery_executor::attempt_count(later_key) == 0 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("delegate's 100 ms lock timeout advances to B before the row deadline");
            blocker
                .commit()
                .await
                .expect("release failed delegate lock");
        };
        let (outcome, ()) = tokio::join!(pass, block_delegate);
        assert_eq!(outcome, MaintenanceOutcome::Partial);
        let mut tx = fixture.uow.begin().await.expect("inspect partial");
        assert!(!CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("delegate pending"));
        assert!(CanonicalMessageRepository::is_terminal(&mut tx, later_key)
            .await
            .expect("later row terminal"));
        assert!(
            EffectReceiptRepository::receipts_complete(&mut tx, later_key)
                .await
                .expect("later receipts")
        );
        tx.commit().await.expect("inspection commit");
        assert_eq!(fixture.count("pending_delivery").await, 1);
        assert_eq!(fixture.count("notification_candidates").await, 1);
        assert_eq!(
            fixture
                .count("groupchat_notification_recovery WHERE completed_at_ms IS NOT NULL")
                .await,
            0
        );
        assert_eq!(
            family_pass(&fixture, &state, &cursor).await,
            MaintenanceOutcome::Complete
        );
        assert_eq!(fixture.count("pending_delivery").await, 1);
        assert_eq!(fixture.count("notification_candidates").await, 2);
        assert_eq!(
            fixture
                .count("groupchat_notification_recovery WHERE completed_at_ms IS NOT NULL")
                .await,
            1
        );
        family_recovered(&fixture, key, 7).await;
        assert_eq!(
            family_pass(&fixture, &state, &cursor).await,
            MaintenanceOutcome::Complete
        );
        assert_eq!(fixture.count("pending_delivery").await, 1);
        assert_eq!(fixture.count("notification_candidates").await, 2);
        family_recovered(&fixture, key, 7).await;
        drop(state);
        fixture.close().await;
    }
    #[tokio::test]
    async fn sqlite_offline_pending_row_and_candidate_recover_once() {
        offline_pending_row_and_candidate_recover_once(IngressFixture::sqlite().await).await;
    }
    #[tokio::test]
    async fn postgres_offline_pending_row_and_candidate_recover_once() {
        if let Some(fixture) = IngressFixture::postgres("family_0").await {
            offline_pending_row_and_candidate_recover_once(fixture).await;
        }
    }
    #[tokio::test]
    async fn sqlite_observer_warning_is_evaluated_once_and_left_pending() {
        observer_recovery(IngressFixture::sqlite().await, true).await;
    }
    #[tokio::test]
    async fn postgres_observer_warning_is_evaluated_once_and_left_pending() {
        if let Some(fixture) = IngressFixture::postgres("recovery_observer_warning").await {
            observer_recovery(fixture, true).await;
        }
    }
    #[tokio::test]
    async fn sqlite_observer_plugin_recovers_once() {
        observer_plugin_recovers_once(IngressFixture::sqlite().await).await;
    }
    #[tokio::test]
    async fn postgres_observer_plugin_recovers_once() {
        if let Some(fixture) = IngressFixture::postgres("family_1").await {
            observer_plugin_recovers_once(fixture).await;
        }
    }
    #[tokio::test]
    async fn sqlite_groupchat_notification_recovery_completes_via_maintenance() {
        groupchat_notification_recovery_completes_via_maintenance(IngressFixture::sqlite().await)
            .await;
    }
    #[tokio::test]
    async fn postgres_groupchat_notification_recovery_completes_via_maintenance() {
        if let Some(fixture) = IngressFixture::postgres("family_2").await {
            groupchat_notification_recovery_completes_via_maintenance(fixture).await;
        }
    }
    #[tokio::test]
    async fn sqlite_live_duplicate_and_recovery_serialize_offline_on_the_canonical_lock() {
        live_duplicate_and_recovery_serialize_offline_on_the_canonical_lock(
            IngressFixture::sqlite().await,
        )
        .await;
    }
    #[tokio::test]
    async fn postgres_live_duplicate_and_recovery_serialize_offline_on_the_canonical_lock() {
        if let Some(fixture) = IngressFixture::postgres("family_4").await {
            live_duplicate_and_recovery_serialize_offline_on_the_canonical_lock(fixture).await;
        }
    }
    #[tokio::test]
    async fn sqlite_row_deadline_bounds_a_stalled_groupchat_delegate_and_later_rows_still_run() {
        row_deadline_bounds_a_stalled_groupchat_delegate_and_later_rows_still_run(
            IngressFixture::sqlite().await,
        )
        .await;
    }
    #[tokio::test]
    async fn postgres_row_deadline_bounds_a_stalled_groupchat_delegate_and_later_rows_still_run() {
        if let Some(fixture) = IngressFixture::postgres("family_5").await {
            row_deadline_bounds_a_stalled_groupchat_delegate_and_later_rows_still_run(fixture)
                .await;
        }
    }
}
mod pin_tests {
    use crate::ingress::{
        commit::commit_submission,
        execute::{execute_effects, test_hooks},
        execute_uow::STALL_DELIVERY_RESOURCE,
        maintenance::{
            run_maintenance_pass_with_cursor, MaintenanceBudget, MaintenanceCursor,
            MaintenanceOutcome,
        },
        test_support::IngressFixture,
        IngressDecision, IngressEffectCapture, IngressSubmission, RecoveryEnvironment,
    };
    use crate::ingress_uow::{CanonicalMessageRepository, EffectReceiptRepository};
    use crate::server::routes::{
        interpret::{
            effects::{EffectSink, ImmediateSink, PlanSink},
            Deps,
        },
        websocket::{
            handlers::message::dispatch_early_message,
            interpret_loop::build_interpret_deps,
            tests::{
                create_test_websocket_state_with_db_pool_and_ingress, register_test_connection,
            },
            DmPairKey, WebSocketState,
        },
    };
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::Duration,
    };
    use waddle_xmpp::{
        ingress::{DigestContext, DigestInput, IngressEffectIntent},
        mam::{ArchivedMessage, SqlxMamStorage},
        registry::OutboundStanza,
        Stanza,
    };
    use waddle_xmpp_core::xep0359::StanzaId;

    struct StateEnvironment(Arc<WebSocketState>);
    impl RecoveryEnvironment for StateEnvironment {
        fn recovery_deps(&self) -> Deps<'_> {
            build_interpret_deps(&self.0, None)
        }
    }
    async fn state_for(f: &IngressFixture) -> Arc<WebSocketState> {
        let pool = crate::db::DatabasePool::new(
            crate::db::DatabaseConfig::new(f.db.driver(), f.db.database_url()),
            crate::db::PoolConfig,
        )
        .await
        .expect("pool");
        let mut state = create_test_websocket_state_with_db_pool_and_ingress(
            Arc::new(pool),
            Arc::new(f.authority().await),
        )
        .await;
        Arc::get_mut(&mut state)
            .expect("exclusive state")
            .deps
            .protocol
            .mam_storage = Arc::new(
            SqlxMamStorage::open(f.db.database_url())
                .await
                .expect("MAM"),
        );
        state
    }
    async fn seed_target(state: &WebSocketState, submission: &IngressSubmission) -> StanzaId {
        let sender = submission.sender.to_bare();
        let peer = submission
            .plan
            .sanitized_message
            .to
            .as_ref()
            .expect("peer")
            .to_bare();
        let target = StanzaId::new("recovery-pin-target", sender.clone().into());
        for (archive, id) in [(&sender, "sender-copy"), (&peer, "peer-copy")] {
            state
                .deps
                .protocol
                .mam_storage
                .store_message(
                    archive,
                    &ArchivedMessage {
                        ordinal: None,
                        id: id.to_owned(),
                        body: Some("important message".into()),
                        stanza_id: Some(target.clone()),
                        message_type: xmpp_parsers::message::MessageType::Chat,
                        ..ArchivedMessage::for_test(
                            submission.sender.clone().into(),
                            peer.clone().into(),
                        )
                    },
                )
                .await
                .expect("archive target");
        }
        StanzaId::new(target.id, peer.into())
    }
    async fn plan(
        state: &WebSocketState,
        submission: &mut IngressSubmission,
        target: &StanzaId,
        unpin: bool,
    ) {
        let message = &mut submission.plan.sanitized_message;
        message.payloads.push(if unpin {
            waddle_xmpp::xep::xep_waddle_pin::build_unpinned_element(target)
        } else {
            waddle_xmpp::xep::build_pinned_message_element(target)
        });
        submission.digest_input = DigestInput::from_parsed(
            message,
            &DigestContext {
                target: submission.target.clone(),
                server_authorities: vec![submission.sender.to_bare()],
                stanza_lang: None,
            },
        )
        .expect("digest");
        let sink = PlanSink::new();
        sink.observe_sender(&submission.sender);
        let capture = IngressEffectCapture::new();
        let mut deps = build_interpret_deps(state, None);
        deps.effects = &sink;
        deps.ingress_effect_capture = Some(capture.clone());
        assert!(
            dispatch_early_message(message, &submission.sender, &deps)
                .await
                .is_some(),
            "production pin planner handles request"
        );
        submission.plan.failure = sink.failure();
        submission.plan.rejection = sink.rejection();
        submission.plan.plan = sink.take().0;
        submission.plan.intents = capture.snapshot().intents;
        assert!(submission
            .plan
            .intents
            .iter()
            .any(|i| matches!(i, IngressEffectIntent::DmPinMutation { .. })));
    }
    async fn abort_before_routes(
        f: &IngressFixture,
        state: &WebSocketState,
        decision: &IngressDecision,
    ) {
        let resource = decision
            .external
            .iter()
            .find_map(|effect| {
                use crate::server::routes::interpret::effects::{
                    delivery::ExternalDeliveryEffect, ExternalEffect,
                };
                match effect {
                    ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                        jid, ..
                    }) => Some(jid.clone()),
                    ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
                        resources,
                        ..
                    }) => resources.first().cloned(),
                    _ => None,
                }
            })
            .expect("first executed pin route");
        let entered = Arc::new(AtomicBool::new(false));
        let deps = build_interpret_deps(state, None);
        assert!(
            tokio::time::timeout(
                Duration::from_secs(2),
                STALL_DELIVERY_RESOURCE.scope(
                    (resource, entered.clone()),
                    execute_effects(
                        &f.uow,
                        &f.db,
                        decision,
                        &ImmediateSink,
                        &deps,
                        Duration::from_secs(10)
                    )
                )
            )
            .await
            .is_err(),
            "Phase C is cancelled at first route"
        );
        assert!(entered.load(Ordering::SeqCst));
    }
    async fn terminal(f: &IngressFixture, decision: &IngressDecision) -> bool {
        let mut tx = f.uow.begin().await.expect("read");
        let terminal =
            CanonicalMessageRepository::is_terminal(&mut tx, decision.message_key.expect("key"))
                .await
                .expect("terminal");
        tx.commit().await.expect("read commit");
        terminal
    }
    async fn mutation_receipted(
        f: &IngressFixture,
        submission: &IngressSubmission,
        decision: &IngressDecision,
    ) -> bool {
        let intent = submission
            .plan
            .intents
            .iter()
            .find(|i| matches!(i, IngressEffectIntent::DmPinMutation { .. }))
            .expect("mutation");
        let receipt = crate::ingress::receipt_key(intent).expect("receipt");
        let mut tx = f.uow.begin().await.expect("receipt read");
        let result = EffectReceiptRepository::contains(
            &mut tx,
            decision.message_key.expect("key"),
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("contains");
        tx.commit().await.expect("commit");
        result
    }
    async fn pass(f: &IngressFixture, state: &Arc<WebSocketState>, cursor: &MaintenanceCursor) {
        assert_eq!(
            run_maintenance_pass_with_cursor(
                &f.db,
                &f.uow,
                MaintenanceBudget {
                    grace: chrono::Duration::zero(),
                    recovery: Duration::from_secs(10),
                    recovery_row: Duration::from_secs(5),
                    hard_deadline: Duration::from_secs(20),
                    ..MaintenanceBudget::DEFAULT
                },
                cursor,
                Some(Arc::new(StateEnvironment(state.clone())))
            )
            .await,
            MaintenanceOutcome::Complete
        );
    }
    fn notification(
        rx: &mut tokio::sync::mpsc::Receiver<OutboundStanza>,
        target: &StanzaId,
        action: &str,
    ) {
        let outbound = rx.try_recv().expect("one participant event");
        let Stanza::Message(message) = outbound.stanza else {
            panic!("message")
        };
        let event = message
            .payloads
            .iter()
            .find(|p| p.name() == "pin-event" && p.ns() == waddle_xmpp::xep::NS_WADDLE_PIN_V0)
            .expect("synthetic pin event");
        assert_eq!(event.attr("action"), Some(action));
        assert_eq!(event.attr("target"), Some(target.id.as_str()));
        assert_eq!(
            waddle_xmpp_core::xep0359::extract_stanza_ids(&message).len(),
            1
        );
        assert!(rx.try_recv().is_err(), "exactly one event");
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Scenario {
        LostRoutes,
        LaterUnpin,
        FailedReceipt,
        ChangedEvidence,
    }
    async fn pin_recovery(f: IngressFixture, scenario: Scenario) {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = state_for(&f).await;
        let mut submission = f.submission(Some("recovery-pin"), "pin request");
        let target = seed_target(&state, &submission).await;
        let peer: jid::FullJid = "juliet@example.com/phone".parse().expect("peer");
        let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
        let (peer_tx, mut peer_rx) = tokio::sync::mpsc::channel(8);
        register_test_connection(&state, &submission.sender, sender_tx).await;
        register_test_connection(&state, &peer, peer_tx).await;
        let pair = DmPairKey::new(submission.sender.to_bare(), peer.to_bare());
        plan(&state, &mut submission, &target, false).await;
        let mut decision = commit_submission(&f.uow, &submission, 1)
            .await
            .expect("pin commit");
        let key = decision.message_key.expect("key");
        let cursor = MaintenanceCursor::default();
        if scenario == Scenario::ChangedEvidence {
            pass(&f, &state, &cursor).await;
            assert_eq!(crate::ingress::recovery_executor::attempt_count(key), 1);
            assert!(!terminal(&f, &decision).await);
            assert!(!state.deps.protocol.dm_pin_store.contains(&pair, &target));
            assert!(sender_rx.try_recv().is_err() && peer_rx.try_recv().is_err());
            decision = commit_submission(&f.uow, &submission, 1)
                .await
                .expect("foreground duplicate");
        }
        if scenario == Scenario::FailedReceipt {
            let intent = submission
                .plan
                .intents
                .iter()
                .find(|i| matches!(i, IngressEffectIntent::DmPinMutation { .. }))
                .expect("mutation");
            test_hooks::fail_receipt_once(
                key,
                crate::ingress::receipt_key(intent).expect("receipt"),
            );
        }
        abort_before_routes(&f, &state, &decision).await;
        assert!(state.deps.protocol.dm_pin_store.contains(&pair, &target));
        assert_eq!(
            mutation_receipted(&f, &submission, &decision).await,
            scenario != Scenario::FailedReceipt
        );
        assert!(sender_rx.try_recv().is_err() && peer_rx.try_recv().is_err());
        if matches!(scenario, Scenario::LaterUnpin | Scenario::FailedReceipt) {
            let mut unpin = f.submission(Some("recovery-unpin"), "unpin request");
            plan(&state, &mut unpin, &target, true).await;
            let unpin = commit_submission(&f.uow, &unpin, 2)
                .await
                .expect("unpin commit");
            let deps = build_interpret_deps(&state, None);
            let report = execute_effects(
                &f.uow,
                &f.db,
                &unpin,
                &ImmediateSink,
                &deps,
                Duration::from_secs(10),
            )
            .await;
            assert!(report.receipt_failures.is_empty());
            assert!(terminal(&f, &unpin).await);
            notification(&mut sender_rx, &target, "unpinned");
            notification(&mut peer_rx, &target, "unpinned");
            assert!(!state.deps.protocol.dm_pin_store.contains(&pair, &target));
        }
        pass(&f, &state, &cursor).await;
        if scenario == Scenario::FailedReceipt {
            assert!(!terminal(&f, &decision).await);
            assert!(sender_rx.try_recv().is_err() && peer_rx.try_recv().is_err());
            assert!(!state.deps.protocol.dm_pin_store.contains(&pair, &target));
            assert_eq!(
                metrics.counter_sum(
                    "ingress.maintenance.unrecoverable_obligations",
                    &[("kind", "route_direct")]
                ),
                Some(1)
            );
            assert_eq!(
                metrics.counter_sum(
                    "ingress.maintenance.unrecoverable_obligations",
                    &[("kind", "dm_pin_mutation")]
                ),
                Some(1)
            );
        } else {
            notification(&mut sender_rx, &target, "pinned");
            notification(&mut peer_rx, &target, "pinned");
            assert!(terminal(&f, &decision).await);
            let mut tx = f.uow.begin().await.expect("all receipt read");
            for receipt in decision.external_receipts.iter().flatten() {
                assert!(EffectReceiptRepository::contains(
                    &mut tx,
                    key,
                    receipt.kind,
                    &receipt.semantic_identity_hash
                )
                .await
                .expect("receipt"));
            }
            tx.commit().await.expect("commit");
        }
        let attempts = crate::ingress::recovery_executor::attempt_count(key);
        assert_eq!(
            attempts,
            if scenario == Scenario::ChangedEvidence {
                2
            } else {
                1
            }
        );
        pass(&f, &state, &cursor).await;
        assert_eq!(
            crate::ingress::recovery_executor::attempt_count(key),
            attempts
        );
        assert!(sender_rx.try_recv().is_err() && peer_rx.try_recv().is_err());
        assert_eq!(
            state.deps.protocol.dm_pin_store.contains(&pair, &target),
            matches!(scenario, Scenario::LostRoutes | Scenario::ChangedEvidence)
        );
        f.close().await;
    }
    #[tokio::test]
    async fn sqlite_receipted_pin_with_lost_routes_recovers_each_route_once() {
        pin_recovery(IngressFixture::sqlite().await, Scenario::LostRoutes).await;
    }
    #[tokio::test]
    async fn postgres_receipted_pin_with_lost_routes_recovers_each_route_once() {
        if let Some(f) = IngressFixture::postgres("pin_lostroutes").await {
            pin_recovery(f, Scenario::LostRoutes).await;
        }
    }
    #[tokio::test]
    async fn sqlite_completed_pin_with_lost_route_is_not_re_pinned_after_unpin() {
        pin_recovery(IngressFixture::sqlite().await, Scenario::LaterUnpin).await;
    }
    #[tokio::test]
    async fn postgres_completed_pin_with_lost_route_is_not_re_pinned_after_unpin() {
        if let Some(f) = IngressFixture::postgres("pin_laterunpin").await {
            pin_recovery(f, Scenario::LaterUnpin).await;
        }
    }
    #[tokio::test]
    async fn sqlite_unreceipted_pin_mutation_is_deferred_after_a_later_unpin() {
        pin_recovery(IngressFixture::sqlite().await, Scenario::FailedReceipt).await;
    }
    #[tokio::test]
    async fn postgres_unreceipted_pin_mutation_is_deferred_after_a_later_unpin() {
        if let Some(f) = IngressFixture::postgres("pin_failedreceipt").await {
            pin_recovery(f, Scenario::FailedReceipt).await;
        }
    }
    #[tokio::test]
    async fn sqlite_changed_evidence_re_evaluates_a_cached_unsupported_row() {
        pin_recovery(IngressFixture::sqlite().await, Scenario::ChangedEvidence).await;
    }
    #[tokio::test]
    async fn postgres_changed_evidence_re_evaluates_a_cached_unsupported_row() {
        if let Some(f) = IngressFixture::postgres("pin_changedevidence").await {
            pin_recovery(f, Scenario::ChangedEvidence).await;
        }
    }
}

#[tokio::test]
async fn sqlite_detached_route_recovers_each_resource_once() {
    let fixture = IngressFixture::sqlite().await;
    detached_recovery(fixture, false).await;
}
#[tokio::test]
async fn postgres_detached_route_recovers_each_resource_once() {
    if let Some(fixture) = IngressFixture::postgres("detached_route_recovers_each_resour").await {
        detached_recovery(fixture, false).await;
    }
}

#[tokio::test]
async fn sqlite_stalled_execution_is_recovered_without_double_append() {
    let fixture = IngressFixture::sqlite().await;
    detached_recovery(fixture, true).await;
}
#[tokio::test]
async fn postgres_stalled_execution_is_recovered_without_double_append() {
    if let Some(fixture) = IngressFixture::postgres("stalled_execution_is_recovered_with").await {
        detached_recovery(fixture, true).await;
    }
}

#[tokio::test]
async fn sqlite_delegated_live_full_target_route_is_left_pending() {
    let fixture = IngressFixture::sqlite().await;
    live_route(fixture, true, false, false).await;
}
#[tokio::test]
async fn postgres_delegated_live_full_target_route_is_left_pending() {
    if let Some(fixture) = IngressFixture::postgres("delegated_live_full_target_route_is").await {
        live_route(fixture, true, false, false).await;
    }
}

#[tokio::test]
async fn sqlite_bare_target_live_route_recovers_once() {
    let fixture = IngressFixture::sqlite().await;
    live_route(fixture, false, false, false).await;
}
#[tokio::test]
async fn postgres_bare_target_live_route_recovers_once() {
    if let Some(fixture) = IngressFixture::postgres("bare_target_live_route_recovers_onc").await {
        live_route(fixture, false, false, false).await;
    }
}

#[tokio::test]
async fn sqlite_detached_no_store_full_target_route_is_deferred() {
    let fixture = IngressFixture::sqlite().await;
    live_route(fixture, true, true, false).await;
}
#[tokio::test]
async fn postgres_detached_no_store_full_target_route_is_deferred() {
    if let Some(fixture) = IngressFixture::postgres("detached_no_store_full_target_route").await {
        live_route(fixture, true, true, false).await;
    }
}

#[tokio::test]
async fn sqlite_headline_route_is_left_pending() {
    let fixture = IngressFixture::sqlite().await;
    live_route(fixture, false, false, true).await;
}
#[tokio::test]
async fn postgres_headline_route_is_left_pending() {
    if let Some(fixture) = IngressFixture::postgres("headline_route_is_left_pending").await {
        live_route(fixture, false, false, true).await;
    }
}

async fn unrecoverable_only_rows_do_not_enter_the_recovery_scan(f: IngressFixture) {
    let sm = persistent_sm(&f).await;
    let state = state_for(&f, sm).await;
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    let mut submission = f.submission(Some("carbons-only"), "unrecoverable carbon");
    submission.plan.intents.push(IngressEffectIntent::Carbons {
        carbon_recipients: vec!["romeo@example.com/laptop".parse().expect("carbon")],
        excluded_source: submission.sender.clone(),
        kind: waddle_xmpp::protocol::CarbonKind::Sent,
    });
    let key = commit_submission(&f.uow, &submission, 5)
        .await
        .expect("carbons commit")
        .message_key
        .expect("key");
    let cursor = MaintenanceCursor::default();
    for _ in 0..2 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
        assert_eq!(super::attempt_count(key), 0);
        assert_pending(&f, key).await;
        assert_eq!(f.count("ingress_effect_receipts").await, 0);
    }
    f.close().await;
}
async fn backdate_created(f: &IngressFixture, key: MessageKey, seconds: i64) {
    let sql = match f.db.driver() {
        crate::db::DatabaseDriver::Postgres => "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE message_key = ?::uuid",
        crate::db::DatabaseDriver::Sqlite => "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE message_key = ?",
    };
    f.execute(
        sql,
        crate::db_params![
            (chrono::Utc::now() - chrono::Duration::seconds(seconds)).to_rfc3339(),
            key.to_storage().to_string()
        ],
    )
    .await;
}
async fn unsupported_backlog_is_evaluated_once_then_skipped(f: IngressFixture) {
    let sm = persistent_sm(&f).await;
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    store_detached(&sm, &resource).await;
    let state = state_for(&f, sm.clone()).await;
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    let mut keys = Vec::new();
    for index in 0..70 {
        let mut submission = direct_submission(
            &f,
            &format!("unsupported-{index}"),
            std::slice::from_ref(&resource),
        );
        retarget(
            &mut submission,
            NormalizedTarget::Bare("room@muc.example.com".parse().expect("room")),
            xmpp_parsers::message::MessageType::Groupchat,
        );
        let key = commit_submission(&f.uow, &submission, 5)
            .await
            .expect("unsupported commit")
            .message_key
            .expect("key");
        backdate_created(&f, key, 200 - index).await;
        keys.push(key);
    }
    let submission = direct_submission(&f, "recoverable-tail", std::slice::from_ref(&resource));
    let tail = commit_submission(&f.uow, &submission, 5)
        .await
        .expect("tail commit")
        .message_key
        .expect("key");
    backdate_created(&f, tail, 90).await;
    let cursor = MaintenanceCursor::default();
    assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Partial);
    assert_eq!(
        keys.iter()
            .map(|key| super::attempt_count(*key))
            .sum::<u64>(),
        64
    );
    assert_eq!(super::attempt_count(tail), 0);
    assert_eq!(append_count(&sm, &resource).await, 0);
    assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Partial);
    assert_eq!(
        keys.iter()
            .map(|key| super::attempt_count(*key))
            .sum::<u64>(),
        70
    );
    assert_recovered(&f, tail, 1).await;
    assert_eq!(append_count(&sm, &resource).await, 1);
    assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
    assert!(keys.iter().all(|key| super::attempt_count(*key) == 1));
    assert_eq!(super::attempt_count(tail), 1);
    assert_eq!(append_count(&sm, &resource).await, 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NULL").await,
        70
    );
    f.close().await;
}
/// Rows whose pending kinds are all unsupported are paged past without an
/// attempt, so a backlog larger than one attempt budget cannot starve a
/// recoverable row created after it: one pass reaches the tail. [R3 P2]
async fn unsupported_kind_backlog_does_not_starve_recoverable_rows(f: IngressFixture) {
    let sm = persistent_sm(&f).await;
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    store_detached(&sm, &resource).await;
    let state = state_for(&f, sm.clone()).await;
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    let mut keys = Vec::new();
    for index in 0..70 {
        let mut submission = f.submission(Some(&format!("carbons-{index}")), "carbon backlog");
        submission.plan.intents.push(IngressEffectIntent::Carbons {
            carbon_recipients: vec!["romeo@example.com/laptop".parse().expect("carbon")],
            excluded_source: submission.sender.clone(),
            kind: waddle_xmpp::protocol::CarbonKind::Sent,
        });
        let key = commit_submission(&f.uow, &submission, 5)
            .await
            .expect("carbons commit")
            .message_key
            .expect("key");
        backdate_created(&f, key, 200 - index).await;
        keys.push(key);
    }
    let submission = direct_submission(
        &f,
        "recoverable-behind-backlog",
        std::slice::from_ref(&resource),
    );
    let tail = commit_submission(&f.uow, &submission, 5)
        .await
        .expect("tail commit")
        .message_key
        .expect("key");
    backdate_created(&f, tail, 90).await;
    let cursor = MaintenanceCursor::default();
    assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
    assert!(keys.iter().all(|key| super::attempt_count(*key) == 0));
    assert_eq!(super::attempt_count(tail), 1);
    assert_recovered(&f, tail, 1).await;
    assert_eq!(append_count(&sm, &resource).await, 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NULL").await,
        70
    );
    f.close().await;
}
async fn row_deadline_bounds_a_stalled_route_and_later_rows_still_run(f: IngressFixture) {
    let sm = persistent_sm(&f).await;
    let first: jid::FullJid = "juliet@example.com/phone".parse().expect("first");
    let second: jid::FullJid = "juliet@example.com/laptop".parse().expect("second");
    for resource in [&first, &second] {
        store_detached(&sm, resource).await;
    }
    let state = state_for(&f, sm.clone()).await;
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    let a = commit_submission(
        &f.uow,
        &direct_submission(&f, "deadline-a", std::slice::from_ref(&first)),
        5,
    )
    .await
    .expect("A commit")
    .message_key
    .expect("A");
    let b = commit_submission(
        &f.uow,
        &direct_submission(&f, "deadline-b", std::slice::from_ref(&second)),
        5,
    )
    .await
    .expect("B commit")
    .message_key
    .expect("B");
    backdate_created(&f, a, 120).await;
    backdate_created(&f, b, 90).await;
    let cursor = MaintenanceCursor::default();
    let budget = MaintenanceBudget {
        recovery_row: Duration::from_millis(200),
        ..immediate_recovery_budget()
    };
    let entered = Arc::new(AtomicBool::new(false));
    let outcome = STALL_DELIVERY_RESOURCE
        .scope(
            (first.clone(), entered.clone()),
            run_maintenance_pass_with_cursor(&f.db, &f.uow, budget, &cursor, Some(env.clone())),
        )
        .await;
    assert!(entered.load(Ordering::SeqCst));
    assert_eq!(outcome, MaintenanceOutcome::Partial);
    assert_pending(&f, a).await;
    assert_recovered(&f, b, 1).await;
    assert_eq!(append_count(&sm, &first).await, 0);
    assert_eq!(append_count(&sm, &second).await, 1);
    for _ in 0..2 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
        assert_recovered(&f, a, 1).await;
        assert_eq!(append_count(&sm, &first).await, 1);
        assert_eq!(append_count(&sm, &second).await, 1);
    }
    f.close().await;
}
async fn forced_stop_during_bound_recovery_is_prompt(f: IngressFixture) {
    use crate::ingress::gc::{run_retention_gc_coordinator, RetentionGcCoordinator};
    use tokio_util::sync::CancellationToken;
    let sm = persistent_sm(&f).await;
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    store_detached(&sm, &resource).await;
    let env: Arc<dyn RecoveryEnvironment> =
        Arc::new(StateEnvironment(state_for(&f, sm.clone()).await));
    let key = commit_submission(
        &f.uow,
        &direct_submission(&f, "forced-stop", std::slice::from_ref(&resource)),
        5,
    )
    .await
    .expect("commit")
    .message_key
    .expect("key");
    let database = f.db.clone();
    let uow = f.uow.clone();
    let bound = env.clone();
    let cursor = MaintenanceCursor::default();
    let entered = Arc::new(AtomicBool::new(false));
    let entered_run = entered.clone();
    let gate = test_hooks::pause_after_recovery_freeze(key);
    let coordinator = RetentionGcCoordinator {
        trigger: Arc::new(tokio::sync::Notify::new()),
        run: Arc::new(move || {
            let database = database.clone();
            let uow = uow.clone();
            let bound = bound.clone();
            let cursor = cursor.clone();
            let resource = resource.clone();
            let entered = entered_run.clone();
            Box::pin(async move {
                STALL_DELIVERY_RESOURCE
                    .scope(
                        (resource, entered),
                        run_maintenance_pass_with_cursor(
                            &database,
                            &uow,
                            immediate_recovery_budget(),
                            &cursor,
                            Some(bound),
                        ),
                    )
                    .await
            })
        }),
        partial_retry_delay: Duration::from_secs(1),
        periodic_interval: Duration::from_secs(30),
    };
    let force_stop = CancellationToken::new();
    let task = tokio::spawn(run_retention_gc_coordinator(
        coordinator,
        CancellationToken::new(),
        force_stop.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), gate.wait_until_reached())
        .await
        .expect("bound recovery reached");
    gate.release();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("delivery stalled");
    force_stop.cancel();
    tokio::time::timeout(Duration::from_millis(100), task)
        .await
        .expect("prompt force stop")
        .expect("coordinator exits");
    assert_pending(&f, key).await;
    assert_eq!(f.count("sm_ingress_appends").await, 0);
    for _ in 0..2 {
        assert_eq!(
            pass(&f, &env, &MaintenanceCursor::default()).await,
            MaintenanceOutcome::Complete
        );
        assert_recovered(&f, key, 1).await;
        assert_eq!(f.count("sm_ingress_appends").await, 1);
    }
    f.close().await;
}
async fn live_duplicate_and_recovery_serialize_detached_appends(f: IngressFixture) {
    let sm = persistent_sm(&f).await;
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    store_detached(&sm, &resource).await;
    let env: Arc<dyn RecoveryEnvironment> =
        Arc::new(StateEnvironment(state_for(&f, sm.clone()).await));
    let submission = direct_submission(&f, "detached-race", std::slice::from_ref(&resource));
    let original = commit_submission(&f.uow, &submission, 5)
        .await
        .expect("Phase B");
    let key = original.message_key.expect("key");
    let duplicate = commit_submission(&f.uow, &submission, 5)
        .await
        .expect("duplicate Phase B");
    assert_eq!(duplicate.message_key, Some(key));
    let mut blocker = f.uow.begin().await.expect("blocker");
    assert!(CanonicalMessageRepository::lock(&mut blocker, key)
        .await
        .expect("canonical lock"));
    let cursor = MaintenanceCursor::default();
    let deps = env.recovery_deps();
    let release = async {
        tokio::time::sleep(Duration::from_millis(25)).await;
        blocker.commit().await.expect("release blocker");
    };
    let (outcome, report, ()) = tokio::join!(
        pass(&f, &env, &cursor),
        execute_effects(
            &f.uow,
            &f.db,
            &duplicate,
            &ImmediateSink,
            &deps,
            Duration::from_secs(10)
        ),
        release
    );
    assert_eq!(outcome, MaintenanceOutcome::Complete);
    assert!(report.receipt_failures.is_empty());
    drop(report);
    for _ in 0..2 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
        assert_recovered(&f, key, 1).await;
        assert_eq!(append_count(&sm, &resource).await, 1);
        assert_eq!(f.count("sm_ingress_appends").await, 1);
    }
    f.close().await;
}

async fn groupchat_inbox_push_route_is_left_pending(fixture: IngressFixture) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = state_for(&fixture, persistent_sm(&fixture).await).await;
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("member");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &resource, tx).await;
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("sender");
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &sender, sender_tx).await;
    socket_tests::create_test_session(&state, "juliet").await;
    use waddle_xmpp::muc::{
        room_actor::{ChangeAffiliation, Join},
        room_registry_actor::CreateRoom,
    };
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "receipts".to_owned(),
            channel_id: "receipts".to_owned(),
            config: Default::default(),
        })
        .await
        .expect("create room");
    for (nick, occupant) in [("romeo", &sender), ("juliet", &resource)] {
        actor
            .ask(ChangeAffiliation {
                jid: occupant.to_bare(),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("member");
        actor
            .ask(Join {
                nick: nick.to_owned(),
                real_jid: occupant.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
    }
    let sink = PlanSink::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps =
        build_interpret_deps(&state, None).with_ingress_effect_capture(Some(capture.clone()));
    deps.effects = &sink;
    let mut submission = fixture.submission(None, "groupchat inbox");
    let mut message = submission.plan.sanitized_message.clone();
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.to = Some(room.clone().into());
    submission.target = waddle_xmpp::ingress::NormalizedTarget::Bare(room.clone());
    submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        &message,
        &waddle_xmpp::ingress::DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("groupchat digest");
    submission.plan.sanitized_message = message.clone();
    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::DispatchToRoom {
            room,
            message: Box::new(message),
        }],
        &deps,
    )
    .await;
    let (plan, execution) = sink.take();
    submission.plan.plan = plan;
    submission.plan.room_execution = execution;
    submission.plan.intents = capture.snapshot().intents;
    assert_eq!(
        submission
            .plan
            .intents
            .iter()
            .filter(|intent| matches!(
                intent,
                IngressEffectIntent::RouteDirect {
                    route_identity: waddle_xmpp::ingress::EffectMessageIdentity::CaptureOrdinal(_),
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(rx.try_recv().is_err(), "planning sends nothing");
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit projection");
    let push_index = decision
        .external
        .iter()
        .position(|effect| {
            matches!(
                effect,
                ExternalEffect::Direct(crate::server::routes::interpret::effects::direct::ExternalDirectEffect::PushInboxUpdate {
                    receipt: Some(_),
                    ..
                })
            )
        })
        .expect("planned push");
    assert_eq!(decision.external_receipts[push_index].len(), 1);
    let key = decision.message_key.expect("key");
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    let cursor = MaintenanceCursor::default();
    let before = metrics
        .counter_sum(
            "ingress.maintenance.unrecoverable_obligations",
            &[("kind", "route_direct")],
        )
        .unwrap_or(0);
    for _ in 0..2 {
        assert_eq!(
            pass(&fixture, &env, &cursor).await,
            MaintenanceOutcome::Complete
        );
        assert_pending(&fixture, key).await;
        assert!(
            rx.try_recv().is_err(),
            "never replay canonical groupchat as inbox push"
        );
        assert!(sender_rx.try_recv().is_err());
    }
    assert!(
        metrics
            .counter_sum(
                "ingress.maintenance.unrecoverable_obligations",
                &[("kind", "route_direct")]
            )
            .unwrap_or(0)
            > before
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_unrecoverable_only_rows_do_not_enter_the_recovery_scan() {
    unrecoverable_only_rows_do_not_enter_the_recovery_scan(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_unrecoverable_only_rows_do_not_enter_the_recovery_scan() {
    if let Some(fixture) = IngressFixture::postgres("unrecoverable_only_rows_do_not_ente").await {
        unrecoverable_only_rows_do_not_enter_the_recovery_scan(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_unsupported_kind_backlog_does_not_starve_recoverable_rows() {
    unsupported_kind_backlog_does_not_starve_recoverable_rows(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_unsupported_kind_backlog_does_not_starve_recoverable_rows() {
    if let Some(f) = IngressFixture::postgres("recovery_kind_backlog").await {
        unsupported_kind_backlog_does_not_starve_recoverable_rows(f).await;
    }
}
#[tokio::test]
async fn sqlite_unsupported_backlog_is_evaluated_once_then_skipped() {
    unsupported_backlog_is_evaluated_once_then_skipped(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_unsupported_backlog_is_evaluated_once_then_skipped() {
    if let Some(fixture) = IngressFixture::postgres("unsupported_backlog_is_evaluated_on").await {
        unsupported_backlog_is_evaluated_once_then_skipped(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_row_deadline_bounds_a_stalled_route_and_later_rows_still_run() {
    row_deadline_bounds_a_stalled_route_and_later_rows_still_run(IngressFixture::sqlite().await)
        .await;
}
#[tokio::test]
async fn postgres_row_deadline_bounds_a_stalled_route_and_later_rows_still_run() {
    if let Some(fixture) = IngressFixture::postgres("row_deadline_bounds_a_stalled_route").await {
        row_deadline_bounds_a_stalled_route_and_later_rows_still_run(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_forced_stop_during_bound_recovery_is_prompt() {
    forced_stop_during_bound_recovery_is_prompt(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_forced_stop_during_bound_recovery_is_prompt() {
    if let Some(fixture) = IngressFixture::postgres("forced_stop_during_bound_recovery_i").await {
        forced_stop_during_bound_recovery_is_prompt(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_live_duplicate_and_recovery_serialize_detached_appends() {
    live_duplicate_and_recovery_serialize_detached_appends(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_live_duplicate_and_recovery_serialize_detached_appends() {
    if let Some(fixture) = IngressFixture::postgres("live_duplicate_and_recovery_seriali").await {
        live_duplicate_and_recovery_serialize_detached_appends(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_groupchat_inbox_push_route_is_left_pending() {
    groupchat_inbox_push_route_is_left_pending(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_groupchat_inbox_push_route_is_left_pending() {
    if let Some(fixture) = IngressFixture::postgres("groupchat_inbox_push_route_is_left_").await {
        groupchat_inbox_push_route_is_left_pending(fixture).await;
    }
}

#[path = "xep0045_decline_recovery_tests.rs"]
mod xep0045_decline_recovery;
