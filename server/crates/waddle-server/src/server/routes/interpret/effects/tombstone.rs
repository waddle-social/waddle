//! Preserve the retraction arm's exemption across its nested cascade.
use super::{EffectSink, PlanSuppressionPolicy, PlannedEffect, RoomExecutionPath};

pub(crate) struct TombstoneExemptSink<'a>(pub &'a dyn EffectSink);

fn exempt(mut effect: PlannedEffect) -> PlannedEffect {
    effect.tombstone_suppression = PlanSuppressionPolicy::Always;
    effect
}

impl EffectSink for TombstoneExemptSink<'_> {
    fn execute<'a>(
        &'a self,
        effect: PlannedEffect,
        deps: &'a super::super::Deps<'_>,
    ) -> super::sink::EffectFuture<'a> {
        self.0.execute(exempt(effect), deps)
    }

    fn record(&self, effect: PlannedEffect) {
        self.0.record(exempt(effect));
    }

    fn snapshot(&self) -> Vec<PlannedEffect> {
        self.0.snapshot()
    }

    fn projection_dependencies(
        &self,
        projection: super::ProjectionRef,
    ) -> Vec<super::PlanEffectDependency> {
        self.0.projection_dependencies(projection)
    }

    fn room_execution(&self) -> RoomExecutionPath {
        self.0.room_execution()
    }

    fn set_room_execution(&self, execution: RoomExecutionPath) {
        self.0.set_room_execution(execution);
    }

    fn observe_message(&self, message: &xmpp_parsers::message::Message) {
        self.0.observe_message(message);
    }
    fn message(&self) -> Option<xmpp_parsers::message::Message> {
        self.0.message()
    }
    fn observe_sender(&self, sender: &jid::FullJid) {
        self.0.observe_sender(sender);
    }

    fn fail_plan(&self, failure: super::PlanFailure) {
        self.0.fail_plan(failure);
    }
    fn is_planning(&self) -> bool {
        self.0.is_planning()
    }
}
