use super::super::{
    ActorRef, Arc, AtomicBool, ConnectionEntry, ConnectionRegistry, Future,
    OrderedRelayDeliveryBridge, OrderedRelayDeliveryServices, Ordering,
    RelayRemoteResourceRegistrationStatus, RelayRemoteResourceUpdateReply,
    RelayRemoteResourceUpdateStatus, RelayUpdateRemoteUserResource, RemoteOwnerRegistration,
    RemoteResourceStateUpdate, ORDERED_DELIVERY_MAILBOX_TIMEOUT,
};
use super::apply_remote_resource_presence_to_registry;
use super::owner_update_order::{
    RemoteResourceUpdateKey, MAX_ORDERED_REMOTE_RESOURCE_UPDATE_REGISTRATIONS,
    MAX_PENDING_REMOTE_RESOURCE_UPDATES_PER_REGISTRATION,
};

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

pub(in super::super) async fn owner_remote_entry_if_current(
    actor: &ActorRef<waddle_xmpp::registry::user_actor::UserActor>,
    registry: &ConnectionRegistry,
    jid: &jid::FullJid,
    owner: &Arc<AtomicBool>,
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
    if !Arc::ptr_eq(&actor_entry.carbons_enabled, owner) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::route_bridge::tests::{
        origin_identity, receiver_identity, services_with_claims, test_peer_id,
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

    async fn seed_owner_registration(
        bridge: &Arc<OrderedRelayDeliveryBridge>,
        services: &Arc<OrderedRelayDeliveryServices>,
        target: &str,
    ) -> (jid::FullJid, RemoteOwnerRegistration) {
        let target = target.parse::<jid::FullJid>().expect("valid full jid");
        let (tx, _rx) = mpsc::channel(1);
        let entry = ConnectionEntry::new(tx);
        let owner = entry.carbons_handle();
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
