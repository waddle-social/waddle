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
    // XEP-0045 §9.2: replaying an earlier membership grant cannot demote
    // an administrator appointed after the original invitation committed.
    actor
        .ask(ChangeAffiliation {
            jid: recipient.clone(),
            affiliation: waddle_xmpp::Affiliation::Admin,
        })
        .await
        .expect("promote invitee before retry");
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
        waddle_xmpp::Affiliation::Admin
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

/// XEP-0359 §3: a client cannot assign the room's stable stanza identity.
async fn forged_room_stamp(fixture: IngressFixture) {
    let state = create_test_websocket_state().await;
    create_test_session(state.as_ref(), "romeo").await;
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("sender");
    let room: jid::BareJid = "stamp@muc.example.com".parse().expect("room");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "stamp".to_owned(),
            channel_id: "stamp".to_owned(),
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
        .expect("join sender");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    register_test_connection(state.as_ref(), &sender, tx).await;
    let peer: jid::FullJid = "juliet@example.com/phone".parse().expect("peer");
    create_test_session(state.as_ref(), "juliet").await;
    actor
        .ask(JoinWithAffiliation {
            sender_jid: peer.clone(),
            nick: "juliet".to_owned(),
            affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Member),
            local_domain: "example.com".to_owned(),
            admission_revision: 0,
            session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        })
        .await
        .expect("join peer");
    let (peer_tx, mut peer_rx) = tokio::sync::mpsc::channel(8);
    register_test_connection(state.as_ref(), &peer, peer_tx).await;
    let mut submission = fixture.submission(Some("forged-room-origin"), "hello");
    submission.target = NormalizedTarget::Bare(room.clone());
    let client_id = xmpp_parsers::message::Id("client-wire-id".to_owned());
    let incoming = &mut submission.plan.sanitized_message;
    incoming.id = Some(client_id.clone());
    incoming.to = Some(room.clone().into());
    incoming.type_ = xmpp_parsers::message::MessageType::Groupchat;
    let forged = waddle_xmpp_core::xep0359::StanzaId::new("client-forged", room.clone().into());
    waddle_xmpp_core::xep0359::add_stanza_id(incoming, &forged);
    let domain: jid::BareJid = "muc.example.com".parse().expect("domain");
    submission.digest_input = crate::ingress::submission::digest_input(
        incoming,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: crate::ingress::submission::digest_authorities(
                incoming,
                fixture.principal.bare_jid(),
                domain.domain(),
            ),
            stanza_lang: None,
        },
    )
    .expect("digest ignores forged room identity");
    let mut dispatcher = waddle_xmpp::protocol::StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut machine = waddle_xmpp::protocol::XmppStateMachine::new("example.com", dispatcher);
    machine.transition_to_ready(sender, false);
    let deps = build_interpret_deps(state.as_ref(), None);
    submission.plan = crate::server::plan_message_dispatch(
        &mut machine,
        submission.plan.sanitized_message,
        &deps,
    )
    .await;
    assert!(
        !waddle_xmpp_core::xep0359::extract_stanza_ids(&submission.plan.sanitized_message)
            .contains(&forged)
    );
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit sanitized room message");
    assert_eq!(
        decision.class,
        crate::ingress::IngressDecisionClass::Accepted
    );
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    let room_id = decision
        .archive_ids
        .iter()
        .find(|(archive, _)| archive == &room)
        .expect("room authoritative identity");
    assert_ne!(room_id.1, forged);
    assert_room_inbox_rows(&fixture, &room, &room_id.1, &client_id).await;
    assert_eq!(
        take_room_frames(&mut rx, &room, &room_id.1, &client_id, 0),
        1,
        "first sender reflection"
    );
    assert_eq!(
        take_room_frames(&mut peer_rx, &room, &room_id.1, &client_id, 1),
        1,
        "first peer fanout"
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(
        fixture
            .count("mam_messages WHERE id = 'client-forged'")
            .await,
        0
    );

    let duplicate = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("real room alias retry");
    assert_eq!(duplicate.message_key, decision.message_key);
    assert_eq!(duplicate.archive_ids, decision.archive_ids);
    let retry_report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &duplicate,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(retry_report.receipt_failures.is_empty());
    assert_room_inbox_rows(&fixture, &room, &room_id.1, &client_id).await;
    assert_eq!(
        take_room_frames(&mut rx, &room, &room_id.1, &client_id, 0),
        1,
        "duplicate sender reflection"
    );
    assert_eq!(
        take_room_frames(&mut peer_rx, &room, &room_id.1, &client_id, 1),
        0,
        "duplicate must not fan out groupchat to non-sender"
    );
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    let mut tx = fixture.uow.begin().await.expect("read canonical envelope");
    assert_eq!(
        crate::ingress_uow::CanonicalMessageRepository::load_envelope(
            &mut tx,
            decision.message_key.expect("key")
        )
        .await
        .expect("envelope"),
        Some(
            crate::ingress_substrate::MessageEnvelope::new(submission.plan.sanitized_message)
                .expect("typed envelope")
        )
    );
    tx.commit().await.expect("read complete");
    fixture.close().await;
}

