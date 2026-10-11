use super::*;
use crate::server::routes::interpret::DeliveryExecutionContext;
use crate::{
    db::{DatabaseConfig, DatabasePool, PoolConfig},
    ingress::{
        commit::commit_submission,
        execute::{execute_effects, terminalize_if_complete, ExternalOutcome},
        test_support::IngressFixture,
        IngressDecisionClass,
    },
    server::routes::{
        interpret::{
            effects::{ImmediateSink, PlanSuppressionPolicy, PlannedEffect},
            Deps,
        },
        websocket::tests::{
            create_test_websocket_state, create_test_websocket_state_with_db_pool_and_ingress,
        },
    },
};
use std::{sync::Arc, time::Duration};
use waddle_xmpp::{ingress::LinkPreviewMediaRefState, registry::ConnectionRegistry};
use waddle_xmpp_core::xep0359::StanzaId;

async fn preview_reference_policy_drift(fixture: IngressFixture) {
    let pool = Arc::new(
        DatabasePool::new(
            DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
            PoolConfig,
        )
        .await
        .expect("shared preview database"),
    );
    let standalone = create_test_websocket_state().await;
    let state = create_test_websocket_state_with_db_pool_and_ingress(
        pool,
        Arc::clone(&standalone.deps.protocol.ingress),
    )
    .await;
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    let mut submission = fixture.submission(Some("preview-policy-drift"), "preview");
    let mut saved = IngressEffectIntent::storage_round_trip_samples()
        .into_iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::LinkPreviewMediaRef { mutation } => Some(mutation),
            _ => None,
        })
        .expect("preview mutation");
    saved.archive = submission.sender.to_bare();
    saved.current_archive_stanza_id =
        waddle_xmpp_core::xep0359::StanzaId::new("preview-archive", saved.archive.clone().into());
    saved.state = LinkPreviewMediaRefState::Current;
    fixture.execute(
        "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        crate::db_params![saved.upload_slot_id.to_string(), saved.archive.to_string(), "preview.png".to_owned(), 1_i64, "image/png".to_owned(), "2099-01-01T00:00:00Z".to_owned()],
    ).await;
    submission.plan.intents = vec![IngressEffectIntent::LinkPreviewMediaRef {
        mutation: saved.clone(),
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(ExternalEffect::Direct(
        ExternalDirectEffect::LinkPreviewRefs {
            mutations: vec![saved.clone()],
        },
    )))
    .with_suppression(PlanSuppressionPolicy::Always)];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("first commit");
    let key = first.message_key.expect("canonical key");
    let completed = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(completed.outcomes[0].1, ExternalOutcome::Done);
    assert!(completed.receipt_failures.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("confirmed preview terminalizes")
    );
    assert_eq!(
        fixture
            .count("link_preview_media_refs WHERE state = 'current'")
            .await,
        1
    );

    // Expired tokens or current host policy can propose a clear on the exact
    // original request. Its Always effect must not outlive its rejected intent.
    let mut offered = saved.clone();
    offered.state = LinkPreviewMediaRefState::Unreferenced;
    submission.plan.intents = vec![IngressEffectIntent::LinkPreviewMediaRef {
        mutation: offered.clone(),
    }];
    for clearing in [false, true] {
        let effect = if clearing {
            ExternalDirectEffect::ClearLinkPreviewRefs {
                mutations: vec![offered.clone()],
            }
        } else {
            ExternalDirectEffect::LinkPreviewRefs {
                mutations: vec![offered.clone()],
            }
        };
        submission.plan.plan =
            vec![
                PlannedEffect::new(Effect::External(ExternalEffect::Direct(effect)))
                    .with_suppression(PlanSuppressionPolicy::Always),
            ];
        let replay = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("policy drift replay");
        assert_eq!(replay.class, IngressDecisionClass::ExistingDivergent);
        assert_eq!(replay.message_key, Some(key));
        assert!(
            replay.external.is_empty(),
            "fully rejected batch is not executed"
        );
        let report = execute_effects(
            &fixture.uow,
            &fixture.db,
            &replay,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        assert!(report.receipt_failures.is_empty());
        assert_eq!(
            fixture
                .count("link_preview_media_refs WHERE state = 'current'")
                .await,
            1
        );
        assert_eq!(
            fixture
                .count("link_preview_media_refs WHERE state = 'unreferenced'")
                .await,
            0
        );
        assert_eq!(fixture.count("ingress_effect_intents").await, 1);
        assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
        assert!(
            terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
                .await
                .expect("recorded authority remains complete")
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_preview_reference_policy_drift_preserves_current_and_terminalizes() {
    preview_reference_policy_drift(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_preview_reference_policy_drift_preserves_current_and_terminalizes() {
    if let Some(fixture) = IngressFixture::postgres("preview_drift").await {
        preview_reference_policy_drift(fixture).await;
    }
}

#[test]
fn preview_reference_batch_filters_divergence_without_dropping_recorded_sibling() {
    let saved = IngressEffectIntent::storage_round_trip_samples()
        .into_iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::LinkPreviewMediaRef { mutation } => Some(mutation),
            _ => None,
        })
        .expect("preview mutation");
    let mut divergent = saved.clone();
    divergent.upload_slot_id = uuid::Uuid::new_v4();
    let intent = IngressEffectIntent::LinkPreviewMediaRef {
        mutation: saved.clone(),
    };
    let mut plan = IngressPlan {
        failure: None,
        plan: Vec::new(),
        intents: Vec::new(),
        room_canonical_message: None,
        sanitized_message: xmpp_parsers::message::Message::new(None),
        rejection: None,
        error_reply: None,
        room_execution: crate::ingress::RoomExecutionPath::None,
    };
    plan.intents = vec![
        intent.clone(),
        IngressEffectIntent::LinkPreviewMediaRef {
            mutation: divergent.clone(),
        },
    ];
    plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs {
            mutations: vec![saved.clone(), divergent],
        }),
    ))];
    let restored = apply_recorded_intents(&plan, &[intent]);
    let Effect::External(ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs {
        mutations,
    })) = &restored.plan[0].effect
    else {
        panic!("preview batch");
    };
    assert_eq!(mutations, &vec![saved]);
}

