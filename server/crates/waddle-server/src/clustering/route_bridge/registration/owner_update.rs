use super::super::{
    ActorRef, Arc, ConnectionEntry, ConnectionRegistry, Future, NodeId, OrderedRelayDeliveryBridge,
    OrderedRelayDeliveryServices, Ordering, RelayRemoteResourceRegistrationStatus,
    RelayRemoteResourceUpdateReply, RelayRemoteResourceUpdateStatus, RelayUpdateRemoteUserResource,
    RemoteOwnerRegistration, RemoteResourceStateUpdate, ORDERED_DELIVERY_MAILBOX_TIMEOUT,
};
use super::apply_remote_resource_presence_to_registry;
use super::owner_update_order::{
    RemoteResourceUpdateKey, MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS,
    MAX_PENDING_REMOTE_RESOURCE_UPDATES_PER_REGISTRATION,
};

#[cfg(test)]
tokio::task_local! {
    static REMOTE_UPDATE_SOURCE_LEASE_GATE: (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
}

impl OrderedRelayDeliveryBridge {
    /// Owner-side entry point for `remote_resource_update` (#1685 interim).
    /// Admission happens synchronously when this is called — the relay
    /// actor calls it in its own turn — so the returned future applies the
    /// update only after every earlier-arrived update for the same exact
    /// registration has finished. When the bounded order map is full the
    /// update is answered `Unavailable` with no effect; both sender paths
    /// treat that as "resync the full snapshot".
    pub(crate) fn admit_remote_user_resource_update(
        self: &Arc<Self>,
        msg: RelayUpdateRemoteUserResource,
    ) -> impl Future<Output = RelayRemoteResourceUpdateReply> + Send + 'static {
        let admitted = self
            .remote_resource_update_order
            .admit(RemoteResourceUpdateKey {
                jid: msg.jid.clone(),
                registration_id: msg.registration_id,
                socket_generation: msg.socket_generation,
            });
        let bridge = Arc::clone(self);
        async move {
            let mut turn = match admitted {
                Ok(turn) => turn,
                Err(full) => {
                    tracing::warn!(
                        jid = %msg.jid,
                        ?full,
                        registration_limit = MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS,
                        queue_limit = MAX_PENDING_REMOTE_RESOURCE_UPDATES_PER_REGISTRATION,
                        "clustered remote-resource update order is full; \
                         answering unavailable without applying"
                    );
                    return RelayRemoteResourceUpdateReply {
                        status: RelayRemoteResourceUpdateStatus::Unavailable,
                    };
                }
            };
            turn.wait_for_predecessors().await;
            let reply = bridge.update_remote_user_resource_on_owner(msg).await;
            // Release the successor only after this update has applied.
            drop(turn);
            reply
        }
    }

    async fn update_remote_user_resource_on_owner(
        &self,
        msg: RelayUpdateRemoteUserResource,
    ) -> RelayRemoteResourceUpdateReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceUpdateReply {
                status: RelayRemoteResourceUpdateStatus::Unavailable,
            };
        };
        let registration = self
            .remote_owner_resources
            .lock()
            .await
            .get(&msg.jid)
            .filter(|registration| {
                registration.registration_id == msg.registration_id
                    && registration.socket_generation == msg.socket_generation
            })
            .cloned();
        let Some(registration) = registration else {
            return RelayRemoteResourceUpdateReply {
                status: RelayRemoteResourceUpdateStatus::StaleRegistration,
            };
        };
        let actor = match services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: msg.jid.to_bare(),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => {
                return RelayRemoteResourceUpdateReply {
                    status: RelayRemoteResourceUpdateStatus::StaleRegistration,
                };
            }
            Err(error) => {
                tracing::warn!(
                    jid = %msg.jid,
                    %error,
                    "clustered remote-resource state update could not resolve owner UserActor"
                );
                return RelayRemoteResourceUpdateReply {
                    status: RelayRemoteResourceUpdateStatus::Unavailable,
                };
            }
        };
        let jid = msg.jid;
        let status = match msg.update {
            RemoteResourceStateUpdate::Presence {
                available,
                priority,
                state,
            } => {
                match owner_remote_entry_if_current(
                    &actor,
                    &services.connection_registry,
                    &jid,
                    &registration.owner,
                )
                .await
                {
                    Ok(_) => {
                        if let Err(status) =
                            remote_owner_entry_source_lease_is_current(&services, &registration)
                                .await
                        {
                            return RelayRemoteResourceUpdateReply { status };
                        }
                        if apply_remote_resource_presence_to_registry(
                            &services.connection_registry,
                            &jid,
                            &registration.owner,
                            available,
                            priority,
                            state,
                        ) {
                            RelayRemoteResourceUpdateStatus::Updated
                        } else {
                            RelayRemoteResourceUpdateStatus::StaleRegistration
                        }
                    }
                    Err(status) => status,
                }
            }
            RemoteResourceStateUpdate::Carbons { enabled } => {
                match owner_remote_entry_if_current(
                    &actor,
                    &services.connection_registry,
                    &jid,
                    &registration.owner,
                )
                .await
                {
                    Ok(entry) => {
                        if let Err(status) =
                            remote_owner_entry_source_lease_is_current(&services, &registration)
                                .await
                        {
                            return RelayRemoteResourceUpdateReply { status };
                        }
                        entry.carbons_enabled.store(enabled, Ordering::Relaxed);
                        RelayRemoteResourceUpdateStatus::Updated
                    }
                    Err(status) => status,
                }
            }
            RemoteResourceStateUpdate::RosterInterested => match owner_remote_entry_if_current(
                &actor,
                &services.connection_registry,
                &jid,
                &registration.owner,
            )
            .await
            {
                Ok(entry) => {
                    if let Err(status) =
                        remote_owner_entry_source_lease_is_current(&services, &registration).await
                    {
                        return RelayRemoteResourceUpdateReply { status };
                    }
                    entry.roster_interested.store(true, Ordering::Relaxed);
                    RelayRemoteResourceUpdateStatus::Updated
                }
                Err(status) => status,
            },
            RemoteResourceStateUpdate::BlocklistInterested => {
                match owner_remote_entry_if_current(
                    &actor,
                    &services.connection_registry,
                    &jid,
                    &registration.owner,
                )
                .await
                {
                    Ok(entry) => {
                        if let Err(status) =
                            remote_owner_entry_source_lease_is_current(&services, &registration)
                                .await
                        {
                            return RelayRemoteResourceUpdateReply { status };
                        }
                        entry.blocklist_interested.store(true, Ordering::Relaxed);
                        RelayRemoteResourceUpdateStatus::Updated
                    }
                    Err(status) => status,
                }
            }
        };
        RelayRemoteResourceUpdateReply { status }
    }
}