/// XEP-0359 §3: strip a room-authority stamp before local room acceptance.
#[tokio::test]
async fn ingress_forged_room_stamp_sqlite() {
    forged_room_stamp(IngressFixture::sqlite().await).await;
}
/// XEP-0359 §3: PostgreSQL preserves the same trusted room stamp on wire and rows.
#[tokio::test]
async fn ingress_forged_room_stamp_postgres() {
    if let Some(fixture) = IngressFixture::postgres("forged_room_stamp").await {
        forged_room_stamp(fixture).await;
    }
}

/// Consume one completed execution's output without assuming inbox and groupchat
/// ordering. Replayed inbox projections are idempotent; groupchat fanout is not.
fn take_room_frames(
    receiver: &mut tokio::sync::mpsc::Receiver<waddle_xmpp::registry::OutboundStanza>,
    room: &jid::BareJid,
    stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    client_id: &xmpp_parsers::message::Id,
    unread: u32,
) -> usize {
    use waddle_xmpp::xep::xep0430::{parse_inbox_entry_with_metadata, NS_INBOX, NS_WADDLE_INBOX};
    let mut groupchats = 0;
    while let Ok(outbound) = receiver.try_recv() {
        let waddle_xmpp::Stanza::Message(message) = outbound.stanza else {
            panic!("expected message")
        };
        let mut bytes = Vec::new();
        minidom::Element::from(message)
            .write_to(&mut bytes)
            .expect("wire serialization");
        let wire = xmpp_parsers::message::Message::try_from(
            minidom::Element::from_reader(bytes.as_slice()).expect("wire XML"),
        )
        .expect("wire message");
        if wire.type_ == xmpp_parsers::message::MessageType::Groupchat {
            assert_eq!(
                wire.bodies
                    .get(&xmpp_parsers::message::Lang::new())
                    .map(String::as_str),
                Some("hello")
            );
            assert_eq!(wire.id.as_ref(), Some(client_id));
            let ids = waddle_xmpp_core::xep0359::extract_stanza_ids(&wire);
            assert!(ids.contains(stanza_id));
            assert!(!ids.iter().any(|id| id.id == "client-forged"));
            groupchats += 1;
        } else {
            assert_eq!(wire.type_, xmpp_parsers::message::MessageType::Headline);
            let push = wire
                .payloads
                .iter()
                .find(|payload| payload.is("push", NS_WADDLE_INBOX))
                .expect("only inbox pushes may accompany groupchat");
            let entry = parse_inbox_entry_with_metadata(
                push.get_child("entry", NS_INBOX).expect("inbox entry"),
                push.get_child("metadata", NS_WADDLE_INBOX),
            )
            .expect("typed inbox push");
            assert_eq!(&entry.partner, room);
            assert_eq!(entry.last_stanza_id, client_id.0);
            assert_eq!(entry.unread, unread);
        }
    }
    groupchats
}

async fn assert_room_inbox_rows(
    fixture: &IngressFixture,
    room: &jid::BareJid,
    stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    client_id: &xmpp_parsers::message::Id,
) {
    use waddle_xmpp::inbox::storage::InboxStorage;
    let inbox = crate::inbox::DatabaseInboxStorage::from_database(fixture.db.clone())
        .await
        .expect("inbox reader");
    for (owner, unread) in [
        (fixture.principal.bare_jid().clone(), 0),
        ("juliet@example.com".parse().expect("peer"), 1),
    ] {
        let entry = inbox
            .list(&owner)
            .await
            .expect("stored inbox")
            .into_iter()
            .find(|entry| &entry.partner == room)
            .expect("room projection");
        assert_eq!(entry.last_stanza_id, client_id.0);
        assert_eq!(entry.unread, unread);
        let connection = fixture.db.guard().await.expect("archive reference read");
        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ? AND id = ?",
                crate::db_params![room.to_string(), stanza_id.id.clone()],
            )
            .await
            .expect("resolve projected archive reference");
        let count: i64 = rows
            .next()
            .await
            .expect("count row")
            .expect("count")
            .get(0)
            .expect("integer count");
        assert_eq!(
            count, 1,
            "trusted archive identity resolves under the room assigning authority"
        );
    }
}
