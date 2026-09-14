//! Trusted bot sends commit under the plugin's configured grant.
use std::sync::Arc;

use jid::BareJid;
use waddle_extensions::{host_tools::InvocationKind, StanzaId};
use waddle_xmpp::ingress::{NormalizedTarget, TransportGeneration};

use crate::ingress::{
    nested::{NestedContinuation, NestedOutcome, NestedRefusal},
    ExtensionPrincipal, IngressDecisionClass, IngressPrincipal, IngressStreamIdentity,
    IngressSubmission,
};

use super::{interpret, ExtensionHostAdapter, ExtensionHostAdapterError, ExtensionInvocation};

/// Serializes the synthetic actor lifecycle across independently created adapters.
#[derive(Default)]
pub struct BotRoomLocks {
    entries: dashmap::DashMap<(waddle_extensions::PluginId, BareJid), Arc<tokio::sync::Mutex<()>>>,
}

impl BotRoomLocks {
    async fn lock(
        &self,
        plugin: &waddle_extensions::PluginId,
        room: &BareJid,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .entries
            .entry((plugin.clone(), room.clone()))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        lock.lock_owned().await
    }
}

impl ExtensionHostAdapter {
    pub(super) async fn dispatch_groupchat(
        &self,
        invocation: &ExtensionInvocation,
        room: BareJid,
        response: interpret::ExtensionRoomMessage,
    ) -> Result<StanzaId, ExtensionHostAdapterError> {
        let offered_id = response.stanza_id.clone().ok_or_else(|| {
            ExtensionHostAdapterError::Protocol("host message has no offered stanza id".to_owned())
        })?;
        let authority = &self.state.deps.protocol.ingress;
        let provider = invocation.kind == InvocationKind::ProviderWebhook;
        let grant = if provider {
            authority
                .active_extension_room_grant(&invocation.plugin_id, &room)
                .await
        } else {
            authority
                .active_extension_send_grant(&invocation.plugin_id)
                .await
        }
        .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?
        .ok_or(ExtensionHostAdapterError::NotAuthorized)?;
        let operation = authority
            .try_begin_nested()
            .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?;
        let sender = self.plugin_actor_jid(&invocation.plugin_id)?;
        let requester = (!provider).then(|| invocation.actor_jid.to_bare());
        // Keep snapshot, first join, and admission ordered for this bot/room.
        // The actor's admission generation must not change under a second join.
        let room_guard = self
            .state
            .deps
            .protocol
            .extension_bot_rooms
            .lock(&invocation.plugin_id, &room)
            .await;
        let deps = self.interpret_deps(invocation.session.as_ref());
        let planned =
            interpret::plan_extension_bot_groupchat(&deps, room.clone(), sender.clone(), response)
                .await
                .map_err(|error| match error {
                    interpret::ExtensionBotDispatchError::InvalidEnvelope => {
                        ExtensionHostAdapterError::NotAuthorized
                    }
                    interpret::ExtensionBotDispatchError::Plan(failure) => {
                        ExtensionHostAdapterError::Plan(failure)
                    }
                    interpret::ExtensionBotDispatchError::Digest(error) => {
                        ExtensionHostAdapterError::Unsupported(error.to_string())
                    }
                    interpret::ExtensionBotDispatchError::RoomNotRegistered => {
                        ExtensionHostAdapterError::RoomNotFound(room.clone())
                    }
                    other => ExtensionHostAdapterError::Protocol(other.to_string()),
                })?;
        if let Some(failure) = planned.plan.failure {
            return Err(ExtensionHostAdapterError::Plan(failure));
        }
        let submission = IngressSubmission {
            identity: IngressStreamIdentity::Extension {
                plugin: invocation.plugin_id.clone(),
                requester: requester.clone(),
            },
            principal: IngressPrincipal::Extension(ExtensionPrincipal {
                grant,
                requester,
                sender: sender.to_bare(),
            }),
            sender: sender.clone(),
            target: NormalizedTarget::Bare(room.clone()),
            digest_input: planned.digest_input,
            plan: planned.plan,
            connection_generation: TransportGeneration::Host,
        };
        let continuation = NestedContinuation::new(
            Arc::clone(&self.state),
            invocation.session.clone(),
            submission.sender.clone(),
        );
        let outcome = operation
            .commit_and_continue(submission, continuation)
            .await;
        if matches!(
            outcome,
            NestedOutcome::Refused(NestedRefusal::Decision(
                IngressDecisionClass::PrincipalMissing
            ))
        ) {
            if let Some((nick, session)) = planned.joined_occupancy {
                // Revoke only this dispatch's join, before another send can reuse it.
                // The normal departure path emits unavailable presence and retains
                // interrupted actor cleanup for the departure janitor.
                let _ = crate::server::routes::websocket::handlers::presence::handle_muc_leave(
                    &self.state,
                    &room,
                    &sender,
                    nick.as_str(),
                    session,
                    None,
                )
                .await;
            }
        }
        drop(room_guard);
        let archive_ids = super::settlement::finish_nested(outcome).await?;
        // A committed denial may have only an error frame and no room archive.
        // If its settlement misses the response deadline, acceptance still uses
        // the offered ID. Successful room sends retain their canonical reply ID.
        Ok(archive_ids
            .into_iter()
            .find(|(archive, _)| archive == &room)
            .and_then(|(_, id)| StanzaId::new(id.id).ok())
            .unwrap_or(offered_id))
    }
}
