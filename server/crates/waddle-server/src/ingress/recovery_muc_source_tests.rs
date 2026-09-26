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

fn system_broadcast() -> (
    MessageEnvelope,
    IngressEffectIntent,
    IngressEffectIntent,
    jid::FullJid,
    jid::FullJid,
) {
    use waddle_xmpp::ingress::{EntityGeneration, StoredMessagePayload};

    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let web: jid::FullJid = "romeo@example.com/web".parse().expect("web");
    let ios: jid::FullJid = "romeo@example.com/ios".parse().expect("ios");
    let id = StanzaId::new("result-id", room.clone().into());
    let mut message = Message::new(None);
    message.type_ = MessageType::Groupchat;
    message.from = Some(room.clone().into());
    waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &id);
    let archived_at = chrono::Utc::now();
    let archive = IngressEffectIntent::SystemMessageArchive {
        sequence: 0,
        archive: room.clone(),
        stanza_id: id.clone(),
        by: room.clone(),
        archived_at,
        ordinal: None,
    };
    let route = IngressEffectIntent::RouteMucSystemBroadcast {
        room,
        occupants: vec![web.clone(), ios.clone()],
        room_generation: EntityGeneration::INITIAL,
        system_message: Some(StoredMessagePayload::new(message).expect("frozen result")),
        route_identity: EffectMessageIdentity::StanzaId(id),
    };
    (
        MessageEnvelope::new(Message::new(None)),
        route,
        archive,
        web,
        ios,
    )
}

#[test]
fn bodyless_system_result_recovers_only_undelivered_occupant_without_pin() {
    use crate::ingress::{
        recorded::RouteProgress,
        recovery_rebuild::{self, RecoveryInput},
    };
    use waddle_xmpp::ingress::MessageKey;

    let (envelope, route, archive, web, ios) = system_broadcast();
    let progress = RouteProgress::from_intent(&route, None, vec![web.clone()])
        .expect("progress")
        .expect("system route");
    let recorded = vec![archive, route.clone()];
    let pending = vec![route.clone()];
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
        resources, stanza, ..
    }) = &rebuilt.decision.external[0]
    else {
        panic!("frozen iOS copy")
    };
    assert_eq!(resources, std::slice::from_ref(&ios));
    let Stanza::Message(copy) = stanza.as_ref() else {
        panic!("groupchat copy")
    };
    assert_eq!(copy.to, Some(ios.into()));
    assert!(copy.bodies.is_empty(), "result is bodyless");
    assert_eq!(
        copy.from,
        Some("room@muc.example.com".parse().expect("room"))
    );
}

#[test]
fn system_broadcast_requires_receipted_pin_when_recorded() {
    use waddle_xmpp::ingress::RoomPinMutation;

    let (envelope, route, archive, _, _) = system_broadcast();
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let pin = IngressEffectIntent::Pin {
        room: room.clone(),
        mutation: RoomPinMutation::Unpin {
            target_stanza_id: StanzaId::new("pin-target", room.into()),
        },
    };
    let recorded = vec![archive, pin.clone(), route.clone()];
    assert_eq!(
        authorized_source(&envelope, &route, &recorded, std::slice::from_ref(&pin)),
        Err(MucRecoveryError::PrerequisitePending)
    );
    assert!(authorized_source(&envelope, &route, &recorded, &[]).is_ok());
}

#[test]
fn system_broadcast_requires_receipted_archive_with_exact_stanza_identity() {
    let (envelope, route, archive, _, _) = system_broadcast();
    assert_eq!(
        authorized_source(
            &envelope,
            &route,
            std::slice::from_ref(&archive),
            std::slice::from_ref(&archive)
        ),
        Err(MucRecoveryError::PrerequisitePending)
    );
    let IngressEffectIntent::SystemMessageArchive {
        sequence,
        archive,
        by,
        archived_at,
        ordinal,
        ..
    } = archive
    else {
        panic!("archive")
    };
    let wrong = IngressEffectIntent::SystemMessageArchive {
        sequence,
        archive,
        stanza_id: StanzaId::new("another-result", by.clone().into()),
        by,
        archived_at,
        ordinal,
    };
    assert_eq!(
        authorized_source(&envelope, &route, &[wrong], &[]),
        Err(MucRecoveryError::PrerequisitePending)
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
