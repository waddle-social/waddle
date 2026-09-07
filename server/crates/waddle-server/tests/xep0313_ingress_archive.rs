//! XEP-0313 archive identity at the ingress transaction and MAM wire boundaries.
pub mod ingress_support;

use std::time::Duration;

use chrono::{DateTime, Utc};
use ingress_support::IngressFixture;
use jid::BareJid;
use waddle_server::{
    ingress::{
        commit::commit_submission,
        effects::{direct::DurableDirectEffect, Effect},
        DurableEffect, IngressDecisionClass, IngressSubmission, PlannedEffect,
    },
    ingress_substrate::{gc_expired_aliases, AliasGcBudget, AliasGcProgress, ALIAS_RETENTION},
    ingress_uow::{MamArchiveRepository, ReconcileVerdict},
};
use waddle_xmpp::{
    ingress::IngressEffectIntent,
    mam::{
        build_result_messages, ArchiveExpectation, ArchivedMessage, ArchivedTombstone,
        MamArchiveKind, MamQuery, MamStorage, SqlxMamStorage, MAM_NS,
    },
};
use waddle_xmpp_core::xep0359::StanzaId;

fn plan(
    fixture: &IngressFixture,
    origin: &str,
    id: &str,
    stamp: DateTime<Utc>,
) -> IngressSubmission {
    let mut submission = fixture.submission(Some(origin), "archive body");
    add_archive(&mut submission, fixture.principal.bare_jid(), id, stamp);
    submission
}

fn add_archive(
    submission: &mut IngressSubmission,
    archive: &BareJid,
    id: &str,
    stamp: DateTime<Utc>,
) {
    let stanza_id = StanzaId::new(id, archive.clone().into());
    let mut message = ArchivedMessage::for_test(
        submission.sender.clone().into(),
        submission
            .plan
            .sanitized_message
            .to
            .clone()
            .expect("target"),
    );
    message.id = id.to_owned();
    message.timestamp = stamp;
    message.body = Some("archive body".to_owned());
    message.message_type = xmpp_parsers::message::MessageType::Chat;
    message.stanza_id = Some(stanza_id.clone());
    message.origin_id = submission.digest_input.origin().cloned();
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ArchiveAuthoritative {
            archive: archive.clone(),
            stanza_id,
            by: archive.clone(),
            archived_at: stamp,
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ArchiveDirect {
                archive: archive.clone(),
                message: Box::new(message),
                archive_expectation: ArchiveExpectation::Fresh,
            },
        ))));
}

/// Assert the actual MAM result builder's wire UID and forwarded payload against queried rows.
async fn query_wire(fixture: &IngressFixture, archive: &BareJid) -> Vec<ArchivedMessage> {
    let store = SqlxMamStorage::open(fixture.db.database_url())
        .await
        .expect("MAM reader");
    let result = store
        .query_messages(archive, MamArchiveKind::Personal, &MamQuery::default())
        .await
        .expect("MAM query");
    let frames = build_result_messages(
        "authority-query",
        &archive.clone().into(),
        &fixture.principal.bare_jid().clone().into(),
        &result.messages,
    );
    assert_eq!(frames.len(), result.messages.len());
    for (frame, row) in frames.into_iter().zip(&result.messages) {
        let xml: minidom::Element = frame.into();
        let result = xml.get_child("result", MAM_NS).expect("MAM result on wire");
        assert_eq!(result.attr("id"), Some(row.id.as_str()));
        let forwarded = result
            .get_child("forwarded", xmpp_parsers::ns::FORWARD)
            .expect("forwarded message");
        let inner = forwarded
            .get_child("message", xmpp_parsers::ns::JABBER_CLIENT)
            .expect("inner stanza");
        assert_eq!(
            inner
                .get_child("body", xmpp_parsers::ns::JABBER_CLIENT)
                .map(minidom::Element::text),
            row.body.clone()
        );
    }
    result.messages
}

