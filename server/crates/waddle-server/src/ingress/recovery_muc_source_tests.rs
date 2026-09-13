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
