//! Infrastructure failures in the first rich-target read must never claim ingress.
use super::super::{effects::PlanFailure, Deps};
use super::plan_message_dispatch;
use crate::ingress::{
    commit::commit_submission, test_support::IngressFixture, IngressDecisionClass,
};
use std::sync::Arc;
use waddle_xmpp::{
    mam::{MamStorage, SqlxMamStorage},
    protocol::{StanzaDispatcher, XmppStateMachine},
    registry::ConnectionRegistry,
};

async fn rich_target_read(fixture: IngressFixture, retract: bool, fail_read: bool) {
    let mam: Arc<dyn MamStorage> = Arc::new(
        SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    if fail_read {
        fixture
            .execute(
                "ALTER TABLE mam_messages RENAME TO unavailable_mam_messages",
                (),
            )
            .await;
    }
    let registry = ConnectionRegistry::new();
    let recipient = "juliet@example.com/phone".parse().expect("recipient");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(recipient, tx);
    let mut deps = Deps::registry_only(&registry);
    deps.mam_storage = Some(&mam);
    let mut submission = fixture.submission(Some("rich-target-retry"), "updated");
    submission.plan.sanitized_message.payloads.push(if retract {
        waddle_xmpp::xep::xep0424::build_retract_element("original")
    } else {
        waddle_xmpp::xep::xep0308::build_replace_element("original")
    });
    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut machine = XmppStateMachine::new("example.com", dispatcher);
    machine.transition_to_ready(submission.sender.clone(), false);
    submission.plan =
        plan_message_dispatch(&mut machine, submission.plan.sanitized_message, &deps).await;
    assert!(rx.try_recv().is_err(), "Phase A cannot deliver anything");
    if fail_read {
        assert_eq!(submission.plan.failure, Some(PlanFailure::RichTargetLookup));
        let failure = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect_err("failed read must not commit an alias");
        assert_eq!(failure.class(), IngressDecisionClass::Storage);
        assert!(!failure.class().advances());
        for table in [
            "ingress_messages",
            "ingress_origin_aliases",
            "ingress_effect_intents",
            "ingress_effect_receipts",
            "ingress_sm_refs",
            "ingress_sm_streams",
            "inbox_entries",
        ] {
            assert_eq!(fixture.count(table).await, 0, "{table}");
        }
        assert_eq!(fixture.count("unavailable_mam_messages").await, 0);
    } else {
        assert_eq!(submission.plan.failure, None);
        assert!(
            submission.plan.error_reply.is_some(),
            "missing target is a semantic reply"
        );
        commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("semantic denial commits");
        assert_eq!(fixture.count("ingress_messages").await, 1);
        assert_eq!(fixture.count("mam_messages").await, 0);
    }
    assert!(rx.try_recv().is_err(), "commit cannot deliver effects");
    drop(mam);
    fixture.close().await;
}

#[tokio::test]
async fn rich_target_correction_read_failure_sqlite() {
    rich_target_read(IngressFixture::sqlite().await, false, true).await;
}

#[tokio::test]
async fn rich_target_correction_read_failure_postgres() {
    if let Some(fixture) = IngressFixture::postgres("rich_correction").await {
        rich_target_read(fixture, false, true).await;
    }
}

#[tokio::test]
async fn rich_target_retraction_read_failure_sqlite() {
    rich_target_read(IngressFixture::sqlite().await, true, true).await;
}

#[tokio::test]
async fn rich_target_retraction_read_failure_postgres() {
    if let Some(fixture) = IngressFixture::postgres("rich_retraction").await {
        rich_target_read(fixture, true, true).await;
    }
}

#[tokio::test]
async fn rich_target_missing_target_commits_semantic_denial_sqlite() {
    rich_target_read(IngressFixture::sqlite().await, false, false).await;
}

#[tokio::test]
async fn rich_target_missing_target_commits_semantic_denial_postgres() {
    if let Some(fixture) = IngressFixture::postgres("rich_missing").await {
        rich_target_read(fixture, false, false).await;
    }
}

async fn groupchat_rich_target_failure(fixture: IngressFixture, stop_actor: bool) {
    use super::super::{
        effects::{EffectSink, PlanSink},
        groupchat_validation::validate_groupchat_rich_targets,
    };
    use kameo::actor::Spawn;
    use waddle_xmpp::muc::{
        room_actor::Join,
        room_registry_actor::{CreateRoom, RoomRegistryActor},
    };
    let room: jid::BareJid = "rich@muc.example.com".parse().expect("room");
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("sender");
    let nick: jid::Jid = room.with_resource_str("romeo").expect("nickname").into();
    let rooms = RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".into(),
        waddle_xmpp::xep::xep0421::OccupantIdSecret::new(vec![b'r'; 32]).expect("secret"),
    ));
    let actor = rooms
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "rich".into(),
            channel_id: "rich".into(),
            config: Default::default(),
        })
        .await
        .expect("create room");
    actor
        .ask(Join {
            nick: "romeo".into(),
            real_jid: sender.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("join");
    let mam: Arc<dyn MamStorage> = Arc::new(
        SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    if stop_actor {
        let mut original =
            waddle_xmpp::mam::ArchivedMessage::for_test(nick.clone(), room.clone().into());
        original.id = "archive-original".into();
        original.stanza_id = Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "original",
            room.clone().into(),
        ));
        original.message_type = xmpp_parsers::message::MessageType::Groupchat;
        original.nickname_generation = Some(1);
        mam.store_message(&room, &original).await.expect("original");
        actor.stop_gracefully().await.expect("stop actor");
        actor.wait_for_shutdown().await;
    } else {
        fixture
            .execute(
                "ALTER TABLE mam_messages RENAME TO unavailable_mam_messages",
                (),
            )
            .await;
    }
    let registry = ConnectionRegistry::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    registry.register(sender.clone(), tx);
    let mut deps = Deps::registry_only(&registry);
    deps.mam_storage = Some(&mam);
    deps.room_registry = Some(&rooms);
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    deps.web_socket_state = Some(state.as_ref());
    let mut submission = fixture.submission(Some("room-rich-retry"), "corrected");
    submission.target = waddle_xmpp::ingress::NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission
        .plan
        .sanitized_message
        .payloads
        .push(waddle_xmpp::xep::xep0308::build_replace_element("original"));
    if stop_actor {
        // Exercise the first validator's per-nickname fallback ask with a real
        // stopped actor and a successfully loaded sender-owned archive row.
        let sink = PlanSink::new();
        let capture = crate::ingress::IngressEffectCapture::new();
        sink.observe_message(&submission.plan.sanitized_message);
        let planned = super::build_plan_deps(&deps, &sink);
        validate_groupchat_rich_targets(
            &planned,
            &room,
            &submission.plan.sanitized_message,
            Some(&nick),
            &actor,
            None,
        )
        .await
        .expect_err("stopped actor cannot validate occupancy");
        submission.plan = super::finish_plan(
            &sink,
            &capture,
            submission.plan.sanitized_message,
            Some(sender),
        );
    } else {
        let mut dispatcher = StanzaDispatcher::new();
        waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
        let mut machine = XmppStateMachine::new("example.com", dispatcher);
        machine.transition_to_ready(sender, false);
        submission.plan =
            plan_message_dispatch(&mut machine, submission.plan.sanitized_message, &deps).await;
    }
    assert_eq!(submission.plan.failure, Some(PlanFailure::RichTargetLookup));
    let failure = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect_err("incomplete rich target validation must fail closed");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    assert!(!failure.class().advances());
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_sm_refs",
        "ingress_sm_streams",
        "ingress_deliveries",
        "inbox_entries",
    ] {
        assert_eq!(fixture.count(table).await, 0, "{table}");
    }
    assert_eq!(
        fixture
            .count(if stop_actor {
                "mam_messages"
            } else {
                "unavailable_mam_messages"
            })
            .await,
        i64::from(stop_actor)
    );
    assert!(rx.try_recv().is_err(), "failed plan emits no sender reply");
    drop(mam);
    fixture.close().await;
}

#[tokio::test]
async fn rich_target_room_archive_read_failure_sqlite() {
    groupchat_rich_target_failure(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn rich_target_room_archive_read_failure_postgres() {
    if let Some(fixture) = IngressFixture::postgres("rich_room_read").await {
        groupchat_rich_target_failure(fixture, false).await;
    }
}

#[tokio::test]
async fn rich_target_room_actor_failure_sqlite() {
    groupchat_rich_target_failure(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn rich_target_room_actor_failure_postgres() {
    if let Some(fixture) = IngressFixture::postgres("rich_room_actor").await {
        groupchat_rich_target_failure(fixture, true).await;
    }
}
