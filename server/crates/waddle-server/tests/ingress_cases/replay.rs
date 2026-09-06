use super::*;
use waddle_server::ingress::{
    effects::{
        delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
        direct::ExternalDirectEffect,
        ProjectionRef,
    },
    execute::execute_effects,
    Deps, ExternalEffect, ImmediateSink, IngressStreamIdentity,
};
use waddle_server::ingress_uow::SmIngressStreamRepository;
use waddle_xmpp::{ingress::WireHandledCount, pending_delivery::SmSessionId};

pub(super) async fn resumable_identity(
    fixture: &IngressFixture,
    name: &str,
    position: u32,
) -> IngressStreamIdentity {
    let stream_id = SmSessionId::new(name);
    let mut tx = fixture.uow.begin().await.expect("stream transaction");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("mint stream");
    tx.commit().await.expect("commit stream");
    IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "unused"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::from_storage(position),
        checkpoint_h: WireHandledCount::from_storage(position),
    }
}

async fn archive_free_invitation_retry(fixture: IngressFixture) {
    use waddle_xmpp::ingress::{
        DigestContext, DigestInput, EffectMessageIdentity, MucInviteLedgerAction,
        MucInviteLedgerMutation, MucInviteMembershipGrant, NormalizedTarget,
    };
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let invitee: jid::BareJid = "juliet@example.com".parse().expect("invitee");
    let recipient: jid::FullJid = "juliet@example.com/web".parse().expect("resource");
    let mut submission = fixture.submission(Some("invite-retry"), "");
    submission.plan.sanitized_message.bodies.clear();
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Normal;
    submission.plan.sanitized_message.to = Some(room.clone().into());
    let ns = waddle_xmpp::muc::presence::NS_MUC_USER;
    submission.plan.sanitized_message.payloads.push(
        minidom::Element::builder("x", ns)
            .append(
                minidom::Element::builder("invite", ns)
                    .attr(minidom::rxml::xml_ncname!("to").to_owned(), invitee.clone())
                    .build(),
            )
            .build(),
    );
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("invitation digest");
    submission.identity = resumable_identity(&fixture, "invitation-stream", 1).await;
    submission.plan.intents = vec![
        IngressEffectIntent::MucInviteMembershipGrant {
            grant: MucInviteMembershipGrant {
                room: room.clone(),
                invitee: invitee.clone(),
                inviter: fixture.principal.bare_jid().clone(),
            },
        },
        IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room,
                invitee: invitee.clone(),
                inviter: fixture.principal.bare_jid().clone(),
                action: MucInviteLedgerAction::Recorded,
                recorded_at: Some(chrono::Utc::now()),
            },
        },
        IngressEffectIntent::RouteDirect {
            recipient: invitee,
            fanout: vec![recipient.clone()],
            route_identity: EffectMessageIdentity::OriginId(
                submission.digest_input.origin().cloned().expect("origin"),
            ),
        },
    ];
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                jid: recipient.clone(),
                stanza: Box::new(waddle_xmpp::Stanza::Message(
                    submission.plan.sanitized_message.clone(),
                )),
                kind: PeerDeliveryKind::RegistryFrame,
                call_setup: None,
            }),
        )));
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("first invitation");
    let registry = waddle_xmpp::registry::ConnectionRegistry::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    registry.register_with_carbons(recipient, sender, false);
    let deps = Deps::new(&registry, "example.com");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    receiver.try_recv().expect("invitation delivered");
    let IngressStreamIdentity::Resumable {
        reserved_wire_position,
        checkpoint_h,
        sm_ingress_id,
        ..
    } = &mut submission.identity
    else {
        panic!("resumable");
    };
    let stream = *sm_ingress_id;
    *reserved_wire_position = WireHandledCount::from_storage(2);
    *checkpoint_h = WireHandledCount::from_storage(2);
    let retry = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("archive-free retry advances");
    assert!(retry.class.advances());
    assert_eq!(retry.message_key, first.message_key);
    assert!(retry.archive_ids.is_empty());
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_sm_refs").await, 2);
    assert_eq!(fixture.count("mam_messages").await, 0);
    let mut missing_authority = submission.clone();
    missing_authority.plan.intents.extend(
        archive_plan(&fixture, None, "", "unrecorded-authority")
            .plan
            .intents,
    );
    if let IngressStreamIdentity::Resumable {
        reserved_wire_position,
        checkpoint_h,
        ..
    } = &mut missing_authority.identity
    {
        *reserved_wire_position = WireHandledCount::from_storage(3);
        *checkpoint_h = WireHandledCount::from_storage(3);
    }
    let missing = commit_submission(&fixture.uow, &missing_authority, 5)
        .await
        .expect_err("planned archive requires recorded authority");
    assert_eq!(missing.class(), IngressDecisionClass::Storage);
    assert_eq!(fixture.count("ingress_sm_refs").await, 2);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    let mut tx = fixture.uow.begin().await.expect("read stream");
    assert_eq!(
        SmIngressStreamRepository::load_stream_checkpoint(&mut tx, stream)
            .await
            .expect("checkpoint"),
        Some(WireHandledCount::from_storage(2))
    );
    tx.commit().await.expect("close read");
    fixture.close().await;
}

