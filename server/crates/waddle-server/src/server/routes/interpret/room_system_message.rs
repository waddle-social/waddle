//! Interpreter arm for [`OutboundEvent::BroadcastRoomSystemMessage`] (#414).
//!
//! Server-authored groupchat messages originating from the room bare
//! JID itself: pin/unpin events, future room-level notifications. The
//! arm:
//!
//! 1. Stamps a fresh XEP-0359 `<stanza-id by='room'/>` so the message
//!    can be referenced by clients (jump-to-from-pin-list).
//! 2. Persists the message to MAM via the regular groupchat archive
//!    helper.
//! 3. Routes a copy to every joined occupant (per-resource fan-out).
//!
//! Bypasses the occupancy gate, rich-target validation, and extension
//! enrichment — the sender is the room itself.

use super::*;
use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};

fn bind_room_system_actor<'a>(deps: &Deps<'a>, room_actor: &ActorRef<RoomActor>) -> Deps<'a> {
    let mut exact_deps = deps.clone();
    exact_deps.room_actor_incarnation = Some(room_actor.clone());
    exact_deps
}

async fn room_system_fanout_authorized(deps: &Deps<'_>, room: &BareJid) -> bool {
    exact_room_actor_for_effect(deps, room).await.is_ok()
}

pub(super) async fn broadcast_room_system_message_event(
    deps: &Deps<'_>,
    room: BareJid,
    mut message: Box<Message>,
    recursion_depth: u8,
) -> Option<String> {
    let Some(room_registry) = deps.room_registry else {
        debug!(
            room = %room,
            "BroadcastRoomSystemMessage: no room_registry in Deps; skipping"
        );
        return None;
    };
    let room_actor = if deps.room_actor_incarnation.is_some() {
        match exact_room_actor_for_effect(deps, &room).await {
            Ok(actor) => actor,
            Err(error) => {
                warn!(
                    room = %room,
                    ?error,
                    "BroadcastRoomSystemMessage: authorizing room incarnation was replaced; \
                     dropping"
                );
                return None;
            }
        }
    } else {
        // Direct server-originated notifications are logical-room operations,
        // so they may resolve the current incarnation. Nested pin events carry
        // `room_actor_incarnation` and must never adopt a replacement actor.
        match room_registry
            .ask(GetRoom {
                room_jid: room.clone(),
            })
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => {
                debug!(
                    room = %room,
                    "BroadcastRoomSystemMessage: room not registered; dropping"
                );
                return None;
            }
            Err(error) => {
                warn!(
                    room = %room,
                    error = ?error,
                    "BroadcastRoomSystemMessage: room registry lookup failed; dropping"
                );
                return None;
            }
        }
    };
    // From this point on, even a direct logical-room event is bound to the
    // exact actor selected above. It may resolve fresh authority once, at
    // entry, but it must not transfer that authority to a later E2.
    let exact_deps = bind_room_system_actor(deps, &room_actor);

    // The system message has `from = room@conf` (bare). The room
    // snapshot query is keyed by a full JID for the sender-occupancy
    // calculation; we don't need that here, so we synthesize a sender
    // full JID purely for the snapshot RPC. The snapshot's occupant
    // list is independent of the sender argument.
    let synthetic_sender = match room.clone().with_resource_str("__system__") {
        Ok(s) => s,
        Err(error) => {
            warn!(
                room = %room,
                ?error,
                "BroadcastRoomSystemMessage: failed to build synthetic sender; dropping"
            );
            return None;
        }
    };
    let snapshot = match room_actor
        .ask(GetRoomSnapshot {
            sender_jid: synthetic_sender,
        })
        .await
    {
        Ok(snap) => snap,
        Err(error) => {
            warn!(
                room = %room,
                error = ?error,
                "BroadcastRoomSystemMessage: GetRoomSnapshot failed; dropping"
            );
            return None;
        }
    };

    #[cfg(feature = "clustering")]
    let exact_fence = match room_actor.ask(GetRoomClaimFence).await {
        Ok(Some(fence)) => Some(fence),
        Ok(None) => None,
        Err(error) => {
            warn!(room = %room, ?error, "BroadcastRoomSystemMessage: exact fence lookup failed");
            None
        }
    };
    #[cfg(feature = "clustering")]
    let exact_deps = {
        let mut exact_deps = exact_deps;
        exact_deps.room_claim_fence = exact_fence.clone();
        exact_deps
    };

    // System messages do not pass through `dispatch_to_room`, so perform
    // the same exact ownership proof here even when MAM is disabled. A
    // cached fence is only an input to the transactional archive check; it
    // is not by itself proof that the claim/node rows are still live.
    #[cfg(feature = "clustering")]
    if let Some(store) = deps.muc_durable_store {
        let exact_fence = exact_fence.as_ref()?;
        match store.check_fenced_fanout_exact(&room, exact_fence).await {
            Ok(true) => {}
            Ok(false) | Err(waddle_xmpp::XmppError::RoomOwnershipLost(_)) => {
                warn!(
                    room = %room,
                    "BroadcastRoomSystemMessage: exact room ownership was lost; demoting the \
                     local actor and dropping"
                );
                let _ = room_registry
                    .ask(DestroyRoomExact {
                        room_jid: room.clone(),
                        expected_actor: room_actor.clone(),
                    })
                    .await;
                return None;
            }
            Err(error) => {
                warn!(
                    room = %room,
                    %error,
                    "BroadcastRoomSystemMessage: exact room ownership could not be checked; \
                     failing closed"
                );
                return None;
            }
        }
    }

    // Stamp a canonical XEP-0359 `<stanza-id by='room'/>` so the
    // message is uniquely addressable in MAM and from clients.
    let stanza_id = uuid::Uuid::new_v4().to_string();
    add_stanza_id(
        &mut message,
        &StanzaId::new(stanza_id.clone(), Jid::from(room.clone())),
    );

    // Archive in MAM. We use `0` for `sender_nickname_generation` —
    // the field is a XEP-0308 LMC-correction window guard for
    // user-authored messages; system messages are never corrected.
    //
    // ADR-0017 Phase 3 Slice 7 FIX 1: this is a groupchat archive write
    // like any other, so it is fenced the same way (`resolve_room_claim_fence`
    // reads the identical typed context `dispatch_to_room`'s pre-fan-out
    // check uses). On ownership loss, skip the fan-out below entirely —
    // this function's fan-out loop is the ONLY fan-out for this message
    // (there is no separate batch to suppress, unlike the interpreter's
    // `ArchiveGroupchat` event arm), so an early return here is the exact
    // "not archived, not fanned out" contract.
    let fence = resolve_room_claim_fence(&exact_deps, &room);
    if fence.is_ownership_uncertain() {
        warn!(
            room = %room,
            "BroadcastRoomSystemMessage: clustered room ownership cannot be resolved; \
             dropping system message entirely"
        );
        return None;
    }
    if let Some(mam_storage) = deps.mam_storage {
        // Room-authored system messages have no occupant sender, so
        // there is no real-JID `<x xmlns='muc#user'/>` to disclose.
        match archive_groupchat_message(mam_storage, &room, &message, 0, &fence, None).await {
            ArchiveGroupchatOutcome::Stored(result) => debug!(
                room = %room,
                stanza_id = %result.stored_id,
                "BroadcastRoomSystemMessage: archived"
            ),
            ArchiveGroupchatOutcome::Skipped => debug!(
                room = %room,
                "BroadcastRoomSystemMessage: archive helper declined (chain bug?)"
            ),
            ArchiveGroupchatOutcome::OwnershipUncertain => {
                warn!(
                    room = %room,
                    "BroadcastRoomSystemMessage: exact room ownership could not be proved; \
                     dropping system message entirely (not archived, not fanned out)"
                );
                return None;
            }
        }
    }

    // Archiving is asynchronous. Bind even a direct logical-room event to the
    // actor chosen at entry, then re-check that exact incarnation before the
    // first externally visible fan-out. A direct E1 snapshot must not escape
    // under E2 merely because the caller arrived without nested batch context.
    if !room_system_fanout_authorized(&exact_deps, &room).await {
        warn!(
            room = %room,
            "BroadcastRoomSystemMessage: selected room incarnation changed while archiving; \
             suppressing fan-out"
        );
        return None;
    }

    // Fan out to every joined occupant. One `RouteToConnection`
    // per occupant full JID with the message's `to` set to that
    // occupant's full JID, matching `ReflectorHandler`'s
    // per-recipient personalization. Without this, downstream
    // recipient-pass logic (incoming-blocking, archive, inbox) sees
    // a stanza addressed to the room bare JID and may misroute.
    for occupant in &snapshot.occupants {
        // Routing one recipient can await actor/storage work. Re-prove E1
        // before every subsequent enqueue and stop immediately if E2 replaced
        // it between recipients.
        if !room_system_fanout_authorized(&exact_deps, &room).await {
            warn!(
                room = %room,
                recipient = %occupant.full_jid,
                "BroadcastRoomSystemMessage: selected room incarnation changed during fan-out; \
                 stopping remaining recipients"
            );
            return None;
        }
        let mut copy = (*message).clone();
        copy.to = Some(Jid::from(occupant.full_jid.clone()));
        route_to_connection(
            &exact_deps,
            Jid::from(occupant.full_jid.clone()),
            Box::new(Stanza::Message(copy)),
            recursion_depth,
        )
        .await;
    }
    // Catch replacement during the final recipient's awaited routing step;
    // earlier replacements are caught by the next loop iteration.
    if !room_system_fanout_authorized(&exact_deps, &room).await {
        warn!(
            room = %room,
            "BroadcastRoomSystemMessage: selected room incarnation changed before fan-out \
             completion"
        );
        return None;
    }
    Some(stanza_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kameo::actor::Spawn;
    #[cfg(feature = "clustering")]
    use waddle_xmpp::muc::room_actor::BindRoomClaimFence;
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DestroyRoom};
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    #[cfg(feature = "clustering")]
    struct AlwaysOwnedStore;

    #[cfg(feature = "clustering")]
    impl waddle_xmpp::muc::MucDurableStore for AlwaysOwnedStore {
        fn load_room_state<'a>(
            &'a self,
            _room_jid: &'a BareJid,
        ) -> waddle_xmpp::muc::MucDurableFuture<'a, Option<waddle_xmpp::muc::DurableRoomState>>
        {
            Box::pin(async { Ok(None) })
        }

        fn save_config<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _waddle_id: &'a str,
            _channel_id: &'a str,
            _config: &'a RoomConfig,
        ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn save_subject<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _subject: Option<&'a waddle_xmpp::muc::SubjectState>,
        ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn save_affiliation<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _entry: &'a waddle_xmpp::muc::affiliation::AffiliationEntry,
        ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[cfg(feature = "clustering")]
    fn claim_fence(room: &BareJid, epoch: i64) -> waddle_xmpp::muc::RoomClaimFenceContext {
        waddle_xmpp::muc::RoomClaimFenceContext {
            entity: waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                room.to_string(),
            ),
            epoch: waddle_xmpp::ownership::ClaimEpoch(epoch),
            owner: waddle_xmpp::ownership::NodeIdentity::new("node-a", "node-a-epoch"),
        }
    }

    #[cfg(feature = "clustering")]
    #[tokio::test]
    async fn clustered_direct_system_event_with_exact_e1_fence_remains_authorized() {
        let connections = ConnectionRegistry::new();
        let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::new(vec![b'c'; 32]).expect("test secret"),
        ));
        let room: BareJid = "clustered-system@muc.example.com"
            .parse()
            .expect("room JID");
        let actor = room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "clustered".to_string(),
                channel_id: "clustered-system".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room");
        let fence = claim_fence(&room, 1);
        actor
            .ask(BindRoomClaimFence {
                fence: fence.clone(),
            })
            .await
            .expect("bind exact fence");
        let durable_store: std::sync::Arc<dyn waddle_xmpp::muc::MucDurableStore> =
            std::sync::Arc::new(AlwaysOwnedStore);
        let mut deps = Deps::registry_only(&connections);
        deps.room_registry = Some(&room_registry);
        deps.muc_durable_store = Some(&durable_store);
        deps.clustered_muc_ownership_required = true;

        let mut message = Message::new(Some(Jid::from(room.clone())));
        message.from = Some(Jid::from(room.clone()));
        message.type_ = XmppMessageType::Groupchat;
        let result = broadcast_room_system_message_event(&deps, room, Box::new(message), 0).await;

        assert!(result.is_some());
        assert!(actor.is_alive());
    }

    #[tokio::test]
    async fn direct_system_event_snapshot_cannot_fan_out_after_actor_replacement() {
        let connections = ConnectionRegistry::new();
        let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::new(vec![b'd'; 32]).expect("test secret"),
        ));
        let room: BareJid = "direct-system@muc.example.com".parse().expect("room JID");
        let original = room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "original".to_string(),
                channel_id: "direct-system-original".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create E1");
        #[cfg(feature = "clustering")]
        let original_fence = claim_fence(&room, 1);
        #[cfg(feature = "clustering")]
        original
            .ask(BindRoomClaimFence {
                fence: original_fence.clone(),
            })
            .await
            .expect("bind E1 fence");
        #[cfg(feature = "clustering")]
        let durable_store: std::sync::Arc<dyn waddle_xmpp::muc::MucDurableStore> =
            std::sync::Arc::new(AlwaysOwnedStore);
        let mut deps = Deps::registry_only(&connections);
        deps.room_registry = Some(&room_registry);
        #[cfg(feature = "clustering")]
        {
            deps.muc_durable_store = Some(&durable_store);
            deps.clustered_muc_ownership_required = true;
        }
        let exact_deps = bind_room_system_actor(&deps, &original);
        #[cfg(feature = "clustering")]
        let exact_deps = {
            let mut exact_deps = exact_deps;
            exact_deps.room_claim_fence = Some(original_fence);
            exact_deps
        };
        // The first recipient's per-enqueue proof succeeds under E1.
        assert!(room_system_fanout_authorized(&exact_deps, &room).await);

        // Model a direct event after it resolved E1 and captured its snapshot,
        // while its asynchronous archive write was still pending.
        room_registry
            .ask(DestroyRoom {
                room_jid: room.clone(),
            })
            .await
            .expect("remove E1");
        let replacement = room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "replacement".to_string(),
                channel_id: "direct-system-replacement".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create E2");
        // Re-entry before recipient two observes replacement and stops the
        // remaining fan-out rather than adopting E2.
        let authorized = room_system_fanout_authorized(&exact_deps, &room).await;

        assert!(!authorized);
        assert!(replacement.is_alive(), "exact E1 check must preserve E2");
    }
}
