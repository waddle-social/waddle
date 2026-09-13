//! XEP-0359 §3 identity and RFC-0018 §3.4 alias-only deduplication.
#[path = "ingress_cases/alias_support.rs"]
mod alias_support;
mod ingress_support;

use alias_support::*;
use ingress_support::IngressFixture;
use jid::BareJid;
use waddle_server::{
    ingress::{
        commit::commit_submission, ExternalEffect, IngressDecision, IngressDecisionClass,
        IngressSubmission,
    },
    ingress_substrate::MessageEnvelope,
    ingress_uow::CanonicalMessageRepository,
};
use waddle_xmpp::{
    ingress::{DigestContext, IngressEffectIntent, NormalizedTarget},
    Stanza,
};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::{
    message::{Lang, Message, MessageType},
    stanza_error::{DefinedCondition, StanzaError},
};

macro_rules! backend_tests {
    ($sqlite:ident, $postgres:ident, $case:ident) => {
        /// XEP-0359 §3: the assigning authority preserves identity across retries.
        #[tokio::test]
        async fn $sqlite() {
            $case(IngressFixture::sqlite().await).await;
        }
        /// XEP-0359 §3: PostgreSQL must implement the same alias contract as SQLite.
        #[tokio::test]
        async fn $postgres() {
            if let Some(fixture) = IngressFixture::postgres(stringify!($case)).await {
                $case(fixture).await;
            }
        }
    };
}

async fn duplicate_reflection(fixture: IngressFixture) {
    for room in [false, true] {
        let original = archive_plan(&fixture, room, if room { "room-first" } else { "dm-first" });
        let first = commit_submission(&fixture.uow, &original, 5)
            .await
            .expect("accept");
        assert_eq!(first.class, IngressDecisionClass::Accepted);
        let frames = wire_messages(&fixture, &first).await;
        assert_eq!(frames.len(), 2);
        let retry = archive_plan(&fixture, room, "discard-retry-id");
        // Reopening the authority must preserve the assigned XEP-0359 identity.
        let restarted = fixture.authority().await;
        let duplicate = restarted.commit(&retry).await;
        assert!(
            restarted
                .drain_and_join(std::time::Duration::from_secs(5))
                .await
        );
        assert_eq!(duplicate.class, IngressDecisionClass::ExistingConsistent);
        assert_eq!(duplicate.message_key, first.message_key);
        assert_eq!(duplicate.archive_ids, first.archive_ids);
        let frames = wire_messages(&fixture, &duplicate).await;
        assert_eq!(frames.len(), 1, "duplicate is sender-only");
        assert_eq!(frames[0].to, Some(original.sender.clone().into()));
        let assigned = &first.archive_ids[0].1;
        assert!(
            waddle_xmpp_core::xep0359::extract_stanza_ids(&frames[0]).contains(assigned),
            "downstream reference restamped to recorded identity"
        );
        assert_canonical(&fixture, &first, &original.plan.sanitized_message).await;
    }
    assert_eq!(fixture.count("ingress_messages").await, 2);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 2);
    assert_eq!(fixture.count("mam_messages").await, 2);
    assert_eq!(
        fixture
            .count("mam_messages WHERE id = 'discard-retry-id'")
            .await,
        0
    );
    assert_eq!(
        fixture
            .optional_text("SELECT rich_payload FROM mam_messages WHERE id = 'room-first'")
            .await,
        None,
        "alias dedupe does not require archived MUC sender metadata"
    );
    fixture.close().await;
}
backend_tests!(
    alias_reflection_sqlite,
    alias_reflection_postgres,
    duplicate_reflection
);

async fn changed_content_conflict(fixture: IngressFixture) {
    let original = archive_plan(&fixture, true, "accepted-id");
    let first = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("first");
    let mut changed = archive_plan(&fixture, true, "conflicting-id");
    changed
        .plan
        .sanitized_message
        .bodies
        .insert(Lang::new(), "changed content".into());
    refresh_digest(&mut changed);
    let conflict = commit_submission(&fixture.uow, &changed, 5)
        .await
        .expect("commit rejection");
    assert_eq!(conflict.class, IngressDecisionClass::AliasConflict);
    assert!(conflict.class.advances());
    let wire = wire_messages(&fixture, &conflict).await;
    assert_eq!(wire.len(), 1);
    assert_error(&wire[0], DefinedCondition::Conflict);
    assert_eq!(wire[0].to, Some(changed.sender.clone().into()));
    assert_canonical(&fixture, &conflict, &changed.plan.sanitized_message).await;
    let retry = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("accepted alias untouched");
    assert_eq!(retry.message_key, first.message_key);
    assert_eq!(fixture.count("ingress_messages").await, 2);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 1);
    fixture.close().await;
}
backend_tests!(
    alias_conflict_sqlite,
    alias_conflict_postgres,
    changed_content_conflict
);

