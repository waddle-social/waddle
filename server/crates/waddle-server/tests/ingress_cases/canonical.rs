use super::*;

async fn accepted_duplicate_rows(fixture: IngressFixture) {
    let original = archive_plan(
        &fixture,
        Some("same-origin"),
        "hello",
        "original-archive-id",
    );
    let first = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("first commit");
    assert_eq!(first.class, IngressDecisionClass::Accepted);
    assert!(first.class.advances());
    let key = first.message_key.expect("canonical key");
    assert_eq!(first.ordinal, None);
    assert_eq!(
        first.archive_ids,
        vec![(
            fixture.principal.bare_jid().clone(),
            StanzaId::new(
                "original-archive-id",
                fixture.principal.bare_jid().clone().into()
            )
        )]
    );
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(fixture.count(table).await, 1, "{table}");
    }
    assert_eq!(fixture.count("ingress_sm_refs").await, 0);
    let mut tx = fixture.uow.begin().await.expect("read envelope");
    assert_eq!(
        CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("load envelope"),
        Some(MessageEnvelope::new(
            original.plan.sanitized_message.clone()
        ))
    );
    tx.commit().await.expect("close read");
    let retry = archive_plan(&fixture, Some("same-origin"), "hello", "discard-this-id");
    let duplicate = commit_submission(&fixture.uow, &retry, 5)
        .await
        .expect("duplicate commit");
    assert_eq!(duplicate.class, IngressDecisionClass::ExistingConsistent);
    assert_eq!(duplicate.message_key, Some(key));
    assert_eq!(duplicate.archive_ids, first.archive_ids);
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(fixture.count(table).await, 1, "duplicate {table}");
    }
    fixture.close().await;
}

