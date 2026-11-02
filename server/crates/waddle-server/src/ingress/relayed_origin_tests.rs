use super::commit::commit_submission;
use super::test_support::IngressFixture;
use super::{IngressCanonicalRef, IngressStreamIdentity, IngressSubmission};
use crate::ingress_substrate::{CanonicalSourceBinding, IngressSubstrateError, MessageEnvelope};
use crate::ingress_uow::{
    CanonicalMessageRepository, ClosedRoomOwnerResultOutcome, ClosedRoomOwnerResultProposal,
    ClosedRoomOwnerResultSource, EffectIntentRepository, EffectReceiptRepository, IngressUowError,
    ReconcileVerdict, RoomOwnerDecision, SmIngressRepository, SmIngressStreamRepository,
};
use crate::server::routes::interpret::effects::{
    AuthorizationDeniedReason, PlanRejection, RoomExecutionPath,
};
use waddle_xmpp::ingress::{
    DeliveryKey, DigestContext, DigestInput, FrozenStanzaError, FrozenStanzaErrorType,
    IngressEffectIntent, IngressEffectKind, IngressOrdinal, MessageKey, NormalizedTarget,
    RelayTargetIdentity, WireHandledCount,
};
use waddle_xmpp::pending_delivery::SmSessionId;

#[cfg(not(feature = "clustering"))]
mod synthetic_alias;

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

