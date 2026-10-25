use super::{
    close_call_setup_from_outcome, debug, deliver_peer_to_live_only, interpret_with_depth, routing,
    run_fanout_recipient_pass, run_headless_recipient_pass, warn, BareJid, DefinedCondition, Deps,
    ErrorType, FanoutPassResult, FullJid, FullJidDeliveryOutcome, Jid, OutboundEvent, Stanza,
    StanzaError, XmppMessageType, MAX_RECIPIENT_PASS_DEPTH,
};
use crate::ingress::identity::IngressAppendObligationRef;
use crate::ingress_uow::DispatchReadiness;
use std::collections::HashSet;
#[path = "route_to_connection_plan.rs"]
pub(super) mod plan;
use std::future::Future;
use std::pin::Pin;
use waddle_xmpp::ingress::IngressEffectIntent;
use waddle_xmpp::telemetry::call::PendingCallSetupRoute;
use xmpp_parsers::iq::Iq;

#[cfg(all(test, feature = "clustering"))]
#[derive(Clone)]
pub(crate) enum ControlledMucRelay {
    Outcome(Option<FullJidDeliveryOutcome>),
    Pending,
    OwnerRefresh(std::sync::Arc<crate::clustering::route_bridge::OrderedRelayDeliveryServices>),
}

#[cfg(all(test, feature = "clustering"))]
tokio::task_local! {
    pub(crate) static CONTROLLED_MUC_RELAY: (ControlledMucRelay, std::sync::Arc<std::sync::Mutex<Vec<jid::FullJid>>>);
}

#[cfg(all(test, feature = "clustering"))]
tokio::task_local! {
    pub(crate) static CONTROLLED_REGISTERED_REMOTE_DELIVERY: (
        FullJidDeliveryOutcome,
        std::sync::Arc<std::sync::Mutex<Vec<(jid::FullJid, Stanza)>>>,
    );
}

#[cfg(test)]
#[path = "routing_plan_failure_tests.rs"]
mod routing_plan_failure_tests;

mod implementation;
use implementation::bounce_for_account_rejection;
pub(crate) use implementation::bounce_undeliverable_iq;
pub(super) use implementation::deliver_bare_jid_via_ordered_relay;
pub(crate) use implementation::deliver_direct_to_full_locally;
pub(crate) use implementation::deliver_direct_to_full_with_registered_remote;
pub(crate) use implementation::deliver_direct_to_full_with_registered_remote_at;
pub(crate) use implementation::deliver_full_jid_via_ordered_relay;
pub(crate) use implementation::deliver_full_jid_via_ordered_relay_at;
pub(crate) use implementation::deliver_ordered_local_copy;
pub(crate) use implementation::deliver_peer_to_full_with_registered_remote;
pub(crate) use implementation::deliver_peer_to_full_with_registered_remote_at;
pub(crate) use implementation::deliver_registered_remote_resource;
pub(crate) use implementation::deliver_registered_remote_resource_at;
pub(crate) use implementation::queue_processed_for_detached;
pub(crate) use implementation::queue_processed_for_detached_at;
pub(super) use implementation::retry_unqueued_detached_as_live;
pub(crate) use implementation::route_to_connection;
pub(crate) use implementation::undeliverable_iq_reply;
pub(crate) use implementation::DetachedQueueOutcome;
