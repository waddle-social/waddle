use std::time::Duration;

use futures::{stream, StreamExt};
use jid::BareJid;

use super::{NodeId, OrderedRelayDeliveryBridge, RelayHandle, RelayTraceContext};
use crate::clustering::relay::{RelaySpaceBookmarkPublication, RelaySpaceBookmarkPublicationReply};
use crate::permissions::host_grant::{GrantOperationId, SpaceBookmarkVersion};

const PEER_FANOUT_CONCURRENCY: usize = 8;
const PEER_FANOUT_DEADLINE: Duration = Duration::from_secs(3);
const RECEIVE_HINT_DEADLINE: Duration = Duration::from_secs(5);

impl OrderedRelayDeliveryBridge {
    pub(crate) async fn offer_space_bookmark_publication(
        &self,
        service: &BareJid,
        node: &str,
        version: &SpaceBookmarkVersion,
    ) {
        let Some(services) = self.services.get() else {
            return;
        };
        let Some(membership) = &services.cluster_membership else {
            return;
        };
        let Some(stamp) = services.node_identity.current_stamp() else {
            return;
        };
        let _ = tokio::time::timeout(PEER_FANOUT_DEADLINE, async {
            let Ok(peers) = membership.peers().await else {
                return;
            };
            stream::iter(peers)
                .map(|peer| {
                    let source = &stamp;
                    async move {
                        if source.is_revoked() || peer.same_incarnation(source.identity()) {
                            return;
                        }
                        let mut handle = RelayHandle::new(
                            NodeId::new(peer.node_id.clone()),
                            self.stop_token.clone(),
                        );
                        let _ = handle
                            .space_bookmark_publication(RelaySpaceBookmarkPublication {
                                target_node_id: peer.node_id,
                                target_node_epoch: peer.node_epoch,
                                service: service.clone(),
                                node: node.to_owned(),
                                room: version.room.clone(),
                                operation: version.operation.as_uuid(),
                                trace: RelayTraceContext::default(),
                            })
                            .await;
                    }
                })
                .buffer_unordered(PEER_FANOUT_CONCURRENCY)
                .for_each(|()| async {})
                .await;
        })
        .await;
    }

    pub(crate) async fn space_bookmark_publication_local(
        &self,
        hint: RelaySpaceBookmarkPublication,
    ) -> RelaySpaceBookmarkPublicationReply {
        use RelaySpaceBookmarkPublicationReply::{Processed, Unavailable};

        let Some(services) = self.services.get() else {
            return Unavailable;
        };
        let Some(stamp) = services.node_identity.current_stamp() else {
            return Unavailable;
        };
        let target = stamp.identity();
        if target.node_id != hint.target_node_id || target.node_epoch != hint.target_node_epoch {
            return Unavailable;
        }
        let Some(state) = services.web_socket_state.upgrade() else {
            return Unavailable;
        };
        if hint.service.as_str() != state.deps.service_domains.spaces.as_str() {
            return Unavailable;
        }
        let Ok(_permit) = self.space_publication_hint_capacity.try_acquire() else {
            return Unavailable;
        };
        tokio::time::timeout(RECEIVE_HINT_DEADLINE, async {
            if !matches!(
                services
                    .node_lease
                    .is_fresh(target, services.node_lease_ttl)
                    .await,
                Ok(true)
            ) || stamp.is_revoked()
            {
                return Unavailable;
            }
            let Ok(store) = crate::permissions::host_grant::HostGrantStore::open(
                services.occupancy_database.clone(),
            ) else {
                return Unavailable;
            };
            let Ok(notification) = store
                .load_space_bookmark_notification(
                    &hint.service,
                    &hint.node,
                    &hint.room,
                    GrantOperationId::from_uuid(hint.operation),
                )
                .await
            else {
                return Unavailable;
            };
            if let Some(notification) = notification {
                crate::server::routes::websocket::local_space_events::deliver_hint(
                    &state,
                    &hint.service,
                    &hint.node,
                    notification,
                    &services.node_identity,
                    &stamp,
                )
                .await;
            }
            Processed
        })
        .await
        .unwrap_or(Unavailable)
    }
}