fn replan_for_successor(
    mut submission: IngressSubmission,
    room: &jid::BareJid,
) -> IngressSubmission {
    let relay_target = RelayTargetIdentity::owner_node("successor", "successor-incarnation");
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

fn closed_owner_route(room: &jid::BareJid) -> IngressEffectIntent {
    IngressEffectIntent::RouteMucGroupchat {
        room: room.clone(),
        occupants: Vec::new(),
        reflection: "room@muc.example.com/__system__"
            .parse()
            .expect("room reflection resource"),
        room_generation: waddle_xmpp::ingress::EntityGeneration::INITIAL,
        route_identity: waddle_xmpp::ingress::EffectMessageIdentity::capture_ordinal(1),
    }
}

fn forbidden_owner_error() -> FrozenStanzaError {
    FrozenStanzaError::new(
        FrozenStanzaErrorType::Cancel,
        waddle_xmpp::StanzaErrorCondition::Forbidden,
    )
}

async fn recorded_intents(fixture: &IngressFixture, key: MessageKey) -> Vec<IngressEffectIntent> {
    let mut tx = fixture.uow.begin().await.expect("read recorded intents");
    let intents = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("load intents");
    tx.commit().await.expect("finish intent read");
    intents
}

async fn seed_dark_source(
    fixture: &IngressFixture,
    submission: &IngressSubmission,
    record_dispatch: bool,
) -> MessageKey {
    let super::IngressPrincipal::Authenticated(principal) = &submission.principal else {
        panic!("dark source test needs an authenticated principal");
    };
    let binding = CanonicalSourceBinding::new(principal.clone(), None);
    seed_dark_source_with_binding(fixture, submission, &binding, record_dispatch).await
}

async fn seed_dark_source_with_binding(
    fixture: &IngressFixture,
    submission: &IngressSubmission,
    binding: &CanonicalSourceBinding,
    record_dispatch: bool,
) -> MessageKey {
    let super::IngressPrincipal::Authenticated(principal) = &submission.principal else {
        panic!("dark source test needs an authenticated principal");
    };
    assert_eq!(binding.principal(), principal, "bind the offered principal");
    let key = MessageKey::new();
    let digest = waddle_xmpp::ingress::digest::v1::digest(&submission.digest_input);
    let envelope = MessageEnvelope::new(submission.plan.sanitized_message.clone());
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("begin dark source transaction");
    if let Some(origin) = submission.digest_input.origin() {
        let resolved = CanonicalMessageRepository::resolve_and_record_alias_with_source(
            &mut tx,
            &submission.sender.to_bare(),
            &submission.target,
            origin,
            &digest,
            binding,
            || key,
        )
        .await
        .expect("insert source alias with its immutable principal");
        assert!(matches!(
            resolved,
            waddle_xmpp::ingress::AliasResolution::Aliased(
                waddle_xmpp::ingress::AliasOutcome::Inserted(inserted)
            ) if inserted == key
        ));
    }
    CanonicalMessageRepository::record_message_with_source(
        &mut tx,
        key,
        &digest,
        Some(&envelope),
        binding,
    )
    .await
    .expect("record bound source payload");
    let intents = if record_dispatch {
        submission.plan.intents.as_slice()
    } else {
        &[]
    };
    EffectIntentRepository::reconcile(&mut tx, key, intents, false)
        .await
        .expect("record source effects");
    if record_dispatch {
        let room = match intents.first() {
            Some(IngressEffectIntent::DispatchToRoomRemote { room, .. }) => room,
            _ => panic!("source dispatch is required"),
        };
        CanonicalMessageRepository::initialize_room_owner_decision(&mut tx, key, room)
            .await
            .expect("initialize the owner decision latch");
    }
    if let Some((sm_ingress_id, ordinal)) = binding.sm_occurrence() {
        SmIngressRepository::insert_sm_ref(
            &mut tx,
            sm_ingress_id,
            ordinal,
            WireHandledCount::from_storage(ordinal.wire_h()),
            key,
        )
        .await
        .expect("bind first source SM occurrence");
    }
    tx.commit().await.expect("commit dark source transaction");
    key
}

#[derive(Debug, PartialEq, Eq)]
struct SourceBindingRow {
    version: Option<i64>,
    bare: Option<String>,
    context: Option<String>,
    context_version: Option<String>,
    auth_epoch: Option<String>,
    sm_ingress_id: Option<String>,
    ingress_ordinal: Option<String>,
    owner_decision: Option<i64>,
}

async fn read_dark_source_binding(fixture: &IngressFixture, key: MessageKey) -> SourceBindingRow {
    let connection = fixture.db.guard().await.expect("source binding connection");
    let mut rows = connection
        .query(
            "SELECT source_binding_version, source_bare_jid, source_auth_context_id, source_auth_context_version, source_auth_epoch, source_sm_ingress_id, source_ingress_ordinal, owner_decision FROM ingress_messages WHERE message_key = ?",
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .expect("read source binding");
    let row = rows
        .next()
        .await
        .expect("source row result")
        .expect("source row");
    SourceBindingRow {
        version: row.get(0).expect("binding version"),
        bare: row.get(1).expect("source bare JID"),
        context: row.get(2).expect("auth context"),
        context_version: row.get(3).expect("auth context version"),
        auth_epoch: row.get(4).expect("auth epoch"),
        sm_ingress_id: row.get(5).expect("SM ingress ID"),
        ingress_ordinal: row.get(6).expect("ingress ordinal"),
        owner_decision: row.get(7).expect("owner decision"),
    }
}

fn assert_owner_specific_dispatch_keys(
    key: MessageKey,
    first: &IngressSubmission,
    replanned: &IngressSubmission,
) {
    let first_effect = first.plan.intents[0].semantic_key();
    let replanned_effect = replanned.plan.intents[0].semantic_key();
    assert_eq!(
        first.plan.intents[0].authority_key(),
        replanned.plan.intents[0].authority_key(),
        "both proposals address the same room authority"
    );
    assert_ne!(first_effect, replanned_effect);
    assert_ne!(
        DeliveryKey::effect(key, &first_effect),
        DeliveryKey::effect(key, &replanned_effect),
        "a delivery key derived from the route changes with its owner"
    );
}

async fn origin_dispatch_replan(fixture: IngressFixture) {
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let mut message_keys = Vec::new();
    let mut effects = Vec::new();
    let mut digests = Vec::new();
    for origin in ["first-submission", "second-submission"] {
        let first = groupchat(fixture.submission(Some(origin), "same body"), &room);
        let replanned = replan_for_successor(first.clone(), &room);
        let digest = waddle_xmpp::ingress::digest::v1::digest(&first.digest_input);
        let accepted = commit_submission(&fixture.uow, &first, 1)
            .await
            .expect("origin records dispatch");
        assert_eq!(accepted.class, super::IngressDecisionClass::Accepted);
        assert_eq!(accepted.alias, super::AliasOutcomeClass::Inserted);
        let key = accepted.message_key.expect("canonical message");
        assert_owner_specific_dispatch_keys(key, &first, &replanned);

        let retry = commit_submission(&fixture.uow, &replanned, 1)
            .await
            .expect("origin retry resolves alias");
        assert_eq!(retry.message_key, Some(key));
        assert_eq!(retry.alias, super::AliasOutcomeClass::Existing);
        assert_eq!(retry.class, super::IngressDecisionClass::ExistingDivergent);
        assert!(matches!(
            retry.verdict,
            Some(ReconcileVerdict::Divergent { ref kinds })
                if kinds.contains(&IngressEffectKind::DispatchToRoomRemote)
        ));
        assert_eq!(recorded_intents(&fixture, key).await, first.plan.intents);

        let mut tx = fixture.uow.begin().await.expect("verify origin alias");
        CanonicalMessageRepository::assert_relayed_alias(
            &mut tx,
            key,
            &first.sender.to_bare(),
            &first.target,
            first.digest_input.origin().expect("origin id"),
            &digest,
        )
        .await
        .expect("original alias still binds the canonical message");
        tx.commit().await.expect("finish alias read");
        message_keys.push(key);
        effects.push(first.plan.intents[0].semantic_key());
        digests.push(digest);
    }
    assert_eq!(
        digests[0], digests[1],
        "origin IDs do not affect the digest"
    );
    assert_eq!(effects[0], effects[1]);
    assert_ne!(message_keys[0], message_keys[1]);
    assert_ne!(
        DeliveryKey::effect(message_keys[0], &effects[0]),
        DeliveryKey::effect(message_keys[1], &effects[1]),
        "distinct submissions retain separate delivery identities"
    );
    assert_eq!(fixture.count("ingress_messages").await, 2);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 2);
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    fixture.close().await;
}

async fn sm_dispatch_replan_without_origin(fixture: IngressFixture) {
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let stream_id = SmSessionId::new("remote-room-no-origin");
    let mut tx = fixture.uow.begin().await.expect("enroll SM stream");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("mint SM stream");
    tx.commit().await.expect("commit SM stream");

    let mut first = groupchat(fixture.submission(None, "same body"), &room);
    assert!(first.digest_input.origin().is_none());
    first.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "unused"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::from_storage(7),
        checkpoint_h: WireHandledCount::from_storage(3),
    };
    let replanned = replan_for_successor(first.clone(), &room);
    let accepted = commit_submission(&fixture.uow, &first, 1)
        .await
        .expect("origin records SM dispatch");
    assert_eq!(accepted.class, super::IngressDecisionClass::Accepted);
    assert_eq!(accepted.alias, super::AliasOutcomeClass::NoOrigin);
    let key = accepted.message_key.expect("SM canonical message");
    assert_owner_specific_dispatch_keys(key, &first, &replanned);
    let retry = commit_submission(&fixture.uow, &replanned, 1)
        .await
        .expect("bound wire position retries");
    assert_eq!(retry.class, super::IngressDecisionClass::ExistingCommitted);
    assert_eq!(retry.message_key, Some(key));
    assert_eq!(retry.alias, super::AliasOutcomeClass::Existing);
    assert_eq!(retry.ordinal, accepted.ordinal);
    assert!(matches!(
        retry.verdict,
        Some(ReconcileVerdict::Divergent { ref kinds })
            if kinds.contains(&IngressEffectKind::DispatchToRoomRemote)
    ));
    assert_eq!(recorded_intents(&fixture, key).await, first.plan.intents);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
    let mut tx = fixture.uow.begin().await.expect("read SM binding");
    assert_eq!(
        SmIngressRepository::lookup_wire_binding(
            &mut tx,
            sm_ingress_id,
            WireHandledCount::from_storage(7),
        )
        .await
        .expect("wire binding"),
        Some((key, accepted.ordinal.expect("committed SM ordinal")))
    );
    tx.commit().await.expect("finish binding read");

    let mut next_submission = first.clone();
    let IngressStreamIdentity::Resumable {
        reserved_wire_position,
        ..
    } = &mut next_submission.identity
    else {
        panic!("expected SM submission");
    };
    *reserved_wire_position = WireHandledCount::from_storage(8);
    let next = commit_submission(&fixture.uow, &next_submission, 1)
        .await
        .expect("next wire position commits independently");
    assert_eq!(next.class, super::IngressDecisionClass::Accepted);
    let next_key = next.message_key.expect("next canonical message");
    assert_ne!(next_key, key);
    assert_ne!(next.ordinal, accepted.ordinal);
    assert_ne!(
        DeliveryKey::effect(key, &first.plan.intents[0].semantic_key()),
        DeliveryKey::effect(next_key, &next_submission.plan.intents[0].semantic_key())
    );
    assert_eq!(fixture.count("ingress_messages").await, 2);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 0);
    assert_eq!(fixture.count("ingress_sm_refs").await, 2);
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_origin_dispatch_replan_retains_alias_and_recorded_owner() {
    origin_dispatch_replan(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_origin_dispatch_replan_retains_alias_and_recorded_owner() {
    if let Some(fixture) = IngressFixture::postgres("origin_dispatch_replan").await {
        origin_dispatch_replan(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_sm_dispatch_replan_without_origin_id() {
    sm_dispatch_replan_without_origin(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_sm_dispatch_replan_without_origin_id() {
    if let Some(fixture) = IngressFixture::postgres("sm_dispatch_replan").await {
        sm_dispatch_replan_without_origin(fixture).await;
    }
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

    // This synthetic relayed PlanRejection has no external frame. It exposes
    // the commit decision for that input, not the real owner planner's output.
    deny(&mut relay);
    let replay = commit_submission(&fixture.uow, &relay, 1)
        .await
        .expect("matching relayed alias");
    assert_eq!(replay.message_key, Some(key));
    assert_eq!(
        replay.class,
        super::IngressDecisionClass::OwnerFirstAcceptance
    );
    assert_eq!(recorded_intents(&fixture, key).await, first.plan.intents);
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

#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn sqlite_dark_source_binding_replays_one_immutable_owner_result() {
    let fixture = IngressFixture::sqlite().await;
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");

    let denied_source = groupchat(fixture.submission(None, "denied"), &room);
    let denied_key = seed_dark_source(&fixture, &denied_source, true).await;
    let denied_digest = waddle_xmpp::ingress::digest::v1::digest(&denied_source.digest_input);
    let super::IngressPrincipal::Authenticated(denied_principal) = &denied_source.principal else {
        panic!("authenticated source");
    };
    let denied_binding = CanonicalSourceBinding::new(denied_principal.clone(), None);
    let before = read_dark_source_binding(&fixture, denied_key).await;
    assert_eq!(before.version, Some(1));
    assert_eq!(before.bare.as_deref(), Some("romeo@example.com"));
    assert!(before.context.is_some());
    assert!(before.context_version.is_some());
    assert!(before.auth_epoch.is_some());
    assert_eq!(before.sm_ingress_id, None, "ephemeral source has no SM ID");
    assert_eq!(
        before.ingress_ordinal, None,
        "ephemeral source has no ordinal"
    );
    assert_eq!(before.owner_decision, Some(0));

    let mut tx = fixture.uow.begin().await.expect("read undecided source");
    let undecided = CanonicalMessageRepository::assert_relayed_source(
        &mut tx,
        denied_key,
        &denied_source.sender,
        &room,
        &denied_digest,
        &denied_binding,
        None,
    )
    .await
    .expect("source must have its exact room dispatch");
    assert_eq!(undecided.decision, RoomOwnerDecision::Undecided);
    assert_eq!(undecided.rejection_child, None);
    tx.commit().await.expect("finish source proof");

    // The remote dispatch may already have completed before the owner result.
    // Preserve that state to prove the rejection latch restarts its source tail.
    let now = chrono::Utc::now();
    let old = now - crate::ingress_substrate::ALIAS_RETENTION - chrono::Duration::days(1);
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("settle source dispatch");
    tx.execute("INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash) SELECT message_key, kind, semantic_identity_hash FROM ingress_effect_intents WHERE message_key = ?", crate::db_params![denied_key.to_storage().to_string()]).await.expect("dispatch receipt");
    tx.execute("UPDATE ingress_messages SET terminal_at = ?, retention_eligible_at = ? WHERE message_key = ?", crate::db_params![old.to_rfc3339(), old.to_rfc3339(), denied_key.to_storage().to_string()]).await.expect("terminal source");
    tx.commit().await.expect("commit settled source");

    let mut denial = denied_source.clone();
    deny(&mut denial);
    let child = MessageKey::new();
    let child_envelope = MessageEnvelope::new(denial.plan.sanitized_message.clone());
    let mut tx = fixture.uow.begin().await.expect("record owner denial");
    let pre_child = CanonicalMessageRepository::assert_relayed_source(
        &mut tx,
        denied_key,
        &denied_source.sender,
        &room,
        &denied_digest,
        &denied_binding,
        None,
    )
    .await
    .expect("lock undecided source before creating child");
    assert_eq!(pre_child.decision, RoomOwnerDecision::Undecided);
    CanonicalMessageRepository::record_message(
        &mut tx,
        child,
        &denied_digest,
        Some(&child_envelope),
    )
    .await
    .expect("record rejection child");
    EffectIntentRepository::reconcile(&mut tx, child, &denial.plan.intents, false)
        .await
        .expect("record child error reply");
    CanonicalMessageRepository::record_owner_rejection(&mut tx, denied_key, child)
        .await
        .expect("bind source to one rejection child");
    CanonicalMessageRepository::decide_room_owner(&mut tx, denied_key, RoomOwnerDecision::Rejected)
        .await
        .expect("latch rejection");
    tx.commit().await.expect("commit owner rejection");

    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("settle rejection child");
    let mut rows = tx
        .query(
            "SELECT retention_eligible_at FROM ingress_messages WHERE message_key = ?",
            crate::db_params![denied_key.to_storage().to_string()],
        )
        .await
        .expect("source tail");
    let tail: String = rows
        .next()
        .await
        .expect("tail row")
        .expect("source exists")
        .get(0)
        .expect("owner result restarted source tail");
    let tail = chrono::DateTime::parse_from_rfc3339(&tail)
        .expect("valid source tail")
        .with_timezone(&chrono::Utc);
    assert!(tail >= now - chrono::Duration::seconds(1));
    drop(rows);
    tx.execute("INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash) SELECT message_key, kind, semantic_identity_hash FROM ingress_effect_intents WHERE message_key = ?", crate::db_params![child.to_storage().to_string()]).await.expect("child receipt");
    tx.execute("UPDATE ingress_messages SET terminal_at = ?, retention_eligible_at = ? WHERE message_key = ?", crate::db_params![old.to_rfc3339(), old.to_rfc3339(), child.to_storage().to_string()]).await.expect("expire child tail");
    tx.commit().await.expect("commit expired child");
    let collected = crate::ingress_substrate::gc_expired_aliases(
        &fixture.db,
        now,
        crate::ingress_substrate::AliasGcBudget {
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            lock_timeout: std::time::Duration::from_secs(1),
            statement_timeout: std::time::Duration::from_secs(2),
            scan_timeout: std::time::Duration::from_secs(2),
            progress: crate::ingress_substrate::AliasGcProgress::default(),
        },
    )
    .await
    .expect("collect after child's standalone tail");
    assert_eq!(
        collected.deleted_messages, 0,
        "the replayable source protects its child"
    );

    let mut tx = fixture.uow.begin().await.expect("replay rejection mapping");
    let replay = CanonicalMessageRepository::assert_relayed_source(
        &mut tx,
        denied_key,
        &denied_source.sender,
        &room,
        &denied_digest,
        &denied_binding,
        None,
    )
    .await
    .expect("same source finds its committed rejection");
    assert_eq!(replay.decision, RoomOwnerDecision::Rejected);
    assert_eq!(replay.rejection_child, Some(child));
    tx.commit().await.expect("finish rejection replay");

    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("attempt to replace denial");
    let error = CanonicalMessageRepository::decide_room_owner(
        &mut tx,
        denied_key,
        RoomOwnerDecision::Accepted,
    )
    .await
    .expect_err("a later acceptance cannot replace the rejection");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);
    assert_eq!(
        read_dark_source_binding(&fixture, denied_key)
            .await
            .owner_decision,
        Some(2)
    );

    let accepted_source = groupchat(fixture.submission(Some("dark-accepted"), "accepted"), &room);
    let accepted_key = seed_dark_source(&fixture, &accepted_source, true).await;
    let accepted_binding_before = read_dark_source_binding(&fixture, accepted_key).await;
    let accepted_digest = waddle_xmpp::ingress::digest::v1::digest(&accepted_source.digest_input);
    let super::IngressPrincipal::Authenticated(accepted_principal) = &accepted_source.principal
    else {
        panic!("authenticated source");
    };
    let binding = CanonicalSourceBinding::new(accepted_principal.clone(), None);
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("replay bound source alias");
    let replay = CanonicalMessageRepository::resolve_and_record_alias_with_source(
        &mut tx,
        &accepted_source.sender.to_bare(),
        &accepted_source.target,
        accepted_source
            .digest_input
            .origin()
            .expect("source origin"),
        &accepted_digest,
        &binding,
        MessageKey::new,
    )
    .await
    .expect("replay the stamped alias");
    assert!(matches!(
        replay,
        waddle_xmpp::ingress::AliasResolution::Aliased(
            waddle_xmpp::ingress::AliasOutcome::Existing(replayed)
        ) if replayed == accepted_key
    ));
    tx.commit().await.expect("finish source alias replay");
    assert_eq!(
        read_dark_source_binding(&fixture, accepted_key).await,
        accepted_binding_before,
        "alias replay preserves the original typed source tuple"
    );
    let mut tx = fixture.uow.begin().await.expect("record owner acceptance");
    CanonicalMessageRepository::decide_room_owner(
        &mut tx,
        accepted_key,
        RoomOwnerDecision::Accepted,
    )
    .await
    .expect("latch acceptance");
    tx.commit().await.expect("commit owner acceptance");
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("attempt to replace acceptance");
    let error = CanonicalMessageRepository::decide_room_owner(
        &mut tx,
        accepted_key,
        RoomOwnerDecision::Rejected,
    )
    .await
    .expect_err("a later rejection cannot replace the acceptance");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);
    assert_eq!(
        read_dark_source_binding(&fixture, accepted_key)
            .await
            .owner_decision,
        Some(1)
    );
    let late_mapping = MessageKey::new();
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("attempt rejection after acceptance");
    let error =
        CanonicalMessageRepository::record_owner_rejection(&mut tx, accepted_key, late_mapping)
            .await
            .expect_err("repository refuses a rejection child after acceptance");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);

    let raw_child = MessageKey::new();
    let child_envelope = MessageEnvelope::new(accepted_source.plan.sanitized_message.clone());
    let mut tx = fixture.uow.begin().await.expect("record raw trigger child");
    CanonicalMessageRepository::record_message(
        &mut tx,
        raw_child,
        &accepted_digest,
        Some(&child_envelope),
    )
    .await
    .expect("raw mapping child row");
    tx.commit().await.expect("commit raw trigger child");
    let connection = fixture.db.guard().await.expect("owner guard connection");
    assert!(
        connection
            .execute(
                "INSERT INTO ingress_owner_rejections (source_message_key, child_message_key) VALUES (?, ?)",
                crate::db_params![
                    accepted_key.to_storage().to_string(),
                    raw_child.to_storage().to_string()
                ],
            )
            .await
            .is_err(),
        "the database also refuses a late rejection mapping"
    );
    assert_eq!(fixture.count("ingress_owner_rejections").await, 1);
    fixture.close().await;
}

#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn sqlite_closed_owner_result_replays_latched_acceptance_before_replanning() {
    let fixture = IngressFixture::sqlite().await;
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    // Originless ephemeral sources still have an exact canonical key and
    // source principal; no client origin ID is needed for the owner latch.
    let source = groupchat(fixture.submission(None, "accepted once"), &room);
    let source_key = seed_dark_source(&fixture, &source, true).await;
    let digest = waddle_xmpp::ingress::digest::v1::digest(&source.digest_input);
    let super::IngressPrincipal::Authenticated(principal) = &source.principal else {
        panic!("authenticated source");
    };
    let binding = CanonicalSourceBinding::new(principal.clone(), None);
    let route = closed_owner_route(&room);

    let mut tx = fixture.uow.begin().await.expect("first owner acceptance");
    let accepted: ClosedRoomOwnerResultOutcome =
        CanonicalMessageRepository::apply_closed_room_owner_result(
            &mut tx,
            ClosedRoomOwnerResultSource::new(
                source_key,
                &source.sender,
                &room,
                &digest,
                &binding,
                None,
            ),
            ClosedRoomOwnerResultProposal::Accepted {
                owner_effects: vec![route.clone()],
            },
        )
        .await
        .expect("record exact-room accepted result");
    assert_eq!(accepted.decision, RoomOwnerDecision::Accepted);
    assert_eq!(accepted.rejection_child, None);
    assert_eq!(accepted.pending_effect_receipts.len(), 1);
    let receipt = &accepted.pending_effect_receipts[0];
    assert!(
        !EffectReceiptRepository::contains(
            &mut tx,
            source_key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("check uncompleted route receipt"),
        "recording the owner intent does not certify external delivery"
    );
    tx.commit().await.expect("commit accepted owner result");

    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("replay with changed policy");
    let replay = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            source_key,
            &source.sender,
            &room,
            &digest,
            &binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Rejected {
            error: forbidden_owner_error(),
        },
    )
    .await
    .expect("latched acceptance precedes changed rejection plan");
    assert_eq!(replay, accepted);
    tx.commit().await.expect("finish accepted replay");
    let recorded = recorded_intents(&fixture, source_key).await;
    assert!(recorded.contains(&route));
    assert_eq!(fixture.count("ingress_owner_rejections").await, 0);
    assert_eq!(
        read_dark_source_binding(&fixture, source_key)
            .await
            .owner_decision,
        Some(1)
    );
    fixture.close().await;
}

#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn sqlite_closed_owner_result_rejects_missing_or_wrong_room_dispatch() {
    let fixture = IngressFixture::sqlite().await;
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let source = groupchat(fixture.submission(None, "unroutable"), &room);
    let source_key = seed_dark_source(&fixture, &source, false).await;
    let digest = waddle_xmpp::ingress::digest::v1::digest(&source.digest_input);
    let super::IngressPrincipal::Authenticated(principal) = &source.principal else {
        panic!("authenticated source");
    };
    let binding = CanonicalSourceBinding::new(principal.clone(), None);
    let mut tx = fixture.uow.begin().await.expect("missing source dispatch");
    let error = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            source_key,
            &source.sender,
            &room,
            &digest,
            &binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Accepted {
            owner_effects: vec![closed_owner_route(&room)],
        },
    )
    .await
    .expect_err("no committed DispatchToRoomRemote means no owner result");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);

    let routed_key = seed_dark_source(&fixture, &source, true).await;
    let other_room: jid::BareJid = "other@muc.example.com".parse().expect("other room");
    let mut tx = fixture.uow.begin().await.expect("wrong owner effect room");
    let error = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            routed_key,
            &source.sender,
            &room,
            &digest,
            &binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Accepted {
            owner_effects: vec![closed_owner_route(&other_room)],
        },
    )
    .await
    .expect_err("owner effects must target the committed room");
    assert!(
        matches!(error, IngressUowError::EffectIntentConflict),
        "unexpected conflict error: {error:?}"
    );
    drop(tx);
    assert_eq!(
        read_dark_source_binding(&fixture, routed_key)
            .await
            .owner_decision,
        Some(0)
    );

    let mut resource_source = groupchat(fixture.submission(None, "resource target"), &room);
    let resource_target: jid::FullJid = "room@muc.example.com/nick"
        .parse()
        .expect("resource target");
    resource_source.plan.sanitized_message.to = Some(resource_target.clone().into());
    resource_source.target = NormalizedTarget::Full(resource_target);
    resource_source.digest_input = DigestInput::from_parsed(
        &resource_source.plan.sanitized_message,
        &DigestContext {
            target: resource_source.target.clone(),
            server_authorities: vec![resource_source.sender.to_bare(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest resource-addressed groupchat");
    let resource_key = seed_dark_source(&fixture, &resource_source, true).await;
    let resource_digest = waddle_xmpp::ingress::digest::v1::digest(&resource_source.digest_input);
    let super::IngressPrincipal::Authenticated(principal) = &resource_source.principal else {
        panic!("authenticated principal");
    };
    let resource_binding = CanonicalSourceBinding::new(principal.clone(), None);
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("resource-addressed source");
    let error = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            resource_key,
            &resource_source.sender,
            &room,
            &resource_digest,
            &resource_binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Accepted {
            owner_effects: vec![closed_owner_route(&room)],
        },
    )
    .await
    .expect_err("room owner source must be addressed to the bare room");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);

    let conflicting_key = seed_dark_source(&fixture, &source, true).await;
    let conflicting_effect = closed_owner_route(&other_room);
    let mut stored = recorded_intents(&fixture, conflicting_key).await;
    stored.push(conflicting_effect);
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("record conflicting existing room route");
    EffectIntentRepository::reconcile(&mut tx, conflicting_key, &stored, false)
        .await
        .expect("preserve the conflicting intent for the negative case");
    tx.commit()
        .await
        .expect("commit conflicting existing route");

    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("reject conflicting stored owner effect");
    let error = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            conflicting_key,
            &source.sender,
            &room,
            &digest,
            &binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Accepted {
            owner_effects: vec![closed_owner_route(&room)],
        },
    )
    .await
    .expect_err("a different preexisting owner effect must not be latched");
    assert!(
        matches!(error, IngressUowError::EffectIntentConflict),
        "unexpected preexisting-effect error: {error:?}"
    );
    drop(tx);
    assert_eq!(
        read_dark_source_binding(&fixture, conflicting_key)
            .await
            .owner_decision,
        Some(0)
    );
    let messages_before_rejection = fixture.count("ingress_messages").await;
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("reject preexisting owner effect before child creation");
    let error = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            conflicting_key,
            &source.sender,
            &room,
            &digest,
            &binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Rejected {
            error: forbidden_owner_error(),
        },
    )
    .await
    .expect_err("a stored room route must not coexist with a rejection");
    assert!(
        matches!(error, IngressUowError::EffectIntentConflict),
        "unexpected rejected-effect error: {error:?}"
    );
    drop(tx);
    assert_eq!(
        fixture.count("ingress_messages").await,
        messages_before_rejection
    );
    assert_eq!(fixture.count("ingress_owner_rejections").await, 0);
    assert_eq!(
        read_dark_source_binding(&fixture, conflicting_key)
            .await
            .owner_decision,
        Some(0)
    );
    fixture.close().await;
}

