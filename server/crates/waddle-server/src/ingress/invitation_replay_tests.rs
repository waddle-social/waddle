//! Execute the actual XEP-0045 invitation plan across the ingress boundary.
use crate::ingress::{
    commit::commit_submission,
    effects::{Effect, ExternalEffect, PlanSink},
    execute::execute_effects,
    test_support::IngressFixture,
    ImmediateSink, IngressPlan, IngressStreamIdentity,
};
use crate::server::routes::websocket::{
    interpret_loop::build_interpret_deps,
    tests::{create_test_session, create_test_websocket_state, register_test_connection},
};
use waddle_xmpp::{
    ingress::{
        DigestContext, DigestInput, IngressEffectIntent, NormalizedTarget, WireHandledCount,
    },
    muc::{
        room_actor::{ChangeAffiliation, GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation},
        room_registry_actor::CreateRoom,
        RoomConfig,
    },
    pending_delivery::SmSessionId,
};

async fn invitation_plan_commit_execute(fixture: IngressFixture) {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "romeo").await;
    create_test_session(state.as_ref(), "juliet").await;
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("sender");
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    let room: jid::BareJid = "retry-invite@muc.example.com".parse().expect("room");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "invite".to_owned(),
            channel_id: "retry".to_owned(),
            config: RoomConfig::default(),
        })
        .await
        .expect("room");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: sender.clone(),
            nick: "romeo".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Member),
            local_domain: "example.com".to_owned(),
            admission_revision: 0,
            session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        })
        .await
        .expect("join inviter");
    actor
        .ask(ChangeAffiliation {
            jid: sender.to_bare(),
            affiliation: waddle_xmpp::Affiliation::Admin,
        })
        .await
        .expect("admin inviter");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    register_test_connection(state.as_ref(), &resource, tx).await;
    let mut submission = fixture.submission(Some("actual-invite-retry"), "");
    let mut message = submission.plan.sanitized_message.clone();
    message.to = Some(room.clone().into());
    message.bodies.clear();
    message.type_ = xmpp_parsers::message::MessageType::Normal;
    let ns = waddle_xmpp::muc::presence::NS_MUC_USER;
    message.payloads.push(
        minidom::Element::builder("x", ns)
            .append(
                minidom::Element::builder("invite", ns)
                    .attr(
                        minidom::rxml::xml_ncname!("to").to_owned(),
                        recipient.clone(),
                    )
                    .build(),
            )
            .build(),
    );
    let sink = PlanSink::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps = build_interpret_deps(state.as_ref(), None)
        .with_ingress_effect_capture(Some(capture.clone()));
    deps.effects = &sink;
    let frames = super::muc_invite::handle_muc_mediated_invite(
        &message,
        state.as_ref(),
        &sender,
        Some(&session),
        &deps,
    )
    .await
    .expect("invitation handled");
    assert!(frames.is_empty());
    let (effects, room_execution) = sink.take();
    assert_eq!(effects.len(), 3);
    assert!(matches!(
        effects[0].effect,
        Effect::External(ExternalEffect::RoomMembershipMutation(_))
    ));
    assert!(matches!(
        effects[1].effect,
        Effect::External(ExternalEffect::InviteLedger(_))
    ));
    assert!(matches!(
        effects[2].effect,
        Effect::External(ExternalEffect::RouteToPeer(_))
    ));
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .get_affiliation(&recipient),
        waddle_xmpp::Affiliation::None
    );
    assert!(rx.try_recv().is_err());
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![sender.to_bare(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    submission.plan = IngressPlan {
        plan: effects,
        intents: capture.snapshot().intents,
        sanitized_message: message,
        error_reply: None,
        rejection: None,
        room_execution,
    };
    assert!(!submission
        .plan
        .intents
        .iter()
        .any(|intent| matches!(intent, IngressEffectIntent::ArchiveAuthoritative { .. })));
    let stream_id = SmSessionId::new("actual-invitation-stream");
    let mut tx = fixture.uow.begin().await.expect("stream transaction");
    let sm_ingress_id = crate::ingress_uow::SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("stream");
    tx.commit().await.expect("mint stream");
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
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit invitation plan");
    let immediate_deps = build_interpret_deps(state.as_ref(), None);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &immediate_deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert!(
        report
            .outcomes
            .iter()
            .all(|(_, outcome)| *outcome == crate::ingress::ExternalOutcome::Done),
        "{report:?}"
    );
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .get_affiliation(&recipient),
        waddle_xmpp::Affiliation::Member
    );
    let delivered = rx.try_recv().expect("committed invitation delivered");
    let waddle_xmpp::Stanza::Message(delivered) = delivered.stanza else {
        panic!("message");
    };
    assert_eq!(delivered.from, Some(room.clone().into()));
    let invites = crate::server::routes::websocket::muc_invites::list_invites(
        state.deps.app_state.db_pool.global_actor().clone(),
        &room,
        &recipient,
    )
    .await
    .expect("ledger");
    assert_eq!(invites.len(), 1);
    if let IngressStreamIdentity::Resumable {
        reserved_wire_position,
        checkpoint_h,
        ..
    } = &mut submission.identity
    {
        *reserved_wire_position = WireHandledCount::from_storage(2);
        *checkpoint_h = WireHandledCount::from_storage(2);
    }
    let retry = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("archive-free actual invitation retry");
    assert_eq!(retry.message_key, first.message_key);
    assert!(retry.class.advances());
    assert!(retry.archive_ids.is_empty());
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &immediate_deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .get_affiliation(&recipient),
        waddle_xmpp::Affiliation::Member
    );
    assert_eq!(
        crate::server::routes::websocket::muc_invites::list_invites(
            state.deps.app_state.db_pool.global_actor().clone(),
            &room,
            &recipient
        )
        .await
        .expect("ledger")
        .len(),
        1
    );
    assert!(
        rx.try_recv().is_err(),
        "retry suppresses duplicate invitation"
    );
    let mut tx = fixture.uow.begin().await.expect("read checkpoint");
    assert_eq!(
        crate::ingress_uow::SmIngressStreamRepository::load_stream_checkpoint(
            &mut tx,
            sm_ingress_id
        )
        .await
        .expect("checkpoint"),
        Some(WireHandledCount::from_storage(2))
    );
    tx.commit().await.expect("read complete");
    assert_eq!(fixture.count("ingress_sm_refs").await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_actual_invitation_plan_replay_sqlite() {
    invitation_plan_commit_execute(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_actual_invitation_plan_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("actual_invitation_replay").await {
        invitation_plan_commit_execute(fixture).await;
    }
}
