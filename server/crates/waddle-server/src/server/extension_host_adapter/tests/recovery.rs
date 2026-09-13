//! Adapter-produced frozen notification work remains recoverable after Phase C fails.
use super::super::ExtensionHostAdapter;
use super::direct_ingress::{adapter, invocation, request};
use crate::{
    db::DatabaseDriver,
    ingress::{test_support::IngressFixture, RecoveryEnvironment},
    ingress_uow::EffectIntentRepository,
    notification_settings_projection::{
        ConversationKind, NotificationSettingsProjection, NotificationSettingsSource,
    },
};
use std::{sync::Arc, time::Duration};
use waddle_xmpp::{
    ingress::{
        IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    },
    xep::NotificationLevel,
};

async fn candidate_failure(f: &IngressFixture) {
    match f.db.driver() {
        DatabaseDriver::Sqlite => f.execute("CREATE TRIGGER fail_extension_candidate BEFORE INSERT ON notification_candidates BEGIN SELECT RAISE(FAIL, 'injected candidate failure'); END", ()).await,
        DatabaseDriver::Postgres => {
            f.execute("CREATE OR REPLACE FUNCTION fail_extension_candidate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected candidate failure'; END $$", ()).await;
            f.execute("CREATE TRIGGER fail_extension_candidate BEFORE INSERT ON notification_candidates FOR EACH ROW EXECUTE FUNCTION fail_extension_candidate()", ()).await;
        }
    }
}

async fn restore_candidates(f: &IngressFixture) {
    f.execute(
        match f.db.driver() {
            DatabaseDriver::Sqlite => "DROP TRIGGER fail_extension_candidate",
            DatabaseDriver::Postgres => {
                "DROP TRIGGER fail_extension_candidate ON notification_candidates"
            }
        },
        (),
    )
    .await;
}

async fn policy(adapter: &ExtensionHostAdapter, mode: NotificationLevel) {
    let sender: jid::BareJid = "romeo@example.com".parse().expect("sender");
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let store = &adapter.state.deps.protocol.notification_settings_projection;
    store
        .upsert(&NotificationSettingsProjection {
            owner_bare_jid: recipient.clone(),
            conversation_jid: sender.clone(),
            conversation_kind: ConversationKind::Direct,
            mode,
            rich_payload_opt_in: false,
            source_version: 1,
            updated_at_ms: chrono::Utc::now().timestamp_millis(),
            source: NotificationSettingsSource::WaddleDmBookmarks,
            source_item_jid: sender.clone(),
        })
        .await
        .expect("notification policy");
    assert_eq!(
        store
            .effective_setting(&recipient, &sender, ConversationKind::Direct)
            .await
            .expect("effective policy"),
        mode
    );
}

async fn assert_frozen_inserted(f: &IngressFixture) {
    let key = f
        .optional_text(
            "SELECT CAST(message_key AS TEXT) FROM ingress_messages WHERE terminal_at IS NULL",
        )
        .await
        .expect("pending canonical key");
    let key = MessageKey::from_storage(key.parse().expect("canonical uuid"));
    let mut tx = f.uow.begin().await.expect("inspect frozen intents");
    let recorded = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("recorded intents");
    assert_eq!(
        recorded
            .iter()
            .filter(|intent| matches!(
                intent,
                IngressEffectIntent::NotificationActivityPreview {
                    mutation: NotificationActivityMutation::NotificationCandidate {
                        outcome: NotificationCandidateOutcome::Inserted,
                        ..
                    },
                    ..
                }
            ))
            .count(),
        1,
        "Phase B freezes the candidate before insertion fails"
    );
    tx.commit().await.expect("inspection complete");
}

async fn recover(f: &IngressFixture, adapter: &ExtensionHostAdapter, expected: i64) {
    let sql = match f.db.driver() {
        DatabaseDriver::Sqlite => "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE terminal_at IS NULL",
        DatabaseDriver::Postgres => "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE terminal_at IS NULL",
    };
    f.execute(
        sql,
        crate::db_params![(chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339()],
    )
    .await;
    adapter.state.deps.protocol.ingress.trigger_maintenance();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if f.count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await
                == expected
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("maintenance settles the adapter's frozen offline work");
}

async fn frozen_candidate_recovers(f: IngressFixture) {
    let adapter = adapter(&f).await;
    let environment: Arc<dyn RecoveryEnvironment> = adapter.state.clone();
    adapter
        .state
        .deps
        .protocol
        .ingress
        .bind_recovery_environment(Arc::downgrade(&environment));
    let mut original_stamp = None;
    for (index, origin) in ["extension-recovery-original", "extension-recovery-witness"]
        .into_iter()
        .enumerate()
    {
        let before = i64::try_from(index).expect("small case index");
        policy(&adapter, NotificationLevel::Always).await;
        candidate_failure(&f).await;
        adapter
            .send_message(&invocation(), request(origin))
            .await
            .expect("commit remains accepted after candidate insertion fails");
        assert_eq!(f.count("ingress_messages").await, before + 1);
        assert_eq!(
            f.count("ingress_messages WHERE terminal_at IS NULL").await,
            1
        );
        assert_eq!(
            f.count("pending_delivery").await,
            before,
            "candidate failure rolls back its pending row atomically"
        );
        assert_eq!(f.count("notification_candidates").await, before);
        assert_frozen_inserted(&f).await;
        // A fresh planning pass would suppress this candidate. Recovery must
        // obey the already frozen Inserted obligation instead.
        policy(&adapter, NotificationLevel::Never).await;
        restore_candidates(&f).await;
        recover(&f, &adapter, before + 1).await;
        assert_eq!(f.count("pending_delivery").await, before + 1);
        assert_eq!(f.count("notification_candidates").await, before + 1);
        let stamp = f.optional_text("SELECT stanza_id FROM notification_candidates ORDER BY created_at_ms, stanza_id LIMIT 1").await;
        if let Some(original) = &original_stamp {
            assert_eq!(
                &stamp, original,
                "second maintenance pass preserves the original candidate"
            );
        } else {
            assert!(stamp.is_some());
            original_stamp = Some(stamp);
        }
    }
    assert_eq!(
        f.count("notification_candidates WHERE sender_jid = 'romeo@example.com/extension-host'")
            .await,
        2
    );
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    drop(environment);
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_direct_frozen_candidate_recovery_sqlite() {
    frozen_candidate_recovers(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_direct_frozen_candidate_recovery_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_direct_recovery").await {
        frozen_candidate_recovers(f).await;
    }
}
