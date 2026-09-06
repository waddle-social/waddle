use super::{
    DurableEffect, Effect, EffectOutcome, EffectSink, ExternalEffect, PlannedEffect,
    RoomExecutionPath,
};

#[cfg(test)]
tokio::task_local! {
    static HANG_EXTERNAL: std::sync::Arc<tokio::sync::Notify>;
}

/// Executes the frozen typed operation and returns its actual result.
#[derive(Clone, Copy, Debug, Default)]
pub struct ImmediateSink;

impl EffectSink for ImmediateSink {
    fn execute<'a>(
        &'a self,
        effect: PlannedEffect,
        deps: &'a super::super::Deps<'_>,
    ) -> super::sink::EffectFuture<'a> {
        Box::pin(async move {
            self.execute_with_applied(effect, deps, &super::AppliedDurableEffects::default())
                .await
        })
    }
    fn is_planning(&self) -> bool {
        false
    }
    fn record(&self, _effect: PlannedEffect) {
        panic!("ImmediateSink requires execute, not record");
    }
    fn set_room_execution(&self, _execution: RoomExecutionPath) {}
}

impl ImmediateSink {
    /// Stall a real Phase-C operation after entry, independently of commit.
    #[cfg(test)]
    pub(crate) async fn with_hanging_external<T>(
        entered: std::sync::Arc<tokio::sync::Notify>,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        HANG_EXTERNAL.scope(entered, future).await
    }

    /// Execute Phase C using only the outcomes published by the committed Phase B attempt.
    pub fn execute_with_applied<'a>(
        &'a self,
        effect: PlannedEffect,
        deps: &'a super::super::Deps<'_>,
        applied: &'a super::AppliedDurableEffects,
    ) -> super::sink::EffectFuture<'a> {
        #[cfg(test)]
        if matches!(&effect.effect, Effect::External(_))
            && HANG_EXTERNAL
                .try_with(|entered| entered.notify_one())
                .is_ok()
        {
            return Box::pin(std::future::pending());
        }
        // Box each executor before composing it into an async state machine.
        // An encompassing async match embeds its largest branch and inflates
        // every websocket future that can dispatch a message.
        match effect.effect {
            Effect::External(ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route)) => Box::pin(super::invite::execute(route, deps)),
            Effect::External(ExternalEffect::RoomMembershipMutation(mutation)) => match mutation {
                super::early::RoomMembershipMutation::GroupDm(mutation) => Box::pin(crate::server::routes::websocket::handlers::message::group_dm_invite::execute_group_dm_membership(*mutation, deps)),
                super::early::RoomMembershipMutation::Muc(mutation) => Box::pin(crate::server::routes::websocket::handlers::message::muc_invite::execute_muc_membership(*mutation, deps)),
            },
            Effect::External(ExternalEffect::InviteLedger(mutation)) => Box::pin(crate::server::routes::websocket::handlers::message::muc_invite::execute_invite_ledger(mutation, deps)),
            Effect::External(ExternalEffect::DmPinMutation(mutation)) => Box::pin(crate::server::routes::websocket::handlers::message::dm_pin::execute_dm_pin(mutation, deps)),
            Effect::Durable(DurableEffect::Direct(effect)) => {
                Box::pin(super::direct_immediate::execute_durable(effect, deps))
            }
            Effect::Durable(DurableEffect::Room(effect)) => {
                Box::pin(super::room_immediate::execute_durable(effect, deps))
            }
            Effect::External(ExternalEffect::Direct(effect)) => Box::pin(
                super::direct_immediate::execute_external(effect, deps, applied),
            ),
            Effect::External(ExternalEffect::Room(effect)) => {
                Box::pin(super::room_immediate::execute_external(effect, deps))
            }
            Effect::External(ExternalEffect::Delivery(effect)) => {
                Box::pin(super::delivery_immediate::execute(effect, deps))
            }
            Effect::External(ExternalEffect::Frame(stanza)) => {
                Box::pin(async move { EffectOutcome::Frames(vec![*stanza]) })
            }
            Effect::Immediate(action) => Box::pin(execute_recovery(action, deps)),
        }
    }
}

async fn execute_recovery(
    action: super::ImmediateAction,
    deps: &super::super::Deps<'_>,
) -> EffectOutcome {
    use waddle_xmpp::muc::room_registry_actor::{DemoteRoomIfExactActor, GetOrCreateRoom};
    let Some(registry) = deps.room_registry else {
        return EffectOutcome::Unavailable;
    };
    match action {
        super::ImmediateAction::DemoteRoomIfExactActor { room, actor } => {
            match registry
                .ask(DemoteRoomIfExactActor {
                    room_jid: room,
                    actor_ref: actor,
                })
                .reply_timeout(std::time::Duration::from_secs(5))
                .await
            {
                Ok(_) => EffectOutcome::Completed,
                Err(_) => EffectOutcome::Unavailable,
            }
        }
        super::ImmediateAction::GetOrCreateRoom { room, snapshot } => {
            match registry
                .ask(GetOrCreateRoom {
                    room_jid: room,
                    waddle_id: snapshot.room.waddle_id,
                    channel_id: snapshot.room.channel_id,
                    config: snapshot.room.config,
                })
                .reply_timeout(std::time::Duration::from_secs(5))
                .await
            {
                Ok(_) => EffectOutcome::Completed,
                Err(_) => EffectOutcome::Unavailable,
            }
        }
    }
}
