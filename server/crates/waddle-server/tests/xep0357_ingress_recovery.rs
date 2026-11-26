//! XEP-0357: recovery preserves the durable offline notification obligation.
#[path = "ingress_support/extension_recovery.rs"]
mod extension_recovery_support;
pub mod ingress_support;

use ingress_support::IngressFixture;
use std::sync::Arc;
use std::time::Duration;
use waddle_server::ingress::{Deps, IngressDecisionClass, IngressSubmission, RecoveryEnvironment};
use waddle_server::notification_outbox::NotificationOutboxStore;
use waddle_server::pending_delivery::DatabasePendingDeliveryStorage;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::QuotaPolicy;
use waddle_xmpp::registry::ConnectionRegistry;

struct OfflineRecoveryEnvironment {
    connections: ConnectionRegistry,
    pending: Arc<dyn PendingDeliveryStorage>,
}

impl RecoveryEnvironment for OfflineRecoveryEnvironment {
    fn recovery_deps(&self) -> Deps<'_> {
        let mut deps = Deps::new(&self.connections, "example.com");
        deps.pending_delivery_storage = Some(&self.pending);
        deps
    }
}

fn offline_plan(fixture: &IngressFixture, origin: &str) -> IngressSubmission {
    extension_recovery_support::offline_submission(
        fixture.submission(Some(origin), "canonical offline body"),
        origin,
    )
}

async fn backdate_pending(fixture: &IngressFixture) -> chrono::DateTime<chrono::Utc> {
    let received_at = chrono::DateTime::from_timestamp_millis(1_700_000_000_123)
        .expect("canonical receipt timestamp");
    let sql = match fixture.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres =>
            "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE terminal_at IS NULL",
        waddle_server::db::DatabaseDriver::Sqlite =>
            "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE terminal_at IS NULL",
    };
    fixture
        .execute(sql, waddle_server::db_params![received_at.to_rfc3339()])
        .await;
    received_at
}

async fn wait_for_terminal_count(fixture: &IngressFixture, expected: i64) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await
                == expected
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("maintenance recovers the recorded notification obligations");
}

