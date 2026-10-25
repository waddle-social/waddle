//! Apply the payload-complete policy decisions retained by reconciliation.
use crate::ingress_substrate::MessageEnvelope;
use crate::ingress_substrate::MucArchiveContext;
use crate::ingress_uow::IngressUowError;
use crate::server::routes::interpret::effects::PlanSuppressionPolicy;
use crate::server::routes::interpret::effects::PlannedEffect;
#[path = "recorded/preview_restore.rs"]
mod preview_restore;
pub(super) use preview_restore::{freeze_preview_intents, restore_pending_preview_effects};
#[cfg(test)]
#[path = "recorded/delivery_replay_tests.rs"]
mod delivery_replay_tests;
#[cfg(test)]
#[path = "recorded/preview_replay_tests.rs"]
mod preview_replay_tests;
#[cfg(test)]
mod tests;

use crate::server::routes::interpret::effects::direct::{
    DurableDirectEffect, ExternalDirectEffect,
};
use crate::server::routes::interpret::effects::room::{
    DurableRoomEffect, ExternalRoomEffect, PlannedGroupchatNotificationRecovery, RoomActorMutation,
};
use crate::server::routes::interpret::effects::{
    DurableEffect, Effect, ExternalEffect, IngressPlan, PlanEffectDependency,
};
use waddle_xmpp::ingress::{
    GroupchatNotificationRecoveryAction, GroupchatNotificationRecoveryMutation,
    InboxProjectionMutation, IngressEffectIntent, RoomPinMutation,
};

mod progress;
mod reflection;
pub(crate) use progress::single_target;
pub use progress::{ProgressObligation, RouteProgress};
pub(super) use reflection::prepare_attempt_reflections;

mod implementation;
pub use implementation::apply_recorded_intents;
pub(super) use implementation::delivery_message;
pub(super) use implementation::external_in_recorded_audience;
pub(super) use implementation::external_route_identity;
use implementation::external_route_recipient;
use implementation::external_route_targets;
pub(super) use implementation::prepared_direct_message;
pub(super) use implementation::recorded_route_obligation;
pub(super) use implementation::restore_archive_projections;
pub use implementation::restore_delivery_payloads;
pub use implementation::restore_room_observer_envelope;
pub use implementation::restore_subject_rejection_replies;
pub use implementation::room_canonical_envelope;
