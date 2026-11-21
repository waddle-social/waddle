use waddle_xmpp::registry::SpaceDeletionSocketOutcome;

use super::{NodeId, OrderedRelayDeliveryBridge, RelayHandle, RelayTraceContext};
use crate::clustering::relay::{RelaySpaceDeletion, RelaySpaceDeletionReply};
use crate::server::routes::websocket::local_space_deletion::{
    offer_on_socket, LocalSpaceDeletionTarget,
};

impl OrderedRelayDeliveryBridge {
    pub(crate) async fn offer_local_space_deletion(
        &self,
        target: LocalSpaceDeletionTarget,
    ) -> Option<SpaceDeletionSocketOutcome> {
        let retry = Some(SpaceDeletionSocketOutcome::RetryBeforeStage);
        let Some(services) = self.services.get() else {
            return retry;
        };
        let Some(stamp) = services.node_identity.current_stamp() else {
            return retry;
        };
        let Some(node) = target
            .binding()
            .connection_ref()
            .node_lease()
            .clustered_node()
        else {
            return retry;
        };
        if node == *stamp.identity() {
            return self
                .space_deletion_local(target)
                .await
                .into_socket_outcome();
        }
        if stamp.is_revoked() {
            return retry;
        }
        let mut handle = RelayHandle::new(NodeId::new(node.node_id), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        handle
            .space_deletion(RelaySpaceDeletion {
                target,
                trace: RelayTraceContext::default(),
            })
            .await
            .ok()?
            .into_socket_outcome()
    }

    pub(crate) async fn space_deletion_local(
        &self,
        target: LocalSpaceDeletionTarget,
    ) -> RelaySpaceDeletionReply {
        use RelaySpaceDeletionReply::{NoObservation, RetryBeforeStage};

        let Some(services) = self.services.get() else {
            return RetryBeforeStage;
        };
        let Some(stamp) = services.node_identity.current_stamp() else {
            return RetryBeforeStage;
        };
        let identity = stamp.identity();
        if target
            .binding()
            .connection_ref()
            .node_lease()
            .clustered_node()
            .as_ref()
            != Some(identity)
            || !matches!(
                services
                    .node_lease
                    .is_fresh(identity, services.node_lease_ttl)
                    .await,
                Ok(true)
            )
            || stamp.is_revoked()
        {
            return RetryBeforeStage;
        }
        let Some(state) = services.web_socket_state.upgrade() else {
            return RetryBeforeStage;
        };
        let Ok(_permit) = self.space_deletion_capacity.try_acquire() else {
            return RetryBeforeStage;
        };
        offer_on_socket(&state, target, Some((&services.node_identity, &stamp)))
            .await
            .map_or(NoObservation, RelaySpaceDeletionReply::from)
    }
}