async fn subject_rebroadcast(fixture: IngressFixture) {
    let mut original = archive_plan(&fixture, true, "subject-id");
    original.plan.sanitized_message.bodies.clear();
    original
        .plan
        .sanitized_message
        .subjects
        .insert(Lang::new(), "topic".into());
    refresh_digest(&mut original);
    add_reflections(&mut original);
    let first = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("subject");
    assert_eq!(wire_messages(&fixture, &first).await.len(), 2);
    let duplicate = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("subject retry");
    assert_eq!(duplicate.message_key, first.message_key);
    // XEP-0045 §8.1: an accepted subject is rebroadcast to every occupant.
    let wire = wire_messages(&fixture, &duplicate).await;
    assert_eq!(wire.len(), 2);
    assert!(wire.iter().all(|message| message.bodies.is_empty()
        && message.subjects.get(&Lang::new()).map(String::as_str) == Some("topic")));
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 1);
    assert_eq!(
        fixture
            .optional_text("SELECT body FROM mam_messages WHERE id = 'subject-id'")
            .await,
        None
    );
    let rich: waddle_xmpp_core::mam::ArchivedRichMessage = serde_json::from_str(
        &fixture
            .optional_text("SELECT rich_payload FROM mam_messages WHERE id = 'subject-id'")
            .await
            .expect("stored subject"),
    )
    .expect("typed rich message");
    assert_eq!(rich.subjects.get("").map(String::as_str), Some("topic"));
    fixture.close().await;
}
backend_tests!(
    alias_subject_rebroadcast_sqlite,
    alias_subject_rebroadcast_postgres,
    subject_rebroadcast
);

async fn subject_with_thread_retry(fixture: IngressFixture) {
    let mut original = archive_plan(&fixture, true, "subject-thread-id");
    original.plan.sanitized_message.bodies.clear();
    original
        .plan
        .sanitized_message
        .subjects
        .insert(Lang::new(), "topic".into());
    original.plan.sanitized_message.thread = Some(xmpp_parsers::message::Thread {
        id: "timeline-thread".into(),
        parent: None,
    });
    refresh_digest(&mut original);
    add_reflections(&mut original);
    let first = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("timeline acceptance");
    assert_eq!(wire_messages(&fixture, &first).await.len(), 2);
    let duplicate = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("timeline retry");
    assert_eq!(duplicate.class, IngressDecisionClass::ExistingConsistent);
    assert!(duplicate.class.advances());
    assert_eq!(duplicate.message_key, first.message_key);
    // XEP-0045 §8.1: subject plus thread is a timeline message, not room state.
    let wire = wire_messages(&fixture, &duplicate).await;
    assert_eq!(wire.len(), 1, "timeline retry suppresses non-senders");
    assert_eq!(wire[0].to, Some(original.sender.clone().into()));
    assert_eq!(wire[0].thread, original.plan.sanitized_message.thread);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 1);
    fixture.close().await;
}
backend_tests!(
    alias_subject_with_thread_retry_sqlite,
    alias_subject_with_thread_retry_postgres,
    subject_with_thread_retry
);

async fn dm_subject_is_digest_content(fixture: IngressFixture) {
    let mut original = archive_plan(&fixture, false, "subject-chat");
    original
        .plan
        .sanitized_message
        .subjects
        .insert(Lang::new(), "first".into());
    refresh_digest(&mut original);
    commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("first subject");
    original
        .plan
        .sanitized_message
        .subjects
        .insert(Lang::new(), "second".into());
    refresh_digest(&mut original);
    let rejection = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("changed DM subject");
    assert_eq!(rejection.class, IngressDecisionClass::AliasConflict);
    assert_error(
        &wire_messages(&fixture, &rejection).await[0],
        DefinedCondition::Conflict,
    );
    assert_eq!(fixture.count("ingress_messages").await, 2);
    assert_eq!(fixture.count("mam_messages").await, 1);
    fixture.close().await;
}
backend_tests!(
    alias_dm_subject_digest_sqlite,
    alias_dm_subject_digest_postgres,
    dm_subject_is_digest_content
);

