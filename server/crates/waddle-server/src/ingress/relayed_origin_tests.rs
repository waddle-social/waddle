use super::commit::commit_submission;
use super::test_support::IngressFixture;
use super::{IngressCanonicalRef, IngressStreamIdentity, IngressSubmission};
use crate::ingress_substrate::IngressSubstrateError;
use crate::ingress_uow::IngressUowError;
use crate::server::routes::interpret::effects::{
    AuthorizationDeniedReason, PlanRejection, RoomExecutionPath,
};
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, FrozenStanzaError, FrozenStanzaErrorType, IngressEffectIntent,
    MessageKey, NormalizedTarget, RelayTargetIdentity,
};

fn groupchat(mut submission: IngressSubmission, room: &jid::BareJid) -> IngressSubmission {
    let message = &mut submission.plan.sanitized_message;
    message.to = Some(room.clone().into());
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.digest_input = DigestInput::from_parsed(
        message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![submission.sender.to_bare(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("groupchat digest");
    let relay_target = RelayTargetIdentity::owner_node("first-owner", "first-incarnation");
    submission.plan.intents = vec![IngressEffectIntent::DispatchToRoomRemote {
        room: room.clone(),
        relay_target: relay_target.clone(),
    }];
    submission.plan.room_execution = RoomExecutionPath::Remote {
        room: room.clone(),
        relay_target,
    };
    submission
}

fn deny(submission: &mut IngressSubmission) {
    let error = FrozenStanzaError::new(
        FrozenStanzaErrorType::Cancel,
        waddle_xmpp::StanzaErrorCondition::Forbidden,
    );
    let mut reply = submission.plan.sanitized_message.clone();
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.payloads.push(error.to_xmpp().into());
    submission.plan.rejection = Some(PlanRejection::AuthorizationDenied(
        AuthorizationDeniedReason::BlockedSender,
    ));
    submission.plan.error_reply = Some(waddle_xmpp::Stanza::Message(reply));
    submission.plan.intents = vec![IngressEffectIntent::ErrorReply {
        recipient: submission.sender.clone(),
        error,
    }];
}

async fn counts(fixture: &IngressFixture) -> Vec<i64> {
    let mut values = Vec::new();
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
    ] {
        values.push(fixture.count(table).await);
    }
    values
}

async fn relayed_alias_guard(fixture: IngressFixture) {
    #[cfg(feature = "clustering")]
    let mut fixture = fixture;
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let first = groupchat(fixture.submission(Some("first"), "same body"), &room);
    let key = commit_submission(&fixture.uow, &first, 1)
        .await
        .expect("origin acceptance")
        .message_key
        .expect("first key");
    let second = groupchat(fixture.submission(Some("second"), "same body"), &room);
    assert_eq!(
        waddle_xmpp::ingress::digest::v1::digest(&first.digest_input),
        waddle_xmpp::ingress::digest::v1::digest(&second.digest_input),
        "origin IDs are not digest material"
    );
    let other_key = commit_submission(&fixture.uow, &second, 1)
        .await
        .expect("second origin acceptance")
        .message_key
        .expect("second key");
    assert_ne!(key, other_key);

    #[cfg(feature = "clustering")]
    let room_fence = fixture.room_fence(&room).await;
    let mut relay = first.clone();
    relay.identity = IngressStreamIdentity::Relayed {
        canonical: IngressCanonicalRef {
            message_key: key,
            sender_bare: relay.sender.to_bare(),
            origin_id: relay.digest_input.origin().cloned(),
        },
        room: room.clone(),
        #[cfg(feature = "clustering")]
        room_fence,
    };
    relay.plan.intents.clear();
    relay.plan.room_execution = RoomExecutionPath::None;
    let baseline = counts(&fixture).await;

    let mut failures = Vec::new();
    let mut wrong_parent = relay.clone();
    if let IngressStreamIdentity::Relayed { canonical, .. } = &mut wrong_parent.identity {
        canonical.message_key = other_key;
    }
    deny(&mut wrong_parent);
    failures.push(wrong_parent);

    let mut missing_parent = relay.clone();
    if let IngressStreamIdentity::Relayed { canonical, .. } = &mut missing_parent.identity {
        canonical.message_key = MessageKey::new();
    }
    deny(&mut missing_parent);
    failures.push(missing_parent);

    let mut missing_alias = relay.clone();
    missing_alias.digest_input =
        groupchat(fixture.submission(Some("missing"), "same body"), &room).digest_input;
    if let IngressStreamIdentity::Relayed { canonical, .. } = &mut missing_alias.identity {
        canonical.origin_id = missing_alias.digest_input.origin().cloned();
    }
    deny(&mut missing_alias);
    failures.push(missing_alias);

    let mut changed_content = relay.clone();
    changed_content.digest_input =
        groupchat(fixture.submission(Some("first"), "changed body"), &room).digest_input;
    failures.push(changed_content);

    let mut changed_target = relay.clone();
    changed_target.target = NormalizedTarget::Bare("other@muc.example.com".parse().expect("room"));
    failures.push(changed_target);

    let mut changed_sender = relay.clone();
    changed_sender.sender = "other@example.com/phone".parse().expect("sender");
    failures.push(changed_sender);

    for submission in failures {
        let failure = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect_err("unproven relayed origin");
        assert!(!failure.class().advances());
        assert!(matches!(
            failure.source,
            IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
        ));
        assert_eq!(
            counts(&fixture).await,
            baseline,
            "failed guard writes nothing"
        );
    }

    // A hash match cannot substitute for the persisted, typed identity tuple.
    let digest = waddle_xmpp::ingress::digest::v1::digest(&relay.digest_input);
    for (column, poisoned) in [
        ("sender_bare_jid", "other@example.com"),
        ("target_jid", "other@muc.example.com"),
        ("origin_id", "other-origin"),
    ] {
        let mut tx = fixture.uow.begin().await.expect("poison transaction");
        let predicate = match fixture.db.driver() {
            crate::db::DatabaseDriver::Postgres => "message_key = ?::uuid",
            crate::db::DatabaseDriver::Sqlite => "message_key = ?",
        };
        tx.transaction_mut()
            .execute(
                &format!("UPDATE ingress_origin_aliases SET {column} = ? WHERE {predicate}"),
                crate::db_params![poisoned, key.to_storage().to_string()],
            )
            .await
            .expect("poison one stored alias component");
        let error = crate::ingress_uow::CanonicalMessageRepository::assert_relayed_alias(
            &mut tx,
            key,
            &relay.sender.to_bare(),
            &relay.target,
            relay.digest_input.origin().expect("origin"),
            &digest,
        )
        .await
        .expect_err("corrupt tuple must not be accepted");
        assert!(matches!(
            error,
            IngressUowError::Substrate(IngressSubstrateError::AliasIdentityConflict)
        ));
        drop(tx);
        assert_eq!(counts(&fixture).await, baseline, "poison rolled back");
    }

    // Preserve the pre-existing owner-first behavior. This does not establish
    // that an origin dispatch is an owner acceptance or solve owner denial.
    deny(&mut relay);
    let replay = commit_submission(&fixture.uow, &relay, 1)
        .await
        .expect("matching relayed alias");
    assert_eq!(replay.message_key, Some(key));
    assert!(replay.class.advances());
    assert_ne!(
        replay.class,
        super::IngressDecisionClass::AuthorizationDenied
    );
    assert_eq!(fixture.count("ingress_messages").await, baseline[0]);
    assert_eq!(fixture.count("ingress_origin_aliases").await, baseline[1]);

    #[cfg(feature = "clustering")]
    {
        use crate::ingress_uow::IngressFencing;
        use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};

        let IngressFencing::Clustered(identity) = fixture.uow.fencing() else {
            panic!("clustered room fixture");
        };
        let identity = identity.clone();
        let successor = NodeIdentity::new("successor", "successor-incarnation");
        fixture
            .execute(
                "UPDATE clustering_claims SET node_id = ?, node_epoch = ?, claim_epoch = ? WHERE entity = ?",
                crate::db_params![
                    successor.node_id.clone(),
                    successor.node_epoch.clone(),
                    18_i64,
                    format!("room_actor:{room}"),
                ],
            )
            .await;
        identity.rotate(successor.clone()).await;
        let stale = commit_submission(&fixture.uow, &relay, 1)
            .await
            .expect_err("old room claim must fail");
        assert!(!stale.class().advances());
        assert_eq!(fixture.count("ingress_messages").await, baseline[0]);
        if let IngressStreamIdentity::Relayed { room_fence, .. } = &mut relay.identity {
            room_fence.owner = successor;
            room_fence.epoch = ClaimEpoch(18);
        }
        let successor_replay = commit_submission(&fixture.uow, &relay, 1)
            .await
            .expect("successor uses the same origin alias");
        assert_eq!(successor_replay.message_key, Some(key));
        assert!(successor_replay.class.advances());
        assert_eq!(fixture.count("ingress_messages").await, baseline[0]);
        assert_eq!(fixture.count("ingress_origin_aliases").await, baseline[1]);
    }
    fixture.close().await;
}

#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn sqlite_relayed_origin_requires_exact_persisted_alias() {
    relayed_alias_guard(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_relayed_origin_requires_exact_persisted_alias() {
    if let Some(fixture) = IngressFixture::postgres("relay_alias_guard").await {
        relayed_alias_guard(fixture).await;
    }
}

#[tokio::test]
async fn postgres_relayed_alias_lock_orders_parent_before_alias() {
    use crate::ingress_uow::{CanonicalMessageRepository, DbRetryClass};

    let Some(fixture) = IngressFixture::postgres("relay_alias_lock").await else {
        return;
    };
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let submission = groupchat(fixture.submission(Some("locked"), "body"), &room);
    let key = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("origin acceptance")
        .message_key
        .expect("key");
    let digest = waddle_xmpp::ingress::digest::v1::digest(&submission.digest_input);
    let origin = submission.digest_input.origin().expect("origin");

    let mut first = fixture.uow.begin().await.expect("first transaction");
    CanonicalMessageRepository::assert_relayed_alias(
        &mut first,
        key,
        &submission.sender.to_bare(),
        &submission.target,
        origin,
        &digest,
    )
    .await
    .expect("lock exact alias");
    let mut contender = fixture.db.begin().await.expect("contender transaction");
    contender
        .execute("SET LOCAL lock_timeout = '100ms'", ())
        .await
        .expect("bounded lock wait");
    let blocked = contender
        .execute(
            "UPDATE ingress_origin_aliases SET origin_id = origin_id WHERE message_key = ?::uuid",
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .expect_err("an alias update must wait for the proof transaction");
    assert!(crate::ingress_uow::is_database_timeout(&blocked));
    drop(contender);
    first.commit().await.expect("release proof locks");

    let mut blocker = fixture.db.begin().await.expect("opposite lock order");
    let mut rows = blocker
        .query(
            "SELECT 1 FROM ingress_origin_aliases WHERE message_key = ?::uuid FOR UPDATE",
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .expect("lock alias first");
    assert!(rows.next().await.expect("alias row").is_some());
    drop(rows);
    let mut attempt = fixture.uow.begin().await.expect("proof attempt");
    let error = CanonicalMessageRepository::assert_relayed_alias(
        &mut attempt,
        key,
        &submission.sender.to_bare(),
        &submission.target,
        origin,
        &digest,
    )
    .await
    .expect_err("alias lock must not wait while parent is held");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::Database {
            retry_class: DbRetryClass::CanonicalLockContention,
        })
    ));
    drop(attempt);
    blocker.commit().await.expect("release inverse lock");

    let mut retry = fixture.uow.begin().await.expect("fresh retry");
    CanonicalMessageRepository::assert_relayed_alias(
        &mut retry,
        key,
        &submission.sender.to_bare(),
        &submission.target,
        origin,
        &digest,
    )
    .await
    .expect("retry succeeds after contention");
    retry.commit().await.expect("retry commit");
    fixture.close().await;
}
