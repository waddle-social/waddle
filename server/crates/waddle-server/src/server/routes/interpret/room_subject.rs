use super::*;
use waddle_xmpp::muc::RoomSubjectTexts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PersistRoomSubjectOutcome {
    Applied,
    NotOwner,
    OwnershipUncertain,
    PersistFailed,
}

pub(super) async fn persist_room_subject_event(
    deps: &Deps<'_>,
    room: BareJid,
    texts: RoomSubjectTexts,
    setter: BareJid,
    setter_nick: String,
    set_at: chrono::DateTime<chrono::Utc>,
) -> PersistRoomSubjectOutcome {
    let room_actor = match exact_room_actor_for_effect(deps, &room).await {
        Ok(actor) => actor,
        Err(RoomEffectAuthorityError::NotOwner) => {
            return PersistRoomSubjectOutcome::NotOwner;
        }
        Err(RoomEffectAuthorityError::OwnershipUncertain) => {
            return PersistRoomSubjectOutcome::OwnershipUncertain;
        }
    };
    match room_actor
        .ask(SetSubject {
            texts,
            setter: setter.clone(),
            setter_nick,
            set_at,
        })
        .await
    {
        Ok(()) => PersistRoomSubjectOutcome::Applied,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::RoomMutationError::NotOwner,
        )) => PersistRoomSubjectOutcome::NotOwner,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::RoomMutationError::OwnershipUncertain,
        )) => PersistRoomSubjectOutcome::OwnershipUncertain,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::RoomMutationError::PersistFailed(_),
        )) => PersistRoomSubjectOutcome::PersistFailed,
        Err(error) => {
            warn!(
                room = %room,
                setter = %setter,
                error = ?error,
                "PersistRoomSubject: SetSubject ask failed; subject left at previous state"
            );
            PersistRoomSubjectOutcome::OwnershipUncertain
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kameo::actor::Spawn;
    use waddle_xmpp::muc::room_actor::GetSnapshot;
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DestroyRoom};
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    #[tokio::test]
    async fn retained_subject_event_cannot_mutate_replacement_room_actor() {
        let connections = ConnectionRegistry::new();
        let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::new(vec![b's'; 32]).expect("test secret"),
        ));
        let room: BareJid = "subject@muc.example.com".parse().expect("room JID");
        let original = room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "original".to_string(),
                channel_id: "subject-original".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create original actor");
        room_registry
            .ask(DestroyRoom {
                room_jid: room.clone(),
            })
            .await
            .expect("remove original actor");
        let replacement = room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "replacement".to_string(),
                channel_id: "subject-replacement".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create replacement actor");

        let mut deps = Deps::registry_only(&connections);
        deps.room_registry = Some(&room_registry);
        deps.room_actor_incarnation = Some(original);
        let outcome = persist_room_subject_event(
            &deps,
            room,
            RoomSubjectTexts::from_iter([(String::new(), "stale subject".to_string())]),
            "alice@example.com".parse().expect("setter JID"),
            "alice".to_string(),
            chrono::Utc::now(),
        )
        .await;

        assert_eq!(outcome, PersistRoomSubjectOutcome::NotOwner);
        let snapshot = replacement
            .ask(GetSnapshot)
            .await
            .expect("replacement snapshot");
        assert!(snapshot.room.subject.is_none());
        assert!(replacement.is_alive(), "exact E1 demotion must preserve E2");
    }
}
