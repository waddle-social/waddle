use super::*;

async fn principal_missing_rolls_back(fixture: IngressFixture) {
    let submission = archive_plan(
        &fixture,
        Some("missing-principal"),
        "hello",
        "never-written",
    );
    fixture.execute("DELETE FROM sessions", ()).await;
    let failure = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect_err("principal must fail closed");
    assert_eq!(failure.class(), IngressDecisionClass::PrincipalMissing);
    assert!(!failure.class().advances());
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_sm_refs",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(fixture.count(table).await, 0, "rolled back {table}");
    }
    fixture.close().await;
}
#[tokio::test]
async fn ingress_principal_missing_sqlite() {
    principal_missing_rolls_back(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_principal_missing_postgres() {
    if let Some(fixture) = IngressFixture::postgres("principal_missing").await {
        principal_missing_rolls_back(fixture).await;
    }
}

async fn lineage_loss(fixture: IngressFixture) {
    let submission = archive_plan(
        &fixture,
        Some("lineage-origin"),
        "uncommitted",
        "never-written",
    );
    fixture.execute("DELETE FROM _lineage", ()).await;
    let failure = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect_err("lineage loss");
    assert_eq!(failure.class(), IngressDecisionClass::Lineage);
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    fixture.close().await;
}
#[tokio::test]
async fn ingress_lineage_loss_sqlite() {
    lineage_loss(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_lineage_loss_postgres() {
    if let Some(fixture) = IngressFixture::postgres("lineage_loss").await {
        lineage_loss(fixture).await;
    }
}

async fn relayed_owner_acceptance(mut fixture: IngressFixture) {
    use waddle_server::ingress::{
        effects::room::{DurableRoomEffect, RoomFenceRequirement},
        IngressCanonicalRef, IngressStreamIdentity,
    };
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    #[cfg(feature = "clustering")]
    let room_fence = fixture.room_fence(&room).await;
    #[cfg(not(feature = "clustering"))]
    let _ = &mut fixture;
    let mut proxy = archive_plan(
        &fixture,
        Some("owner-origin"),
        "room body",
        "sender-authority",
    );
    proxy.target = waddle_xmpp::ingress::NormalizedTarget::Bare(room.clone());
    proxy.plan.sanitized_message.to = Some(room.clone().into());
    proxy.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    proxy.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        &proxy.plan.sanitized_message,
        &waddle_xmpp::ingress::DigestContext {
            target: proxy.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("room-shaped digest");
    for effect in &mut proxy.plan.plan {
        if let Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
            message,
            ..
        })) = &mut effect.effect
        {
            message.to = room.clone().into();
            message.message_type = xmpp_parsers::message::MessageType::Groupchat;
        }
    }
    let proxy_decision = commit_submission(&fixture.uow, &proxy, 5)
        .await
        .expect("proxy acceptance");
    let mut owner = proxy.clone();
    for intent in &mut owner.plan.intents {
        if let IngressEffectIntent::ArchiveAuthoritative {
            archive,
            by,
            stanza_id,
            ..
        } = intent
        {
            *archive = room.clone();
            *by = room.clone();
            stanza_id.by = room.clone().into();
            stanza_id.id = "room-authority".to_owned();
        }
    }
    for effect in &mut owner.plan.plan {
        if let Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
            message,
            ..
        })) = &effect.effect
        {
            let mut message = message.clone();
            message.id = "room-authority".to_owned();
            message.to = room.clone().into();
            message.message_type = xmpp_parsers::message::MessageType::Groupchat;
            message.stanza_id = Some(StanzaId::new("room-authority", room.clone().into()));
            #[cfg(feature = "clustering")]
            let fence = RoomFenceRequirement::Guarded(room_fence.clone());
            #[cfg(not(feature = "clustering"))]
            let fence = RoomFenceRequirement::Unfenced;
            effect.effect =
                Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
                    room: room.clone(),
                    message,
                    fence,
                    archive_expectation: ArchiveExpectation::Fresh,
                }));
        }
    }
    owner.identity = IngressStreamIdentity::Relayed {
        canonical: IngressCanonicalRef {
            message_key: proxy_decision.message_key.expect("proxy canonical"),
            sender_bare: fixture.principal.bare_jid().clone(),
            origin_id: proxy.digest_input.origin().cloned(),
        },
        room: room.clone(),
        #[cfg(feature = "clustering")]
        room_fence,
    };
    let mut wrong_content = owner.clone();
    wrong_content.plan.sanitized_message.bodies.insert(
        xmpp_parsers::message::Lang::default(),
        "content from another canonical message".into(),
    );
    wrong_content.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        &wrong_content.plan.sanitized_message,
        &waddle_xmpp::ingress::DigestContext {
            target: wrong_content.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("different owner content digest");
    let failure = commit_submission(&fixture.uow, &wrong_content, 5)
        .await
        .expect_err("relayed key cannot be attached to different content");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(fixture.count(table).await, 1, "wrong owner content {table}");
    }
    let mut tx = fixture.uow.begin().await.expect("original proxy envelope");
    assert_eq!(
        CanonicalMessageRepository::load_envelope(
            &mut tx,
            proxy_decision.message_key.expect("proxy key"),
        )
        .await
        .expect("load proxy envelope"),
        Some(MessageEnvelope::new(proxy.plan.sanitized_message.clone()).expect("proxy envelope")),
    );
    tx.commit().await.expect("close envelope read");
    let first = commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner first acceptance");
    assert_eq!(first.class, IngressDecisionClass::OwnerFirstAcceptance);
    let duplicate = commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner duplicate");
    assert_eq!(duplicate.class, IngressDecisionClass::OwnerDuplicate);
    assert_eq!(duplicate.message_key, proxy_decision.message_key);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("ingress_sm_refs").await, 0);
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert_eq!(fixture.count("mam_messages").await, 2);
    #[cfg(feature = "clustering")]
    {
        fixture
            .execute(
                "UPDATE clustering_claims SET claim_epoch = claim_epoch + 1",
                (),
            )
            .await;
        let failure = commit_submission(&fixture.uow, &owner, 5)
            .await
            .expect_err("deposed owner");
        assert_eq!(failure.class(), IngressDecisionClass::ClaimFenceMissing);
        assert_eq!(fixture.count("ingress_messages").await, 1);
        assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    }
    fixture.close().await;
}
#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn ingress_owner_acceptance_sqlite() {
    relayed_owner_acceptance(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_owner_acceptance_postgres() {
    if let Some(fixture) = IngressFixture::postgres("owner_acceptance").await {
        relayed_owner_acceptance(fixture).await;
    }
}

async fn unsupported_epoch(fixture: IngressFixture) {
    // The Postgres activation guard admits exactly one +1 step per
    // transaction, so walk 0 -> 1 -> 2 instead of jumping straight past the
    // supported epoch.
    let sql = match fixture.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres => "UPDATE ingress_protocol_epoch SET epoch = epoch + 1, activated_at = ?::timestamptz, lineage_uuid = ?::uuid WHERE id = 1",
        waddle_server::db::DatabaseDriver::Sqlite => "UPDATE ingress_protocol_epoch SET epoch = epoch + 1, activated_at = ?, lineage_uuid = ? WHERE id = 1",
    };
    for _ in 0..2 {
        fixture
            .execute(
                sql,
                waddle_server::db_params![
                    chrono::Utc::now().to_rfc3339(),
                    uuid::Uuid::new_v4().to_string()
                ],
            )
            .await;
    }
    let failure = commit_submission(&fixture.uow, &fixture.submission(None, "not committed"), 5)
        .await
        .expect_err("unsupported epoch");
    assert_eq!(failure.class(), IngressDecisionClass::EpochUnsupported);
    assert_eq!(fixture.count("ingress_messages").await, 0);
    fixture.close().await;
}
#[tokio::test]
async fn ingress_epoch_unsupported_sqlite() {
    unsupported_epoch(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_epoch_unsupported_postgres() {
    if let Some(fixture) = IngressFixture::postgres("epoch_unsupported").await {
        unsupported_epoch(fixture).await;
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_room_generation_stale_postgres() {
    use waddle_server::ingress::{effects::room::RoomFenceRequirement, RoomExecutionPath};
    let Some(mut fixture) = IngressFixture::postgres("room_generation_stale").await else {
        return;
    };
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    let mut fence = fixture.room_fence(&room).await;
    fence.entity = waddle_xmpp::ownership::Entity::new(
        waddle_xmpp::ownership::EntityType::RoomActor,
        "other@conference.example.com",
    );
    let mut submission = archive_plan(
        &fixture,
        Some("stale-room-origin"),
        "not committed",
        "stale-room-archive",
    );
    submission.plan.room_execution = RoomExecutionPath::Local {
        room,
        fence: RoomFenceRequirement::Guarded(fence),
        snapshot_generation: 17,
    };
    let failure = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect_err("snapshot fence belongs to another room");
    assert_eq!(failure.class(), IngressDecisionClass::RoomGenerationStale);
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(fixture.count(table).await, 0, "stale room rollback {table}");
    }
    fixture.close().await;
}
