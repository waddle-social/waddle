use super::super::*;
use super::owner::{unregister_remote_owner_actor_entry, RemoteOwnerActorUnregisterOutcome};

impl OrderedRelayDeliveryBridge {
    pub(super) async fn retire_remote_owner_registration(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) -> bool {
        let mut handle =
            RelayHandle::new(registration.socket_node.clone(), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        let detach = handle
            .force_detach_remote_user_resource(RelayForceDetachRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                origin: waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
                requester_bare_jid: jid.to_bare(),
                trace: RelayTraceContext::default(),
            })
            .await;
        self.finish_remote_owner_registration_retire(services, jid, registration, detach)
            .await
    }

    pub(in super::super) async fn finish_remote_owner_registration_retire(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
        detach: Result<RelayForceDetachRemoteUserResourceReply, RelayAskError>,
    ) -> bool {
        let reply = match detach {
            Ok(reply) => reply,
            Err(error) if ask_error_proves_remote_resource_ref_stale(&error) => {
                tracing::info!(
                    jid = %jid,
                    ?error,
                    "clustered remote-resource replacement cleaning stale old-socket mirror"
                );
                if matches!(
                    unregister_remote_owner_actor_entry(services, jid, &registration.owner).await,
                    RemoteOwnerActorUnregisterOutcome::Failed
                ) {
                    return false;
                }
                services
                    .connection_registry
                    .unregister_if_owner(jid, &registration.owner);
                return true;
            }
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    ?error,
                    "clustered remote-resource replacement refused uncertain old-socket detach"
                );
                return false;
            }
        };
        if !matches!(
            reply.status,
            RelayRemoteResourceForceDetachStatus::Detached
                | RelayRemoteResourceForceDetachStatus::NotLive
        ) {
            tracing::warn!(
                jid = %jid,
                status = ?reply.status,
                "clustered remote-resource replacement refused uncertain old-socket detach"
            );
            return false;
        }
        if matches!(
            unregister_remote_owner_actor_entry(services, jid, &registration.owner).await,
            RemoteOwnerActorUnregisterOutcome::Failed
        ) {
            return false;
        }
        services
            .connection_registry
            .unregister_if_owner(jid, &registration.owner);
        true
    }

    pub(in super::super) async fn cleanup_remote_owner_resource_if_registration(
        &self,
        jid: &jid::FullJid,
        registration_id: RemoteResourceRegistrationId,
    ) {
        let Some(services) = self.services.get().cloned() else {
            return;
        };
        let registration = self
            .remote_owner_resources
            .lock()
            .await
            .get(jid)
            .filter(|registration| registration.registration_id == registration_id)
            .cloned();
        let Some(registration) = registration else {
            return;
        };
        if matches!(
            unregister_remote_owner_actor_entry(&services, jid, &registration.owner).await,
            RemoteOwnerActorUnregisterOutcome::Failed
        ) {
            return;
        }
        services
            .connection_registry
            .unregister_if_owner(jid, &registration.owner);
        let mut registrations = self.remote_owner_resources.lock().await;
        if registrations
            .get(jid)
            .is_some_and(|registration| registration.registration_id == registration_id)
        {
            registrations.remove(jid);
        }
    }
}
