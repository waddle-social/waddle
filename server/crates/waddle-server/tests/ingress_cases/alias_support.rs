use super::*;
use waddle_server::ingress::{
    effects::{
        direct::DurableDirectEffect,
        room::{DurableRoomEffect, RoomFenceRequirement},
        Effect,
    },
    execute::execute_effects,
    Deps, DurableEffect, ImmediateSink, PlanSuppressionPolicy, PlannedEffect,
};
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};

pub(super) fn archive_plan(fixture: &IngressFixture, room: bool, id: &str) -> IngressSubmission {
    let mut submission = fixture.submission(Some("stable-origin"), "hello");
    if room {
        let target: BareJid = "room@muc.example.com".parse().expect("room");
        submission.target = NormalizedTarget::Bare(target.clone());
        submission.plan.sanitized_message.to = Some(target.into());
        submission.plan.sanitized_message.type_ = MessageType::Groupchat;
    }
    refresh_digest(&mut submission);
    let archive = if room {
        submission
            .plan
            .sanitized_message
            .to
            .as_ref()
            .expect("target")
            .to_bare()
    } else {
        fixture.principal.bare_jid().clone()
    };
    let stamp = StanzaId::new(id, archive.clone().into());
    waddle_xmpp_core::xep0359::add_stanza_id(&mut submission.plan.sanitized_message, &stamp);
    let mut archived = ArchivedMessage::for_test(
        submission.sender.clone().into(),
        submission
            .plan
            .sanitized_message
            .to
            .clone()
            .expect("target"),
    );
    archived.id = id.to_owned();
    archived.body = Some("hello".to_owned());
    archived.message_type = submission.plan.sanitized_message.type_.clone();
    archived.origin_id = submission.digest_input.origin().cloned();
    archived.stanza_id = Some(stamp.clone());
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ArchiveAuthoritative {
            archive: archive.clone(),
            by: archive.clone(),
            stanza_id: stamp,
            archived_at: archived.timestamp,
        });
    let effect = if room {
        DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
            room: archive,
            message: Box::new(archived),
            fence: RoomFenceRequirement::Unfenced,
            archive_expectation: ArchiveExpectation::Fresh,
        })
    } else {
        DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
            archive,
            message: Box::new(archived),
            archive_expectation: ArchiveExpectation::Fresh,
        })
    };
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(effect)));
    add_reflections(&mut submission);
    submission
}

pub(super) fn refresh_digest(submission: &mut IngressSubmission) {
    let offered = &submission.plan.sanitized_message;
    for effect in &mut submission.plan.plan {
        let archived = match &mut effect.effect {
            Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                message,
                ..
            }))
            | Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
                message,
                ..
            })) => message,
            _ => continue,
        };
        archived.body = offered.bodies.get(&Lang::new()).cloned();
        archived.from = offered.from.clone().expect("sender");
        archived.to = offered.to.clone().expect("recipient");
        archived.message_type = offered.type_.clone();
        if !offered.subjects.is_empty() {
            archived.rich.get_or_insert_with(Default::default).subjects = offered
                .subjects
                .iter()
                .map(|(lang, subject)| (lang.0.clone(), subject.clone()))
                .collect();
        }
    }

    let domain: BareJid = "muc.example.com".parse().expect("MUC domain");
    submission.digest_input = waddle_server::ingress::submission::digest_input(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: waddle_server::ingress::submission::digest_authorities(
                &submission.plan.sanitized_message,
                submission.principal.bare_jid(),
                domain.domain(),
            ),
            stanza_lang: None,
        },
    )
    .expect("digest");
}

pub(super) fn add_reflections(submission: &mut IngressSubmission) {
    submission
        .plan
        .plan
        .retain(|effect| !matches!(effect.effect, Effect::External(_)));
    for recipient in [
        submission.sender.clone(),
        "juliet@example.com/phone".parse().expect("peer"),
    ] {
        let mut message = submission.plan.sanitized_message.clone();
        message.to = Some(recipient.into());
        let mut effect = PlannedEffect::new(Effect::External(ExternalEffect::Frame(Box::new(
            Stanza::Message(message),
        ))));
        effect.suppression = PlanSuppressionPolicy::SenderOnly;
        submission.plan.plan.push(effect);
    }
}

/// Exercise the production executor, then the same typed XML serialization boundary
/// used for transport frames. Return decoded wire messages for semantic assertions.
pub(super) async fn wire_messages(
    fixture: &IngressFixture,
    decision: &IngressDecision,
) -> Vec<Message> {
    let registry = waddle_xmpp::registry::ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        decision,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    report
        .frame_obligations
        .into_iter()
        .flat_map(|obligation| obligation.frames)
        .map(|stanza| {
            let Stanza::Message(message) = stanza else {
                panic!("message frame")
            };
            let element = minidom::Element::from(message);
            let mut wire = Vec::new();
            element.write_to(&mut wire).expect("wire encoding");
            Message::try_from(minidom::Element::from_reader(wire.as_slice()).expect("wire XML"))
                .expect("wire message")
        })
        .collect()
}

pub(super) async fn assert_canonical(
    fixture: &IngressFixture,
    decision: &IngressDecision,
    expected: &Message,
) {
    let mut tx = fixture.uow.begin().await.expect("read envelope");
    assert_eq!(
        CanonicalMessageRepository::load_envelope(
            &mut tx,
            decision.message_key.expect("canonical key")
        )
        .await
        .expect("envelope"),
        Some(MessageEnvelope::new(expected.clone()))
    );
    tx.commit().await.expect("finish read");
}

pub(super) fn assert_error(message: &Message, condition: DefinedCondition) {
    assert_eq!(message.type_, MessageType::Error);
    let error = message
        .payloads
        .iter()
        .find_map(|payload| StanzaError::try_from(payload.clone()).ok())
        .expect("standard stanza error");
    assert_eq!(error.defined_condition, condition);
}

/// Freeze the room-authored archive identity independently of the authenticated
/// sender that owns the ingress alias.
pub(super) fn room_archive_identity(submission: &mut IngressSubmission, generation: u64) {
    for planned in &mut submission.plan.plan {
        if let Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
            message,
            room,
            ..
        })) = &mut planned.effect
        {
            message.from = room
                .with_resource_str("reused-nick")
                .expect("room occupant")
                .into();
            message.nickname_generation = Some(generation);
            message.rich.get_or_insert_with(Default::default).muc_sender =
                Some(waddle_xmpp_core::mam::ArchivedMucSender {
                    jid: submission.sender.clone().into(),
                    affiliation: waddle_xmpp::Affiliation::Member,
                    role: waddle_xmpp::Role::Participant,
                });
        }
    }
}
