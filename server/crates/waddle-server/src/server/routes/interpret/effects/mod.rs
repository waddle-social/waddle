//! Typed effects shared by immediate dispatch and ingress planning.
pub mod delivery;
mod delivery_immediate;
pub mod direct;
mod direct_immediate;
mod immediate;
mod outcome;
pub mod room;
mod room_immediate;
pub use outcome::EffectOutcome;
mod plan;
mod planned;
mod policy;
mod policy_metadata;
mod sink;

pub use immediate::ImmediateSink;
pub use plan::PlanSink;
pub use planned::{
    DurableEffect, Effect, ExternalEffect, ImmediateAction, IngressPlan, PlannedEffect,
    RoomExecutionPath,
};
pub use policy::{PlanEffectDependency, PlanSuppressionPolicy};
pub use sink::EffectSink;