/// XEP-0313 §5.1.3 and §6.3: retries and missing-row repair retain the archive UID and order.
async fn identity_repair_tombstone(fixture: IngressFixture) {
    let archive = fixture.principal.bare_jid().clone();
    let stamp = Utc::now() - chrono::Duration::minutes(2);
    let original = plan(&fixture, "archive-a", "archive-a-id", stamp);
    let inserted = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("inserted");
    assert_eq!(inserted.class, IngressDecisionClass::Accepted);
    let existing = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("existing");
    assert_eq!(existing.archive_ids, inserted.archive_ids);
    commit_submission(
        &fixture.uow,
        &plan(
            &fixture,
            "archive-b",
            "archive-b-id",
            stamp + chrono::Duration::seconds(10),
        ),
        5,
    )
    .await
    .expect("later message");
    let before = query_wire(&fixture, &archive).await;
    fixture
        .execute(
            "DELETE FROM mam_messages WHERE id = ?",
            waddle_server::db_params!["archive-a-id".to_owned()],
        )
        .await;
    let repaired = commit_submission(
        &fixture.uow,
        &plan(&fixture, "archive-a", "discarded-retry-id", Utc::now()),
        5,
    )
    .await
    .expect("repair");
    assert_eq!(repaired.archive_ids, inserted.archive_ids);
    let after = query_wire(&fixture, &archive).await;
    assert_eq!(
        before
            .iter()
            .map(|row| (&row.id, row.timestamp))
            .collect::<Vec<_>>(),
        after
            .iter()
            .map(|row| (&row.id, row.timestamp))
            .collect::<Vec<_>>()
    );
    assert_eq!(fixture.count("ingress_messages").await, 2);
    let mut tx = fixture.uow.begin().await.expect("tombstone transaction");
    MamArchiveRepository::replace_with_tombstone(
        &mut tx,
        &archive,
        &StanzaId::new("archive-a-id", archive.clone().into()),
        &ArchivedTombstone {
            retraction_id: None,
            stamp: Utc::now(),
            moderation: None,
            sender_scope: None,
        },
    )
    .await
    .expect("retract");
    tx.commit().await.expect("commit tombstone");
    let swallowed = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("tombstone retry");
    assert_eq!(swallowed.archive_ids, inserted.archive_ids);
    let rows = query_wire(&fixture, &archive).await;
    assert_eq!(rows.len(), 2);
    assert!(rows[0].body.is_none());
    assert_eq!(
        fixture
            .optional_text("SELECT body FROM mam_messages WHERE id = 'archive-a-id'")
            .await,
        None
    );
    assert_eq!(fixture.count("mam_messages").await, 2);
    fixture.close().await;
}

/// XEP-0313 §4.1 and §6.3: personal archives assign independently scoped UIDs.
async fn distinct_archives(fixture: IngressFixture) {
    let sender = fixture.principal.bare_jid().clone();
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    let stamp = Utc::now();
    let mut submission = plan(&fixture, "two-archives", "sender-id", stamp);
    add_archive(&mut submission, &recipient, "recipient-id", stamp);
    let committed = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("headless archives");
    assert_eq!(committed.archive_ids.len(), 2);
    let sender_rows = query_wire(&fixture, &sender).await;
    let recipient_rows = query_wire(&fixture, &recipient).await;
    assert_eq!(sender_rows[0].id, "sender-id");
    assert_eq!(recipient_rows[0].id, "recipient-id");
    assert_eq!(sender_rows[0].body, recipient_rows[0].body);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("mam_messages").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

/// XEP-0313 §5.1.3: reconciliation preserves assigned identity while repairing omitted intents.
async fn reconciliation(fixture: IngressFixture) {
    use waddle_xmpp::ingress::EffectMessageIdentity;
    let original = plan(&fixture, "reconcile", "recorded-id", Utc::now());
    let first = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("first");
    assert_eq!(first.verdict, Some(ReconcileVerdict::FirstCommit));
    let consistent = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("consistent");
    assert_eq!(consistent.verdict, Some(ReconcileVerdict::Consistent));
    let mut repaired_plan = original.clone();
    repaired_plan
        .plan
        .intents
        .push(IngressEffectIntent::RouteDirect {
            recipient: "juliet@example.com".parse().expect("recipient"),
            fanout: vec!["juliet@example.com/phone".parse().expect("phone")],
            route_identity: EffectMessageIdentity::capture_ordinal(0),
        });
    let repaired = commit_submission(&fixture.uow, &repaired_plan, 5)
        .await
        .expect("repaired");
    assert_eq!(repaired.class, IngressDecisionClass::ExistingRepaired);
    assert!(
        matches!(&repaired.verdict, Some(ReconcileVerdict::Repaired { inserted }) if inserted.len() == 1)
    );
    let mut divergent_plan = repaired_plan;
    if let IngressEffectIntent::RouteDirect { fanout, .. } = &mut divergent_plan.plan.intents[1] {
        *fanout = vec!["juliet@example.com/laptop".parse().expect("laptop")];
    }
    let divergent = commit_submission(&fixture.uow, &divergent_plan, 5)
        .await
        .expect("recorded audience wins");
    assert_eq!(divergent.class, IngressDecisionClass::ExistingDivergent);
    assert!(
        matches!(&divergent.verdict, Some(ReconcileVerdict::Divergent { kinds }) if kinds.len() == 1)
    );
    let mut tx = fixture.uow.begin().await.expect("read recorded audience");
    let recorded = waddle_server::ingress_uow::EffectIntentRepository::load(
        &mut tx,
        first.message_key.expect("key"),
    )
    .await
    .expect("recorded intents");
    assert!(recorded.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { fanout, .. } if fanout == &vec!["juliet@example.com/phone".parse::<jid::FullJid>().expect("phone")])));
    tx.commit().await.expect("close recorded audience read");

    let mut contradiction = plan(&fixture, "contradiction", "contradiction-id", Utc::now());
    let mut contradictory_id = contradiction.plan.intents[0].clone();
    if let IngressEffectIntent::ArchiveAuthoritative { stanza_id, .. } = &mut contradictory_id {
        stanza_id.id = "other-id".to_owned();
    }
    contradiction.plan.intents.push(contradictory_id);
    let failure = commit_submission(&fixture.uow, &contradiction, 5)
        .await
        .expect_err("immutable contradiction rolls back");
    assert_eq!(failure.class(), IngressDecisionClass::IntentContradiction);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_effect_intents").await, 2);
    let rows = query_wire(&fixture, fixture.principal.bare_jid()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "recorded-id");
    fixture.close().await;
}

