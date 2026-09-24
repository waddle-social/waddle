//! A room-authored reflection carries its own exact-resource receipt.
use super::*;
use crate::ingress::{
    commit::commit_submission, identity::IngressAppendObligationRef, test_support::IngressFixture,
};
use waddle_xmpp::ingress::{EffectMessageIdentity, EntityGeneration, IngressEffectIntent};

async fn reflection_authority(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("reflection-authority"), "frozen reflection");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let target = submission.sender.clone();
    let stamp = waddle_xmpp_core::xep0359::StanzaId::new("room-stamp", room.clone().into());
    let route = IngressEffectIntent::RouteMucGroupchat {
        room: room.clone(),
        occupants: vec![target.clone()],
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
    submission.plan.intents = vec![route, reflection.clone()];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let obligation = IngressAppendObligationRef {
        archive_positions: Vec::new(),
        dispatch_stream: None,
        message_key: decision.message_key.expect("key"),
        sender_bare: room,
        receipt: crate::ingress::receipt_key(&reflection).expect("receipt"),
        received_at: None,
    };
    let stanza = Stanza::Message(message.clone());
    check_stanza_binding(
        &stanza,
        &obligation.sender_bare,
        obligation.receipt.kind.to_storage(),
    )
    .expect("room authors the reflection");
    check_canonical_obligation(&fixture.db, &stanza, &obligation)
        .await
        .expect("original sender identity does not reject the room-authored reflection");
    let mut sibling = message.clone();
    sibling.to = Some(
        target
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
    message
        .bodies
        .insert(Default::default(), "changed content".into());
    assert!(
        check_canonical_obligation(&fixture.db, &Stanza::Message(message), &obligation)
            .await
            .is_err()
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_room_reflection_append_authority() {
    reflection_authority(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_room_reflection_append_authority() {
    if let Some(fixture) = IngressFixture::postgres("room_reflection_auth").await {
        reflection_authority(fixture).await;
    }
}