#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn sqlite_closed_owner_rejection_binds_child_and_receipt_identity_atomically() {
    let fixture = IngressFixture::sqlite().await;
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let source = groupchat(
        fixture.submission(Some("closed-owner-rejection"), "denied once"),
        &room,
    );
    let first_stream = SmSessionId::new("closed-owner-first-stream");
    let later_stream = SmSessionId::new("closed-owner-later-stream");
    let mut tx = fixture.uow.begin().await.expect("enroll source streams");
    let first_sm = SmIngressStreamRepository::mint(&mut tx, &first_stream)
        .await
        .expect("first stream identity");
    let later_sm = SmIngressStreamRepository::mint(&mut tx, &later_stream)
        .await
        .expect("later stream identity");
    tx.commit().await.expect("commit stream identities");
    let super::IngressPrincipal::Authenticated(principal) = &source.principal else {
        panic!("authenticated source");
    };
    let first_binding =
        CanonicalSourceBinding::new(principal.clone(), Some((first_sm, IngressOrdinal::FIRST)));
    let source_key = seed_dark_source_with_binding(&fixture, &source, &first_binding, true).await;
    let digest = waddle_xmpp::ingress::digest::v1::digest(&source.digest_input);

    // A later authenticated alias retransmission binds its own SM reference;
    // it cannot replace the immutable first occurrence stored on the source.
    let later_binding =
        CanonicalSourceBinding::new(principal.clone(), Some((later_sm, IngressOrdinal::FIRST)));
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("resolve later source occurrence");
    let replay = CanonicalMessageRepository::resolve_and_record_alias_with_source(
        &mut tx,
        &source.sender.to_bare(),
        &source.target,
        source.digest_input.origin().expect("source origin"),
        &digest,
        &later_binding,
        MessageKey::new,
    )
    .await
    .expect("resolve the existing source alias");
    assert!(matches!(
        replay,
        waddle_xmpp::ingress::AliasResolution::Aliased(
            waddle_xmpp::ingress::AliasOutcome::Existing(key)
        ) if key == source_key
    ));
    SmIngressRepository::insert_sm_ref(
        &mut tx,
        later_sm,
        IngressOrdinal::FIRST,
        WireHandledCount::from_storage(1),
        source_key,
    )
    .await
    .expect("bind later occurrence to canonical key");
    tx.commit().await.expect("commit later source occurrence");

    let wrong_stream_binding =
        CanonicalSourceBinding::new(principal.clone(), Some((later_sm, IngressOrdinal::FIRST)));
    let mut tx = fixture.uow.begin().await.expect("forged source stream");
    let error = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            source_key,
            &source.sender,
            &room,
            &digest,
            &wrong_stream_binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Rejected {
            error: forbidden_owner_error(),
        },
    )
    .await
    .expect_err("a later alias occurrence is not the immutable first source stream");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);

    let forged_sender: jid::FullJid = "mercutio@example.com/tablet"
        .parse()
        .expect("forged sender");
    let mut tx = fixture.uow.begin().await.expect("forged source sender");
    let error = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            source_key,
            &forged_sender,
            &room,
            &digest,
            &first_binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Rejected {
            error: forbidden_owner_error(),
        },
    )
    .await
    .expect_err("a different authenticated sender cannot resolve this source");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);

    let messages_before = fixture.count("ingress_messages").await;
    let mut tx = fixture.uow.begin().await.expect("stage owner rejection");
    let staged = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            source_key,
            &source.sender,
            &room,
            &digest,
            &first_binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Rejected {
            error: forbidden_owner_error(),
        },
    )
    .await
    .expect("stage one rejection child and latch");
    assert_eq!(staged.decision, RoomOwnerDecision::Rejected);
    let child = staged.rejection_child.expect("one rejection child");
    assert_eq!(staged.pending_effect_receipts.len(), 1);
    let receipt = &staged.pending_effect_receipts[0];
    assert!(
        !EffectReceiptRepository::contains(
            &mut tx,
            child,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("check child receipt before delivery"),
        "the UOW returns the receipt identity but does not claim the frame was delivered"
    );
    drop(tx);
    assert_eq!(fixture.count("ingress_messages").await, messages_before);
    assert_eq!(fixture.count("ingress_owner_rejections").await, 0);
    assert_eq!(
        read_dark_source_binding(&fixture, source_key)
            .await
            .owner_decision,
        Some(0)
    );

    let mut tx = fixture.uow.begin().await.expect("retry owner rejection");
    let rejected = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            source_key,
            &source.sender,
            &room,
            &digest,
            &first_binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Rejected {
            error: forbidden_owner_error(),
        },
    )
    .await
    .expect("commit one rejection child");
    assert_eq!(rejected.decision, RoomOwnerDecision::Rejected);
    let committed_child = rejected.rejection_child.expect("committed child");
    assert_ne!(
        committed_child, child,
        "rolled-back child key was not reused"
    );
    let rejected_receipt = rejected
        .pending_effect_receipts
        .first()
        .expect("pending ErrorReply receipt");
    tx.commit().await.expect("commit rejected owner result");
    assert_eq!(fixture.count("ingress_owner_rejections").await, 1);

    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("replay rejected owner result");
    let replay = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            source_key,
            &source.sender,
            &room,
            &digest,
            &first_binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Accepted {
            owner_effects: vec![closed_owner_route(&room)],
        },
    )
    .await
    .expect("replay rejection before changed acceptance plan");
    assert_eq!(replay, rejected);
    assert_eq!(replay.rejection_child, Some(committed_child));
    tx.commit().await.expect("finish rejected replay");

    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("check receipt remains pending");
    assert!(
        !EffectReceiptRepository::contains(
            &mut tx,
            committed_child,
            rejected_receipt.kind,
            &rejected_receipt.semantic_identity_hash,
        )
        .await
        .expect("read pending receipt"),
        "replay preserves intent and receipt identity without certifying delivery"
    );
    tx.commit().await.expect("finish pending receipt check");
    assert_eq!(fixture.count("ingress_owner_rejections").await, 1);
    let stored = read_dark_source_binding(&fixture, source_key).await;
    assert_eq!(
        stored.sm_ingress_id.as_deref(),
        Some(first_sm.to_storage().to_string().as_str())
    );
    assert_eq!(stored.ingress_ordinal.as_deref(), Some("1"));
    assert_eq!(stored.owner_decision, Some(2));
    let mut tx = fixture.uow.begin().await.expect("verify both occurrences");
    assert_eq!(
        SmIngressRepository::lookup(&mut tx, first_sm, IngressOrdinal::FIRST)
            .await
            .expect("first occurrence ref"),
        Some(source_key)
    );
    assert_eq!(
        SmIngressRepository::lookup(&mut tx, later_sm, IngressOrdinal::FIRST)
            .await
            .expect("later occurrence ref"),
        Some(source_key)
    );
    tx.commit().await.expect("finish occurrence checks");
    fixture.close().await;
}

