//! Demand recovery for committed background work, with no room creation.
use super::*;

/// Return a local open room, or restore its existing durable lifecycle.
/// Missing durable state is terminal absence, never permission to create.
/// A fresh remote claim is reported as `ClaimHeldByAnotherNode`.
pub struct GetOrRestoreDurableRoom {
    pub room_jid: BareJid,
}

impl kameo::message::Message<GetOrRestoreDurableRoom> for RoomRegistryActor {
    type Reply = DelegatedReply<Result<Option<ActorRef<RoomActor>>, RoomRegistryError>>;

    async fn handle(
        &mut self,
        msg: GetOrRestoreDurableRoom,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let room = msg.room_jid;
        if let Some(pending) = self.pending_room_preparations.get_mut(&room) {
            let restoring = match &pending.origin {
                RoomPreparationOrigin::Demand { prepared_spec } => {
                    prepared_spec.expected_lifecycle.is_some()
                }
                RoomPreparationOrigin::Reclaimed { .. } => true,
            };
            if !restoring || !Self::preparation_waiter_capacity_available(pending) {
                return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(room)));
            }
            let (delegated, reply) = ctx.reply_sender();
            if let Some(reply) = reply {
                pending
                    .waiters
                    .push(RoomPreparationWaiter::Lookup { reply });
            }
            return delegated;
        }
        match tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            self.destroy_completion_pending(&room),
        )
        .await
        {
            Ok(Ok(true)) => return ctx.reply(Ok(None)),
            Ok(Ok(false)) => {}
            Ok(Err(error)) => return ctx.reply(Err(error)),
            Err(_) => return ctx.reply(Err(RoomRegistryError::OwnershipUnavailable(room))),
        }
        match self.live_room(&room).await {
            Ok(Some(actor)) => {
                return match actor
                    .ask(GetRoomSealState)
                    .mailbox_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                    .reply_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                    .await
                {
                    Ok(RoomSealState::Open) => ctx.reply(Ok(Some(actor))),
                    Ok(_) => {
                        ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(room)))
                    }
                    Err(_) => ctx.reply(Err(RoomRegistryError::OwnershipUnavailable(room))),
                };
            }
            Ok(None) => {}
            Err(RoomRegistryError::RoomActorStateLost(_))
                if self.durable_store.is_some() && !self.rooms.contains_key(&room) => {}
            Err(error) => return ctx.reply(Err(error)),
        }
        let Some(store) = self.durable_store.clone() else {
            return ctx.reply(Ok(None));
        };
        if self.handoff_in_window(&room) || !self.can_admit_new_room_ownership_responsibility() {
            return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(room)));
        }
        // Initial creation interrupted before publication is not an existing
        // usable room. Reuse its terminal cleanup before attempting recovery.
        if let Err(error) = self
            .reconcile_stranded_preparing_room(&room, ctx.actor_ref())
            .await
        {
            return ctx.reply(Err(error));
        }
        let fence = match self.acquire_room_claim(&room, ctx.actor_ref()).await {
            Ok(fence) => fence,
            Err(error) => return ctx.reply(Err(error)),
        };
        store.establish_claim_fence(&room, fence.clone());
        let snapshot = match tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            store.load_room_state_fenced(&room, &fence),
        )
        .await
        {
            Ok(Ok(Some(snapshot))) => snapshot,
            Ok(Ok(None)) => {
                self.release_room_claim(&room, &fence).await;
                return ctx.reply(Ok(None));
            }
            _ => {
                self.release_room_claim(&room, &fence).await;
                return ctx.reply(Err(RoomRegistryError::OwnershipUnavailable(room)));
            }
        };
        let Some(coordinates) = snapshot.coordinates else {
            self.release_room_claim(&room, &fence).await;
            return ctx.reply(Err(RoomRegistryError::OwnershipUnavailable(room)));
        };
        let spec = Arc::new(RoomCreationSpec {
            expected_lifecycle: Some(coordinates.lifecycle),
            waddle_id: snapshot.waddle_id,
            channel_id: snapshot.channel_id,
            config: snapshot.config,
            initial_affiliations: Vec::new(),
            live_room_restore: None,
        });
        let (guard, _) = match self
            .prepare_room(
                room.clone(),
                RoomPreparationSpec {
                    waddle_id: spec.waddle_id.clone(),
                    channel_id: spec.channel_id.clone(),
                    config: spec.config.clone(),
                    initial_affiliations: Vec::new(),
                    live_room_restore: None,
                },
                &fence,
            )
            .await
        {
            Ok(prepared) => prepared,
            Err(_) => {
                self.release_room_claim(&room, &fence).await;
                return ctx.reply(Err(RoomRegistryError::OwnershipUnavailable(room)));
            }
        };
        self.poisoned_rooms.remove(&room);
        // This second fenced restoration verifies the captured lifecycle
        // before Activate/Publish. A destroy/recreate race cannot become Create.
        let (delegated, reply) = ctx.reply_sender();
        self.start_pending_preparation(
            room,
            fence,
            RoomPreparationOrigin::Demand {
                prepared_spec: spec,
            },
            guard,
            reply.map(|reply| RoomPreparationWaiter::Lookup { reply }),
            ctx.actor_ref().clone(),
        );
        delegated
    }
}
