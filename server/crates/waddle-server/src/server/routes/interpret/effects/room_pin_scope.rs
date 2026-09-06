//! Bind generated pin messages to the actor mutation that makes them true.
use super::{EffectSink, PlanEffectDependency, PlannedEffect, RoomExecutionPath};

pub(crate) struct RoomPinSink<'a> {
    pub inner: &'a dyn EffectSink,
    pub dependency: PlanEffectDependency,
}

impl RoomPinSink<'_> {
    fn bind(&self, mut effect: PlannedEffect) -> PlannedEffect {
        if !effect.dependencies.contains(&self.dependency) {
            effect.dependencies.push(self.dependency.clone());
        }
        if let super::Effect::Durable(super::DurableEffect::Room(
            super::room::DurableRoomEffect::ArchiveGroupchat {
                room,
                message,
                fence,
                archive_expectation,
            },
        )) = effect.effect
        {
            effect.dependencies.retain(|dependency| {
                !matches!(dependency, PlanEffectDependency::AfterArchive { .. })
            });
            effect.effect = super::Effect::External(super::ExternalEffect::Room(
                super::room::ExternalRoomEffect::ArchiveAfterPin {
                    room,
                    message,
                    fence,
                    archive_expectation,
                },
            ));
        }
        effect
    }
}

impl EffectSink for RoomPinSink<'_> {
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