#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn postgres_closed_owner_result_serializes_acceptance_and_rejection() {
    let Some(fixture) = IngressFixture::postgres("closed_owner_result_serializes").await else {
        return;
    };
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let source = groupchat(
        fixture.submission(Some("closed-owner-pg-race"), "race once"),
        &room,
    );
    let source_key = seed_dark_source(&fixture, &source, true).await;
    let digest = waddle_xmpp::ingress::digest::v1::digest(&source.digest_input);
    let super::IngressPrincipal::Authenticated(principal) = &source.principal else {
        panic!("authenticated source");
    };
    let binding = CanonicalSourceBinding::new(principal.clone(), None);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let accept_uow = fixture.uow.clone();
    let reject_uow = fixture.uow.clone();
    let accept_barrier = barrier.clone();
    let reject_barrier = barrier;
    let accept_source = source.clone();
    let reject_source = source.clone();
    let accept_binding = binding.clone();
    let reject_binding = binding;
    let accept_effect = closed_owner_route(&room);
    let accept = async {
        let mut tx = accept_uow
            .begin()
            .await
            .expect("begin accepted owner result");
        accept_barrier.wait().await;
        let outcome = CanonicalMessageRepository::apply_closed_room_owner_result(
            &mut tx,
            ClosedRoomOwnerResultSource::new(
                source_key,
                &accept_source.sender,
                &room,
                &digest,
                &accept_binding,
                None,
            ),
            ClosedRoomOwnerResultProposal::Accepted {
                owner_effects: vec![accept_effect],
            },
        )
        .await
        .expect("serialize accepted owner result");
        tx.commit().await.expect("commit accepted owner result");
        outcome
    };
    let reject = async {
        let mut tx = reject_uow
            .begin()
            .await
            .expect("begin rejected owner result");
        reject_barrier.wait().await;
        let outcome = CanonicalMessageRepository::apply_closed_room_owner_result(
            &mut tx,
            ClosedRoomOwnerResultSource::new(
                source_key,
                &reject_source.sender,
                &room,
                &digest,
                &reject_binding,
                None,
            ),
            ClosedRoomOwnerResultProposal::Rejected {
                error: forbidden_owner_error(),
            },
        )
        .await
        .expect("serialize rejected owner result");
        tx.commit().await.expect("commit rejected owner result");
        outcome
    };
    let (accepted, rejected) = tokio::join!(accept, reject);
    assert_eq!(accepted, rejected, "one immutable result wins the row lock");
    assert!(matches!(
        accepted.decision,
        RoomOwnerDecision::Accepted | RoomOwnerDecision::Rejected
    ));
    assert_eq!(
        fixture.count("ingress_owner_rejections").await,
        if accepted.decision == RoomOwnerDecision::Rejected {
            1
        } else {
            0
        }
    );
    fixture.close().await;
}