async fn concurrent_streams(fixture: IngressFixture) {
    let first = archive_plan(&fixture, false, "concurrent-a");
    let mut second = archive_plan(&fixture, false, "concurrent-b");
    second.sender = "romeo@example.com/laptop"
        .parse()
        .expect("second connection");
    second.plan.sanitized_message.from = Some(second.sender.clone().into());
    refresh_digest(&mut second);
    add_reflections(&mut second);
    let (a, b) = tokio::join!(
        commit_submission(&fixture.uow, &first, 5),
        commit_submission(&fixture.uow, &second, 5)
    );
    let a = a.expect("first concurrent submission");
    let b = b.expect("second concurrent submission");
    assert_eq!(a.message_key, b.message_key);
    assert_eq!(a.archive_ids, b.archive_ids);
    assert!(
        (a.class == IngressDecisionClass::Accepted
            && b.class == IngressDecisionClass::ExistingConsistent)
            || (b.class == IngressDecisionClass::Accepted
                && a.class == IngressDecisionClass::ExistingConsistent)
    );
    assert_eq!(
        wire_messages(&fixture, &a).await.len() + wire_messages(&fixture, &b).await.len(),
        3
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 1);
    fixture.close().await;
}
backend_tests!(
    alias_concurrent_streams_sqlite,
    alias_concurrent_streams_postgres,
    concurrent_streams
);

async fn rejoin_and_nickname_reuse(fixture: IngressFixture) {
    let mut original = archive_plan(&fixture, true, "before-rejoin");
    room_archive_identity(&mut original, 1);
    let first = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("first nickname generation");
    let mut retry = archive_plan(&fixture, true, "after-rejoin");
    retry.sender = "romeo@example.com/new-connection"
        .parse()
        .expect("reconnected sender");
    retry.plan.sanitized_message.from = Some(retry.sender.clone().into());
    refresh_digest(&mut retry);
    room_archive_identity(&mut retry, 2);
    add_reflections(&mut retry);
    let duplicate = commit_submission(&fixture.uow, &retry, 5)
        .await
        .expect("retry after failed resume");
    assert_eq!(duplicate.message_key, first.message_key);
    assert_eq!(wire_messages(&fixture, &duplicate).await.len(), 1);
    // A rejoin changes room nickname generation and archived full JID, not the alias owner.
    assert_eq!(fixture.count("mam_messages").await, 1);
    let other = waddle_xmpp::auth::AuthenticatedPrincipalRef::new(
        "mercutio@example.com".parse().expect("other sender"),
        fixture.principal.auth_context_id().clone(),
        fixture.principal.auth_context_version(),
        fixture.principal.auth_epoch(),
    );
    fixture.execute("INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, ?, ?)", waddle_server::db_params![other.bare_jid().to_string(), "mercutio".to_string(), "mercutio".to_string(), chrono::Utc::now().to_rfc3339(), chrono::Utc::now().to_rfc3339()]).await;
    fixture
        .execute(
            "UPDATE sessions SET user_jid = ?",
            waddle_server::db_params![other.bare_jid().to_string()],
        )
        .await;
    let mut reused = archive_plan(&fixture, true, "different-bare-jid");
    reused.principal = other.clone();
    reused.identity = waddle_server::ingress::IngressStreamIdentity::Ephemeral { principal: other };
    reused.sender = "mercutio@example.com/phone".parse().expect("new occupant");
    reused.plan.sanitized_message.from = Some(reused.sender.clone().into());
    refresh_digest(&mut reused);
    room_archive_identity(&mut reused, 3);
    add_reflections(&mut reused);
    let distinct = commit_submission(&fixture.uow, &reused, 5)
        .await
        .expect("same nick, other bare JID");
    assert_eq!(distinct.class, IngressDecisionClass::Accepted);
    assert_ne!(distinct.message_key, first.message_key);
    assert_eq!(wire_messages(&fixture, &distinct).await.len(), 2);
    assert_eq!(fixture.count("ingress_messages").await, 2);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 2);
    assert_eq!(fixture.count("mam_messages").await, 2);
    fixture.close().await;
}
backend_tests!(
    alias_rejoin_and_nick_reuse_sqlite,
    alias_rejoin_and_nick_reuse_postgres,
    rejoin_and_nickname_reuse
);

