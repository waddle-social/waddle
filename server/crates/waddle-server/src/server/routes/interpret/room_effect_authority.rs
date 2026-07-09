use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoomEffectAuthorityError {
    NotOwner,
    OwnershipUncertain,
}

async fn demote_expected_actor(
    room_registry: &ActorRef<RoomRegistryActor>,
    room: &BareJid,
    expected_actor: &ActorRef<RoomActor>,
) {
    let _ = room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::DestroyRoomExact {
            room_jid: room.clone(),
            expected_actor: expected_actor.clone(),
        })
        .await;
    expected_actor.kill();
}

/// Resolve the exact actor incarnation that authorized this nested room
/// event. A room-only `GetRoom` is used solely as a CAS comparison; its
/// returned actor is never adopted as fresh authority for the old event.
pub(super) async fn exact_room_actor_for_effect(
    deps: &Deps<'_>,
    room: &BareJid,
) -> Result<ActorRef<RoomActor>, RoomEffectAuthorityError> {
    let Some(room_registry) = deps.room_registry else {
        return Err(RoomEffectAuthorityError::OwnershipUncertain);
    };
    let Some(expected_actor) = deps.room_actor_incarnation.as_ref() else {
        return Err(RoomEffectAuthorityError::OwnershipUncertain);
    };
    let current_actor = match room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            demote_expected_actor(room_registry, room, expected_actor).await;
            return Err(RoomEffectAuthorityError::NotOwner);
        }
        Err(_) => return Err(RoomEffectAuthorityError::OwnershipUncertain),
    };
    if current_actor != *expected_actor {
        demote_expected_actor(room_registry, room, expected_actor).await;
        return Err(RoomEffectAuthorityError::NotOwner);
    }

    #[cfg(feature = "clustering")]
    if deps.clustered_muc_ownership_required || deps.muc_durable_store.is_some() {
        let Some(expected_fence) = deps.room_claim_fence.as_ref() else {
            return Err(RoomEffectAuthorityError::OwnershipUncertain);
        };
        match expected_actor.ask(GetRoomClaimFence).await {
            Ok(Some(bound_fence)) if bound_fence == *expected_fence => {}
            Ok(Some(_)) => {
                demote_expected_actor(room_registry, room, expected_actor).await;
                return Err(RoomEffectAuthorityError::NotOwner);
            }
            Ok(None) | Err(_) => return Err(RoomEffectAuthorityError::OwnershipUncertain),
        }
    }

    Ok(expected_actor.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kameo::actor::Spawn;
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DestroyRoom};
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    async fn replaced_room(
        name: &str,
    ) -> (
        ActorRef<RoomRegistryActor>,
        BareJid,
        ActorRef<RoomActor>,
        ActorRef<RoomActor>,
    ) {
        let registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::new(vec![b'e'; 32]).expect("test secret"),
        ));
        let room: BareJid = format!("{name}@muc.example.com").parse().expect("room JID");
        let original = registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "original".to_string(),
                channel_id: format!("{name}-original"),
                config: RoomConfig::default(),
            })
            .await
            .expect("create E1");
        registry
            .ask(DestroyRoom {
                room_jid: room.clone(),
            })
            .await
            .expect("remove E1");
        let replacement = registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "replacement".to_string(),
                channel_id: format!("{name}-replacement"),
                config: RoomConfig::default(),
            })
            .await
            .expect("create E2");
        (registry, room, original, replacement)
    }

    #[tokio::test]
    async fn retained_archive_effect_cannot_adopt_replacement_actor() {
        let connections = ConnectionRegistry::new();
        let (room_registry, room, original, replacement) = replaced_room("archive-authority").await;
        let mut deps = Deps::registry_only(&connections);
        deps.room_registry = Some(&room_registry);
        deps.room_actor_incarnation = Some(original);

        let sender: FullJid = "alice@example.com/web".parse().expect("sender JID");
        let mut message = Message::new(Some(Jid::from(room.clone())));
        message.from = Some(
            room.clone()
                .with_resource_str("alice")
                .map(Jid::from)
                .expect("room occupant JID"),
        );
        message.type_ = XmppMessageType::Groupchat;
        let archive_outcome = super::super::archive_groupchat_event::archive_groupchat_event(
            &deps,
            room,
            sender,
            Box::new(message),
            0,
            None,
        )
        .await;

        assert!(matches!(
            archive_outcome,
            super::super::archive_groupchat_event::ArchiveGroupchatEventOutcome::OwnershipUncertain(
                _
            )
        ));
        assert!(
            replacement.is_alive(),
            "exact E1 rejection must preserve E2"
        );
    }

    #[tokio::test]
    async fn retained_fanout_effect_cannot_deliver_after_actor_replacement() {
        let connections = ConnectionRegistry::new();
        let (room_registry, room, original, replacement) = replaced_room("fanout-authority").await;
        let recipient: FullJid = "bob@example.com/web".parse().expect("recipient JID");
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        connections.register_with_carbons(recipient.clone(), tx, false);
        let mut deps = Deps::registry_only(&connections);
        deps.room_registry = Some(&room_registry);
        deps.room_actor_incarnation = Some(original);

        let mut message = Message::new(Some(Jid::from(recipient.clone())));
        message.from = Some(
            room.clone()
                .with_resource_str("alice")
                .map(Jid::from)
                .expect("room occupant JID"),
        );
        message.type_ = XmppMessageType::Groupchat;
        let outcome = interpret(
            vec![OutboundEvent::RouteToConnection {
                jid: Jid::from(recipient),
                stanza: Box::new(Stanza::Message(message)),
            }],
            &deps,
        )
        .await;

        assert!(outcome.room_ownership_uncertain);
        assert!(
            rx.try_recv().is_err(),
            "stale E1 fanout reached a recipient"
        );
        assert!(
            replacement.is_alive(),
            "exact E1 rejection must preserve E2"
        );
    }
}