#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn sqlite_dark_source_keeps_first_sm_coordinate_across_alias_retransmit() {
    let fixture = IngressFixture::sqlite().await;
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let source = groupchat(
        fixture.submission(Some("dark-sm-coordinate"), "body"),
        &room,
    );
    let first_stream = SmSessionId::new("dark-source-first-stream");
    let later_stream = SmSessionId::new("dark-source-later-stream");
    let mut tx = fixture.uow.begin().await.expect("enroll source streams");
    let first_sm = SmIngressStreamRepository::mint(&mut tx, &first_stream)
        .await
        .expect("first stream ID");
    let later_sm = SmIngressStreamRepository::mint(&mut tx, &later_stream)
        .await
        .expect("later stream ID");
    tx.commit().await.expect("commit source streams");

    let super::IngressPrincipal::Authenticated(principal) = &source.principal else {
        panic!("authenticated source");
    };
    let first_binding =
        CanonicalSourceBinding::new(principal.clone(), Some((first_sm, IngressOrdinal::FIRST)));
    let key = seed_dark_source_with_binding(&fixture, &source, &first_binding, true).await;
    let digest = waddle_xmpp::ingress::digest::v1::digest(&source.digest_input);

    // The same authenticated origin can be retransmitted at a later SM
    // occurrence. The alias remains canonical, while the independent SM
    // reference binds the new occurrence to that same key.
    let later_binding =
        CanonicalSourceBinding::new(principal.clone(), Some((later_sm, IngressOrdinal::FIRST)));
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("resolve later source occurrence");
    let replay = CanonicalMessageRepository::resolve_and_record_alias_with_source(
        &mut tx,
        &source.sender.to_bare(),
        &source.target,
        source.digest_input.origin().expect("source origin"),
        &digest,
        &later_binding,
        MessageKey::new,
    )
    .await
    .expect("later occurrence reuses the canonical alias");
    assert!(matches!(
        replay,
        waddle_xmpp::ingress::AliasResolution::Aliased(
            waddle_xmpp::ingress::AliasOutcome::Existing(replayed)
        ) if replayed == key
    ));
    SmIngressRepository::insert_sm_ref(
        &mut tx,
        later_sm,
        IngressOrdinal::FIRST,
        WireHandledCount::from_storage(1),
        key,
    )
    .await
    .expect("bind later SM occurrence to canonical key");
    tx.commit().await.expect("commit later occurrence binding");

    let stored = read_dark_source_binding(&fixture, key).await;
    assert_eq!(
        stored.sm_ingress_id.as_deref(),
        Some(first_sm.to_storage().to_string().as_str())
    );
    assert_eq!(stored.ingress_ordinal.as_deref(), Some("1"));
    let mut tx = fixture.uow.begin().await.expect("verify SM references");
    assert_eq!(
        SmIngressRepository::lookup(&mut tx, first_sm, IngressOrdinal::FIRST)
            .await
            .expect("first source reference"),
        Some(key)
    );
    assert_eq!(
        SmIngressRepository::lookup(&mut tx, later_sm, IngressOrdinal::FIRST)
            .await
            .expect("later source reference"),
        Some(key)
    );
    tx.commit().await.expect("finish SM reference checks");
    fixture.close().await;
}

