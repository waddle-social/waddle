use super::*;
use waddle_xmpp::{
    ingress::EntityGeneration,
    muc::{RoomSubjectTexts, SubjectState},
};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::MessageType;

fn groupchat() -> (MessageEnvelope, IngressEffectIntent) {
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("sender");
    let id = StanzaId::new("frozen-id", room.clone().into());
    let mut message = Message::new(None);
    message.type_ = MessageType::Groupchat;
    message.from = Some(room.with_resource_str("romeo").expect("nick").into());
    waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &id);
    waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
        &mut message,
        &waddle_xmpp::xep::xep0421::OccupantId("occupant".into()),
    );
    (
        MessageEnvelope::new(message),
        IngressEffectIntent::RouteMucGroupchat {
            room,
            occupants: vec![sender.clone()],
            reflection: sender,
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::StanzaId(id),
        },
    )
}

#[test]
fn muc_recovery_reports_missing_canonical_provenance() {
    let (envelope, intent) = groupchat();
    let mut old = envelope.message().clone();
    old.from = Some("romeo@example.com/phone".parse().expect("real sender"));
    assert_eq!(
        authorized_source(&MessageEnvelope::new(old), &intent, &[], &[]),
        Err(MucRecoveryError::Source(
            CanonicalSourceError::MissingCanonicalProvenance
        ))
    );
}

#[test]
fn muc_recovery_reports_missing_system_payload_before_prerequisites() {
    let (envelope, groupchat) = groupchat();
    let IngressEffectIntent::RouteMucGroupchat {
        room,
        occupants,
        room_generation,
        route_identity,
        ..
    } = groupchat
    else {
        panic!("groupchat")
    };
    let intent = IngressEffectIntent::RouteMucSystemBroadcast {
        room,
        occupants,
        room_generation,
        route_identity,
        system_message: None,
    };
    assert_eq!(
        authorized_source(&envelope, &intent, &[], &[]),
        Err(MucRecoveryError::Source(
            CanonicalSourceError::MissingPayload
        ))
    );
}

#[test]
fn muc_recovery_subject_requires_recorded_receipted_mutation() {
    let (envelope, intent) = groupchat();
    let mut source = envelope.message().clone();
    source.subjects.insert(Default::default(), "subject".into());
    let envelope = MessageEnvelope::new(source);
    let mutation = IngressEffectIntent::RoomSubjectMutation {
        room: "room@muc.example.com".parse().expect("room"),
        state: SubjectState {
            texts: RoomSubjectTexts::from_iter([(String::new(), "subject".into())]),
            setter: "romeo@example.com".parse().expect("sender"),
            setter_nick: "romeo".into(),
            set_at: chrono::Utc::now(),
        },
    };
    assert_eq!(
        authorized_source(&envelope, &intent, &[], &[]),
        Err(MucRecoveryError::PrerequisitePending)
    );
    assert_eq!(
        authorized_source(
            &envelope,
            &intent,
            std::slice::from_ref(&mutation),
            std::slice::from_ref(&mutation)
        ),
        Err(MucRecoveryError::PrerequisitePending)
    );
    assert_eq!(
        authorized_source(&envelope, &intent, &[mutation], &[]),
        Ok(envelope.message())
    );
}

#[test]
fn muc_recovery_body_with_subject_does_not_require_subject_mutation() {
    let (envelope, intent) = groupchat();
    let mut source = envelope.message().clone();
    source
        .subjects
        .insert(Default::default(), "body heading".into());
    source
        .bodies
        .insert(Default::default(), "ordinary body".into());
    let envelope = MessageEnvelope::new(source);
    assert_eq!(
        authorized_source(&envelope, &intent, &[], &[]),
        Ok(envelope.message())
    );
}

#[test]
fn original_reflection_recovers_after_occupant_receipt_without_new_audience() {
    use crate::ingress::{
        recorded::RouteProgress,
        recovery_rebuild::{self, RecoveryInput},
    };
    use waddle_xmpp::ingress::MessageKey;
    let (envelope, room) = groupchat();
    let reflection =
        crate::ingress::reflection_dispatch::original_intent(&room).expect("reflection");
    let recorded = vec![room, reflection.clone()];
    let pending = vec![reflection.clone()];
    let progress = RouteProgress::from_intent(&reflection, None, vec![])
        .expect("progress")
        .expect("direct");
    let rebuilt = recovery_rebuild::rebuild(RecoveryInput {
        key: MessageKey::new(),
        envelope: &envelope,
        created_at: chrono::Utc::now(),
        recorded: &recorded,
        unreceipted: &pending,
        route_progress: vec![progress],
        host_owned_resources: vec![],
        departed_occupants: vec![],
        blocked_recipients: &[],
    })
    .expect("rebuild");
    assert!(rebuilt.unsupported_receipts.is_empty());
    assert_eq!(rebuilt.decision.external.len(), 1);
    let ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
        resources,
        stanza,
        route_identity,
        ..
    }) = &rebuilt.decision.external[0]
    else {
        panic!("frozen reflection")
    };
    assert_eq!(
        resources,
        &["romeo@example.com/phone"
            .parse::<jid::FullJid>()
            .expect("original")]
    );
    let IngressEffectIntent::RouteDirect {
        route_identity: expected,
        ..
    } = &reflection
    else {
        panic!("reflection route")
    };
    assert_eq!(route_identity.as_ref(), Some(expected));
    let Stanza::Message(message) = stanza.as_ref() else {
        panic!("message")
    };
    let mut expected_message = envelope.message().clone();
    expected_message.to = Some(resources[0].clone().into());
    assert_eq!(message, &expected_message);
    assert_eq!(
        rebuilt.decision.external_receipts[0],
        vec![crate::ingress::receipt_key(&reflection).expect("receipt")]
    );
}
