//! Pure selection and reconstruction of obligations retained by canonical ingress.
use super::decision::{self, AliasOutcomeClass, IngressDecision, IngressDecisionClass};
use super::recorded::RouteProgress;
use crate::ingress_substrate::MessageEnvelope;
use crate::ingress_uow::{IngressUowError, ReconcileVerdict};
use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
use crate::server::routes::interpret::effects::direct::ExternalDirectEffect;
use crate::server::routes::interpret::effects::{
    Effect, ExternalEffect, IngressPlan, PlanEffectDependency, PlannedEffect, RoomExecutionPath,
};
use crate::server::routes::websocket::handlers::message::{dm_pin, muc_direct};
use crate::server::routes::websocket::muc_invites::INVITE_TTL;
use chrono::{DateTime, Utc};
use waddle_xmpp::inbox::storage::{
    GroupchatNotificationRecovery, GroupchatNotificationRecoveryKey,
};
use waddle_xmpp::ingress::{
    EffectMessageIdentity, GroupchatNotificationRecoveryAction, IngressEffectIntent,
    IngressEffectKind, MessageKey, NormalizedTarget, NotificationActivityMutation,
    PendingDeliveryMutation,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::MessageType;

#[cfg(test)]
#[path = "recovery_rebuild_tests.rs"]
mod tests;

#[path = "recovery_muc.rs"]
mod muc;

#[path = "recovery_rebuild_carbons.rs"]
mod carbons;

#[path = "recovery_invitation.rs"]
mod invitation;

mod implementation;
pub(super) use implementation::policy_checked_direct_route;
pub(super) use implementation::rebuild;
pub(super) use implementation::rebuildable_direct_route;
pub(super) use implementation::RebuiltRecovery;
pub(super) use implementation::RecoveryInput;
pub(crate) use implementation::RECOVERABLE_KINDS;
