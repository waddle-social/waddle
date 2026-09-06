use super::*;

fn inbox_push_plan(
    fixture: &IngressFixture,
    origin: &str,
    id: &str,
    timestamp: i64,
) -> IngressSubmission {
    use waddle_server::ingress::{
        effects::{direct::ExternalDirectEffect, ProjectionRef},
        ExternalEffect,
    };
    use waddle_xmpp::{
        inbox::{ConversationKind, InboxEntry},
        ingress::InboxProjectionMutation,
    };
    let mut submission = archive_plan(fixture, Some(origin), "inbox body", id);
    let owner = fixture.principal.bare_jid().clone();
    let entry = InboxEntry::new(
        "juliet@example.com".parse().expect("peer"),
        ConversationKind::Direct,
        id,
        timestamp,
    );
    assert_eq!(entry.unread, 0);
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
    let projection = ProjectionRef(submission.plan.plan.len());
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
            ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate { owner, projection }),
        )));
    submission
}

async fn committed_inbox_projection(fixture: IngressFixture) {
    use waddle_server::ingress::{
        effects::{direct::ExternalDirectEffect, ProjectionRef},
        ExternalEffect,
    };
    let first_plan = inbox_push_plan(&fixture, "inbox-first", "inbox-first-id", 10);
    let first = commit_submission(&fixture.uow, &first_plan, 5)
        .await
        .expect("initial inbox commit");
    assert_pushed_entry(&fixture, &first, 1).await;
    let projection = ProjectionRef(1);
    assert_eq!(
        first
            .applied_durable
            .inbox(projection)
            .expect("committed entry")
            .unread,
        1
    );
    assert!(
        matches!(&first.external[0], ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate { projection: actual, .. }) if *actual == projection)
    );
    let duplicate = commit_submission(&fixture.uow, &first_plan, 5)
        .await
        .expect("duplicate inbox commit");
    assert_eq!(
        duplicate
            .applied_durable
            .inbox(projection)
            .expect("duplicate committed entry")
            .unread,
        1
    );
    let later = commit_submission(
        &fixture.uow,
        &inbox_push_plan(&fixture, "inbox-later", "inbox-later-id", 20),
        5,
    )
    .await
    .expect("later inbox commit");
    let later_entry = later
        .applied_durable
        .inbox(projection)
        .expect("later committed entry");
    assert_eq!(later_entry.unread, 2);
    let retry = commit_submission(&fixture.uow, &first_plan, 5)
        .await
        .expect("old retry reads current projection");
    let retried_entry = retry
        .applied_durable
        .inbox(projection)
        .expect("current inbox entry");
    assert_eq!(retried_entry.unread, 2);
    assert_eq!(retried_entry, later_entry);
    assert_pushed_entry(&fixture, &retry, 2).await;
    assert_eq!(fixture.count("ingress_deliveries").await, 2);

    let mut read = fixture.submission(Some("inbox-read"), "mark read");
    read.plan.intents.push(IngressEffectIntent::InboxProject {
        owner: fixture.principal.bare_jid().clone(),
        mutation: waddle_xmpp::ingress::InboxProjectionMutation::GroupchatChannelRead {
            room: "juliet@example.com".parse().expect("peer"),
        },
    });
    read.plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::MarkInboxRead {
                owner: fixture.principal.bare_jid().clone(),
                channel: "juliet@example.com".parse().expect("peer"),
                thread: None,
            },
        ))));
    read.plan.plan.push(PlannedEffect::new(Effect::External(
        ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
            owner: fixture.principal.bare_jid().clone(),
            projection: ProjectionRef(0),
        }),
    )));
    read.identity = super::replay::resumable_identity(&fixture, "marker-read-stream", 1).await;
    let marked = commit_submission(&fixture.uow, &read, 5)
        .await
        .expect("mark read commit");
    assert_eq!(
        marked
            .applied_durable
            .inbox(ProjectionRef(0))
            .expect("committed read entry")
            .unread,
        0
    );
    assert_pushed_entry(&fixture, &marked, 0).await;
    let newer = commit_submission(
        &fixture.uow,
        &inbox_push_plan(&fixture, "inbox-after-marker", "inbox-after-marker-id", 30),
        5,
    )
    .await
    .expect("new message after displayed marker");
    assert_pushed_entry(&fixture, &newer, 1).await;
    let replay = commit_submission(&fixture.uow, &read, 5)
        .await
        .expect("replay bound marker");
    assert_eq!(replay.class, IngressDecisionClass::ExistingCommitted);
    assert_eq!(replay.message_key, marked.message_key);
    assert_eq!(
        replay.applied_durable.inbox(ProjectionRef(0)),
        newer.applied_durable.inbox(projection)
    );
    assert_pushed_entry(&fixture, &replay, 1).await;

    fixture.close().await;
}

#[tokio::test]
async fn ingress_committed_inbox_projection_sqlite() {
    committed_inbox_projection(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_committed_inbox_projection_postgres() {
    if let Some(fixture) = IngressFixture::postgres("committed_inbox_projection").await {
        committed_inbox_projection(fixture).await;
    }
}

pub(super) async fn assert_pushed_entry(
    fixture: &IngressFixture,
    decision: &waddle_server::ingress::IngressDecision,
    unread: u32,
) {
    use kameo::actor::Spawn;
    use waddle_server::ingress::{execute::execute_effects, Deps, ExternalOutcome, ImmediateSink};
    use waddle_xmpp::xep::xep0430::{parse_inbox_entry_with_metadata, NS_INBOX, NS_WADDLE_INBOX};
    use waddle_xmpp::{
        registry::{ConnectionRegistry, RegisterUserResource, UserRegistryActor},
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
            entry: registry.get_entry(&resource).expect("registered entry"),
            jid: resource,
        })
        .await
        .expect("actor registration");
    let mut deps = Deps::new(&registry, "example.com");
    deps.user_registry = Some(&users);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        decision,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes.len(), 1);
    // The sink exposes no delivery receipt; inspect the actual pushed stanza.
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
    assert!(report.receipt_failures.is_empty());
    assert!(report.terminalization_failure.is_none());
    let Stanza::Message(message) = receiver.try_recv().expect("committed inbox push").stanza else {
        panic!("message push");
    };
    let push = message
        .payloads
        .iter()
        .find(|payload| payload.is("push", NS_WADDLE_INBOX))
        .expect("push wrapper");
    let entry = parse_inbox_entry_with_metadata(
        push.get_child("entry", NS_INBOX).expect("XEP-0430 entry"),
        push.get_child("metadata", NS_WADDLE_INBOX),
    )
    .expect("pushed entry");
    assert_eq!(entry.unread, unread);
    assert!(receiver.try_recv().is_err(), "one push per projection");
}