async fn tombstoned_alias(fixture: IngressFixture) {
    let original = archive_plan(&fixture, false, "retracted-id");
    let first = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("original");
    assert_eq!(wire_messages(&fixture, &first).await.len(), 2);
    let mut tx = fixture.uow.begin().await.expect("retraction transaction");
    waddle_server::ingress_uow::MamArchiveRepository::replace_with_tombstone(
        &mut tx,
        fixture.principal.bare_jid(),
        &first.archive_ids[0].1,
        &waddle_xmpp_core::mam::ArchivedTombstone {
            retraction_id: None,
            stamp: chrono::Utc::now(),
            moderation: None,
            sender_scope: None,
        },
    )
    .await
    .expect("tombstone");
    tx.commit().await.expect("commit retraction");
    let retry = archive_plan(&fixture, false, "never-resurrect");
    let duplicate = commit_submission(&fixture.uow, &retry, 5)
        .await
        .expect("retry retracted request");
    assert_eq!(duplicate.message_key, first.message_key);
    assert!(
        wire_messages(&fixture, &duplicate).await.is_empty(),
        "tombstone swallows even sender reflection"
    );
    assert_eq!(
        fixture
            .optional_text("SELECT body FROM mam_messages WHERE id = 'retracted-id'")
            .await,
        None
    );
    assert_eq!(fixture.count("mam_messages").await, 1);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    fixture.close().await;
}
backend_tests!(
    alias_tombstone_sqlite,
    alias_tombstone_postgres,
    tombstoned_alias
);

async fn semantic_malformed(fixture: IngressFixture) {
    use waddle_server::ingress::effects::{PlanRejection, SemanticMalformedReason};
    let mut submission = fixture.submission(Some("malformed-origin"), "offered");
    let error = StanzaError::new(
        xmpp_parsers::stanza_error::ErrorType::Modify,
        DefinedCondition::BadRequest,
        "en",
        "malformed payload",
    );
    let mut reply = submission.plan.sanitized_message.clone();
    reply.to = reply.from.take();
    reply.type_ = MessageType::Error;
    reply.payloads.push(error.clone().into());
    submission.plan.error_reply = Some(Stanza::Message(reply));
    submission.plan.rejection = Some(PlanRejection::SemanticMalformed(
        SemanticMalformedReason::MalformedPayload,
    ));
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ErrorReply {
            recipient: submission.sender.clone(),
            error: waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&error).expect("typed error"),
        });
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("rejection commit");
    assert_eq!(decision.class, IngressDecisionClass::SemanticMalformed);
    assert!(decision.class.advances());
    let wire = wire_messages(&fixture, &decision).await;
    assert_eq!(wire.len(), 1);
    assert_error(&wire[0], DefinedCondition::BadRequest);
    assert_canonical(&fixture, &decision, &submission.plan.sanitized_message).await;
    assert_eq!(fixture.count("ingress_messages").await, 1);
    // XEP-0359 §5: the committed denial owns the offered origin id, so a
    // retransmission resolves to it instead of re-deciding under new policy.
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 0);
    fixture.close().await;
}
backend_tests!(
    alias_malformed_sqlite,
    alias_malformed_postgres,
    semantic_malformed
);

