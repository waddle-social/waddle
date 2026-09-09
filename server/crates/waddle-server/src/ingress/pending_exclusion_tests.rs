//! Specialized invitation fallbacks remain outside ordinary offline restoration.
use super::*;

async fn specialized_replay(fixture: IngressFixture, live: bool, decline: bool) {
    let storage = std::sync::Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
            fixture.db.clone(),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("dialect pending storage"),
    );
    let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_pending_storage(std::sync::Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()), storage).await;
    let registry = ConnectionRegistry::new();
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let resource = recipient.with_resource_str("phone").expect("resource");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    if live {
        registry.register(resource.clone(), sender);
    }
    let mut submission = fixture.submission(Some("specialized-replay"), "invitation response");
    let ns = waddle_xmpp::muc::presence::NS_MUC_USER;
    submission.plan.sanitized_message.payloads.push(
        minidom::Element::builder("x", ns)
            .append(
                minidom::Element::builder(if decline { "decline" } else { "invite" }, ns)
                    .attr(
                        minidom::rxml::xml_ncname!("from").to_owned(),
                        submission.sender.to_bare().to_string(),
                    )
                    .build(),
            )
            .build(),
    );
    submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &waddle_xmpp::ingress::DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("invitation digest");
    let message = Box::new(submission.plan.sanitized_message.clone());
    let identity = EffectMessageIdentity::capture_ordinal(0);
    let fallback = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Transient(message.clone()),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    submission
        .plan
        .intents
        .push(IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient: recipient.clone(),
                row_id: fallback.id.clone(),
            },
        });
    // Match deliver_muc_user_message: capture an identity even for an empty
    // offline fanout, and suppress sender-only duplicates after settlement.
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RouteDirect {
            recipient: recipient.clone(),
            fanout: if live { vec![resource.clone()] } else { vec![] },
            route_identity: identity.clone(),
        });
    let route = MucUserRoute {
        route_identity: Some(identity),
        recipient: recipient.clone(),
        resources: if live { vec![resource] } else { vec![] },
        message,
        fallback,
        failure: None,
    };
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(if live {
            ExternalEffect::RouteToPeer(route)
        } else {
            ExternalEffect::QueueOfflineDelivery(route)
        }))
        .with_suppression(crate::ingress::PlanSuppressionPolicy::SenderOnly),
    );
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("specialized commit");
    // This bounded fixture intentionally omits ledger/membership obligations.
    // Their restorers authorize specialized recovery; here an unexecuted replay
    // may be suppressed, but must never be rebuilt as ordinary offline work.
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("unexecuted specialized replay");
    assert_eq!(first.external.len(), 1);
    assert!(
        retry.arm_owned_receipts.is_empty(),
        "specialized fallback is never ordinary arm-owned"
    );
    assert!(first.arm_owned_receipts.is_empty());
    assert!(
        retry.external.iter().all(|effect| matches!(
            effect,
            ExternalEffect::RouteToPeer(_) | ExternalEffect::QueueOfflineDelivery(_)
        )),
        "an unexecuted specialized fallback cannot become an ordinary queue effect"
    );
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(report.receipt_failures.is_empty());
    assert_eq!(receiver.try_recv().is_ok(), live);
    assert_eq!(
        state
            .deps
            .protocol
            .pending_delivery_storage
            .count(&recipient)
            .await
            .expect("specialized rows"),
        u32::from(!live)
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    let settled = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("completed specialized replay");
    assert!(settled.arm_owned_receipts.is_empty());
    assert!(
        settled.external.iter().all(|effect| !matches!(
            effect,
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery { .. })
        )),
        "live receipt without row never becomes ordinary pending work"
    );
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &settled,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    assert!(
        receiver.try_recv().is_err(),
        "completed specialized replay has no live duplicate"
    );
    assert_eq!(
        state
            .deps
            .protocol
            .pending_delivery_storage
            .count(&recipient)
            .await
            .expect("replayed specialized rows"),
        u32::from(!live)
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close().await;
}

macro_rules! dialect_tests {
    ($sqlite:ident, $postgres:ident, $live:expr, $decline:expr) => {
        #[tokio::test]
        async fn $sqlite() {
            specialized_replay(IngressFixture::sqlite().await, $live, $decline).await;
        }
        #[tokio::test]
        async fn $postgres() {
            if let Some(fixture) = IngressFixture::postgres(stringify!($postgres)).await {
                specialized_replay(fixture, $live, $decline).await;
            }
        }
    };
}
dialect_tests!(
    sqlite_invite_live_exclusion,
    postgres_invite_live_exclusion,
    true,
    false
);
dialect_tests!(
    sqlite_invite_offline_exclusion,
    postgres_invite_offline_exclusion,
    false,
    false
);
dialect_tests!(
    sqlite_decline_live_exclusion,
    postgres_decline_live_exclusion,
    true,
    true
);
dialect_tests!(
    sqlite_decline_offline_exclusion,
    postgres_decline_offline_exclusion,
    false,
    true
);