#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn sqlite_dark_source_binding_rejects_mismatch_and_legacy_rows() {
    use waddle_xmpp::auth::{
        AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
    };

    let fixture = IngressFixture::sqlite().await;
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let source = groupchat(fixture.submission(Some("dark-mismatch"), "body"), &room);
    let key = seed_dark_source(&fixture, &source, true).await;
    let digest = waddle_xmpp::ingress::digest::v1::digest(&source.digest_input);
    let super::IngressPrincipal::Authenticated(principal) = &source.principal else {
        panic!("authenticated source");
    };
    let binding = CanonicalSourceBinding::new(principal.clone(), None);
    let changed_principal = AuthenticatedPrincipalRef::new(
        principal.bare_jid().clone(),
        AuthContextId::new(uuid::Uuid::new_v4()),
        AuthContextVersion::new(1),
        PrincipalAuthEpoch::new(1),
    );
    let conflicting_binding = CanonicalSourceBinding::new(changed_principal.clone(), None);
    let mut tx = fixture.uow.begin().await.expect("mismatched principal");
    let error = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            key,
            &source.sender,
            &room,
            &digest,
            &conflicting_binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Accepted {
            owner_effects: vec![closed_owner_route(&room)],
        },
    )
    .await
    .expect_err("different authentication context cannot claim source");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);

    let other_room: jid::BareJid = "other@muc.example.com".parse().expect("other room");
    let mut tx = fixture.uow.begin().await.expect("mismatched room");
    let error = CanonicalMessageRepository::assert_relayed_source(
        &mut tx,
        key,
        &source.sender,
        &other_room,
        &digest,
        &binding,
        None,
    )
    .await
    .expect_err("source cannot authorize a different room");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);

    let mut tx = fixture.uow.begin().await.expect("conflicting alias replay");
    let error = CanonicalMessageRepository::resolve_and_record_alias_with_source(
        &mut tx,
        &source.sender.to_bare(),
        &source.target,
        source.digest_input.origin().expect("origin ID"),
        &digest,
        &conflicting_binding,
        MessageKey::new,
    )
    .await
    .expect_err("origin alias cannot change its source principal");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::SourceBindingConflict)
    ));
    drop(tx);

    let unbound = groupchat(fixture.submission(Some("legacy-dark"), "legacy"), &room);
    let legacy_key = MessageKey::new();
    let legacy_digest = waddle_xmpp::ingress::digest::v1::digest(&unbound.digest_input);
    let envelope = MessageEnvelope::new(unbound.plan.sanitized_message.clone());
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("legacy source transaction");
    let inserted = CanonicalMessageRepository::resolve_and_record_alias(
        &mut tx,
        &unbound.sender.to_bare(),
        &unbound.target,
        unbound.digest_input.origin().expect("legacy origin"),
        &legacy_digest,
        || legacy_key,
    )
    .await
    .expect("insert legacy origin alias");
    assert!(matches!(
        inserted,
        waddle_xmpp::ingress::AliasResolution::Aliased(
            waddle_xmpp::ingress::AliasOutcome::Inserted(inserted_key)
        ) if inserted_key == legacy_key
    ));
    CanonicalMessageRepository::record_message(
        &mut tx,
        legacy_key,
        &legacy_digest,
        Some(&envelope),
    )
    .await
    .expect("record legacy null-bound source");
    EffectIntentRepository::reconcile(&mut tx, legacy_key, &unbound.plan.intents, false)
        .await
        .expect("record legacy dispatch");
    tx.commit().await.expect("commit legacy source");
    let super::IngressPrincipal::Authenticated(legacy_principal) = &unbound.principal else {
        panic!("authenticated source");
    };
    let legacy_binding = CanonicalSourceBinding::new(legacy_principal.clone(), None);
    let mut tx = fixture.uow.begin().await.expect("replay legacy alias");
    let replay = CanonicalMessageRepository::resolve_and_record_alias_with_source(
        &mut tx,
        &unbound.sender.to_bare(),
        &unbound.target,
        unbound.digest_input.origin().expect("legacy origin"),
        &legacy_digest,
        &legacy_binding,
        MessageKey::new,
    )
    .await
    .expect("local replay may resolve a legacy alias");
    assert!(matches!(
        replay,
        waddle_xmpp::ingress::AliasResolution::Aliased(
            waddle_xmpp::ingress::AliasOutcome::Existing(replayed)
        ) if replayed == legacy_key
    ));
    tx.commit().await.expect("finish legacy alias replay");
    let legacy_binding_row = read_dark_source_binding(&fixture, legacy_key).await;
    assert_eq!(legacy_binding_row.version, None);
    assert_eq!(legacy_binding_row.owner_decision, None);
    let mut tx = fixture.uow.begin().await.expect("legacy relay proof");
    let error = CanonicalMessageRepository::assert_relayed_source(
        &mut tx,
        legacy_key,
        &unbound.sender,
        &room,
        &legacy_digest,
        &legacy_binding,
        None,
    )
    .await
    .expect_err("legacy null binding must fail closed");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);
    let mut tx = fixture.uow.begin().await.expect("legacy owner result");
    let error = CanonicalMessageRepository::apply_closed_room_owner_result(
        &mut tx,
        ClosedRoomOwnerResultSource::new(
            legacy_key,
            &unbound.sender,
            &room,
            &legacy_digest,
            &legacy_binding,
            None,
        ),
        ClosedRoomOwnerResultProposal::Rejected {
            error: forbidden_owner_error(),
        },
    )
    .await
    .expect_err("the closed owner API rejects legacy-null source bindings");
    assert!(matches!(
        error,
        IngressUowError::Substrate(IngressSubstrateError::RelayedAliasUnproven)
    ));
    drop(tx);
    assert_eq!(fixture.count("ingress_owner_rejections").await, 0);
    fixture.close().await;
}
