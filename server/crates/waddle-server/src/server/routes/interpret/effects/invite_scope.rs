//! Bind invitation delivery to the ledger mutation that authorizes it.
use super::{EffectSink, PlanEffectDependency, PlannedEffect, RoomExecutionPath};
use crate::server::routes::websocket::muc_invites::OutstandingInvite;

pub(crate) struct ScopedInviteSink<'a> {
    pub inner: &'a dyn EffectSink,
    pub invite: OutstandingInvite,
    pub failure: Option<super::invite::InviteDeliveryFailure>,
}

impl ScopedInviteSink<'_> {
    fn bind(&self, mut effect: PlannedEffect) -> PlannedEffect {
        let dependency = PlanEffectDependency::AfterInviteLedger {
            invite: self.invite.clone(),
        };
        if !effect.dependencies.contains(&dependency) {
            effect.dependencies.push(dependency);
        }
        if let super::Effect::External(
            super::ExternalEffect::RouteToPeer(route)
            | super::ExternalEffect::QueueOfflineDelivery(route),
        ) = &mut effect.effect
        {
            route.failure = self.failure.clone().map(Box::new);
        }
        effect
    }
}

impl EffectSink for ScopedInviteSink<'_> {
    fn execute<'a>(
        &'a self,
        effect: PlannedEffect,
        deps: &'a super::super::Deps<'_>,
    ) -> super::sink::EffectFuture<'a> {
        self.inner.execute(self.bind(effect), deps)
    }
    fn record(&self, effect: PlannedEffect) {
        self.inner.record(self.bind(effect));
    }
    fn is_planning(&self) -> bool {
        self.inner.is_planning()
    }
    fn observe_message(&self, message: &xmpp_parsers::message::Message) {
        self.inner.observe_message(message);
    }
    fn message(&self) -> Option<xmpp_parsers::message::Message> {
        self.inner.message()
    }
    fn observe_sender(&self, sender: &jid::FullJid) {
        self.inner.observe_sender(sender);
    }
    fn snapshot(&self) -> Vec<PlannedEffect> {
        self.inner.snapshot()
    }
    fn room_execution(&self) -> RoomExecutionPath {
        self.inner.room_execution()
    }
    fn set_room_execution(&self, execution: RoomExecutionPath) {
        self.inner.set_room_execution(execution);
    }
    fn rejection(&self) -> Option<super::PlanRejection> {
        self.inner.rejection()
    }
    fn set_rejection(&self, rejection: super::PlanRejection) {
        self.inner.set_rejection(rejection);
    }
}
