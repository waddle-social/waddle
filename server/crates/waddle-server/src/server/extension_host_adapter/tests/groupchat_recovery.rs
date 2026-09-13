//! Real extension groupchat sends preserve notification work across Phase C failures.
use super::groupchat_ingress::GroupchatFixture;
use super::groupchat_receipts::{intents, key};
use crate::{
    db::DatabaseDriver,
    ingress::{test_support::IngressFixture, RecoverySweepOutcome},
    ingress_uow::{CanonicalMessageRepository, EffectReceiptRepository},
    notification_settings_projection::{
        ConversationKind, NotificationSettingsProjection, NotificationSettingsSource,
    },
    server::routes::interpret::reconcile_groupchat_notification_recovery,
};
use waddle_xmpp::{
    inbox::storage::GroupchatNotificationRecovery,
    ingress::{
        GroupchatNotificationRecoveryAction, IngressEffectIntent, NotificationActivityMutation,
        NotificationCandidateOutcome,
    },
    xep::NotificationLevel,
};

async fn policy(fixture: &GroupchatFixture, mode: NotificationLevel) {
    let recipient = "juliet@example.com".parse().expect("recipient");
    let store = &fixture
        .adapter
        .state
        .deps
        .protocol
        .notification_settings_projection;
    store
        .upsert(&NotificationSettingsProjection {
            owner_bare_jid: recipient,
            conversation_jid: fixture.room.clone(),
            conversation_kind: ConversationKind::PrivateGroup,
            mode,
            rich_payload_opt_in: false,
            source_version: 1,
            updated_at_ms: chrono::Utc::now().timestamp_millis(),
            source: NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: fixture.room.clone(),
        })
        .await
        .expect("room notification policy");
    assert_eq!(
        store
            .effective_setting(
                &"juliet@example.com".parse().expect("recipient"),
                &fixture.room,
                ConversationKind::PrivateGroup
            )
            .await
            .expect("effective room policy"),
        mode
    );
}

async fn pending(
    f: &IngressFixture,
    fixture: &GroupchatFixture,
) -> Vec<GroupchatNotificationRecovery> {
    assert_eq!(
        f.count("ingress_messages").await,
        1,
        "adapter send commits one canonical message"
    );
    let canonical = key(f).await;
    let recoveries = fixture
        .adapter
        .state
        .deps
        .protocol
        .inbox_storage
        .list_pending_groupchat_notification_recoveries(10)
        .await
        .expect("pending recovery rows");
    assert!(
        recoveries.iter().any(|r| r.key.recipient
            == "juliet@example.com"
                .parse::<jid::BareJid>()
                .expect("recipient")),
        "offline durable recipient has recovery work"
    );
    assert!(
        recoveries
            .iter()
            .all(|r| r.message_key == canonical && r.key.room == fixture.room),
        "recovery work references the canonical room message"
    );
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NULL").await,
        1
    );
    recoveries
}

async fn settle(
    f: &IngressFixture,
    fixture: &GroupchatFixture,
    recoveries: &[GroupchatNotificationRecovery],
) {
    for recovery in recoveries {
        assert_eq!(
            reconcile_groupchat_notification_recovery(&fixture.adapter.state, recovery)
                .await
                .expect("reconcile recovery"),
            RecoverySweepOutcome::Completed
        );
    }
    assert_eq!(
        f.count("notification_candidates WHERE recipient_bare_jid = 'juliet@example.com'")
            .await,
        1
    );
    let candidate_count = f.count("notification_candidates").await;
    for recovery in recoveries {
        assert_eq!(
            reconcile_groupchat_notification_recovery(&fixture.adapter.state, recovery)
                .await
                .expect("repeat recovery"),
            RecoverySweepOutcome::Completed
        );
    }
    assert_eq!(
        f.count("notification_candidates").await,
        candidate_count,
        "repeated reconciliation never duplicates candidates"
    );
    assert!(fixture
        .adapter
        .state
        .deps
        .protocol
        .inbox_storage
        .list_pending_groupchat_notification_recoveries(10)
        .await
        .expect("remaining recovery work")
        .is_empty());
    let canonical = key(f).await;
    let mut tx = f.uow.begin().await.expect("verify settlement");
    assert!(
        EffectReceiptRepository::receipts_complete(&mut tx, canonical)
            .await
            .expect("complete receipt coverage")
    );
    assert!(CanonicalMessageRepository::is_terminal(&mut tx, canonical)
        .await
        .expect("terminal canonical row"));
    tx.commit().await.expect("inspection commit");
}