async fn remote_owner_entry_source_lease_is_current(
    services: &OrderedRelayDeliveryServices,
    registration: &RemoteOwnerRegistration,
) -> Result<(), RelayRemoteResourceUpdateStatus> {
    #[cfg(test)]
    if let Ok((entered, release)) = REMOTE_UPDATE_SOURCE_LEASE_GATE.try_with(Clone::clone) {
        entered.notify_one();
        release.notified().await;
    }

    remote_source_connection_lease_is_current(
        services,
        &registration.socket_node,
        &registration.connection_ref,
    )
    .await
    .map(|_| ())
    .map_err(|status| match status {
        RelayRemoteResourceRegistrationStatus::StaleRegistration => {
            RelayRemoteResourceUpdateStatus::StaleRegistration
        }
        RelayRemoteResourceRegistrationStatus::Unavailable => {
            RelayRemoteResourceUpdateStatus::Unavailable
        }
        RelayRemoteResourceRegistrationStatus::Registered
        | RelayRemoteResourceRegistrationStatus::NotOwner
        | RelayRemoteResourceRegistrationStatus::Busy => {
            RelayRemoteResourceUpdateStatus::StaleRegistration
        }
    })
}

pub(in super::super) async fn owner_remote_entry_if_current(
    actor: &ActorRef<waddle_xmpp::registry::user_actor::UserActor>,
    registry: &ConnectionRegistry,
    jid: &jid::FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
) -> Result<ConnectionEntry, RelayRemoteResourceUpdateStatus> {
    let actor_entry = match actor
        .ask(waddle_xmpp::registry::GetConnectionEntry { jid: jid.clone() })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(Some(entry)) => entry,
        Ok(None) => return Err(RelayRemoteResourceUpdateStatus::StaleRegistration),
        Err(_) => return Err(RelayRemoteResourceUpdateStatus::Unavailable),
    };
    if actor_entry.binding_id() != *owner {
        return Err(RelayRemoteResourceUpdateStatus::StaleRegistration);
    }
    let Some(registry_entry) = registry.entry_if_owner(jid, owner) else {
        return Err(RelayRemoteResourceUpdateStatus::StaleRegistration);
    };
    if !Arc::ptr_eq(
        &registry_entry.carbons_enabled,
        &actor_entry.carbons_enabled,
    ) {
        return Err(RelayRemoteResourceUpdateStatus::StaleRegistration);
    }
    Ok(registry_entry)
}