#[test]
fn changed_preview_enrichment_restores_all_pending_approved_slots_only() {
    let original = IngressEffectIntent::storage_round_trip_samples()
        .into_iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::LinkPreviewMediaRef { mutation } => Some(mutation),
            _ => None,
        })
        .expect("preview");
    let mut second = original.clone();
    second.upload_slot_id = uuid::Uuid::new_v4();
    let recorded = vec![
        IngressEffectIntent::LinkPreviewMediaRef {
            mutation: original.clone(),
        },
        IngressEffectIntent::LinkPreviewMediaRef {
            mutation: second.clone(),
        },
    ];
    let mut offered = original.clone();
    offered.upload_slot_id = uuid::Uuid::new_v4();
    let mut plan = IngressPlan {
        failure: None,
        rejection: None,
        error_reply: None,
        room_canonical_message: None,
        sanitized_message: xmpp_parsers::message::Message::new(None),
        room_execution: crate::ingress::RoomExecutionPath::None,
        intents: vec![IngressEffectIntent::LinkPreviewMediaRef {
            mutation: offered.clone(),
        }],
        plan: vec![PlannedEffect::new(Effect::External(
            ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs {
                mutations: vec![offered.clone()],
            }),
        ))],
    };
    freeze_preview_intents(&mut plan, &recorded);
    assert!(
        plan.intents.is_empty(),
        "today's enrichment cannot assign another slot"
    );
    let mut plan = apply_recorded_intents(&plan, &recorded);
    assert!(restore_pending_preview_effects(
        &mut plan, &recorded, &recorded
    ));
    let restored: Vec<_> = plan
        .plan
        .iter()
        .flat_map(|planned| match &planned.effect {
            Effect::External(ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs {
                mutations,
            })) => mutations.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(restored, vec![original, second]);
    assert!(!restored.contains(&offered));
    assert!(
        !restore_pending_preview_effects(&mut plan, &recorded, &recorded),
        "repeat restoration is idempotent"
    );
    let mut complete = apply_recorded_intents(&plan, &recorded);
    complete.plan.clear();
    assert!(
        !restore_pending_preview_effects(&mut complete, &recorded, &[]),
        "completed slots stay complete"
    );
}

async fn changed_enrichment_reuses_original_preview_key(fixture: IngressFixture) {
    let mut original = IngressEffectIntent::storage_round_trip_samples()
        .into_iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::LinkPreviewMediaRef { mutation } => Some(mutation),
            _ => None,
        })
        .expect("preview");
    original.archive = fixture.principal.bare_jid().clone();
    original.current_archive_stanza_id =
        StanzaId::new("frozen-preview", original.archive.clone().into());
    original.state = LinkPreviewMediaRefState::Current;
    let mut offered = original.clone();
    offered.upload_slot_id = uuid::Uuid::new_v4();
    for slot in [original.upload_slot_id, offered.upload_slot_id] {
        fixture.execute("INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
            crate::db_params![slot.to_string(), original.archive.to_string(), "preview.png".to_owned(), 1_i64, "image/png".to_owned(), "2099-01-01T00:00:00Z".to_owned()]).await;
    }
    let mut submission = fixture.submission(Some("changed-preview-enrichment"), "same source");
    submission.plan.intents = vec![IngressEffectIntent::LinkPreviewMediaRef {
        mutation: original.clone(),
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs {
            mutations: vec![original.clone()],
        }),
    ))];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("original approval");
    // Process loss occurred before projection. Today's fetch would produce
    // another cached slot, but the original pending approval owns this retry.
    submission.plan.intents = vec![IngressEffectIntent::LinkPreviewMediaRef {
        mutation: offered.clone(),
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs {
            mutations: vec![offered.clone()],
        }),
    ))];
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry");
    assert_eq!(replay.message_key, first.message_key);
    assert_eq!(replay.external.len(), 1);
    assert_eq!(replay.external_receipts, first.external_receipts);
    assert!(
        matches!(&replay.external[0], ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs { mutations }) if mutations == &vec![original.clone()])
    );
    assert_eq!(fixture.count("ingress_effect_intents").await, 1);
    let registry = ConnectionRegistry::new();
    let deps = Deps::registry_only(&registry);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(fixture.count("link_preview_media_refs").await, 1);
    let slot = fixture
        .optional_text("SELECT upload_slot_id FROM link_preview_media_refs")
        .await;
    assert_eq!(slot, Some(original.upload_slot_id.to_string()));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(fixture.count("ingress_deliveries").await, 1);
    let completed_replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("completed retry");
    assert!(
        completed_replay.external.is_empty(),
        "fresh enrichment stays rejected after completion"
    );
    assert_eq!(fixture.count("ingress_effect_intents").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_changed_enrichment_reuses_original_preview_key_and_payload() {
    changed_enrichment_reuses_original_preview_key(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_changed_enrichment_reuses_original_preview_key_and_payload() {
    if let Some(fixture) = IngressFixture::postgres("preview_new_enrichment").await {
        changed_enrichment_reuses_original_preview_key(fixture).await;
    }
}
