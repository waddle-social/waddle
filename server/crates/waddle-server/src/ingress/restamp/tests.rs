use super::*;
use crate::server::routes::interpret::effects::{PlannedEffect, RoomExecutionPath};
use waddle_xmpp::ingress::{EffectMessageIdentity, RetractionTombstoneMutation};
use waddle_xmpp_core::xep0359::{
    build_origin_id_element, build_stanza_id_element, extract_origin_id, extract_stanza_ids,
};

fn jid(value: &str) -> BareJid {
    value.parse().expect("test jid")
}

fn fixture() -> (IngressPlan, BareJid, StanzaId, StanzaId) {
    let owner = jid("alice@example.test");
    let minted = StanzaId::new("provisional", owner.clone().into());
    let recorded = StanzaId::new("committed", owner.clone().into());
    let mut message = Message::new(Some(jid("bob@example.test").into()));
    message
        .payloads
        .push(build_stanza_id_element(&minted.id, &minted.by));
    message
        .payloads
        .push(build_origin_id_element("client-origin"));
    let plan = IngressPlan {
        failure: None,
        plan: Vec::new(),
        intents: vec![IngressEffectIntent::ArchiveAuthoritative {
            archive: owner.clone(),
            stanza_id: minted.clone(),
            by: owner.clone(),
            archived_at: chrono::Utc::now(),
        }],
        sanitized_message: message,
        rejection: None,
        error_reply: None,
        room_execution: RoomExecutionPath::None,
    };
    (plan, owner, minted, recorded)
}

#[test]
fn restamp_preserves_origin_other_authorities_and_input_plan() {
    let (mut plan, owner, minted, recorded) = fixture();
    let foreign = StanzaId::new("provisional", jid("elsewhere.example.test").into());
    plan.sanitized_message
        .payloads
        .push(build_stanza_id_element(&foreign.id, &foreign.by));
    let result = restamp_plan(&plan, &[(owner, ArchiveRole::Sender, recorded.clone())]);
    assert_eq!(
        extract_stanza_ids(&result.sanitized_message),
        vec![recorded, foreign]
    );
    assert_eq!(extract_stanza_ids(&plan.sanitized_message)[0], minted);
    assert_eq!(
        extract_origin_id(&result.sanitized_message)
            .expect("origin")
            .id,
        "client-origin"
    );
}

#[test]
fn restamp_updates_archive_inbox_dependencies_and_external_frames() {
    let (mut plan, owner, minted, recorded) = fixture();
    let mut archive =
        ArchivedMessage::for_test(owner.clone().into(), jid("bob@example.test").into());
    archive.id = minted.id.clone();
    archive.stanza_id = Some(minted.clone());
    archive.stanza_xml = Some(String::from(&minidom::Element::from(
        plan.sanitized_message.clone(),
    )));
    let entry = InboxEntry::new(
        jid("bob@example.test"),
        ConversationKind::Direct,
        &minted.id,
        0,
    );
    plan.plan = vec![
        PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ArchiveDirect {
                archive: owner.clone(),
                message: Box::new(archive),
                archive_expectation: waddle_xmpp::mam::ArchiveExpectation::Fresh,
            },
        ))),
        PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ProjectInbox {
                owner: owner.clone(),
                entry: Box::new(entry.clone()),
                increment_unread: true,
            },
        )))
        .with_dependency(PlanEffectDependency::AfterArchive {
            archive: owner.clone(),
            minted: minted.clone(),
        }),
        PlannedEffect::new(Effect::External(ExternalEffect::Direct(
            ExternalDirectEffect::PushInboxUpdate {
                owner: owner.clone(),
                projection: crate::server::routes::interpret::effects::ProjectionRef(1),
            },
        ))),
        PlannedEffect::new(Effect::External(ExternalEffect::Frame(Box::new(
            Stanza::Message(plan.sanitized_message.clone()),
        )))),
    ];
    let result = restamp_plan(
        &plan,
        &[(owner.clone(), ArchiveRole::Sender, recorded.clone())],
    );
    let Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
        message, ..
    })) = &result.plan[0].effect
    else {
        panic!("archive");
    };
    assert_eq!(message.id, recorded.id);
    assert_eq!(message.stanza_id.as_ref(), Some(&recorded));
    let xml = message
        .stanza_xml
        .as_ref()
        .expect("xml")
        .parse::<minidom::Element>()
        .expect("valid xml");
    assert_eq!(
        extract_stanza_ids(&Message::try_from(xml).expect("message")),
        vec![recorded.clone()]
    );
    let Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ProjectInbox { entry, .. })) =
        &result.plan[1].effect
    else {
        panic!("inbox");
    };
    assert_eq!(entry.last_stanza_id, recorded.id);
    assert_eq!(
        result.plan[1].dependencies,
        vec![PlanEffectDependency::AfterArchive {
            archive: owner,
            minted: recorded.clone()
        }]
    );
    let Effect::External(ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
        projection,
        ..
    })) = &result.plan[2].effect
    else {
        panic!("push");
    };
    assert_eq!(
        *projection,
        crate::server::routes::interpret::effects::ProjectionRef(1)
    );
    let Effect::External(ExternalEffect::Frame(stanza)) = &result.plan[3].effect else {
        panic!("frame");
    };
    let Stanza::Message(message) = stanza.as_ref() else {
        panic!("message");
    };
    assert_eq!(extract_stanza_ids(message), vec![recorded]);
}