/// XEP-0313 §6.3: durable archive identity outlives incomplete delivery and canonical retention.
async fn receipts_retention(fixture: IngressFixture) {
    use waddle_server::ingress::{
        execute::{execute_effects, terminalize_if_complete},
        Deps, ExternalEffect, ImmediateSink,
    };
    use waddle_xmpp::{ingress::FrozenStanzaError, registry::ConnectionRegistry, Stanza};
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};
    let mut submission = plan(
        &fixture,
        "receipt-origin",
        "retained-archive-id",
        Utc::now(),
    );
    let error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::NotAcceptable,
        "en",
        "reply",
    );
    let mut reply = submission.plan.sanitized_message.clone();
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.to = reply.from.take();
    reply.payloads.push(error.clone().into());
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ErrorReply {
            recipient: submission.sender.clone(),
            error: FrozenStanzaError::from_xmpp(&error).expect("typed error"),
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(ExternalEffect::Frame(
            Box::new(Stanza::Message(reply)),
        ))));
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("partial"));
    assert_eq!(
        collect(
            &fixture,
            Utc::now() + ALIAS_RETENTION + chrono::Duration::days(1)
        )
        .await,
        0
    );
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let mut report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.frame_obligations.len(), 1);
    let Stanza::Message(frame) = &report.frame_obligations[0].frames[0] else {
        panic!("reply frame");
    };
    let xml: minidom::Element = frame.clone().into();
    assert!(xml
        .get_child("error", xmpp_parsers::ns::JABBER_CLIENT)
        .expect("wire error")
        .get_child("not-acceptable", xmpp_parsers::ns::XMPP_STANZAS)
        .is_some());
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
    assert!(report
        .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(5))
        .await
        .expect("frame written"));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(collect(&fixture, Utc::now()).await, 0);
    assert_eq!(
        collect(
            &fixture,
            Utc::now() + ALIAS_RETENTION + chrono::Duration::days(1)
        )
        .await,
        1
    );
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(
        query_wire(&fixture, fixture.principal.bare_jid()).await[0].id,
        "retained-archive-id"
    );
    fixture.close().await;
}

async fn collect(fixture: &IngressFixture, now: DateTime<Utc>) -> usize {
    gc_expired_aliases(
        &fixture.db,
        now,
        AliasGcBudget {
            deadline: tokio::time::Instant::now() + Duration::from_secs(10),
            lock_timeout: Duration::from_millis(100),
            statement_timeout: Duration::from_secs(1),
            scan_timeout: Duration::from_secs(1),
            progress: AliasGcProgress::default(),
        },
    )
    .await
    .expect("GC")
    .deleted_messages
}

macro_rules! backend_tests {
    ($sqlite:ident, $postgres:ident, $case:ident) => {
        #[tokio::test]
        async fn $sqlite() {
            $case(IngressFixture::sqlite().await).await;
        }
        #[tokio::test]
        async fn $postgres() {
            if let Some(fixture) = IngressFixture::postgres(stringify!($case)).await {
                $case(fixture).await;
            }
        }
    };
}
backend_tests!(
    identity_repair_tombstone_sqlite,
    identity_repair_tombstone_postgres,
    identity_repair_tombstone
);
backend_tests!(
    distinct_archives_sqlite,
    distinct_archives_postgres,
    distinct_archives
);
backend_tests!(
    reconciliation_sqlite,
    reconciliation_postgres,
    reconciliation
);
backend_tests!(
    receipts_retention_sqlite,
    receipts_retention_postgres,
    receipts_retention
);

