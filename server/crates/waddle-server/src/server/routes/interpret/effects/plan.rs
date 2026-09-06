use super::{EffectSink, PlannedEffect, RoomExecutionPath};
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct PlanSink {
    sender: Mutex<Option<jid::FullJid>>,
    plan: Mutex<Vec<PlannedEffect>>,
    room_execution: Mutex<RoomExecutionPath>,
}

impl PlanSink {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn take(&self) -> (Vec<PlannedEffect>, RoomExecutionPath) {
        let plan = std::mem::take(&mut *self.plan.lock().expect("plan mutex"));
        let room = std::mem::take(&mut *self.room_execution.lock().expect("room execution mutex"));
        (plan, room)
    }
    pub fn snapshot(&self) -> Vec<PlannedEffect> {
        self.plan.lock().expect("plan mutex").clone()
    }
}

impl EffectSink for PlanSink {
    fn execute<'a>(
        &'a self,
        effect: PlannedEffect,
        deps: &'a super::super::Deps<'_>,
    ) -> super::sink::EffectFuture<'a> {
        Box::pin(async move {
            if matches!(effect.effect, super::Effect::Immediate(_)) {
                return super::ImmediateSink.execute(effect, deps).await;
            }
            self.record_with_outcome(effect)
        })
    }
    fn snapshot(&self) -> Vec<PlannedEffect> {
        PlanSink::snapshot(self)
    }
    fn projection_dependencies(
        &self,
        projection: super::ProjectionRef,
    ) -> Vec<super::PlanEffectDependency> {
        self.plan
            .lock()
            .expect("plan mutex")
            .get(projection.0)
            .map(|effect| effect.dependencies.clone())
            .unwrap_or_default()
    }
    fn room_execution(&self) -> RoomExecutionPath {
        self.room_execution
            .lock()
            .expect("room execution mutex")
            .clone()
    }
    fn is_planning(&self) -> bool {
        true
    }
    fn observe_sender(&self, sender: &jid::FullJid) {
        self.sender
            .lock()
            .expect("sender mutex")
            .get_or_insert_with(|| sender.clone());
    }
    fn record(&self, effect: PlannedEffect) {
        self.record_with_outcome(effect);
    }
    fn set_room_execution(&self, execution: RoomExecutionPath) {
        *self.room_execution.lock().expect("room execution mutex") = execution;
    }
}

impl PlanSink {
    fn record_with_outcome(&self, mut effect: PlannedEffect) -> super::EffectOutcome {
        super::policy_metadata::apply_policy(
            &mut effect,
            self.sender.lock().expect("sender mutex").as_ref(),
        );
        if let super::Effect::External(super::ExternalEffect::Frame(stanza)) = &effect.effect {
            if let waddle_xmpp::Stanza::Message(message) = stanza.as_ref() {
                for dependency in super::policy_metadata::message_dependencies(message) {
                    if !effect.dependencies.contains(&dependency) {
                        effect.dependencies.push(dependency);
                    }
                }
            }
        }
        let mut plan = self.plan.lock().expect("plan mutex");
        let outcome = effect.assumed_outcome(super::ProjectionRef(plan.len()));
        plan.push(effect);
        outcome
    }
}
