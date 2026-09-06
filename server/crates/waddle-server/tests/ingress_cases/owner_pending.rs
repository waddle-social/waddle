use super::*;
use waddle_server::ingress::{
    effects::room::ExternalRoomEffect, ExternalEffect, RoomExecutionPath,
};
use waddle_xmpp::ingress::{DigestContext, DigestInput, NormalizedTarget, RelayTargetIdentity};

fn remote_groupchat(
    fixture: &IngressFixture,
    stanza_lang: Option<xmpp_parsers::message::Lang>,
) -> IngressSubmission {
    use waddle_server::clustering::ordered_relay::{MucProxyOrigin, OrderedRelayMucProxyKind};
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    let mut submission = fixture.submission(Some("owner-pending"), "groupchat");
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone(), room.clone()],
            stanza_lang,
        },
    )
    .expect("groupchat digest");
    let relay_target = RelayTargetIdentity::owner_node("room-owner", "owner-epoch");
    submission.plan.room_execution = RoomExecutionPath::Remote {
        room: room.clone(),
        relay_target: relay_target.clone(),
    };
    submission.plan.intents = vec![IngressEffectIntent::DispatchToRoomRemote {
        room: room.clone(),
        relay_target,
    }];
    let sender_entity = waddle_xmpp::ownership::Entity::new(
        waddle_xmpp::ownership::EntityType::UserActor,
        submission.principal.bare_jid().to_string(),
    );
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(ExternalEffect::Room(
        ExternalRoomEffect::RelayMucProxy {
            admission: None,
            room,
            stanza: Box::new(waddle_xmpp::Stanza::Message(
                submission.plan.sanitized_message.clone(),
            )),
            kind: OrderedRelayMucProxyKind::GroupchatMessage,
            muc_origin: MucProxyOrigin::Server,
            origin: waddle_server::ingress::effects::room::OrderedRelayRouteOrigin {
                kind: waddle_server::ingress::effects::room::OrderedRelayRouteOriginKind::Entity(
                    sender_entity.clone(),
                ),
                sender_entity,
                inbound_sequence: 1,
                handoff: None,
            },
            reflect_replies_to_sender: true,
        },
    )))];
    submission
}