async fn lost_offline_notification_candidate_recovers_once(
    fixture: IngressFixture,
    extension: bool,
) {
    let store = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("notification schema");
    let environment: Arc<dyn RecoveryEnvironment> = Arc::new(OfflineRecoveryEnvironment {
        connections: ConnectionRegistry::new(),
        pending: Arc::new(
            DatabasePendingDeliveryStorage::open(
                Some(fixture.db.database_url()),
                QuotaPolicy::Unlimited,
            )
            .await
            .expect("pending storage"),
        ),
    });
    let authority = fixture.authority().await;
    authority.bind_recovery_environment(Arc::downgrade(&environment));
    let submission = if extension {
        extension_recovery_support::offline_submission(
            extension_recovery_support::extension_submission(
                &fixture,
                "push-recovery-original",
                "canonical offline body",
            )
            .await,
            "push-recovery-original",
        )
    } else {
        offline_plan(&fixture, "push-recovery-original")
    };
    let decision = authority.commit(&submission).await;
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_eq!(fixture.count("ingress_effect_intents").await, 3);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("pending_delivery").await, 0);
    assert_eq!(
        store.count_all_candidates().await.expect("candidate count"),
        0
    );
    // Deliberately discard Phase C: only the public maintenance executor can enqueue this notification.
    drop(decision);
    if extension {
        extension_recovery_support::revoke_after_commit(&fixture, &submission).await;
        assert_eq!(
            authority.commit(&submission).await.class,
            IngressDecisionClass::PrincipalMissing
        );
    }
    // No push policy store exists in the recovery environment: only the frozen
    // Inserted obligation can authorize materializing this candidate.
    let canonical_receipt_at = backdate_pending(&fixture).await;
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 1).await;
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(
        store.count_all_candidates().await.expect("candidate count"),
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    let socket_identity_filter = "notification_candidates WHERE recipient_bare_jid = 'juliet@example.com' AND sender_jid = 'romeo@example.com/phone' AND conversation_jid = 'romeo@example.com' AND stanza_id_by = 'juliet@example.com' AND stanza_id = 'push-recovery-original' AND class = 'dm' AND reason = 'offline_dm'";
    let extension_identity_filter = "notification_candidates WHERE recipient_bare_jid = 'juliet@example.com' AND sender_jid = 'romeo@example.com/extension-host' AND conversation_jid = 'romeo@example.com' AND stanza_id_by = 'juliet@example.com' AND stanza_id = 'push-recovery-original' AND class = 'dm' AND reason = 'offline_dm'";
    let identity_filter = if extension {
        extension_identity_filter
    } else {
        socket_identity_filter
    };
    assert_eq!(
        fixture.count(identity_filter).await,
        1,
        "the recorded recipient and stanza identity reach the XEP-0357 outbox"
    );
    let created_at = fixture.optional_text("SELECT CAST(created_at_ms AS TEXT) FROM notification_candidates WHERE stanza_id = 'push-recovery-original'").await;

    assert_eq!(
        created_at,
        Some(canonical_receipt_at.timestamp_millis().to_string()),
        "recovered candidates retain canonical receipt ordering"
    );

    // A separate lost obligation proves the next pass completed, rather than relying on a sleep.
    let sentinel = offline_plan(&fixture, "push-recovery-second-pass");
    assert_eq!(
        authority.commit(&sentinel).await.class,
        IngressDecisionClass::Accepted
    );
    backdate_pending(&fixture).await;
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 2).await;
    assert_eq!(
        store.count_all_candidates().await.expect("candidate count"),
        2
    );
    assert_eq!(
        fixture.count(identity_filter).await,
        1,
        "a second pass creates no duplicate original candidate"
    );
    assert_eq!(fixture.optional_text("SELECT CAST(created_at_ms AS TEXT) FROM notification_candidates WHERE stanza_id = 'push-recovery-original'").await, created_at, "a second pass does not replace the original candidate");
    assert_eq!(fixture.count("pending_delivery").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 6);
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(authority);
    drop(environment);
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_lost_offline_notification_candidate_recovers_once() {
    lost_offline_notification_candidate_recovers_once(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn postgres_lost_offline_notification_candidate_recovers_once() {
    if let Some(fixture) = IngressFixture::postgres("push_recovery").await {
        lost_offline_notification_candidate_recovers_once(fixture, false).await;
    }
}

#[tokio::test]
async fn sqlite_extension_frozen_notification_recovers_after_revocation() {
    lost_offline_notification_candidate_recovers_once(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn postgres_extension_frozen_notification_recovers_after_revocation() {
    if let Some(fixture) = IngressFixture::postgres("extension_push_recovery").await {
        lost_offline_notification_candidate_recovers_once(fixture, true).await;
    }
}

#[tokio::test]
async fn sqlite_retraction_discharges_a_frozen_offline_candidate_intent() {
    use waddle_xmpp::ingress::{DigestContext, DigestInput};
    use waddle_xmpp::xep::xep0424::build_retract_element;

    let fixture = IngressFixture::sqlite().await;
    let store = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("notification schema");
    let environment: Arc<dyn RecoveryEnvironment> = Arc::new(OfflineRecoveryEnvironment {
        connections: ConnectionRegistry::new(),
        pending: Arc::new(
            DatabasePendingDeliveryStorage::open(
                Some(fixture.db.database_url()),
                QuotaPolicy::Unlimited,
            )
            .await
            .expect("pending storage"),
        ),
    });
    let authority = fixture.authority().await;
    authority.bind_recovery_environment(Arc::downgrade(&environment));
    let mut submission = offline_plan(&fixture, "retraction-with-frozen-candidate");
    submission
        .plan
        .sanitized_message
        .payloads
        .push(build_retract_element("earlier-message"));
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("retraction digest");
    assert_eq!(
        authority.commit(&submission).await.class,
        IngressDecisionClass::Accepted
    );
    assert_eq!(fixture.count("ingress_effect_intents").await, 3);

    backdate_pending(&fixture).await;
    authority.trigger_maintenance();
    wait_for_terminal_count(&fixture, 1).await;
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    assert_eq!(
        store.count_all_candidates().await.expect("candidate count"),
        0
    );
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(authority);
    drop(environment);
    drop(store);
    fixture.close().await;
}

#[cfg(feature = "test-support")]
mod retraction_recovery {
    use super::{IngressDecisionClass, IngressFixture, RecoveryEnvironment};
    use std::sync::Arc;
    use std::time::Duration;
    use waddle_server::db::{DatabaseConfig, DatabasePool, PoolConfig};
    use waddle_server::ingress::commit::commit_submission;
    use waddle_server::ingress::effects::delivery::{
        ExternalDeliveryEffect, PreparedOfflineNotification,
    };
    use waddle_server::ingress::effects::direct::DurableDirectEffect;
    use waddle_server::ingress::effects::Effect;
    use waddle_server::ingress::execute::execute_effects;
    use waddle_server::ingress::{DurableEffect, ExternalEffect, ImmediateSink, PlannedEffect};
    use waddle_server::test_support::{
        recover_pending_notifications_for_test, seed_local_account,
        websocket_state_with_ingress_factory,
    };
    use waddle_xmpp::ingress::{
        DigestContext, DigestInput, IngressEffectIntent, PendingDeliveryMutation,
    };
    use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};
    use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};
    use waddle_xmpp::xep::xep0424::build_retract_element;
    use waddle_xmpp_core::xep0359::StanzaId;

    async fn settle_retraction_without_candidate(fixture: IngressFixture) {
        let pool = Arc::new(
            DatabasePool::try_new(
                DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
                PoolConfig,
            )
            .await
            .expect("shared database pool"),
        );
        let (state, authority) =
            websocket_state_with_ingress_factory(pool, || fixture.authority()).await;
        seed_local_account(&state, "juliet").await;
        let origin = "pending-retraction";
        let fallback = "This message was retracted";
        let mut submission = fixture.submission(Some(origin), fallback);
        let message = &mut submission.plan.sanitized_message;
        message
            .payloads
            .push(build_retract_element("earlier-message"));
        submission.digest_input = DigestInput::from_parsed(
            message,
            &DigestContext {
                target: submission.target.clone(),
                server_authorities: vec![fixture.principal.bare_jid().clone()],
                stanza_lang: None,
            },
        )
        .expect("retraction digest");
        let recipient = message.to.as_ref().expect("recipient").to_bare();
        let stamp = StanzaId::new(origin, recipient.clone().into());
        let mut archived =
            ArchivedMessage::for_test(submission.sender.clone().into(), recipient.clone().into());
        archived.id = origin.to_owned();
        archived.body = Some(fallback.to_owned());
        archived.message_type = message.type_.clone();
        archived.stanza_id = Some(stamp.clone());
        archived.origin_id = submission.digest_input.origin().cloned();
        archived.stanza_xml = Some(String::from(&minidom::Element::from(message.clone())));
        let row = PendingRow {
            id: PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: archived.timestamp,
            payload: PendingPayload::Archived(stamp.clone()),
            flushed_in_session: None,
            outbound_sequence: None,
        };
        submission.plan.intents.extend([
            IngressEffectIntent::ArchiveAuthoritative {
                archive: recipient.clone(),
                stanza_id: stamp.clone(),
                by: recipient.clone(),
                archived_at: archived.timestamp,
                ordinal: None,
            },
            IngressEffectIntent::PendingDelivery {
                mutation: PendingDeliveryMutation::Archived {
                    recipient: recipient.clone(),
                    row_id: row.id.clone(),
                    archive_stanza_id: stamp,
                },
            },
        ]);
        submission.plan.plan.extend([
            PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
                DurableDirectEffect::ArchiveDirect {
                    archive: recipient,
                    message: Box::new(archived),
                    archive_expectation: ArchiveExpectation::Fresh,
                },
            ))),
            PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
                ExternalDeliveryEffect::QueueOfflineDelivery {
                    row,
                    prepared_notification: PreparedOfflineNotification::RetryLater,
                    original_message: Box::new(message.clone()),
                },
            ))),
        ]);
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("commit canonical retraction");
        assert_eq!(decision.class, IngressDecisionClass::Accepted);
        let report = execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &state.recovery_deps(),
            Duration::from_secs(5),
        )
        .await;
        assert!(report.receipt_failures.is_empty(), "{report:?}");
        assert_eq!(
            fixture
                .count("pending_delivery WHERE notification_outboxed_at_ms IS NULL")
                .await,
            1,
            "the notification sweep must replay the canonical envelope"
        );
        assert_eq!(
            recover_pending_notifications_for_test(&state).await,
            (1, false)
        );
        assert_eq!(
            fixture
                .count("pending_delivery WHERE notification_outboxed_at_ms IS NOT NULL")
                .await,
            1
        );
        assert_eq!(fixture.count("notification_candidates").await, 0);
        assert!(authority.drain_and_join(Duration::from_secs(15)).await);
        drop(state);
        drop(authority);
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_retraction_receipt_recovery_completes_without_candidate() {
        settle_retraction_without_candidate(IngressFixture::sqlite().await).await;
    }

    #[tokio::test]
    async fn postgres_retraction_receipt_recovery_completes_without_candidate() {
        if let Some(fixture) = IngressFixture::postgres("retraction_recovery").await {
            settle_retraction_without_candidate(fixture).await;
        }
    }
}

