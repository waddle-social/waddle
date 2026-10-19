//! XEP-0333 planning must preserve both channel and thread read obligations.
use super::super::{
    build_plan_deps,
    effects::{PlanFailure, PlanSink},
    interpret,
    message_plan::finish_plan,
    Deps,
};
use crate::ingress::{
    commit::commit_submission, test_support::IngressFixture, IngressDecisionClass,
    IngressEffectCapture, IngressStreamIdentity, IngressSubmission,
};
use crate::ingress_uow::SmIngressStreamRepository;
use std::sync::Arc;
use waddle_xmpp::{
    inbox::{storage::InboxStorage, ConversationKind, InboxEntry},
    ingress::WireHandledCount,
    mam::{ArchivedMessage, MamStorage, SqlxMamStorage},
    pending_delivery::SmSessionId,
    protocol::OutboundEvent,
    registry::ConnectionRegistry,
};

async fn plan_marker(submission: &mut IngressSubmission, deps: &Deps<'_>, room: &jid::BareJid) {
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let planned = build_plan_deps(deps, &sink).with_ingress_effect_capture(Some(capture.clone()));
    interpret(
        vec![OutboundEvent::MarkInboxReadFromDisplayed {
            owner: submission.sender.to_bare(),
            room: room.clone(),
            displayed_message_id: "displayed-target".into(),
        }],
        &planned,
    )
    .await;
    submission.plan = finish_plan(
        &sink,
        &capture,
        submission.plan.sanitized_message.clone(),
        Some(submission.sender.clone()),
    );
}

async fn mam_failure_is_nonadvancing_with_healthy_retry(fixture: IngressFixture) {
    let owner = fixture.principal.bare_jid().clone();
    let room: jid::BareJid = "team@conf.example.com".parse().expect("room");
    let mam: Arc<dyn MamStorage> = Arc::new(
        SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    let inbox: Arc<dyn InboxStorage> = Arc::new(
        crate::inbox::DatabaseInboxStorage::open(Some(fixture.db.database_url()))
            .await
            .expect("inbox"),
    );
    for thread in [None, Some("roadmap")] {
        let mut entry = InboxEntry::new(
            room.clone(),
            ConversationKind::MucRoom,
            "displayed-target",
            0,
        );
        if let Some(thread) = thread {
            entry = entry.with_thread(thread);
        }
        inbox.upsert(&owner, entry, true).await.expect("unread row");
    }
    let mut archived = ArchivedMessage::for_test(owner.clone().into(), room.clone().into());
    archived.stanza_id = Some(waddle_xmpp_core::xep0359::StanzaId::new(
        "displayed-target",
        room.clone().into(),
    ));
    archived.thread = Some(waddle_xmpp_core::ThreadInfo {
        id: waddle_xmpp_core::mam::ThreadId::new("roadmap").expect("thread"),
        parent: None,
    });
    mam.store_message(&room, &archived)
        .await
        .expect("archive target");
    let mut submission = fixture.submission(Some("displayed-marker-retry"), "displayed marker");
    let stream_id = SmSessionId::new("displayed-marker-mam-failure");
    let mut tx = fixture.uow.begin().await.expect("begin stream");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("mint stream");
    tx.commit().await.expect("stream commit");
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "single-node"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::new(1),
        checkpoint_h: WireHandledCount::new(1),
    };
    let registry = ConnectionRegistry::new();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);
    fixture
        .execute(
            "ALTER TABLE mam_messages RENAME TO unavailable_mam_messages",
            (),
        )
        .await;
    plan_marker(&mut submission, &deps, &room).await;
    assert_eq!(submission.plan.failure, Some(PlanFailure::RichTargetLookup));
    assert!(
        submission.plan.intents.is_empty(),
        "neither mark-read may be captured"
    );
    // Recover before commit: the failure must be sticky on the frozen plan.
    fixture
        .execute(
            "ALTER TABLE unavailable_mam_messages RENAME TO mam_messages",
            (),
        )
        .await;
    let failure = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect_err("incomplete plan");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    assert!(!failure.class().advances());
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_sm_refs",
        "ingress_deliveries",
    ] {
        assert_eq!(fixture.count(table).await, 0, "no writes to {table}");
    }
    assert_eq!(
        fixture
            .count("ingress_sm_streams WHERE handled_ordinal = 0 AND checkpoint_h = 0")
            .await,
        1
    );
    assert_eq!(inbox.list(&owner).await.expect("channels")[0].unread, 1);
    assert_eq!(
        inbox.list_threads(&owner, &room).await.expect("threads")[0].unread,
        1
    );
    plan_marker(&mut submission, &deps, &room).await;
    assert_eq!(submission.plan.failure, None);
    assert_eq!(
        submission.plan.intents.len(),
        2,
        "channel and thread obligations"
    );
    let decision = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("healthy retry commits");
    assert!(decision.class.advances());
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(
        fixture
            .count("ingress_sm_streams WHERE handled_ordinal = 1 AND checkpoint_h = 1")
            .await,
        1
    );
    assert_eq!(inbox.list(&owner).await.expect("channels")[0].unread, 0);
    assert_eq!(
        inbox.list_threads(&owner, &room).await.expect("threads")[0].unread,
        0
    );
    drop(mam);
    drop(inbox);
    fixture.close().await;
}

#[tokio::test]
async fn displayed_marker_mam_failure_nonadvancing_retry_sqlite() {
    mam_failure_is_nonadvancing_with_healthy_retry(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn displayed_marker_mam_failure_nonadvancing_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("displayed_marker_mam_failure").await {
        mam_failure_is_nonadvancing_with_healthy_retry(fixture).await;
    }
}