async fn owner_pending_retry(fixture: IngressFixture) {
    let submission = remote_groupchat(&fixture, None);
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("origin commits");
    assert!(first.archive_ids.is_empty());
    let retry = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("retry before owner acceptance");
    assert!(retry.class.advances());
    assert_eq!(retry.message_key, first.message_key);
    assert_eq!(
        retry.external_receipts[0].len(),
        1,
        "owner dispatch must receipt its recorded obligation"
    );
    let ExternalEffect::Room(ExternalRoomEffect::RelayMucProxy {
        admission: Some(admission),
        ..
    }) = &retry.external[0]
    else {
        panic!("retry must carry authorized owner dispatch");
    };
    assert_eq!(Some(admission.canonical.message_key), first.message_key);
    assert_eq!(admission.principal, fixture.principal);
    fixture
        .execute("DELETE FROM ingress_effect_intents", ())
        .await;
    let failure = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect_err("missing room obligation must remain storage failure");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_owner_pending_retry_sqlite() {
    owner_pending_retry(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_owner_pending_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("owner_pending_retry").await {
        owner_pending_retry(fixture).await;
    }
}

async fn relayed_language_matches_canonical(mut fixture: IngressFixture) {
    use waddle_server::clustering::codec::RemoteStanza;
    use waddle_server::clustering::ordered_relay::{
        MucProxyOrigin, OrderedRelayMucProxyKind, OrderedRelayPayload,
    };
    use waddle_server::ingress::{
        effects::room::{DurableRoomEffect, RoomFenceRequirement},
        identity::IngressRelayAdmission,
        IngressStreamIdentity,
    };
    let language = xmpp_parsers::message::Lang("en".to_owned());
    let submission = remote_groupchat(&fixture, Some(language.clone()));
    let NormalizedTarget::Bare(room) = &submission.target else {
        panic!("room target")
    };
    let room = room.clone();
    let room_fence = if fixture.db.driver() == waddle_server::db::DatabaseDriver::Postgres {
        Some(fixture.room_fence(&room).await)
    } else {
        None
    };
    let origin = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("origin commits language");
    let ExternalEffect::Room(ExternalRoomEffect::RelayMucProxy {
        admission: Some(admission),
        ..
    }) = &origin.external[0]
    else {
        panic!("owner admission");
    };
    assert_eq!(admission.stanza_lang, Some(language.clone()));
    let wire = OrderedRelayPayload::MucProxy {
        canonical: Some(admission.canonical.clone()),
        principal: Some(admission.principal.clone()),
        stanza_lang: admission.stanza_lang.clone(),
        room_jid: room.clone(),
        kind: OrderedRelayMucProxyKind::GroupchatMessage,
        origin: MucProxyOrigin::Server,
        stanza: RemoteStanza(waddle_xmpp::Stanza::Message(
            submission.plan.sanitized_message.clone(),
        )),
    };
    let decoded: OrderedRelayPayload =
        serde_json::from_slice(&serde_json::to_vec(&wire).expect("encode proxy"))
            .expect("decode proxy");
    let OrderedRelayPayload::MucProxy {
        canonical,
        principal,
        stanza_lang,
        stanza,
        ..
    } = decoded
    else {
        panic!("proxy")
    };
    let admission = IngressRelayAdmission::from_parts(canonical, principal, stanza_lang)
        .expect("complete admission");
    let waddle_xmpp::Stanza::Message(message) = stanza.0 else {
        panic!("message")
    };
    let mut owner = submission.clone();
    owner.digest_input = waddle_server::ingress::submission::digest_input(
        &message,
        &DigestContext {
            target: owner.target.clone(),
            server_authorities: vec![owner.principal.bare_jid().clone(), room.clone()],
            stanza_lang: admission.stanza_lang,
        },
    )
    .expect("owner digest preserves language");
    assert_eq!(
        waddle_xmpp::ingress::digest::v1::digest(&owner.digest_input),
        waddle_xmpp::ingress::digest::v1::digest(&submission.digest_input)
    );
    let digest_without_language = waddle_server::ingress::submission::digest_input(
        &message,
        &DigestContext {
            target: owner.target.clone(),
            server_authorities: vec![owner.principal.bare_jid().clone(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("missing language digest");
    assert_ne!(
        waddle_xmpp::ingress::digest::v1::digest(&owner.digest_input),
        waddle_xmpp::ingress::digest::v1::digest(&digest_without_language)
    );
    assert_eq!(Some(admission.canonical.message_key), origin.message_key);
    let Some(room_fence) = room_fence else {
        // SQLite exercises the shared wire/digest binding against canonical
        // authority without manufacturing distributed room-claim behavior.
        let replay = commit_submission(&fixture.uow, &owner, 5)
            .await
            .expect("decoded language preserves origin retry identity");
        assert!(replay.class.advances());
        assert_eq!(replay.message_key, origin.message_key);
        assert_eq!(fixture.count("ingress_messages").await, 1);
        fixture.close().await;
        return;
    };
    owner.identity = IngressStreamIdentity::Relayed {
        canonical: admission.canonical,
        room: room.clone(),
        room_fence: room_fence.clone(),
    };
    let stanza_id = StanzaId::new("room-language-id", room.clone().into());
    let mut archived = ArchivedMessage::for_test(owner.sender.clone().into(), room.clone().into());
    archived.id = stanza_id.id.clone();
    archived.stanza_id = Some(stanza_id.clone());
    archived.message_type = xmpp_parsers::message::MessageType::Groupchat;
    owner.plan.intents = vec![IngressEffectIntent::ArchiveAuthoritative {
        archive: room.clone(),
        by: room.clone(),
        stanza_id: stanza_id.clone(),
        archived_at: archived.timestamp,
    }];
    owner.plan.plan = vec![PlannedEffect::new(Effect::Durable(DurableEffect::Room(
        DurableRoomEffect::ArchiveGroupchat {
            room: room.clone(),
            message: Box::new(archived),
            fence: RoomFenceRequirement::Guarded(room_fence),
            archive_expectation: ArchiveExpectation::Fresh,
        },
    )))];
    owner.plan.room_execution = RoomExecutionPath::None;
    let mut lost_language = owner.clone();
    lost_language.digest_input = digest_without_language;
    let failure = commit_submission(&fixture.uow, &lost_language, 5)
        .await
        .expect_err("dropping language contradicts canonical identity");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    let accepted = commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner accepts explicit language");
    assert_eq!(accepted.class, IngressDecisionClass::OwnerFirstAcceptance);
    assert_eq!(accepted.message_key, origin.message_key);
    assert!(accepted.archive_ids.contains(&(room, stanza_id)));
    fixture.close().await;
}

#[tokio::test]
async fn ingress_relayed_language_sqlite() {
    relayed_language_matches_canonical(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_relayed_language_postgres() {
    if let Some(fixture) = IngressFixture::postgres("relayed_language").await {
        relayed_language_matches_canonical(fixture).await;
    }
}
