use super::*;

async fn resumable_wire_binding(fixture: IngressFixture) {
    use waddle_server::{
        ingress::IngressStreamIdentity,
        ingress_uow::{SmIngressRepository, SmIngressStreamRepository},
    };
    use waddle_xmpp::{
        ingress::{IngressOrdinal, WireHandledCount},
        pending_delivery::SmSessionId,
    };
    let stream_id = SmSessionId::new("authority-wire-stream");
    let mut tx = fixture.uow.begin().await.expect("enroll stream");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("mint stream");
    tx.commit().await.expect("commit stream");
    let mut submission = archive_plan(
        &fixture,
        Some("wire-origin"),
        "same position",
        "wire-archive",
    );
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused-single-node", "unused-epoch"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::from_storage(7),
        checkpoint_h: WireHandledCount::from_storage(3),
    };
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit bound position");
    assert_eq!(first.class, IngressDecisionClass::Accepted);
    assert_eq!(first.ordinal, Some(IngressOrdinal::FIRST));
    let retry = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("retry same wire position");
    assert_eq!(retry.class, IngressDecisionClass::ExistingCommitted);
    assert_eq!(retry.message_key, first.message_key);
    assert_eq!(retry.ordinal, first.ordinal);
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_sm_refs",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(fixture.count(table).await, 1, "bound retry {table}");
    }
    let mut denied_replay = submission.clone();
    let error = xmpp_parsers::stanza_error::StanzaError::new(
        xmpp_parsers::stanza_error::ErrorType::Auth,
        xmpp_parsers::stanza_error::DefinedCondition::NotAuthorized,
        "en",
        "policy changed after commit",
    );
    let mut reply = denied_replay.plan.sanitized_message.clone();
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.to = reply.from.take();
    reply.payloads.push(error.clone().into());
    denied_replay.plan.error_reply = Some(waddle_xmpp::Stanza::Message(reply));
    denied_replay
        .plan
        .intents
        .push(IngressEffectIntent::ErrorReply {
            recipient: "romeo@example.com/phone".parse().expect("recipient"),
            error: waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&error).expect("error"),
        });
    let accepted_replay = commit_submission(&fixture.uow, &denied_replay, 5)
        .await
        .expect("committed acceptance survives policy denial");
    assert_eq!(
        accepted_replay.class,
        IngressDecisionClass::ExistingCommitted
    );
    assert_eq!(accepted_replay.message_key, first.message_key);
    assert_eq!(accepted_replay.ordinal, first.ordinal);
    assert!(accepted_replay.external.is_empty());
    assert_eq!(fixture.count("ingress_effect_intents").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    let mut changed = archive_plan(
        &fixture,
        Some("wire-origin"),
        "different offered body",
        "must-not-replace-wire-archive",
    );
    changed.identity = submission.identity.clone();
    let failure = commit_submission(&fixture.uow, &changed, 5)
        .await
        .expect_err("retained wire position cannot identify different content");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_sm_refs",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(
            fixture.count(table).await,
            1,
            "different wire content rollback {table}"
        );
    }
    let mut tx = fixture.uow.begin().await.expect("read binding");
    assert_eq!(
        SmIngressRepository::lookup_wire_binding(
            &mut tx,
            sm_ingress_id,
            WireHandledCount::from_storage(7)
        )
        .await
        .expect("wire binding"),
        Some((first.message_key.expect("key"), IngressOrdinal::FIRST))
    );
    assert_eq!(
        SmIngressStreamRepository::load_stream_checkpoint(&mut tx, sm_ingress_id)
            .await
            .expect("checkpoint"),
        Some(WireHandledCount::from_storage(3))
    );
    tx.commit().await.expect("close read");
    fixture.close().await;
}
#[tokio::test]
async fn ingress_wire_binding_sqlite() {
    resumable_wire_binding(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_wire_binding_postgres() {
    if let Some(fixture) = IngressFixture::postgres("wire_binding").await {
        resumable_wire_binding(fixture).await;
    }
}

async fn committed_rejection_wire_replay(fixture: IngressFixture) {
    use waddle_server::{
        ingress::IngressStreamIdentity,
        ingress_uow::{EffectIntentRepository, SmIngressStreamRepository},
    };
    use waddle_xmpp::{
        ingress::{FrozenStanzaError, IngressOrdinal, WireHandledCount},
        pending_delivery::SmSessionId,
        Stanza,
    };
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};
    let stream_id = SmSessionId::new("rejection-wire-stream");
    let mut tx = fixture.uow.begin().await.expect("enroll rejection stream");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("mint rejection stream");
    tx.commit().await.expect("commit stream");
    let mut rejected = archive_plan(
        &fixture,
        Some("denied-wire-origin"),
        "offered body",
        "must-not-archive",
    );
    rejected.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "unused"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::from_storage(4),
        checkpoint_h: WireHandledCount::from_storage(4),
    };
    let error = StanzaError::new(
        ErrorType::Auth,
        DefinedCondition::NotAuthorized,
        "en",
        "original denial",
    );
    let frozen = FrozenStanzaError::from_xmpp(&error).expect("freeze denial");
    let mut reply = rejected.plan.sanitized_message.clone();
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.to = reply.from.take();
    reply.payloads.push(error.into());
    rejected.plan.error_reply = Some(Stanza::Message(reply));
    rejected.plan.intents.push(IngressEffectIntent::ErrorReply {
        recipient: "romeo@example.com/phone".parse().expect("recipient"),
        error: frozen.clone(),
    });
    let first = commit_submission(&fixture.uow, &rejected, 5)
        .await
        .expect("commit rejection position");
    assert_eq!(first.class, IngressDecisionClass::AuthorizationDenied);
    let mut now_allowed = archive_plan(
        &fixture,
        Some("denied-wire-origin"),
        "offered body",
        "new-policy-archive",
    );
    now_allowed.identity = rejected.identity;
    let replay = commit_submission(&fixture.uow, &now_allowed, 5)
        .await
        .expect("replay committed rejection");
    assert_eq!(replay.class, IngressDecisionClass::ExistingCommitted);
    assert_eq!(replay.message_key, first.message_key);
    assert_eq!(replay.ordinal, Some(IngressOrdinal::FIRST));
    assert_eq!(replay.external.len(), 1);
    assert!(replay.archive_ids.is_empty());
    let waddle_server::ingress::ExternalEffect::Frame(frame) = &replay.external[0] else {
        panic!("committed denial must reply with a frame");
    };
    let Stanza::Message(reply) = frame.as_ref() else {
        panic!("denial must remain a message error");
    };
    assert_eq!(reply.bodies, rejected.plan.sanitized_message.bodies);
    assert_eq!(reply.to, rejected.plan.sanitized_message.from);
    assert!(reply.payloads.iter().any(|payload| {
        StanzaError::try_from(payload.clone())
            .ok()
            .and_then(|error| FrozenStanzaError::from_xmpp(&error).ok())
            .as_ref()
            == Some(&frozen)
    }));

    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
    assert_eq!(fixture.count("ingress_sm_refs").await, 1);
    assert_eq!(fixture.count("ingress_effect_intents").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("mam_messages").await, 0);
    let mut tx = fixture.uow.begin().await.expect("read committed rejection");
    let key = first.message_key.expect("rejected key");
    let intents = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("load rejection intent");
    assert!(
        matches!(intents.as_slice(), [IngressEffectIntent::ErrorReply { error, .. }] if error == &frozen)
    );
    assert_eq!(
        CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("load offered envelope"),
        Some(
            MessageEnvelope::new(rejected.plan.sanitized_message)
                .expect("original offered envelope")
        )
    );
    tx.commit().await.expect("close read");
    fixture.close().await;
}
#[tokio::test]
async fn ingress_committed_rejection_wire_replay_sqlite() {
    committed_rejection_wire_replay(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_committed_rejection_wire_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("committed_rejection_wire_replay").await {
        committed_rejection_wire_replay(fixture).await;
    }
}

async fn ordinal_collision_rolls_back(fixture: IngressFixture) {
    use waddle_server::{
        ingress::IngressStreamIdentity,
        ingress_uow::{SmIngressRepository, SmIngressStreamRepository},
    };
    use waddle_xmpp::{
        ingress::{IngressOrdinal, MessageKey, WireHandledCount},
        pending_delivery::SmSessionId,
    };
    let stream_id = SmSessionId::new("ordinal-conflict-stream");
    let previous = fixture.submission(None, "previous position");
    let key = MessageKey::new();
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("seed inconsistent ordinal");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("mint stream");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &waddle_xmpp::ingress::digest::v1::digest(&previous.digest_input),
        Some(&MessageEnvelope::new(previous.plan.sanitized_message).expect("previous envelope")),
    )
    .await
    .expect("seed previous canonical");
    SmIngressRepository::insert_sm_ref(
        &mut tx,
        sm_ingress_id,
        IngressOrdinal::FIRST,
        WireHandledCount::from_storage(99),
        key,
    )
    .await
    .expect("seed retained ordinal");
    tx.commit().await.expect("commit retained reference");
    let mut submission = archive_plan(
        &fixture,
        Some("ordinal-conflict-origin"),
        "new position",
        "rolled-back-archive",
    );
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "unused"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::from_storage(1),
        checkpoint_h: WireHandledCount::from_storage(1),
    };
    let failure = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect_err("ordinal already belongs to previous key");
    assert_eq!(failure.class(), IngressDecisionClass::SmOrdinalConflict);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_sm_refs").await, 1);
    for table in [
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "mam_messages",
    ] {
        assert_eq!(fixture.count(table).await, 0, "ordinal rollback {table}");
    }
    fixture.close().await;
}
#[tokio::test]
async fn ingress_ordinal_collision_sqlite() {
    ordinal_collision_rolls_back(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_ordinal_collision_postgres() {
    if let Some(fixture) = IngressFixture::postgres("ordinal_collision").await {
        ordinal_collision_rolls_back(fixture).await;
    }
}
