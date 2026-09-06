use super::{PlannedEffect, RoomExecutionPath};

/// Interpreter write boundary. Reads execute independently of this sink.
/// Planning records the operation and uses its assumed successful outcome;
/// immediate dispatch runs the existing typed storage and delivery operation.
pub type EffectFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = super::EffectOutcome> + Send + 'a>>;

pub trait EffectSink: Send + Sync {
    fn execute<'a>(
        &'a self,
        effect: PlannedEffect,
        deps: &'a super::super::Deps<'_>,
    ) -> EffectFuture<'a>;
    fn snapshot(&self) -> Vec<PlannedEffect> {
        Vec::new()
    }
    /// Dependencies of one frozen projection, without cloning the full plan.
    fn projection_dependencies(
        &self,
        _projection: super::ProjectionRef,
    ) -> Vec<super::PlanEffectDependency> {
        Vec::new()
    }
    fn room_execution(&self) -> RoomExecutionPath {
        RoomExecutionPath::None
    }
    fn observe_sender(&self, _sender: &jid::FullJid) {}
    fn is_planning(&self) -> bool;
    fn record(&self, effect: PlannedEffect);
    fn set_room_execution(&self, execution: RoomExecutionPath);
}