#[cfg(feature = "test-support")]
mod late_pending_parent {
    use super::IngressFixture;
    use waddle_server::ingress::commit::commit_submission;
    use waddle_server::ingress::{IngressDecisionClass, IngressSubmission};
    use waddle_server::notification_outbox::{NotificationCandidate, NotificationOutboxStore};
    use waddle_server::test_support::attach_existing_pending_notification_for_test;
    use waddle_xmpp::ingress::{
        DigestContext, DigestInput, EffectMessageIdentity, IngressEffectIntent, MessageKey,
    };
    use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};
    use waddle_xmpp::xep::xep0424::build_retract_element;
    use waddle_xmpp::xep::xep0428::{set_fallback_payloads, FallbackIndication};
    use waddle_xmpp::xep::xep0461::{set_reply_payload, ReplyReference, NS_REPLY};
    use waddle_xmpp_core::xep0359::StanzaId;

    async fn late_parent(
        fixture: &IngressFixture,
        mut submission: IngressSubmission,
        stamp: &StanzaId,
    ) -> (MessageKey, IngressEffectIntent, PendingRow) {
        let recipient = stamp.by.to_bare();
        let intent = IngressEffectIntent::RouteDirect {
            prepared: None,
            recipient: recipient.clone(),
            fanout: vec![recipient.with_resource_str("phone").expect("resource")],
            route_identity: EffectMessageIdentity::capture_ordinal(1),
        };
        submission.plan.intents.push(intent.clone());
        submission.digest_input = DigestInput::from_parsed(
            &submission.plan.sanitized_message,
            &DigestContext {
                target: submission.target.clone(),
                server_authorities: vec![fixture.principal.bare_jid().clone()],
                stanza_lang: None,
            },
        )
        .expect("message digest");
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("commit late parent");
        assert_eq!(decision.class, IngressDecisionClass::Accepted);
        let row = PendingRow {
            id: PendingRowId::fresh(),
            recipient,
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Archived(stamp.clone()),
            flushed_in_session: None,
            outbound_sequence: None,
        };
        (decision.message_key.expect("canonical parent"), intent, row)
    }

    async fn legacy_reply_retains_lineage(fixture: IngressFixture) {
        let store = NotificationOutboxStore::try_new(fixture.db.clone())
            .await
            .expect("notification schema");
        let raw = "> Original quote\nHere's my reply";
        let mut submission = fixture.submission(None, raw);
        let stamp = StanzaId::new(
            "legacy-reply",
            submission
                .plan
                .sanitized_message
                .to
                .clone()
                .expect("recipient"),
        );
        let message = &mut submission.plan.sanitized_message;
        set_reply_payload(message, &ReplyReference::new("original"));
        set_fallback_payloads(
            message,
            &[FallbackIndication::for_range(
                NS_REPLY,
                0,
                "> Original quote\n".chars().count(),
            )],
        );
        let sender = submission.sender.clone();
        let (key, intent, row) = late_parent(&fixture, submission, &stamp).await;
        assert!(
            !attach_existing_pending_notification_for_test(&fixture.uow, key, &intent, &row)
                .await
                .expect("a reply cannot reuse missing notification custody")
        );

        let candidate = NotificationCandidate::direct_message(
            row.recipient.clone(),
            sender.into(),
            stamp,
            false,
        )
        .expect("legacy candidate")
        .with_last_message_body(Some("Different archived preview".into()));
        store.insert_candidate(&candidate).await.expect("candidate");
        assert!(
            !attach_existing_pending_notification_for_test(&fixture.uow, key, &intent, &row)
                .await
                .expect("a different stored body is not the legacy preview")
        );
        assert_eq!(fixture.count("ingress_effect_descendants").await, 0);

        fixture
            .execute(
                "UPDATE notification_candidates SET last_message_body = ?",
                waddle_server::db_params![raw],
            )
            .await;
        assert!(
            attach_existing_pending_notification_for_test(&fixture.uow, key, &intent, &row)
                .await
                .expect("reusing the legacy raw preview must retain the existing candidate")
        );
        assert_eq!(fixture.count("notification_candidates").await, 1);
        assert_eq!(fixture.count("ingress_effect_descendants").await, 1);
        assert_eq!(
            fixture
                .optional_text("SELECT last_message_body FROM notification_candidates")
                .await,
            Some(raw.to_owned()),
            "the existing candidate is left intact"
        );
        drop(store);
        fixture.close().await;
    }

    async fn retraction_requires_no_lineage(fixture: IngressFixture) {
        let store = NotificationOutboxStore::try_new(fixture.db.clone())
            .await
            .expect("notification schema");
        let mut submission = fixture.submission(None, "This message was retracted");
        let stamp = StanzaId::new(
            "pending-retraction",
            submission
                .plan
                .sanitized_message
                .to
                .clone()
                .expect("recipient"),
        );
        submission
            .plan
            .sanitized_message
            .payloads
            .push(build_retract_element("original"));
        let (key, intent, row) = late_parent(&fixture, submission, &stamp).await;
        assert!(
            attach_existing_pending_notification_for_test(&fixture.uow, key, &intent, &row)
                .await
                .expect("the canonical retraction proves notification suppression")
        );
        assert_eq!(fixture.count("notification_candidates").await, 0);
        assert_eq!(fixture.count("ingress_effect_descendants").await, 0);
        drop(store);
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_late_pending_reply_retains_legacy_preview_lineage() {
        legacy_reply_retains_lineage(IngressFixture::sqlite().await).await;
    }

    #[tokio::test]
    async fn postgres_late_pending_reply_retains_legacy_preview_lineage() {
        if let Some(fixture) = IngressFixture::postgres("late_pending_reply").await {
            legacy_reply_retains_lineage(fixture).await;
        }
    }

    #[tokio::test]
    async fn sqlite_late_pending_retraction_needs_no_candidate_lineage() {
        retraction_requires_no_lineage(IngressFixture::sqlite().await).await;
    }

    #[tokio::test]
    async fn postgres_late_pending_retraction_needs_no_candidate_lineage() {
        if let Some(fixture) = IngressFixture::postgres("late_pending_retraction").await {
            retraction_requires_no_lineage(fixture).await;
        }
    }
}