async fn remote_owner(mut fixture: IngressFixture) {
    use waddle_server::ingress::{
        effects::{
            room::{DurableRoomEffect, RoomFenceRequirement},
            DurableEffect, Effect,
        },
        IngressCanonicalRef, IngressStreamIdentity,
    };
    let room: BareJid = "room@muc.example.com".parse().expect("room");
    #[cfg(feature = "clustering")]
    let fence = fixture.room_fence(&room).await;
    #[cfg(not(feature = "clustering"))]
    let _ = &mut fixture;
    let mut proxy = archive_plan(&fixture, false, "sender-archive");
    proxy.target = NormalizedTarget::Bare(room.clone());
    proxy.plan.sanitized_message.to = Some(room.clone().into());
    proxy.plan.sanitized_message.type_ = MessageType::Groupchat;
    refresh_digest(&mut proxy);
    let accepted = commit_submission(&fixture.uow, &proxy, 5)
        .await
        .expect("proxy acceptance");
    let mut owner = archive_plan(&fixture, true, "room-archive");
    owner.identity = IngressStreamIdentity::Relayed {
        canonical: IngressCanonicalRef {
            message_key: accepted.message_key.expect("proxy key"),
            sender_bare: fixture.principal.bare_jid().clone(),
            origin_id: proxy.digest_input.origin().cloned(),
        },
        room: room.clone(),
        #[cfg(feature = "clustering")]
        room_fence: fence.clone(),
    };
    if let Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
        fence: requirement,
        ..
    })) = &mut owner.plan.plan[0].effect
    {
        #[cfg(feature = "clustering")]
        {
            *requirement = RoomFenceRequirement::Guarded(fence);
        }
        #[cfg(not(feature = "clustering"))]
        {
            *requirement = RoomFenceRequirement::Unfenced;
        }
    }
    let first = commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner acceptance");
    assert_eq!(first.class, IngressDecisionClass::OwnerFirstAcceptance);
    assert_eq!(wire_messages(&fixture, &first).await.len(), 2);
    let duplicate = commit_submission(&fixture.uow, &owner, 5)
        .await
        .expect("owner duplicate");
    assert_eq!(duplicate.class, IngressDecisionClass::OwnerDuplicate);
    assert_eq!(duplicate.message_key, accepted.message_key);
    let wire = wire_messages(&fixture, &duplicate).await;
    assert_eq!(wire.len(), 1);
    assert_eq!(wire[0].to, Some(owner.sender.clone().into()));
    assert!(waddle_xmpp_core::xep0359::extract_stanza_ids(&wire[0])
        .contains(&StanzaId::new("room-archive", room.into())));
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 2);
    #[cfg(feature = "clustering")]
    {
        fixture
            .execute(
                "UPDATE clustering_claims SET claim_epoch = claim_epoch + 1",
                (),
            )
            .await;
        let deposed = commit_submission(&fixture.uow, &owner, 5)
            .await
            .expect_err("deposed owner");
        assert_eq!(deposed.class(), IngressDecisionClass::ClaimFenceMissing);
        assert!(!deposed.class().advances());
        assert_eq!(fixture.count("ingress_messages").await, 1);
        assert_eq!(fixture.count("mam_messages").await, 2);
        assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    }
    fixture.close().await;
}
/// XEP-0359 §3: a relayed retry preserves the room's assigning authority.
#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn alias_remote_owner_sqlite() {
    remote_owner(IngressFixture::sqlite().await).await;
}
/// XEP-0359 §3: only the live owner may accept or deduplicate a room alias.
#[tokio::test]
async fn alias_remote_owner_postgres() {
    if let Some(fixture) = IngressFixture::postgres("alias_remote_owner").await {
        remote_owner(fixture).await;
    }
}

#[path = "ingress_cases/detached_progress_support.rs"]
pub mod detached_progress_support;

