use super::super::*;
use super::apply_remote_resource_presence_to_registry;

impl OrderedRelayDeliveryBridge {
    pub(crate) async fn update_remote_user_resource_on_owner(
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
