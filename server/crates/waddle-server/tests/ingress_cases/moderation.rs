use super::*;
use waddle_xmpp::ingress::RetractionTombstoneMutation;
use waddle_xmpp_core::mam::{
    ArchivedModeration, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone, RichMessageId,
    RichText,
};

async fn committed_moderation_survives_planned_retraction(fixture: IngressFixture) {
    let archive = fixture.principal.bare_jid().clone();
    commit_submission(
        &fixture.uow,
        &archive_plan(&fixture, None, "original", "moderated-target"),
        5,
    )
    .await
    .expect("archive target");
    let target = StanzaId::new("moderated-target", archive.clone().into());
    // Phase A has captured a retraction of a then-live message.
    let mut planned = fixture.submission(None, "retraction");
    planned
        .plan
        .intents
        .push(IngressEffectIntent::RetractionTombstone {
            mutation: RetractionTombstoneMutation {
                archive: archive.clone(),
                target_stanza_id: target.clone(),
                retraction_stanza_id: StanzaId::new("retract", archive.clone().into()),
            },
        });
    planned
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
    // Another transaction terminalizes it before Phase B starts.
    let moderated = ArchivedRichMessage {
        payload: Some(ArchivedRichPayload::Tombstone(ArchivedTombstone {
            retraction_id: None,
            stamp: chrono::Utc::now(),
            moderation: Some(ArchivedModeration {
                target_id: RichMessageId::new("moderated-target").expect("target"),
                moderated_by: "moderator@example.com".parse().expect("moderator"),
                stamp: None,
                reason: Some(RichText::new("moderation reason").expect("reason")),
            }),
            sender_scope: Some(fixture.principal.bare_jid().clone()),
        })),
        reply: None,
        references: Vec::new(),
        mentions: Vec::new(),
        subjects: Default::default(),
        occupant_id: None,
        muc_sender: None,
    };
    let encoded = serde_json::to_string(&moderated).expect("stored moderation");
    fixture
        .execute(
            "UPDATE mam_messages SET body = NULL, rich_payload = ? WHERE id = 'moderated-target'",
            waddle_server::db_params![encoded.clone()],
        )
        .await;
    let decision = commit_submission(&fixture.uow, &planned, 5)
        .await
        .expect("commit retraction");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_eq!(
        fixture
            .optional_text("SELECT rich_payload FROM mam_messages WHERE id = 'moderated-target'")
            .await,
        Some(encoded)
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_moderation_survives_retraction_sqlite() {
    committed_moderation_survives_planned_retraction(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_moderation_survives_retraction_postgres() {
    if let Some(fixture) = IngressFixture::postgres("moderation_preserved").await {
        committed_moderation_survives_planned_retraction(fixture).await;
    }
}