#[test]
fn restamp_intent_routes_and_retractions_preserve_historical_targets() {
    let (mut plan, owner, minted, recorded) = fixture();
    let historical = StanzaId::new("previous-message", owner.clone().into());
    plan.intents.push(IngressEffectIntent::RouteDirect {
        recipient: jid("bob@example.test"),
        fanout: Vec::new(),
        route_identity: EffectMessageIdentity::StanzaId(minted.clone()),
    });
    plan.intents.push(IngressEffectIntent::RetractionTombstone {
        mutation: RetractionTombstoneMutation {
            archive: owner.clone(),
            target_stanza_id: historical.clone(),
            retraction_stanza_id: minted,
        },
    });
    let result = restamp_plan(&plan, &[(owner, ArchiveRole::Sender, recorded.clone())]);
    let IngressEffectIntent::RouteDirect { route_identity, .. } = &result.intents[1] else {
        panic!("route");
    };
    assert_eq!(
        route_identity,
        &EffectMessageIdentity::StanzaId(recorded.clone())
    );
    let IngressEffectIntent::RetractionTombstone { mutation } = &result.intents[2] else {
        panic!("retraction");
    };
    assert_eq!(mutation.target_stanza_id, historical);
    assert_eq!(mutation.retraction_stanza_id, recorded);
}

#[test]
fn restamp_descends_into_forwarded_messages() {
    let (mut plan, owner, _, recorded) = fixture();
    let forwarded = xmpp_parsers::forwarding::Forwarded {
        delay: None,
        message: plan.sanitized_message.clone(),
    };
    let mut wrapper = Message::new(Some(owner.clone().into()));
    wrapper.payloads.push(forwarded.into());
    plan.error_reply = Some(Stanza::Message(wrapper));
    let result = restamp_plan(&plan, &[(owner, ArchiveRole::Sender, recorded.clone())]);
    let Stanza::Message(wrapper) = result.error_reply.expect("reply") else {
        panic!("message");
    };
    let forwarded = xmpp_parsers::forwarding::Forwarded::try_from(wrapper.payloads[0].clone())
        .expect("forwarded");
    let message = forwarded.message;
    assert_eq!(extract_stanza_ids(&message), vec![recorded]);
}

