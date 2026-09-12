//! XEP-0424 further-distribution suppression survives ingress alias retention.
pub mod ingress_support;

use ingress_support::IngressFixture;
use waddle_server::ingress::{
    commit::commit_submission,
    effects::{
        room::{DurableRoomEffect, RoomFenceRequirement},
        Effect,
    },
    DurableEffect, IngressDecisionClass, IngressSubmission, PlannedEffect,
};
use waddle_xmpp::{
    ingress::IngressEffectIntent,
    mam::{ArchiveExpectation, ArchivedMessage, ArchivedTombstone},
};
use waddle_xmpp_core::xep0359::StanzaId;

fn archive_plan(fixture: &IngressFixture, id: &str) -> IngressSubmission {
    use waddle_xmpp::ingress::{DigestContext, DigestInput, NormalizedTarget};
    let mut submission = fixture.submission(Some("expired-alias-origin"), "retracted body");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("groupchat digest");
    let stamp = StanzaId::new(id, room.clone().into());
    let mut message = ArchivedMessage::for_test(
        "room@muc.example.com/romeo".parse().expect("occupant"),
        room.clone().into(),
    );
    message.id = id.into();
    message.body = Some("retracted body".into());
    message.message_type = xmpp_parsers::message::MessageType::Groupchat;
    message.origin_id = submission.digest_input.origin().cloned();
    message.stanza_id = Some(stamp.clone());
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ArchiveAuthoritative {
            archive: room.clone(),
            stanza_id: stamp,
            by: room.clone(),
            archived_at: message.timestamp,
            ordinal: None,
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ArchiveGroupchat {
                room,
                message: Box::new(message),
                fence: RoomFenceRequirement::Unfenced,
                archive_expectation: ArchiveExpectation::Fresh,
            },
        ))));
    submission
}

async fn tombstone_survives_expired_origin_alias(fixture: IngressFixture) {
    use waddle_server::ingress::{effects::PlanEffectDependency, ExternalEffect};
    use waddle_server::ingress_uow::MamArchiveRepository;
    use waddle_xmpp::{
        inbox::{ConversationKind, InboxEntry},
        ingress::InboxProjectionMutation,
        Stanza,
    };

    let archive: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let original = archive_plan(&fixture, "old-id");
    let first = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("initial archive");
    let mut tx = fixture.uow.begin().await.expect("retraction transaction");
    MamArchiveRepository::replace_with_tombstone(
        &mut tx,
        &archive,
        &StanzaId::new("old-id", archive.clone().into()),
        &ArchivedTombstone {
            retraction_id: None,
            stamp: chrono::Utc::now(),
            moderation: None,
            sender_scope: None,
        },
    )
    .await
    .expect("retract original");
    tx.commit().await.expect("commit retraction");
    fixture
        .execute("DELETE FROM ingress_origin_aliases", ())
        .await;

    let mut retry = archive_plan(&fixture, "new-id");
    let dependency = PlanEffectDependency::AfterArchive {
        archive: archive.clone(),
        minted: StanzaId::new("new-id", archive.clone().into()),
    };
    let entry = InboxEntry::new(
        archive.clone(),
        ConversationKind::MucRoom,
        "new-id",
        chrono::Utc::now().timestamp(),
    )
    .with_preview("retracted body");
    retry.plan.intents.push(IngressEffectIntent::InboxProject {
        owner: fixture.principal.bare_jid().clone(),
        mutation: InboxProjectionMutation::GroupchatChannel {
            room: archive.clone(),
            increment_unread: true,
        },
    });
    retry.plan.plan.push(
        PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ProjectGroupchatInbox {
                archive_stanza_id: StanzaId::new("new-id", archive.clone().into()),
                owner: fixture.principal.bare_jid().clone(),
                entry: Box::new(entry),
                is_recipient: true,
                recovery: None,
            },
        )))
        .with_dependency(dependency.clone()),
    );
    retry.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Frame(Box::new(
            Stanza::Message(retry.plan.sanitized_message.clone()),
        ))))
        .with_dependency(dependency),
    );
    let decision = commit_submission(&fixture.uow, &retry, 5)
        .await
        .expect("fresh alias retry advances despite tombstone");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_ne!(decision.message_key, first.message_key);
    assert!(decision.external.is_empty(), "no further distribution");
    assert_eq!(fixture.count("inbox_entries").await, 0);
    assert_eq!(fixture.count("ingress_deliveries").await, 0);
    assert_eq!(fixture.count("mam_messages").await, 1);
    assert_eq!(fixture.count("mam_messages WHERE id = 'new-id'").await, 0);
    assert_eq!(
        fixture
            .optional_text("SELECT body FROM mam_messages WHERE id = 'old-id'")
            .await,
        None
    );
    assert!(fixture
        .optional_text("SELECT rich_payload FROM mam_messages WHERE id = 'old-id'")
        .await
        .expect("tombstone")
        .contains("Tombstone"));
    fixture.close().await;
}

#[tokio::test]
async fn ingress_tombstone_expired_origin_alias_suppresses_dependents_sqlite() {
    tombstone_survives_expired_origin_alias(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_tombstone_expired_origin_alias_suppresses_dependents_postgres() {
    if let Some(fixture) = IngressFixture::postgres("tombstone_expired_alias").await {
        tombstone_survives_expired_origin_alias(fixture).await;
    }
}