#[tokio::test]
async fn ingress_accepted_duplicate_rows_sqlite() {
    accepted_duplicate_rows(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_accepted_duplicate_rows_postgres() {
    if let Some(fixture) = IngressFixture::postgres("accepted_duplicate").await {
        accepted_duplicate_rows(fixture).await;
    }
}

async fn alias_conflict_rejection(fixture: IngressFixture) {
    let first = commit_submission(
        &fixture.uow,
        &archive_plan(&fixture, Some("conflicting-origin"), "original", "original"),
        5,
    )
    .await
    .expect("accept original");
    let offered = archive_plan(
        &fixture,
        Some("conflicting-origin"),
        "different",
        "never-archived",
    );
    let rejected = commit_submission(&fixture.uow, &offered, 5)
        .await
        .expect("commit rejection");
    assert_eq!(rejected.class, IngressDecisionClass::AliasConflict);
    assert!(rejected.class.advances());
    assert_ne!(rejected.message_key, first.message_key);
    assert_eq!(fixture.count("ingress_messages").await, 2);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 1);
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(rejected.external.len(), 1);
    let mut tx = fixture.uow.begin().await.expect("read rejection");
    assert_eq!(
        CanonicalMessageRepository::load_envelope(
            &mut tx,
            rejected.message_key.expect("rejection key")
        )
        .await
        .expect("rejection envelope"),
        Some(MessageEnvelope::new(offered.plan.sanitized_message))
    );
    tx.commit().await.expect("close read");
    fixture.close().await;
}
#[tokio::test]
async fn ingress_alias_conflict_sqlite() {
    alias_conflict_rejection(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_alias_conflict_postgres() {
    if let Some(fixture) = IngressFixture::postgres("alias_conflict").await {
        alias_conflict_rejection(fixture).await;
    }
}

async fn no_origin_distinct_rows(fixture: IngressFixture) {
    let first = commit_submission(
        &fixture.uow,
        &archive_plan(&fixture, None, "same body", "first-no-origin"),
        5,
    )
    .await
    .expect("first no-origin commit");
    let second = commit_submission(
        &fixture.uow,
        &archive_plan(&fixture, None, "same body", "second-no-origin"),
        5,
    )
    .await
    .expect("second no-origin commit");
    assert_eq!(first.class, IngressDecisionClass::Accepted);
    assert_eq!(second.class, IngressDecisionClass::Accepted);
    assert_ne!(first.message_key, second.message_key);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
    for table in [
        "ingress_messages",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(fixture.count(table).await, 2, "no origin {table}");
    }
    fixture.close().await;
}
#[tokio::test]
async fn ingress_no_origin_sqlite() {
    no_origin_distinct_rows(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_no_origin_postgres() {
    if let Some(fixture) = IngressFixture::postgres("no_origin").await {
        no_origin_distinct_rows(fixture).await;
    }
}

async fn missing_archive_repaired(fixture: IngressFixture) {
    let original = archive_plan(
        &fixture,
        Some("repair-origin"),
        "repair body",
        "repair-original",
    );
    let first = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("first commit");
    fixture.execute("DELETE FROM mam_messages", ()).await;
    let repaired = commit_submission(
        &fixture.uow,
        &archive_plan(
            &fixture,
            Some("repair-origin"),
            "repair body",
            "discard-repair-id",
        ),
        5,
    )
    .await
    .expect("repair archive");
    assert_eq!(repaired.message_key, first.message_key);
    assert_eq!(repaired.archive_ids, first.archive_ids);
    assert_eq!(
        fixture
            .count("mam_messages WHERE id = 'repair-original'")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("mam_messages WHERE id = 'discard-repair-id'")
            .await,
        0
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    fixture.close().await;
}
#[tokio::test]
async fn ingress_missing_archive_repair_sqlite() {
    missing_archive_repaired(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_missing_archive_repair_postgres() {
    if let Some(fixture) = IngressFixture::postgres("missing_archive_repair").await {
        missing_archive_repaired(fixture).await;
    }
}

async fn groupchat_alias_conflict_replies_to_authenticated_sender(fixture: IngressFixture) {
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let make_submission = |body: &str| {
        let mut submission = fixture.submission(Some("room-conflict"), body);
        let target = waddle_xmpp::ingress::NormalizedTarget::Bare(room.clone());
        let message = &mut submission.plan.sanitized_message;
        message.to = Some(room.clone().into());
        message.type_ = xmpp_parsers::message::MessageType::Groupchat;
        submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
            message,
            &waddle_xmpp::ingress::DigestContext {
                target: target.clone(),
                server_authorities: vec![submission.principal.bare_jid().clone(), room.clone()],
                stanza_lang: None,
            },
        )
        .expect("groupchat digest");
        submission.target = target;
        // Phase A freezes the canonical room prototype with its occupant address.
        message.from = Some(room.with_resource_str("romeo").expect("occupant").into());
        let stanza_id = StanzaId::new("room-recorded-id", room.clone().into());
        waddle_xmpp_core::xep0359::add_stanza_id(message, &stanza_id);
        submission
            .plan
            .intents
            .push(IngressEffectIntent::ArchiveAuthoritative {
                archive: room.clone(),
                by: room.clone(),
                stanza_id,
                archived_at: chrono::Utc::now(),
            });
        submission
    };
    let original = make_submission("original");
    let accepted = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("first commit");
    assert!(accepted.class.advances());
    let offered = make_submission("different");
    let decision = commit_submission(&fixture.uow, &offered, 5)
        .await
        .expect("conflict commit");
    assert_eq!(decision.class, IngressDecisionClass::AliasConflict);
    let [waddle_server::ingress::effects::ExternalEffect::Frame(stanza)] =
        decision.external.as_slice()
    else {
        panic!("one standard error frame")
    };
    let waddle_xmpp::Stanza::Message(reply) = stanza.as_ref() else {
        panic!("message error")
    };
    assert_eq!(reply.to, Some(offered.sender.clone().into()));
    assert_eq!(reply.from, Some(room.into()));
    let mut tx = fixture.uow.begin().await.expect("read intents");
    let intents = waddle_server::ingress_uow::EffectIntentRepository::load(
        &mut tx,
        decision.message_key.expect("rejection key"),
    )
    .await
    .expect("recorded error intent");
    assert!(
        matches!(intents.as_slice(), [IngressEffectIntent::ErrorReply { recipient, .. }] if recipient == &offered.sender)
    );
    tx.commit().await.expect("finish read");
    fixture.close().await;
}

#[tokio::test]
async fn ingress_groupchat_alias_conflict_sender_sqlite() {
    groupchat_alias_conflict_replies_to_authenticated_sender(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_groupchat_alias_conflict_sender_postgres() {
    if let Some(fixture) = IngressFixture::postgres("groupchat_conflict_sender").await {
        groupchat_alias_conflict_replies_to_authenticated_sender(fixture).await;
    }
}
