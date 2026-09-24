//! Frozen room copies retain payload order and exact-resource authority.
use super::*;
use crate::ingress::{
    commit::commit_submission, identity::IngressAppendObligationRef, test_support::IngressFixture,
};
use waddle_xmpp::ingress::{EffectMessageIdentity, EntityGeneration, IngressEffectIntent};

async fn room_authority(fixture: IngressFixture, archived: bool) {
    let mut submission = fixture.submission(Some("reflection-authority"), "frozen reflection");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let target = submission.sender.clone();
    let occupant: jid::FullJid = "juliet@example.com/phone".parse().expect("occupant");
    let stamp = waddle_xmpp_core::xep0359::StanzaId::new("room-stamp", room.clone().into());
    let route = IngressEffectIntent::RouteMucGroupchat {
        room: room.clone(),
        occupants: vec![target.clone(), occupant.clone()],
        reflection: target.clone(),
        room_generation: EntityGeneration::INITIAL,
        route_identity: EffectMessageIdentity::stanza(stamp.clone()),
    };
    let reflection =
        crate::ingress::reflection_dispatch::original_intent(&route).expect("reflection");
    let mut message = submission.plan.sanitized_message.clone();
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.from = Some(room.with_resource_str("sender").expect("nick").into());
    message.to = Some(target.clone().into());
    waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &stamp);
    waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
        &mut message,
        &waddle_xmpp::xep::xep0421::OccupantId("occupant".into()),
    );
    crate::ingress::test_support::capture_room_message(&mut submission.plan, &message);
    submission.plan.intents = vec![route.clone(), reflection.clone()];
    if archived {
        submission
            .plan
            .intents
            .push(IngressEffectIntent::ArchiveAuthoritative {
                ordinal: None,
                archive: room.clone(),
                by: room.clone(),
                stanza_id: stamp,
                archived_at: chrono::Utc::now(),
            });
    }
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    for (recipient, intent) in [(&occupant, &route), (&target, &reflection)] {
        let receipt = crate::ingress::receipt_key(intent).expect("receipt");
        let message_key = decision.message_key.expect("key");
        let archive_positions = crate::ingress_uow::ArchiveDispatchRepository::positions_pooled(
            &fixture.db,
            message_key,
            &receipt,
        )
        .await
        .expect("archive positions");
        let obligation = IngressAppendObligationRef {
            archive_positions,
            dispatch_stream: None,
            message_key,
            sender_bare: room.clone(),
            receipt,
            received_at: None,
        };
        let mut copy = message.clone();
        copy.to = Some(recipient.clone().into());
        let stanza = Stanza::Message(copy.clone());
        check_stanza_binding(
            &stanza,
            &obligation.sender_bare,
            obligation.receipt.kind.to_storage(),
        )
        .expect("room authors the copy");
        check_canonical_obligation(&fixture.db, &stanza, &obligation)
            .await
            .expect("frozen occupant and original-reflection payloads remain authorized");
        let mut sibling = copy.clone();
        sibling.to = Some(
            recipient
                .to_bare()
                .with_resource_str("sibling")
                .expect("sibling")
                .into(),
        );
        assert!(
            check_canonical_obligation(&fixture.db, &Stanza::Message(sibling), &obligation)
                .await
                .is_err()
        );
        copy.bodies
            .insert(Default::default(), "changed content".into());
        assert!(
            check_canonical_obligation(&fixture.db, &Stanza::Message(copy), &obligation)
                .await
                .is_err()
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_room_reflection_append_authority() {
    room_authority(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn postgres_room_reflection_append_authority() {
    if let Some(fixture) = IngressFixture::postgres("room_reflection_auth").await {
        room_authority(fixture, false).await;
    }
}

#[tokio::test]
async fn sqlite_archived_room_occupant_and_reflection_append_authority() {
    room_authority(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn postgres_archived_room_occupant_and_reflection_append_authority() {
    if let Some(fixture) = IngressFixture::postgres("archived_room_auth").await {
        room_authority(fixture, true).await;
    }
}