pub(super) async fn remote_owner_registration_is_current(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
    registration: &RemoteOwnerRegistration,
) -> Result<(), RelayRemoteResourceRegistrationStatus> {
    remote_source_connection_lease_is_current(
        services,
        &registration.socket_node,
        &registration.connection_ref,
    )
    .await?;
    let actor = match services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: jid.to_bare(),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => return Err(RelayRemoteResourceRegistrationStatus::StaleRegistration),
        Err(_) => return Err(RelayRemoteResourceRegistrationStatus::Unavailable),
    };
    owner_remote_entry_if_current(
        &actor,
        &services.connection_registry,
        jid,
        &registration.owner,
    )
    .await
    .map(|_| ())
    .map_err(|status| match status {
        RelayRemoteResourceUpdateStatus::Updated => {
            RelayRemoteResourceRegistrationStatus::Registered
        }
        RelayRemoteResourceUpdateStatus::StaleRegistration => {
            RelayRemoteResourceRegistrationStatus::StaleRegistration
        }
        RelayRemoteResourceUpdateStatus::Unavailable => {
            RelayRemoteResourceRegistrationStatus::Unavailable
        }
    })
}

pub(super) async fn remote_source_connection_lease_is_current(
    services: &OrderedRelayDeliveryServices,
    socket_node: &NodeId,
    connection_ref: &waddle_xmpp::ownership::ConnectionRef,
) -> Result<waddle_xmpp::ownership::NodeIdentity, RelayRemoteResourceRegistrationStatus> {
    match tokio::time::timeout(
        ORDERED_DELIVERY_MAILBOX_TIMEOUT,
        services.node_lease.unexpired_node_identity(socket_node),
    )
    .await
    {
        Ok(Ok(Some(identity)))
            if identity.node_id == socket_node.as_str()
                && waddle_xmpp::ownership::NodeLeaseIdentity::from_node_identity(&identity)
                    == *connection_ref.node_lease() =>
        {
            Ok(identity)
        }
        Ok(Ok(Some(_))) => Err(RelayRemoteResourceRegistrationStatus::StaleRegistration),
        Ok(Ok(None)) => Err(RelayRemoteResourceRegistrationStatus::StaleRegistration),
        Ok(Err(_)) | Err(_) => Err(RelayRemoteResourceRegistrationStatus::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::route_bridge::tests::{
        origin_identity, receiver_identity, services_with_claims, test_peer_id, SocketLeaseRead,
        StaticNodeLease,
    };
    use crate::clustering::route_bridge::{
        RemoteResourceRegistrationId, RemoteResourceSocketGeneration,
    };
    use crate::clustering::trace_context::RelayTraceContext;
    use crate::clustering::NodeId;
    use crate::config::ClusteringMessagingConfig;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};
    use tokio_util::sync::CancellationToken;
    use waddle_xmpp::ownership::NodeIdentity;

    /// Long enough for an unfenced update to finish its two local asks.
    const BLOCKED_WINDOW: Duration = Duration::from_millis(250);

    async fn wired_bridge() -> (
        Arc<OrderedRelayDeliveryBridge>,
        Arc<OrderedRelayDeliveryServices>,
    ) {
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                receiver_identity(),
                receiver_identity(),
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(services.clone());
        (bridge, services)
    }

    async fn wired_bridge_with_socket_lease() -> (
        Arc<OrderedRelayDeliveryBridge>,
        Arc<OrderedRelayDeliveryServices>,
        Arc<std::sync::Mutex<SocketLeaseRead>>,
    ) {
        let peer_id = test_peer_id();
        let socket_read = Arc::new(std::sync::Mutex::new(SocketLeaseRead::Present(
            NodeIdentity::new("socket-node", "fixture-epoch"),
        )));
        let mut services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            peer_id.clone(),
        )
        .await;
        services.node_lease = Arc::new(StaticNodeLease {
            socket_read: Some(Arc::clone(&socket_read)),
            origin: origin_identity(),
            peer_id,
        });
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(services.clone());
        (bridge, services, socket_read)
    }

    async fn seed_owner_registration(
        bridge: &Arc<OrderedRelayDeliveryBridge>,
        services: &Arc<OrderedRelayDeliveryServices>,
        target: &str,
    ) -> (jid::FullJid, RemoteOwnerRegistration) {
        let target = target.parse::<jid::FullJid>().expect("valid full jid");
        let (tx, _rx) = mpsc::channel(1);
        let connection_ref = waddle_xmpp::ownership::ConnectionRef::fresh(
            waddle_xmpp::ownership::NodeLeaseIdentity::from_node_identity(&NodeIdentity::new(
                "socket-node",
                "fixture-epoch",
            )),
        );
        let entry = ConnectionEntry::remote_hosted_for_connection(tx, connection_ref.clone());
        let owner = entry.binding_id();
        services
            .connection_registry
            .register_entry(target.clone(), entry.clone());
        services
            .user_registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: target.clone(),
                entry,
            })
            .await
            .expect("register owner mirror");
        let registration = RemoteOwnerRegistration {
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            socket_identity: NodeIdentity::new("fixture-socket", "fixture-epoch"),
            connection_ref,
            unregister_pending: false,
            registration_id: RemoteResourceRegistrationId::fresh(),
            socket_node: NodeId::new("socket-node".to_owned()),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            owner,
        };
        bridge
            .remote_owner_resources
            .lock()
            .await
            .insert(target.clone(), registration.clone());
        (target, registration)
    }

    fn presence_update(
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
        priority: i8,
    ) -> RelayUpdateRemoteUserResource {
        RelayUpdateRemoteUserResource {
            jid: jid.clone(),
            registration_id: registration.registration_id,
            socket_generation: registration.socket_generation,
            update: RemoteResourceStateUpdate::Presence {
                available: true,
                priority,
                state: None,
            },
            trace: RelayTraceContext::default(),
        }
    }

    #[tokio::test]
    async fn delayed_remote_state_update_rejects_rotated_source_lease_before_mutation() {
        let (bridge, services, socket_read) = wired_bridge_with_socket_lease().await;
        let (jid, registration) =
            seed_owner_registration(&bridge, &services, "juliet@example.test/phone").await;

        assert_eq!(
            bridge
                .admit_remote_user_resource_update(presence_update(&jid, &registration, 1))
                .await
                .status,
            RelayRemoteResourceUpdateStatus::Updated
        );
        assert_eq!(mirrored_priority(&services, &jid), Some(1));

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut pending = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let gate = (Arc::clone(&entered), Arc::clone(&release));
            let update = presence_update(&jid, &registration, 9);
            async move {
                REMOTE_UPDATE_SOURCE_LEASE_GATE
                    .scope(gate, bridge.admit_remote_user_resource_update(update))
                    .await
            }
        });
        timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("update reached its source lease boundary");
        *socket_read
            .lock()
            .expect("socket lease fixture lock must not be poisoned") =
            SocketLeaseRead::Present(NodeIdentity::new("socket-node", "epoch-after-rotation"));
        release.notify_one();

        assert_eq!(
            timeout(Duration::from_secs(5), &mut pending)
                .await
                .expect("update completes")
                .expect("update task")
                .status,
            RelayRemoteResourceUpdateStatus::StaleRegistration
        );
        assert_eq!(
            mirrored_priority(&services, &jid),
            Some(1),
            "a queued update from the previous source lease cannot change the mirror"
        );
    }

    fn mirrored_priority(
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
    ) -> Option<i8> {
        services
            .connection_registry
            .presence_state(jid)
            .map(|state| state.priority)
    }

    #[tokio::test]
    async fn same_registration_updates_apply_in_arrival_order_when_the_first_is_delayed() {
        let (bridge, services) = wired_bridge().await;
        let (jid, registration) =
            seed_owner_registration(&bridge, &services, "juliet@example.test/phone").await;

        // Arrival order: A (priority 1) then B (priority 2).
        let first =
            bridge.admit_remote_user_resource_update(presence_update(&jid, &registration, 1));
        let second =
            bridge.admit_remote_user_resource_update(presence_update(&jid, &registration, 2));

        // B's handler is scheduled while A's has not started (delayed).
        let mut second = tokio::spawn(second);
        assert!(
            timeout(BLOCKED_WINDOW, &mut second).await.is_err(),
            "the later-arrived update must wait for the earlier one"
        );
        assert_eq!(
            mirrored_priority(&services, &jid),
            None,
            "the later-arrived update must not apply before the earlier one"
        );

        let first = tokio::spawn(first);
        assert_eq!(
            timeout(Duration::from_secs(5), first)
                .await
                .expect("first update finishes")
                .expect("first task")
                .status,
            RelayRemoteResourceUpdateStatus::Updated
        );
        assert_eq!(
            timeout(Duration::from_secs(5), second)
                .await
                .expect("second update finishes")
                .expect("second task")
                .status,
            RelayRemoteResourceUpdateStatus::Updated
        );
        assert_eq!(
            mirrored_priority(&services, &jid),
            Some(2),
            "the owner mirror ends on the last-arrived update"
        );
        assert_eq!(
            bridge.remote_resource_update_order.tracked_registrations(),
            0
        );
    }

    #[tokio::test]
    async fn updates_for_different_registrations_still_run_concurrently() {
        let (bridge, services) = wired_bridge().await;
        let (juliet, juliet_registration) =
            seed_owner_registration(&bridge, &services, "juliet@example.test/phone").await;
        let (romeo, romeo_registration) =
            seed_owner_registration(&bridge, &services, "romeo@example.test/laptop").await;

        // Juliet's update is admitted and left pending (not started).
        let juliet_pending = bridge.admit_remote_user_resource_update(presence_update(
            &juliet,
            &juliet_registration,
            1,
        ));
        // A second Juliet update, to show her registration really is held.
        let mut juliet_blocked =
            tokio::spawn(bridge.admit_remote_user_resource_update(presence_update(
                &juliet,
                &juliet_registration,
                2,
            )));

        let romeo_reply = timeout(
            Duration::from_secs(5),
            bridge.admit_remote_user_resource_update(presence_update(
                &romeo,
                &romeo_registration,
                5,
            )),
        )
        .await
        .expect("another registration's update is not held behind Juliet's");
        assert_eq!(romeo_reply.status, RelayRemoteResourceUpdateStatus::Updated);
        assert_eq!(mirrored_priority(&services, &romeo), Some(5));
        assert!(
            timeout(BLOCKED_WINDOW, &mut juliet_blocked).await.is_err(),
            "Juliet's later update is still waiting behind her first"
        );
        assert_eq!(mirrored_priority(&services, &juliet), None);

        drop(juliet_pending);
        assert_eq!(
            timeout(Duration::from_secs(5), juliet_blocked)
                .await
                .expect("released once the predecessor is dropped")
                .expect("juliet task")
                .status,
            RelayRemoteResourceUpdateStatus::Updated
        );
        assert_eq!(mirrored_priority(&services, &juliet), Some(2));
    }
}
