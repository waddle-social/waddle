use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_substrate::{AliasGcBudget, AliasGcProgress};
use crate::ingress_uow::{
    settle_recorded, CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
};
use crate::notification_outbox::NotificationOutboxStore;
use crate::push_service::test_support::{notification_item, owner, scalar_i64};
use crate::push_service::{PushDevicePlatform, PushDeviceRegistration};
use waddle_xmpp::ingress::{
    IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    SemanticDigest,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProofScenario {
    Live,
    Parked,
    OwnerFirst,
    AtomicOwner,
    ProviderFirst,
    Rollback,
    Contended,
}

async fn live_outbox_keeps_collected_parent_acceptance(
    fixture: IngressFixture,
    scenario: ProofScenario,
) {
    let outbox = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("upstream schema");
    let push = DatabasePushServiceStore::new_with_secret_key(
        fixture.db.clone(),
        &rand::random::<[u8; 32]>(),
    )
    .await
    .expect("provider");
    crate::push_registrations::DatabasePushRegistrationStore::new(fixture.db.clone())
        .await
        .expect("registration schema");
    let node = push
        .ensure_node(&owner(), "outbox-proof")
        .await
        .expect("node");
    push.upsert_device(
        &owner(),
        PushDeviceRegistration::new("proof-device", node.node(), PushDevicePlatform::Fcm, "test"),
    )
    .await
    .expect("device");
    push.register_first_party_node_for_owner(&owner(), "push.example.com", node.node(), None)
        .await
        .expect("registration");
    let delivery = uuid::Uuid::new_v4();
    let service: BareJid = "push.example.com".parse().expect("service");
    let item = notification_item(&delivery.to_string());
    let accepted = push
        .enqueue_canonical_notification_publish_job(
            node.node(),
            &item,
            &owner(),
            &service,
            None,
            delivery,
        )
        .await
        .expect("accepted provider work");
    let payload = String::from(item.payload.as_ref().expect("payload"));
    let context = Element::builder(
        "context",
        crate::notification_outbox::WADDLE_PUSH_CONTEXT_NS,
    )
    .build();
    let conn = fixture.db.guard().await.expect("upstream row");
    conn.execute("INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, approved_payload_xml, queue_acceptance_may_exist, status, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, 1, ?, 1, 1)", crate::db_params![delivery.to_string(), owner().to_string(), service.to_string(), node.node(), "bob@example.com", "bob@example.com/phone", "[\"bob@example.com/phone\"]", "dm", String::from(&context), payload, if scenario == ProofScenario::Parked {"failed"} else {"queued"}]).await.expect("unresolved upstream acceptance");
    drop(conn);
    let key = MessageKey::new();
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: owner(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: "bob@example.com".parse().expect("conversation"),
            archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                "proof-retention",
                owner().into(),
            ),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let mut tx = fixture.uow.begin().await.expect("parent");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [93; 32]).expect("digest"),
        None,
    )
    .await
    .expect("canonical row");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("intent");
    EffectDescendantRepository::attach(&mut tx, key, &intent.semantic_key(), delivery)
        .await
        .expect("accepted descendant");
    settle_recorded(&mut tx, key, std::slice::from_ref(&intent))
        .await
        .expect("receipt");
    CanonicalMessageRepository::terminalize(
        &mut tx,
        key,
        chrono::Utc::now() - chrono::Duration::days(20),
    )
    .await
    .expect("terminal parent");
    EffectDescendantRepository::settle_all(
        &mut tx,
        delivery,
        chrono::Utc::now() - chrono::Duration::days(20),
    )
    .await
    .expect("provider completion");
    tx.commit().await.expect("settled custody");
    let old = chrono::Utc::now() - chrono::Duration::days(20);
    let sql = match fixture.db.driver() { crate::db::DatabaseDriver::Postgres => "UPDATE ingress_messages SET retention_eligible_at = ?::timestamptz WHERE message_key = ?::uuid", crate::db::DatabaseDriver::Sqlite => "UPDATE ingress_messages SET retention_eligible_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE message_key = ?" };
    fixture
        .execute(
            sql,
            crate::db_params![old.to_rfc3339(), key.to_storage().to_string()],
        )
        .await;
    let budget = AliasGcBudget {
        deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        lock_timeout: std::time::Duration::from_secs(1),
        statement_timeout: std::time::Duration::from_secs(2),
        scan_timeout: std::time::Duration::from_secs(2),
        progress: AliasGcProgress::default(),
    };
    assert_eq!(
        crate::ingress_substrate::gc_expired_aliases(&fixture.db, chrono::Utc::now(), budget)
            .await
            .expect("collected canonical parent")
            .deleted_messages,
        1
    );
    assert_eq!(fixture.count("ingress_effect_descendants").await, 0);
    let safe = push
        .enqueue_notification_publish_job_from_user_server(
            node.node(),
            &notification_item("other-safe-wire-history"),
            &owner(),
        )
        .await
        .expect("wire history");
    push.execute("UPDATE push_publish_jobs SET status = ?, created_at_ms = ?, updated_at_ms = ? WHERE job_id = ?", crate::db_params![PUBLISH_JOB_STATUS_PUBLISHED, 1_i64, old.timestamp_millis(), accepted.job_id().to_string()]).await.expect("old completed provider acceptance");
    push.execute("UPDATE push_publish_jobs SET status = ?, created_at_ms = ?, updated_at_ms = ? WHERE job_id = ?", crate::db_params![PUBLISH_JOB_STATUS_PUBLISHED, 2_i64, old.timestamp_millis(), safe.job_id().to_string()]).await.expect("old safe wire row");
    if matches!(
        scenario,
        ProofScenario::Live | ProofScenario::Parked | ProofScenario::ProviderFirst
    ) {
        let mut tx = push.db.begin_immediate().await.expect("quota prune");
        prune_publish_jobs_tx(&mut tx, node.node(), 1)
            .await
            .expect("headroom");
        tx.commit().await.expect("prune commit");
        assert_eq!(
            scalar_i64(
                &push,
                "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ?",
                crate::db_params![accepted.job_id().to_string()]
            )
            .await,
            1,
            "unacknowledged canonical proof outlives its collected Foundation parent"
        );
        assert_eq!(
            scalar_i64(
                &push,
                "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ?",
                crate::db_params![safe.job_id().to_string()]
            )
            .await,
            0,
            "safe wire history makes quota headroom"
        );
    }
    if !matches!(scenario, ProofScenario::Live | ProofScenario::Parked) {
        let claim = outbox
            .claim_due_outbox_jobs(1)
            .await
            .expect("claim owner")
            .remove(0);
        if scenario == ProofScenario::AtomicOwner {
            assert!(outbox
                .mark_job_published(&claim)
                .await
                .expect("atomic owner completion"));
            assert_eq!(
                scalar_i64(
                    &push,
                    "SELECT upstream_completed FROM push_publish_jobs WHERE job_id = ?",
                    crate::db_params![accepted.job_id().to_string()]
                )
                .await,
                1
            );
        } else {
            push.execute("UPDATE notification_outbox SET status = 'published', claim_token = NULL, claimed_at_ms = NULL WHERE job_id = ? AND status = 'in-progress' AND claim_token = ?", crate::db_params![claim.job_id().as_str(), claim.claim_token()]).await.expect("legacy crash before completion ACK");
        }
        push.execute(
            "UPDATE notification_outbox SET updated_at_ms = 1 WHERE job_id = ?",
            crate::db_params![delivery.to_string()],
        )
        .await
        .expect("aged completed owner before ack");
        if scenario == ProofScenario::Rollback {
            match fixture.db.driver() {
                crate::db::DatabaseDriver::Sqlite => {
                    push.execute("CREATE TRIGGER reject_owner_delete BEFORE DELETE ON notification_outbox BEGIN SELECT RAISE(ABORT, 'owner delete fault'); END", ()).await.expect("delete fault");
                }
                crate::db::DatabaseDriver::Postgres => {
                    push.execute("CREATE FUNCTION reject_owner_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'owner delete fault'; END $$", ()).await.expect("fault function");
                    push.execute("CREATE TRIGGER reject_owner_delete BEFORE DELETE ON notification_outbox FOR EACH ROW EXECUTE FUNCTION reject_owner_delete()", ()).await.expect("delete fault");
                }
            }
            assert!(outbox
                .prune_completed_before(crate::time::now_ms(), 16)
                .await
                .is_err());
            assert_eq!(
                scalar_i64(
                    &push,
                    "SELECT upstream_completed FROM push_publish_jobs WHERE job_id = ?",
                    crate::db_params![accepted.job_id().to_string()]
                )
                .await,
                0,
                "ACK rolls back with failed owner deletion"
            );
            assert_eq!(fixture.count("notification_outbox").await, 1);
            let drop_trigger = match fixture.db.driver() {
                crate::db::DatabaseDriver::Sqlite => "DROP TRIGGER reject_owner_delete",
                crate::db::DatabaseDriver::Postgres => {
                    "DROP TRIGGER reject_owner_delete ON notification_outbox"
                }
            };
            push.execute(drop_trigger, ())
                .await
                .expect("remove delete fault");
        }
        if scenario == ProofScenario::Contended {
            let mut held = push
                .db
                .begin_immediate()
                .await
                .expect("provider owner lock");
            held.execute("UPDATE push_publish_jobs SET upstream_completed = upstream_completed WHERE job_id = ?", crate::db_params![accepted.job_id().to_string()]).await.expect("hold provider row");
            assert_eq!(
                outbox
                    .prune_completed_before(crate::time::now_ms(), 16)
                    .await
                    .expect("NOWAIT defers reverse order")
                    .jobs_deleted,
                0
            );
            assert_eq!(fixture.count("notification_outbox").await, 1);
            held.rollback().await.expect("release provider row");
        }
        assert_eq!(
            outbox
                .prune_completed_before(crate::time::now_ms(), 16)
                .await
                .expect("atomic exact owner ACK transfer")
                .jobs_deleted,
            1
        );
        assert_eq!(
            scalar_i64(
                &push,
                "SELECT upstream_completed FROM push_publish_jobs WHERE job_id = ?",
                crate::db_params![accepted.job_id().to_string()]
            )
            .await,
            1
        );
        let mut tx = push.db.begin_immediate().await.expect("proof reclamation");
        prune_publish_jobs_tx(&mut tx, node.node(), 0)
            .await
            .expect("acknowledged terminal proof can prune");
        tx.commit().await.expect("proof prune commit");
        assert_eq!(
            scalar_i64(
                &push,
                "SELECT COUNT(*) FROM push_publish_jobs WHERE node = ?",
                crate::db_params![node.node()]
            )
            .await,
            0
        );
    }
    drop(outbox);
    drop(push);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_live_outbox_protects_provider_proof_after_foundation_gc() {
    live_outbox_keeps_collected_parent_acceptance(
        IngressFixture::sqlite().await,
        ProofScenario::Live,
    )
    .await;
}
#[tokio::test]
async fn postgres_live_outbox_protects_provider_proof_after_foundation_gc() {
    if let Some(f) = IngressFixture::postgres("live_outbox_proof").await {
        live_outbox_keeps_collected_parent_acceptance(f, ProofScenario::Live).await;
    }
}
#[tokio::test]
async fn sqlite_parked_outbox_protects_provider_proof_after_foundation_gc() {
    live_outbox_keeps_collected_parent_acceptance(
        IngressFixture::sqlite().await,
        ProofScenario::Parked,
    )
    .await;
}
#[tokio::test]
async fn postgres_parked_outbox_protects_provider_proof_after_foundation_gc() {
    if let Some(f) = IngressFixture::postgres("parked_outbox_proof").await {
        live_outbox_keeps_collected_parent_acceptance(f, ProofScenario::Parked).await;
    }
}