#[test]
fn restamp_updates_offline_reference_and_original_payload_together() {
    use waddle_xmpp::pending_delivery::{PendingRow, PendingRowId};
    let (mut plan, owner, minted, recorded) = fixture();
    plan.plan.push(PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
            prepared_notification: PreparedOfflineNotification::Suppressed,
            row: PendingRow {
                id: PendingRowId::new("pending-row"),
                recipient: owner.clone(),
                original_receipt_at: chrono::Utc::now(),
                payload: PendingPayload::Archived(minted),
                flushed_in_session: None,
                outbound_sequence: None,
            },
            original_message: Box::new(plan.sanitized_message.clone()),
        }),
    )));
    let result = restamp_plan(&plan, &[(owner, ArchiveRole::Sender, recorded.clone())]);
    let Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
        row,
        original_message,
        ..
    })) = &result.plan[0].effect
    else {
        panic!("offline delivery");
    };
    let PendingPayload::Archived(id) = &row.payload else {
        panic!("archive reference");
    };
    assert_eq!(id, &recorded);
    assert_eq!(extract_stanza_ids(original_message), vec![recorded]);
}

#[test]
fn restamp_requires_recorded_archive_and_assigning_authority_to_match() {
    let (plan, owner, minted, _) = fixture();
    let other = jid("other@example.test");
    for recorded in [
        (
            other.clone(),
            ArchiveRole::Sender,
            StanzaId::new("recorded", owner.clone().into()),
        ),
        (
            owner,
            ArchiveRole::Sender,
            StanzaId::new("recorded", other.into()),
        ),
    ] {
        let result = restamp_plan(&plan, &[recorded]);
        assert_eq!(
            extract_stanza_ids(&result.sanitized_message),
            vec![minted.clone()]
        );
    }
}

#[test]
fn system_archive_and_peer_delivery_share_recorded_identity() {
    use crate::server::routes::interpret::effects::invite::MucUserRoute;
    use crate::server::routes::interpret::effects::room::RoomFenceRequirement;
    use waddle_xmpp::pending_delivery::{PendingRow, PendingRowId};
    let (mut plan, room, minted, recorded) = fixture();
    let mut message = plan.sanitized_message.clone();
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.from = Some(room.clone().into());
    let mut archive = ArchivedMessage::for_test(room.clone().into(), room.clone().into());
    archive.id = minted.id.clone();
    archive.stanza_id = Some(minted.clone());
    archive.stanza_xml = Some(String::from(&minidom::Element::from(message.clone())));
    plan.plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ArchiveGroupchat {
                room: room.clone(),
                message: Box::new(archive),
                fence: RoomFenceRequirement::Unfenced,
                archive_expectation: waddle_xmpp::mam::ArchiveExpectation::Fresh,
            },
        ))));
    let recipient = jid("bob@example.test");
    let route = MucUserRoute {
        route_identity: Some(EffectMessageIdentity::StanzaId(minted)),
        recipient: recipient.clone(),
        resources: Vec::new(),
        message: Box::new(message.clone()),
        fallback: PendingRow {
            id: PendingRowId::new("system-row"),
            recipient,
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Transient(Box::new(message)),
            flushed_in_session: None,
            outbound_sequence: None,
        },
        failure: None,
    };
    plan.plan.push(PlannedEffect::new(Effect::External(
        ExternalEffect::RouteToPeer(route.clone()),
    )));
    plan.plan.push(PlannedEffect::new(Effect::External(
        ExternalEffect::QueueOfflineDelivery(route),
    )));
    let stamped = restamp_plan(&plan, &[(room, ArchiveRole::Sender, recorded.clone())]);
    let Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
        message, ..
    })) = &stamped.plan[0].effect
    else {
        panic!("system archive");
    };
    assert_eq!(message.id, recorded.id);
    assert_eq!(message.stanza_id.as_ref(), Some(&recorded));
    for planned in &stamped.plan[1..] {
        let Effect::External(
            ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route),
        ) = &planned.effect
        else {
            panic!("system delivery");
        };
        assert_eq!(extract_stanza_ids(&route.message), vec![recorded.clone()]);
        assert_eq!(
            route.route_identity,
            Some(EffectMessageIdentity::StanzaId(recorded.clone()))
        );
        let PendingPayload::Transient(message) = &route.fallback.payload else {
            panic!("fallback");
        };
        assert_eq!(extract_stanza_ids(message), vec![recorded.clone()]);
    }
    let mut saved_intents = stamped.intents.clone();
    let IngressEffectIntent::ArchiveAuthoritative { archived_at, .. } = &mut saved_intents[0]
    else {
        panic!("archive intent");
    };
    *archived_at = chrono::DateTime::from_timestamp(123, 0).expect("timestamp");
    let reconciled = crate::ingress::recorded::apply_recorded_intents(&stamped, &saved_intents);
    let Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
        message, ..
    })) = &reconciled.plan[0].effect
    else {
        panic!("recorded archive");
    };
    assert_eq!(message.timestamp.timestamp(), 123);
}

