use super::*;
use crate::ingress::effects::room::{DurableRoomEffect, RoomFenceRequirement};
use crate::ingress::effects::{DurableEffect, Effect};
use jid::BareJid;
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};
use waddle_xmpp_core::xep0359::StanzaId;

#[test]
fn correction_authority_attaches_only_to_the_exact_archive_without_observers() {
    let sink = PlanSink::new();
    let room: BareJid = "room@conference.example.test".parse().expect("room");
    let other: BareJid = "other@conference.example.test".parse().expect("other");
    for (archive_room, id) in [
        (&room, "revision"),
        (&room, "sibling"),
        (&other, "revision"),
    ] {
        let mut message = ArchivedMessage::for_test(
            archive_room
                .with_resource_str("alice")
                .expect("nick")
                .into(),
            archive_room.clone().into(),
        );
        message.id = id.into();
        sink.record(PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ArchiveGroupchat {
                room: archive_room.clone(),
                message: Box::new(message),
                fence: RoomFenceRequirement::Unfenced,
                archive_expectation: ArchiveExpectation::Fresh,
                correction_target: None,
            },
        ))));
    }
    let target = StanzaId::new("original", room.clone().into());
    let revision = StanzaId::new("revision", room.clone().into());
    sink.set_room_correction_target(&room, &revision, &target);
    // A foreign authority cannot overwrite the already validated binding.
    sink.set_room_correction_target(&room, &revision, &StanzaId::new("foreign", other.into()));
    for (index, planned) in sink.snapshot().iter().enumerate() {
        let Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
            correction_target,
            ..
        })) = &planned.effect
        else {
            panic!("archive effect");
        };
        assert_eq!(
            correction_target.as_ref(),
            if index == 0 { Some(&target) } else { None }
        );
    }
}