#[tokio::test]
async fn sqlite_completed_owner_before_provider_prune_transfers_exact_ack() {
    live_outbox_keeps_collected_parent_acceptance(
        IngressFixture::sqlite().await,
        ProofScenario::OwnerFirst,
    )
    .await;
}
#[tokio::test]
async fn postgres_completed_owner_before_provider_prune_transfers_exact_ack() {
    if let Some(f) = IngressFixture::postgres("owner_first_ack").await {
        live_outbox_keeps_collected_parent_acceptance(f, ProofScenario::OwnerFirst).await;
    }
}
#[tokio::test]
async fn sqlite_provider_prune_before_completed_owner_keeps_then_reclaims_proof() {
    live_outbox_keeps_collected_parent_acceptance(
        IngressFixture::sqlite().await,
        ProofScenario::ProviderFirst,
    )
    .await;
}
#[tokio::test]
async fn postgres_provider_prune_before_completed_owner_keeps_then_reclaims_proof() {
    if let Some(f) = IngressFixture::postgres("provider_first_ack").await {
        live_outbox_keeps_collected_parent_acceptance(f, ProofScenario::ProviderFirst).await;
    }
}
#[tokio::test]
async fn sqlite_failed_owner_delete_rolls_back_provider_ack() {
    live_outbox_keeps_collected_parent_acceptance(
        IngressFixture::sqlite().await,
        ProofScenario::Rollback,
    )
    .await;
}
#[tokio::test]
async fn postgres_failed_owner_delete_rolls_back_provider_ack() {
    if let Some(f) = IngressFixture::postgres("owner_ack_rollback").await {
        live_outbox_keeps_collected_parent_acceptance(f, ProofScenario::Rollback).await;
    }
}
#[tokio::test]
async fn postgres_owner_ack_defers_contended_provider_row_without_reverse_wait() {
    if let Some(f) = IngressFixture::postgres("owner_ack_nowait").await {
        live_outbox_keeps_collected_parent_acceptance(f, ProofScenario::Contended).await;
    }
}