#[tokio::test]
async fn xep0359_detached_progress_recorded_archive_ids_sqlite() {
    detached_progress_support::cross_archive_retry(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn xep0359_detached_progress_recorded_archive_ids_postgres() {
    if let Some(fixture) = IngressFixture::postgres("xep0359_dp_archives").await {
        detached_progress_support::cross_archive_retry(fixture).await;
    }
}

struct StanzaIdRecoveryEnvironment {
    connections: waddle_xmpp::registry::ConnectionRegistry,
    sm: std::sync::Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
}

impl waddle_server::ingress::RecoveryEnvironment for StanzaIdRecoveryEnvironment {
    fn recovery_deps(&self) -> waddle_server::ingress::Deps<'_> {
        let mut deps = waddle_server::ingress::Deps::new(&self.connections, "example.com");
        deps.sm_session_registry = Some(&self.sm);
        deps
    }
}

async fn recover_stanza_id_pass(
    fixture: &IngressFixture,
    authority: &waddle_server::ingress::IngressAuthority,
    terminals: i64,
) {
    let sql = match fixture.db.driver() {
        waddle_server::db::DatabaseDriver::Postgres => "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE terminal_at IS NULL",
        waddle_server::db::DatabaseDriver::Sqlite => "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE terminal_at IS NULL",
    };
    fixture
        .execute(
            sql,
            waddle_server::db_params![
                (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339()
            ],
        )
        .await;
    authority.trigger_maintenance();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await
                == terminals
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("maintenance completion witness");
}

async fn recovery_preserves_recipient_stanza_id(fixture: IngressFixture) {
    use std::{sync::Arc, time::Duration};
    use waddle_server::ingress::{
        effects::{direct::DurableDirectEffect, Effect},
        DurableEffect, PlannedEffect, RecoveryEnvironment,
    };
    use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};
    let sm = detached_progress_support::registry(&fixture).await;
    let [resource, witness_resource, _] = detached_progress_support::resources();
    detached_progress_support::attach(&sm, &resource).await;
    detached_progress_support::attach(&sm, &witness_resource).await;
    let authority = fixture.authority().await;
    let environment: Arc<dyn RecoveryEnvironment> = Arc::new(StanzaIdRecoveryEnvironment {
        connections: waddle_xmpp::registry::ConnectionRegistry::new(),
        sm: sm.clone(),
    });
    authority.bind_recovery_environment(Arc::downgrade(&environment));
    let mut submission =
        fixture.submission(Some("sid-lost-phase-c"), "canonical recipient delivery");
    let recipient = resource.to_bare();
    let recorded = StanzaId::new("recipient-recorded-before-crash", recipient.clone().into());
    let mut archived =
        ArchivedMessage::for_test(submission.sender.clone().into(), recipient.clone().into());
    archived.id = recorded.id.clone();
    archived.body = Some("canonical recipient delivery".to_owned());
    archived.message_type = MessageType::Chat;
    archived.stanza_id = Some(recorded.clone());
    archived.origin_id = submission.digest_input.origin().cloned();
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ArchiveAuthoritative {
            archive: recipient.clone(),
            by: recipient.clone(),
            stanza_id: recorded.clone(),
            archived_at: archived.timestamp,
            ordinal: None,
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ArchiveDirect {
                archive: recipient,
                message: Box::new(archived),
                archive_expectation: ArchiveExpectation::Fresh,
            },
        ))));
    detached_progress_support::route(&mut submission, std::slice::from_ref(&resource), 1);
    let planned = submission.plan.plan.last_mut().expect("direct delivery");
    let Effect::External(ExternalEffect::Delivery(
        waddle_server::ingress::effects::delivery::ExternalDeliveryEffect::QueueDetached {
            stanza,
            ..
        },
    )) = &mut planned.effect
    else {
        panic!("planned detached delivery");
    };
    let Stanza::Message(delivery) = stanza.as_mut() else {
        panic!("direct message");
    };
    waddle_xmpp_core::xep0359::add_stanza_id(delivery, &recorded);
    // The envelope lacks the recipient stamp; recovery must use the recorded intent.
    assert!(
        waddle_xmpp_core::xep0359::extract_stanza_ids(&submission.plan.sanitized_message)
            .is_empty()
    );
    assert_eq!(
        authority.commit(&submission).await.class,
        IngressDecisionClass::Accepted
    );
    assert_eq!(fixture.count("mam_messages").await, 1);
    assert!(detached_progress_support::queued(&sm, &resource)
        .await
        .unacked_stanzas
        .is_empty());
    recover_stanza_id_pass(&fixture, &authority, 1).await;
    let first = detached_progress_support::queued(&sm, &resource).await;
    assert_eq!(first.unacked_stanzas.len(), 1);
    let element: minidom::Element = first.unacked_stanzas[0]
        .stanza_xml
        .parse()
        .expect("SM wire XML");
    let message = Message::try_from(element).expect("recovered direct message");
    assert_eq!(
        waddle_xmpp_core::xep0359::extract_stanza_ids(&message),
        vec![recorded]
    );
    assert_eq!(message.bodies, submission.plan.sanitized_message.bodies);
    assert_eq!(fixture.count("sm_ingress_appends").await, 1);
    let mut witness = fixture.submission(Some("sid-second-pass"), "completion witness");
    detached_progress_support::route(&mut witness, std::slice::from_ref(&witness_resource), 1);
    assert_eq!(
        authority.commit(&witness).await.class,
        IngressDecisionClass::Accepted
    );
    recover_stanza_id_pass(&fixture, &authority, 2).await;
    let second = detached_progress_support::queued(&sm, &resource).await;
    assert_eq!(second.outbound_count, first.outbound_count);
    assert_eq!(second.unacked_stanzas.len(), 1);
    assert_eq!(
        second.unacked_stanzas[0].stanza_xml,
        first.unacked_stanzas[0].stanza_xml
    );
    assert_eq!(
        fixture
            .count("sm_ingress_appends WHERE resource = 'juliet@example.com/a'")
            .await,
        1
    );
    assert_eq!(fixture.count("mam_messages").await, 1);
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(environment);
    drop(authority);
    drop(sm);
    fixture.close().await;
}
backend_tests!(
    recovery_recipient_stanza_id_sqlite,
    recovery_recipient_stanza_id_postgres,
    recovery_preserves_recipient_stanza_id
);