#[tokio::test]
async fn ingress_archive_free_invitation_retry_sqlite() {
    archive_free_invitation_retry(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_archive_free_invitation_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("archive_free_invitation_retry").await {
        archive_free_invitation_retry(fixture).await;
    }
}

fn room_projection(
    fixture: &IngressFixture,
    origin: &str,
    id: &str,
    timestamp: i64,
    thread: Option<&waddle_xmpp_core::mam::ThreadId>,
) -> IngressSubmission {
    use waddle_server::ingress::effects::room::DurableRoomEffect;
    use waddle_xmpp::{
        inbox::{ConversationKind, InboxEntry},
        ingress::InboxProjectionMutation,
    };
    let mut submission = archive_plan(fixture, Some(origin), "room message", id);
    let room: jid::BareJid = "read-room@muc.example.com".parse().expect("room");
    let stanza_id = StanzaId::new(id, room.clone().into());
    let Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
        message, ..
    })) = &submission.plan.plan[0].effect
    else {
        panic!("archive fixture");
    };
    let mut archived = *message.clone();
    archived.to = room.clone().into();
    archived.message_type = xmpp_parsers::message::MessageType::Groupchat;
    archived.stanza_id = Some(stanza_id.clone());
    submission.plan.intents[0] = IngressEffectIntent::ArchiveAuthoritative {
        archive: room.clone(),
        by: room.clone(),
        stanza_id,
        archived_at: archived.timestamp,
    };
    submission.plan.plan[0] = PlannedEffect::new(Effect::Durable(DurableEffect::Room(
        DurableRoomEffect::ArchiveGroupchat {
            room: room.clone(),
            message: Box::new(archived),
            fence: waddle_server::ingress::effects::room::RoomFenceRequirement::Unfenced,
            archive_expectation: ArchiveExpectation::Fresh,
        },
    )));
    submission.target = waddle_xmpp::ingress::NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &waddle_xmpp::ingress::DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("room message digest");
    let owner = fixture.principal.bare_jid().clone();
    let mut entry = InboxEntry::new(room.clone(), ConversationKind::MucRoom, id, timestamp);
    if let Some(thread) = thread {
        entry = entry.with_thread(thread.as_str());
    }
    let mutation = match thread {
        Some(thread_id) => InboxProjectionMutation::GroupchatThread {
            room,
            thread_id: thread_id.clone(),
        },
        None => InboxProjectionMutation::GroupchatChannel {
            room,
            increment_unread: true,
        },
    };
    submission
        .plan
        .intents
        .push(IngressEffectIntent::InboxProject {
            owner: owner.clone(),
            mutation,
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ProjectGroupchatInbox {
                owner: owner.clone(),
                entry: Box::new(entry),
                is_recipient: true,
                recovery: None,
            },
        ))));
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
                owner,
                projection: ProjectionRef(1),
            }),
        )));
    submission
}