#[test]
fn sender_and_generated_archives_retain_distinct_recorded_identities() {
    let (mut plan, room, sender_minted, sender_recorded) = fixture();
    let system_minted = StanzaId::new("system-provisional", room.clone().into());
    let system_recorded = StanzaId::new("system-committed", room.clone().into());
    let archived_at = chrono::DateTime::from_timestamp(123, 0).expect("timestamp");
    plan.intents
        .push(IngressEffectIntent::SystemMessageArchive {
            sequence: 0,
            archive: room.clone(),
            by: room.clone(),
            stanza_id: system_minted.clone(),
            archived_at,
        });
    assert_ne!(
        plan.intents[0].authority_key(),
        plan.intents[1].authority_key()
    );
    for minted in [&sender_minted, &system_minted] {
        let mut message = Message::new(Some(room.clone().into()));
        message
            .payloads
            .push(build_stanza_id_element(&minted.id, &minted.by));
        plan.plan
            .push(PlannedEffect::new(Effect::External(ExternalEffect::Frame(
                Box::new(Stanza::Message(message)),
            ))));
    }
    let recorded = [
        (
            room.clone(),
            ArchiveRole::SystemMessage { sequence: 0 },
            system_recorded.clone(),
        ),
        (room, ArchiveRole::Sender, sender_recorded.clone()),
    ];
    let stamped = restamp_plan(&plan, &recorded);
    for (effect, expected) in stamped.plan.iter().zip([sender_recorded, system_recorded]) {
        let Effect::External(ExternalEffect::Frame(frame)) = &effect.effect else {
            panic!("frame");
        };
        let Stanza::Message(message) = frame.as_ref() else {
            panic!("message");
        };
        assert_eq!(extract_stanza_ids(message), vec![expected]);
    }
    assert_eq!(
        extract_stanza_ids(&plan.sanitized_message),
        vec![sender_minted]
    );
    let saved = stamped.intents[1].clone();
    saved
        .with_encoded_v1(|kind, payload| {
            assert_eq!(
                IngressEffectIntent::decode_v1(kind, payload).expect("decode"),
                saved
            );
        })
        .expect("encode");
}

#[test]
fn restamp_room_projection_preserves_client_id_and_updates_archive_dependency() {
    let (mut plan, room, minted, recorded) = fixture();
    let owner = jid("recipient@example.test");
    plan.plan.push(
        PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ProjectGroupchatInbox {
                archive_stanza_id: minted.clone(),
                owner,
                entry: Box::new(InboxEntry::new(
                    room.clone(),
                    ConversationKind::MucRoom,
                    "client-wire-id",
                    0,
                )),
                is_recipient: true,
                recovery: None,
            },
        )))
        .with_dependency(PlanEffectDependency::AfterArchive {
            archive: room.clone(),
            minted,
        }),
    );
    let result = restamp_plan(
        &plan,
        &[(room.clone(), ArchiveRole::Sender, recorded.clone())],
    );
    let Effect::Durable(DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
        entry,
        archive_stanza_id,
        ..
    })) = &result.plan[0].effect
    else {
        panic!("room inbox projection");
    };
    assert_eq!(entry.last_stanza_id, "client-wire-id");
    assert_eq!(archive_stanza_id, &recorded);
    assert_eq!(
        result.plan[0].dependencies,
        vec![PlanEffectDependency::AfterArchive {
            archive: room,
            minted: recorded,
        }]
    );
}