async fn deferred_policy_recovers(f: IngressFixture) {
    let fixture = GroupchatFixture::new(&f).await;
    policy(&fixture, NotificationLevel::Always).await;
    f.execute("ALTER TABLE notification_settings_projection RENAME TO notification_settings_projection_unavailable", ()).await;
    fixture
        .send("extension-groupchat-deferred-policy")
        .await
        .expect("policy failure preserves committed send");
    let recoveries = pending(&f, &fixture).await;
    assert!(intents(&f).await.iter().any(|intent| matches!(intent,
        IngressEffectIntent::GroupchatNotificationRecovery { mutation }
        if mutation.action == GroupchatNotificationRecoveryAction::DeferredPolicy
            && mutation.recipient == "juliet@example.com".parse::<jid::BareJid>().expect("recipient")
    )), "T0 lookup failure records DeferredPolicy");
    assert_eq!(f.count("notification_candidates").await, 0);
    for recovery in &recoveries {
        assert_eq!(
            reconcile_groupchat_notification_recovery(&fixture.adapter.state, recovery)
                .await
                .expect("unavailable policy remains recoverable"),
            RecoverySweepOutcome::Pending
        );
    }
    f.execute("ALTER TABLE notification_settings_projection_unavailable RENAME TO notification_settings_projection", ()).await;
    settle(&f, &fixture, &recoveries).await;
    fixture.close(f).await;
}

async fn frozen_candidate_recovers(f: IngressFixture) {
    let fixture = GroupchatFixture::new(&f).await;
    policy(&fixture, NotificationLevel::Always).await;
    match f.db.driver() {
        DatabaseDriver::Sqlite => f.execute("CREATE TRIGGER fail_groupchat_candidate BEFORE INSERT ON notification_candidates BEGIN SELECT RAISE(FAIL, 'injected groupchat candidate failure'); END", ()).await,
        DatabaseDriver::Postgres => {
            f.execute("CREATE OR REPLACE FUNCTION fail_groupchat_candidate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected groupchat candidate failure'; END $$", ()).await;
            f.execute("CREATE TRIGGER fail_groupchat_candidate BEFORE INSERT ON notification_candidates FOR EACH ROW EXECUTE FUNCTION fail_groupchat_candidate()", ()).await;
        }
    }
    fixture
        .send("extension-groupchat-frozen-candidate")
        .await
        .expect("candidate failure preserves committed send");
    let recoveries = pending(&f, &fixture).await;
    assert_eq!(intents(&f).await.iter().filter(|intent| matches!(intent,
        IngressEffectIntent::NotificationActivityPreview {
            owner,
            mutation: NotificationActivityMutation::NotificationCandidate { outcome: NotificationCandidateOutcome::Inserted, .. }, ..
        } if owner == &"juliet@example.com".parse::<jid::BareJid>().expect("recipient")
    )).count(), 1, "Phase B freezes exactly one offline recipient candidate");
    assert_eq!(f.count("notification_candidates").await, 0);
    // A fresh T0 evaluation would suppress this notification. Recovery must
    // honor the durable Inserted decision instead.
    policy(&fixture, NotificationLevel::Never).await;
    f.execute(
        match f.db.driver() {
            DatabaseDriver::Sqlite => "DROP TRIGGER fail_groupchat_candidate",
            DatabaseDriver::Postgres => {
                "DROP TRIGGER fail_groupchat_candidate ON notification_candidates"
            }
        },
        (),
    )
    .await;
    settle(&f, &fixture, &recoveries).await;
    fixture.close(f).await;
}

#[tokio::test]
async fn extension_groupchat_deferred_policy_recovery_sqlite() {
    deferred_policy_recovers(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_deferred_policy_recovery_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_deferred_policy").await {
        deferred_policy_recovers(f).await;
    }
}
#[tokio::test]
async fn extension_groupchat_frozen_candidate_recovery_sqlite() {
    frozen_candidate_recovers(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_groupchat_frozen_candidate_recovery_postgres() {
    if let Some(f) = IngressFixture::postgres("groupchat_frozen_candidate").await {
        frozen_candidate_recovers(f).await;
    }
}