async fn recovery_does_not_rebuild_groupchat_inbox_push(fixture: IngressFixture) {
    use std::{sync::Arc, time::Duration};
    use waddle_server::ingress::{
        effects::{direct::ExternalDirectEffect, room::DurableRoomEffect, Effect, ProjectionRef},
        DurableEffect, PlannedEffect, RecoveryEnvironment,
    };
    use waddle_xmpp::{
        inbox::{ConversationKind, InboxEntry},
        ingress::{EffectMessageIdentity, InboxProjectionMutation},
    };
    let sm = detached_progress_support::registry(&fixture).await;
    let [resource, witness_resource, _] = detached_progress_support::resources();
    detached_progress_support::attach(&sm, &resource).await;
    detached_progress_support::attach(&sm, &witness_resource).await;
    let authority = fixture.authority().await;
    let environment: Arc<dyn RecoveryEnvironment> = Arc::new(StanzaIdRecoveryEnvironment {
        connections: waddle_xmpp::registry::ConnectionRegistry::new(),
        sm: sm.clone(),
    });
    authority.bind_recovery_environment(Arc::downgrade(&environment));
    let mut submission = archive_plan(&fixture, true, "groupchat-push-recorded");
    submission
        .plan
        .plan
        .retain(|effect| !matches!(effect.effect, Effect::External(_)));
    let room = submission
        .plan
        .sanitized_message
        .to
        .as_ref()
        .expect("room target")
        .to_bare();
    let recipient = resource.to_bare();
    let route = IngressEffectIntent::RouteDirect {
        recipient: recipient.clone(),
        fanout: vec![resource.clone()],
        route_identity: EffectMessageIdentity::capture_ordinal(1),
    };
    let projection = ProjectionRef(submission.plan.plan.len());
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ProjectGroupchatInbox {
                archive_stanza_id: StanzaId::new("groupchat-push-recorded", room.clone().into()),
                owner: recipient.clone(),
                entry: Box::new(InboxEntry::new(
                    room.clone(),
                    ConversationKind::MucRoom,
                    "groupchat-push-recorded",
                    chrono::Utc::now().timestamp_millis(),
                )),
                is_recipient: true,
                recovery: None,
            },
        ))));
    submission
        .plan
        .intents
        .push(IngressEffectIntent::InboxProject {
            owner: recipient.clone(),
            mutation: InboxProjectionMutation::GroupchatChannel {
                room,
                increment_unread: true,
            },
        });
    submission.plan.intents.push(route.clone());
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
                owner: recipient,
                projection,
                receipt: Some(Box::new(route)),
            }),
        )));
    let decision = authority.commit(&submission).await;
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert!(decision.external.iter().any(|effect| matches!(
        effect,
        ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
            receipt: Some(_),
            ..
        })
    )));
    assert_eq!(fixture.count("sm_ingress_appends").await, 0);
    // Each newly committed supported row witnesses a real pass over the older
    // unsupported groupchat row. The push needs its projection, not the envelope.
    for pass in 1..=2 {
        let mut witness = fixture.submission(None, &format!("groupchat pass {pass}"));
        detached_progress_support::route(&mut witness, std::slice::from_ref(&witness_resource), 1);
        assert_eq!(
            authority.commit(&witness).await.class,
            IngressDecisionClass::Accepted
        );
        recover_stanza_id_pass(&fixture, &authority, pass).await;
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NULL")
                .await,
            1
        );
        assert!(
            detached_progress_support::queued(&sm, &resource)
                .await
                .unacked_stanzas
                .is_empty(),
            "canonical groupchat must never masquerade as synthetic inbox push"
        );
        assert_eq!(
            fixture
                .count("sm_ingress_appends WHERE resource = 'juliet@example.com/a'")
                .await,
            0
        );
        assert_eq!(fixture.count("mam_messages").await, 1);
    }
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(environment);
    drop(authority);
    drop(sm);
    fixture.close().await;
}
backend_tests!(
    recovery_groupchat_inbox_push_deferred_sqlite,
    recovery_groupchat_inbox_push_deferred_postgres,
    recovery_does_not_rebuild_groupchat_inbox_push
);