/// XEP-0313 §3.2 archive order: replaying A after B cannot rewind the inbox's archive reference.
async fn monotonic_inbox(fixture: IngressFixture) {
    use waddle_server::ingress::{
        effects::{direct::ExternalDirectEffect, ProjectionRef},
        ExternalEffect,
    };
    use waddle_xmpp::{
        inbox::{ConversationKind, InboxEntry},
        ingress::InboxProjectionMutation,
    };
    let stamp = Utc::now() - chrono::Duration::minutes(1);
    let projection = ProjectionRef(1);
    let make = |origin, id, stamp: DateTime<Utc>| {
        let mut submission = plan(&fixture, origin, id, stamp);
        let owner = fixture.principal.bare_jid().clone();
        let entry = InboxEntry::new(
            "juliet@example.com".parse().expect("peer"),
            ConversationKind::Direct,
            id,
            stamp.timestamp(),
        );
        submission
            .plan
            .intents
            .push(IngressEffectIntent::InboxProject {
                owner: owner.clone(),
                mutation: InboxProjectionMutation::Direct {
                    entry: entry.clone(),
                    increment_unread: true,
                },
            });
        submission
            .plan
            .plan
            .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
                DurableDirectEffect::ProjectInbox {
                    owner: owner.clone(),
                    entry: Box::new(entry),
                    increment_unread: true,
                },
            ))));
        submission
            .plan
            .plan
            .push(PlannedEffect::new(Effect::External(
                ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
                    owner,
                    projection,
                    receipt: None,
                }),
            )));
        submission
    };
    let a = make("inbox-a", "inbox-a-id", stamp);
    commit_submission(&fixture.uow, &a, 5).await.expect("A");
    let b = commit_submission(
        &fixture.uow,
        &make(
            "inbox-b",
            "inbox-b-id",
            stamp + chrono::Duration::seconds(10),
        ),
        5,
    )
    .await
    .expect("B");
    let retry = commit_submission(&fixture.uow, &a, 5)
        .await
        .expect("retry A");
    assert_eq!(
        retry.applied_durable.inbox(projection),
        b.applied_durable.inbox(projection)
    );
    assert_eq!(
        retry
            .applied_durable
            .inbox(projection)
            .expect("projection")
            .unread,
        2
    );
    assert_eq!(fixture.count("ingress_deliveries").await, 2);
    assert_eq!(fixture.count("mam_messages").await, 2);
    assert_eq!(fixture.count("inbox_entries WHERE unread = 2").await, 1);
    assert_eq!(
        fixture
            .optional_text("SELECT last_stanza_id FROM inbox_entries")
            .await,
        Some(
            b.applied_durable
                .inbox(projection)
                .expect("B projection")
                .last_stanza_id
                .clone()
        )
    );
    assert_inbox_wire(&fixture, &retry).await;
    fixture.close().await;
}

async fn assert_inbox_wire(
    fixture: &IngressFixture,
    decision: &waddle_server::ingress::IngressDecision,
) {
    use kameo::actor::Spawn;
    use waddle_server::ingress::{execute::execute_effects, Deps, ImmediateSink};
    use waddle_xmpp::{
        registry::{ConnectionRegistry, RegisterUserResource, UserRegistryActor},
        xep::xep0430::{parse_inbox_entry_with_metadata, NS_INBOX, NS_WADDLE_INBOX},
        Stanza,
    };
    let registry = ConnectionRegistry::new();
    let users = UserRegistryActor::spawn(UserRegistryActor::new());
    let resource = fixture
        .principal
        .bare_jid()
        .with_resource_str("phone")
        .expect("resource");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    registry.register_with_carbons(resource.clone(), sender, false);
    users
        .ask(RegisterUserResource {
            entry: registry.get_entry(&resource).expect("entry"),
            jid: resource,
        })
        .await
        .expect("register");
    let mut deps = Deps::new(&registry, "example.com");
    deps.user_registry = Some(&users);
    execute_effects(
        &fixture.uow,
        &fixture.db,
        decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    let Stanza::Message(message) = receiver.try_recv().expect("wire push").stanza else {
        panic!("message");
    };
    let push = message
        .payloads
        .iter()
        .find(|payload| payload.is("push", NS_WADDLE_INBOX))
        .expect("push");
    let entry = parse_inbox_entry_with_metadata(
        push.get_child("entry", NS_INBOX).expect("entry"),
        push.get_child("metadata", NS_WADDLE_INBOX),
    )
    .expect("decode push");
    assert_eq!(entry.unread, 2);
    assert_eq!(
        &entry,
        decision
            .applied_durable
            .inbox(waddle_server::ingress::effects::ProjectionRef(1))
            .expect("committed projection")
    );
    assert!(receiver.try_recv().is_err());
}
backend_tests!(
    monotonic_inbox_sqlite,
    monotonic_inbox_postgres,
    monotonic_inbox
);