async fn exact_canonical_terminal_proof(fixture: IngressFixture) {
    let push = DatabasePushServiceStore::new_with_secret_key(
        fixture.db.clone(),
        &rand::random::<[u8; 32]>(),
    )
    .await
    .expect("provider");
    let target = push
        .ensure_node(&owner(), "proof-target")
        .await
        .expect("node");
    let other = push
        .ensure_node(&owner(), "proof-other")
        .await
        .expect("other node");
    let node =
        crate::notification_outbox::PushServiceNodeName::new(target.node()).expect("typed node");
    let service: BareJid = "push.example.com".parse().expect("service");
    let delivery = uuid::Uuid::new_v4();
    let item = notification_item(&delivery.to_string());
    let options = Element::builder("x", waddle_xmpp::xep::NS_DATA_FORMS).build();
    let payload = String::from(item.payload.as_ref().expect("payload"));
    push.execute("INSERT INTO push_publish_jobs (job_id, owner_bare_jid, push_service_jid, node, item_id, payload_xml, publish_options_xml, ancestry_job_id, acceptance_scope, status, terminal_disposition, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'canonical', 'failed', 'registration-revoked', 1, 1)", crate::db_params!["exact-proof", owner().to_string(), service.to_string(), target.node(), delivery.to_string(), payload.clone(), String::from(&options), delivery.to_string()]).await.expect("explicit cancellation record");
    assert_eq!(
        push.canonical_terminal_notification_proof(
            &service,
            &node,
            &item,
            &owner(),
            Some(&options),
            delivery
        )
        .await
        .expect("exact proof"),
        Some(CanonicalNotificationTerminalProof::RegistrationRevoked)
    );
    let outbox = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("local owner schema");
    let context = Element::builder(
        "context",
        crate::notification_outbox::WADDLE_PUSH_CONTEXT_NS,
    )
    .build();
    push.execute("INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, approved_payload_xml, approved_publish_options_xml, queue_acceptance_may_exist, status, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, 'bob@example.com', 'bob@example.com/phone', '[\"bob@example.com/phone\"]', 'dm', 1, ?, ?, ?, 1, 'published', 1, 1)", crate::db_params![delivery.to_string(), owner().to_string(), service.to_string(), target.node(), String::from(&context), payload.clone(), String::from(&options)]).await.expect("completed exact upstream");
    let changes = [
        (
            "owner_bare_jid",
            "mallory@example.com".to_owned(),
            owner().to_string(),
        ),
        (
            "push_service_jid",
            "foreign.example.com".to_owned(),
            service.to_string(),
        ),
        ("node", other.node().to_owned(), target.node().to_owned()),
        ("item_id", "wire-id".to_owned(), delivery.to_string()),
        (
            "ancestry_job_id",
            uuid::Uuid::new_v4().to_string(),
            delivery.to_string(),
        ),
        (
            "acceptance_scope",
            "wire".to_owned(),
            "canonical".to_owned(),
        ),
        (
            "payload_xml",
            String::from(
                &Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH)
                    .append("different")
                    .build(),
            ),
            payload,
        ),
        (
            "publish_options_xml",
            String::from(
                &Element::builder("x", waddle_xmpp::xep::NS_DATA_FORMS)
                    .append("different")
                    .build(),
            ),
            String::from(&options),
        ),
    ];
    for (column, changed, original) in changes {
        let sql = format!("UPDATE push_publish_jobs SET {column} = ?, upstream_completed = 0 WHERE job_id = 'exact-proof'");
        push.execute(&sql, crate::db_params![changed])
            .await
            .expect("foreign tuple");
        assert_eq!(
            push.canonical_terminal_notification_proof(
                &service,
                &node,
                &item,
                &owner(),
                Some(&options),
                delivery
            )
            .await
            .expect("proof"),
            None,
            "foreign {column} cannot resolve uncertainty"
        );
        push.acknowledge_canonical_outbox_completion(
            &service,
            &node,
            &item,
            &owner(),
            Some(&options),
            delivery,
        )
        .await
        .expect("foreign ACK is ignored");
        let mut tx = fixture
            .db
            .begin_immediate()
            .await
            .expect("local prune transfer");
        acknowledge_completed_outbox_tx(&mut tx, &delivery.to_string())
            .await
            .expect("foreign prune transfer ignored");
        tx.commit().await.expect("transfer commit");
        assert_eq!(
            scalar_i64(
                &push,
                "SELECT upstream_completed FROM push_publish_jobs WHERE job_id = 'exact-proof'",
                ()
            )
            .await,
            0,
            "foreign {column} cannot acquire ACK"
        );
        push.execute(&sql, crate::db_params![original])
            .await
            .expect("restore exact tuple");
    }
    push.acknowledge_canonical_outbox_completion(
        &service,
        &node,
        &item,
        &owner(),
        Some(&options),
        delivery,
    )
    .await
    .expect("positive ACK");
    assert_eq!(
        scalar_i64(
            &push,
            "SELECT upstream_completed FROM push_publish_jobs WHERE job_id = 'exact-proof'",
            ()
        )
        .await,
        1
    );
    drop(outbox);
    drop(push);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_canonical_proof_and_ack_require_exact_frozen_acceptance() {
    exact_canonical_terminal_proof(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_canonical_proof_and_ack_require_exact_frozen_acceptance() {
    if let Some(f) = IngressFixture::postgres("exact_terminal_proof").await {
        exact_canonical_terminal_proof(f).await;
    }
}

#[tokio::test]
async fn sqlite_claim_fenced_owner_completion_atomically_acks_local_provider() {
    live_outbox_keeps_collected_parent_acceptance(
        IngressFixture::sqlite().await,
        ProofScenario::AtomicOwner,
    )
    .await;
}
#[tokio::test]
async fn postgres_claim_fenced_owner_completion_atomically_acks_local_provider() {
    if let Some(f) = IngressFixture::postgres("atomic_owner_ack").await {
        live_outbox_keeps_collected_parent_acceptance(f, ProofScenario::AtomicOwner).await;
    }
}
