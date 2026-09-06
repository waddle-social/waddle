use super::{
    delivery::ExternalDeliveryEffect,
    direct::{DurableDirectEffect, ExternalDirectEffect},
    room::{DurableRoomEffect, ExternalRoomEffect, RoomFenceRequirement},
    PlanEffectDependency, PlanSuppressionPolicy,
};
use jid::BareJid;
use waddle_xmpp::{
    ingress::{IngressEffectIntent, RelayTargetIdentity},
    Stanza,
};
use xmpp_parsers::message::Message;

#[derive(Clone, Debug)]
pub enum DurableEffect {
    Direct(DurableDirectEffect),
    Room(DurableRoomEffect),
}

#[derive(Clone, Debug)]
pub enum ExternalEffect {
    RouteToPeer(super::invite::MucUserRoute),
    QueueOfflineDelivery(super::invite::MucUserRoute),
    RoomMembershipMutation(super::early::RoomMembershipMutation),
    InviteLedger(
        crate::server::routes::websocket::handlers::message::muc_invite::InviteLedgerMutation,
    ),
    DmPinMutation(crate::server::routes::websocket::handlers::message::dm_pin::DmPinMutation),
    Frame(Box<Stanza>),
    Direct(ExternalDirectEffect),
    Room(ExternalRoomEffect),
    Delivery(ExternalDeliveryEffect),
}

/// Recovery actions may run before commit: they repair an unavailable actor,
/// never apply the planned message. Metrics likewise convey no message authority.
#[derive(Clone, Debug)]
pub enum ImmediateAction {
    /// Evicts only the failed actor incarnation so a successor can recover.
    DemoteRoomIfExactActor {
        room: BareJid,
        actor: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    },
    /// Reconstructs a missing room actor from committed state after eviction.
    GetOrCreateRoom {
        room: BareJid,
        snapshot: Box<waddle_xmpp::muc::room_actor::RoomSnapshot>,
    },
}

#[derive(Clone, Debug)]
pub enum Effect {
    Durable(DurableEffect),
    External(ExternalEffect),
    Immediate(ImmediateAction),
}

#[derive(Clone, Debug)]
pub struct PlannedEffect {
    pub effect: Effect,
    pub dependencies: Vec<PlanEffectDependency>,
    /// Duplicate policy. A sender reply can survive a duplicate and still be
    /// swallowed by a request tombstone, so these policies are independent.
    pub suppression: PlanSuppressionPolicy,
    pub tombstone_suppression: PlanSuppressionPolicy,
}

impl PlannedEffect {
    pub fn new(effect: Effect) -> Self {
        Self {
            effect,
            dependencies: Vec::new(),
            suppression: PlanSuppressionPolicy::Always,
            tombstone_suppression: PlanSuppressionPolicy::TombstoneSwallowed,
        }
    }
    pub fn with_dependency(mut self, dependency: PlanEffectDependency) -> Self {
        self.dependencies.push(dependency);
        self
    }
    pub fn with_suppression(mut self, policy: PlanSuppressionPolicy) -> Self {
        self.suppression = policy;
        self
    }
}

#[derive(Clone, Debug, Default)]
pub enum RoomExecutionPath {
    #[default]
    None,
    Local {
        room: BareJid,
        /// Single-node rooms have no distributed claim; never manufacture one.
        fence: RoomFenceRequirement,
        snapshot_generation: u64,
    },
    Remote {
        room: BareJid,
        relay_target: RelayTargetIdentity,
    },
}

#[derive(Clone, Debug)]
pub struct IngressPlan {
    pub rejection: Option<super::PlanRejection>,
    pub plan: Vec<PlannedEffect>,
    pub intents: Vec<IngressEffectIntent>,
    pub sanitized_message: Message,
    pub error_reply: Option<Stanza>,
    pub room_execution: RoomExecutionPath,
}
