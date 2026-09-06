use super::*;
use waddle_server::ingress::effects::direct::DurableDirectEffect;
use waddle_xmpp::ingress::RetractionTombstoneMutation;
use waddle_xmpp_core::mam::ArchivedTombstone;

/// XEP-0424 §Tombstones: the retraction must replace the archived row that the
/// recorded intent names, scoped to the archive that assigned its stanza-id.
async fn retraction_tombstone(fixture: IngressFixture) {
    let archive = fixture.principal.bare_jid().clone();
    let original = archive_plan(&fixture, Some("retracted-origin"), "hello", "retracted-id");
    commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("archive the original");
    assert_eq!(
        fixture
            .optional_text("SELECT body FROM mam_messages WHERE id = 'retracted-id'")
            .await
            .as_deref(),
        Some("hello")
    );

    let target = StanzaId::new("retracted-id", archive.clone().into());
    let retraction = StanzaId::new("retraction-id", archive.clone().into());
    let mut submission = fixture.submission(Some("retraction-origin"), "retraction");
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RetractionTombstone {
            mutation: RetractionTombstoneMutation {
                archive: archive.clone(),
                target_stanza_id: target.clone(),
                retraction_stanza_id: retraction,
            },
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::RetractionTombstone {
                archive,
                target,
                tombstone: ArchivedTombstone {
                    retraction_id: None,
                    stamp: chrono::Utc::now(),
                    moderation: None,
                    sender_scope: None,
                },
            },
        ))));

    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit the retraction");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_eq!(
        fixture
            .optional_text("SELECT body FROM mam_messages WHERE id = 'retracted-id'")
            .await,
        None,
        "the tombstoned row keeps no body"
    );
    let rich = fixture
        .optional_text("SELECT rich_payload FROM mam_messages WHERE id = 'retracted-id'")
        .await
        .expect("tombstone payload");
    assert!(rich.contains("Tombstone"), "{rich}");
    // The retraction's own canonical row is separate; only the target changed.
    assert_eq!(fixture.count("mam_messages").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_retraction_tombstone_sqlite() {
    retraction_tombstone(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_retraction_tombstone_postgres() {
    if let Some(fixture) = IngressFixture::postgres("retraction_tombstone").await {
        retraction_tombstone(fixture).await;
    }
}

async fn tombstoned_retry_does_not_recreate_inbox(fixture: IngressFixture) {
    use waddle_server::ingress::effects::PlanEffectDependency;
    use waddle_server::ingress_uow::MamArchiveRepository;
    use waddle_xmpp::{
        inbox::{ConversationKind, InboxEntry},
        ingress::InboxProjectionMutation,
    };
    let archive = fixture.principal.bare_jid().clone();
    let mut retry = archive_plan(
        &fixture,
        Some("tombstoned-inbox-origin"),
        "must stay retracted",
        "tombstoned-inbox-id",
    );
    let first = commit_submission(&fixture.uow, &retry, 5)
        .await
        .expect("initial canonical archive");
    let target = StanzaId::new("tombstoned-inbox-id", archive.clone().into());
    let mut transaction = fixture.uow.begin().await.expect("tombstone transaction");
    MamArchiveRepository::replace_with_tombstone(
        &mut transaction,
        &archive,
        &target,
        &ArchivedTombstone {
            retraction_id: None,
            stamp: chrono::Utc::now(),
            moderation: None,
            sender_scope: None,
        },
    )
    .await
    .expect("replace original with tombstone");
    transaction.commit().await.expect("commit tombstone");
    // The original transaction has no inbox projection/marker. Repair must not
    // resurrect one after its archive authority has been retracted.
    let entry = InboxEntry::new(
        "juliet@example.com".parse().expect("peer"),
        ConversationKind::Direct,
        "tombstoned-inbox-id",
        chrono::Utc::now().timestamp(),
    )
    .with_preview("must stay retracted");
    retry.plan.intents.push(IngressEffectIntent::InboxProject {
        owner: archive.clone(),
        mutation: InboxProjectionMutation::Direct {
            entry: entry.clone(),
            increment_unread: true,
        },
    });
    retry.plan.plan.push(
        PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ProjectInbox {
                owner: archive.clone(),
                entry: Box::new(entry),
                increment_unread: true,
            },
        )))
        .with_dependency(PlanEffectDependency::AfterArchive {
            archive,
            minted: target,
        }),
    );
    let decision = commit_submission(&fixture.uow, &retry, 5)
        .await
        .expect("repair respects tombstone");
    assert_eq!(decision.class, IngressDecisionClass::ExistingRepaired);
    assert_eq!(decision.message_key, first.message_key);
    assert_eq!(fixture.count("inbox_entries").await, 0);
    assert_eq!(fixture.count("ingress_deliveries").await, 0);
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert!(decision.receipts_pending.is_empty());
    assert!(waddle_server::ingress::execute::terminalize_if_complete(
        &fixture.uow,
        decision.message_key.expect("canonical row"),
    )
    .await
    .expect("suppressed projection discharges its intent"));
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .optional_text("SELECT body FROM mam_messages WHERE id = 'tombstoned-inbox-id'")
            .await,
        None
    );
    fixture.close().await;
}

#[tokio::test]
async fn ingress_tombstoned_retry_keeps_inbox_missing_sqlite() {
    tombstoned_retry_does_not_recreate_inbox(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_tombstoned_retry_keeps_inbox_missing_postgres() {
    if let Some(fixture) = IngressFixture::postgres("tombstone_inbox_missing").await {
        tombstoned_retry_does_not_recreate_inbox(fixture).await;
    }
}