async fn displayed_marker_replay(fixture: IngressFixture) {
    use waddle_xmpp::ingress::InboxProjectionMutation;
    for thread in [
        None,
        Some(waddle_xmpp_core::mam::ThreadId::new("discussion").expect("thread")),
    ] {
        let suffix = if thread.is_some() {
            "thread"
        } else {
            "channel"
        };
        let seed = room_projection(
            &fixture,
            &format!("seed-{suffix}"),
            &format!("seed-id-{suffix}"),
            10,
            thread.as_ref(),
        );
        let first = commit_submission(&fixture.uow, &seed, 5)
            .await
            .expect("initial room message");
        super::projections::assert_pushed_entry(&fixture, &first, 1).await;
        let owner = fixture.principal.bare_jid().clone();
        let room: jid::BareJid = "read-room@muc.example.com".parse().expect("room");
        let mut read = fixture.submission(Some(&format!("displayed-{suffix}")), "");
        read.plan.sanitized_message.bodies.clear();
        read.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
        read.plan.sanitized_message.payloads.push(
            waddle_xmpp::xep::xep0333::build_displayed_element(&format!("seed-id-{suffix}")),
        );
        read.target = waddle_xmpp::ingress::NormalizedTarget::Bare(room.clone());
        read.plan.sanitized_message.to = Some(room.clone().into());
        read.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
            &read.plan.sanitized_message,
            &waddle_xmpp::ingress::DigestContext {
                target: read.target.clone(),
                server_authorities: vec![owner.clone(), room.clone()],
                stanza_lang: None,
            },
        )
        .expect("displayed marker digest");
        read.identity = resumable_identity(&fixture, &format!("displayed-{suffix}"), 1).await;
        read.plan.intents.push(IngressEffectIntent::InboxProject {
            owner: owner.clone(),
            mutation: match &thread {
                Some(thread_id) => InboxProjectionMutation::GroupchatThreadRead {
                    room: room.clone(),
                    thread_id: thread_id.clone(),
                },
                None => InboxProjectionMutation::GroupchatChannelRead { room: room.clone() },
            },
        });
        read.plan
            .plan
            .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
                DurableDirectEffect::MarkInboxRead {
                    owner: owner.clone(),
                    channel: room,
                    thread: thread.clone(),
                },
            ))));
        read.plan.plan.push(PlannedEffect::new(Effect::External(
            ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
                owner,
                projection: ProjectionRef(0),
            }),
        )));
        let marked = commit_submission(&fixture.uow, &read, 5)
            .await
            .expect("displayed marker");
        super::projections::assert_pushed_entry(&fixture, &marked, 0).await;
        let newer = commit_submission(
            &fixture.uow,
            &room_projection(
                &fixture,
                &format!("newer-{suffix}"),
                &format!("newer-id-{suffix}"),
                20,
                thread.as_ref(),
            ),
            5,
        )
        .await
        .expect("newer room message");
        super::projections::assert_pushed_entry(&fixture, &newer, 1).await;
        let replay = commit_submission(&fixture.uow, &read, 5)
            .await
            .expect("ExistingCommitted displayed marker");
        assert_eq!(replay.class, IngressDecisionClass::ExistingCommitted);
        assert_eq!(replay.message_key, marked.message_key);
        assert_eq!(
            replay.applied_durable.inbox(ProjectionRef(0)),
            newer.applied_durable.inbox(ProjectionRef(1))
        );
        super::projections::assert_pushed_entry(&fixture, &replay, 1).await;
    }
    fixture.close().await;
}
#[tokio::test]
async fn ingress_displayed_marker_replay_sqlite() {
    displayed_marker_replay(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_displayed_marker_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("displayed_marker_replay").await {
        displayed_marker_replay(fixture).await;
    }
}
