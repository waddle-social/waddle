use std::time::Duration;

use super::{NodeId, NodeIdentity, OrderedRelayDeliveryBridge, RelayHandle};
use crate::clustering::relay::{RelayEventPlanningAccess, RelayEventPlanningAccessReply};

impl OrderedRelayDeliveryBridge {
    pub(crate) fn event_planning_access_handle(&self, owner: &NodeIdentity) -> RelayHandle {
        RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
            .with_ask_timeouts(Duration::from_secs(1), Duration::from_secs(5))
    }

    pub(crate) async fn event_planning_access_local(
        &self,
        message: RelayEventPlanningAccess,
    ) -> RelayEventPlanningAccessReply {
        match self
            .services
            .get()
            .and_then(|services| services.web_socket_state.upgrade())
        {
            Some(state) => {
                crate::server::routes::websocket::handlers::iq::event_planning_access::relayed_event_planning_access(&state, message).await
            }
            None => RelayEventPlanningAccessReply::NotOwner,
        }
    }
}
