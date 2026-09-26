//! Server-side implementation surface for extension host tools.
//!
//! This module implements the `waddle-extensions` host-tool trait and limits
//! every operation to existing XMPP-native state:
//! channel discovery, XEP-0503 Spaces-on-PubSub, XEP-0045 room actors,
//! RFC 6121 roster/presence, XEP-0313 MAM, and the typed message pipeline.

use std::sync::Arc;

use jid::{BareJid, FullJid, Jid};
use kameo::actor::ActorRef;
use waddle_extensions::{host_tools::InvocationKind, DisplayText, PluginId, RoomJid, StanzaId};
use waddle_xmpp::{
    muc::{
        room_actor::{GetSnapshot, RoomActor},
        room_registry_actor::GetRoom,
    },
    roster::{RosterItem, Subscription},
};

use crate::{
    auth::Session,
    db::roster::DatabaseRosterStorage,
    permissions::{CheckPermission, Object, ObjectType, Permission, Subject},
    server::bootstrap_membership::DEPLOYMENT_SERVER_ID,
};

use super::routes::{
    interpret::{self, Deps},
    websocket::WebSocketState,
};

mod conversions;
mod direct;
mod groupchat;
mod host_tools;
mod queries;
mod settlement;
mod types;

use conversions::*;
pub use groupchat::BotRoomLocks;
pub use types::*;

#[derive(Clone)]
pub struct ExtensionHostAdapter {
    state: Arc<WebSocketState>,
}

use direct::DirectDispatchMessage;

impl ExtensionHostAdapter {
    pub fn new(state: Arc<WebSocketState>) -> Self {
        Self { state }
    }

    pub async fn send_message(
        &self,
        invocation: &ExtensionInvocation,
        request: HostSendMessage,
    ) -> Result<StanzaId, ExtensionHostAdapterError> {
        if invocation.kind == InvocationKind::RoomMessageObserve {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        }
        match request.target {
            HostMessageTarget::Room(room) => {
                if invocation
                    .source_room
                    .as_ref()
                    .is_some_and(|source_room| source_room != &room)
                {
                    return Err(ExtensionHostAdapterError::NotAuthorized);
                }
                if invocation.kind == InvocationKind::ProviderWebhook {
                    self.authorize_provider_room(invocation, &room).await?;
                } else {
                    self.authorize_room(invocation, &room, Permission::SendMessage)
                        .await?;
                }
                let response = interpret::ExtensionRoomMessage {
                    plugin: invocation.plugin_id.clone(),
                    room: RoomJid::new(room.to_string())
                        .map_err(|error| ExtensionHostAdapterError::Protocol(error.to_string()))?,
                    body: DisplayText::new(request.body)
                        .map_err(|error| ExtensionHostAdapterError::Protocol(error.to_string()))?,
                    preferred_nick: Some(self.extension_bot_nick(&invocation.plugin_id)),
                    bot_hat_label: self.extension_bot_hat_label(&invocation.plugin_id),
                    stanza_id: Some(request.stanza_id),
                    thread_id: request.thread_id,
                    reply_to: request.reply_to,
                    markup: request.markup,
                    extensions: request.extensions,
                };
                if response.extensions.as_ref().is_some_and(|envelope| {
                    envelope_has_cross_room_launch(envelope, &room)
                        || envelope_has_roomless_launch(envelope)
                }) {
                    return Err(ExtensionHostAdapterError::NotAuthorized);
                }
                self.dispatch_groupchat(invocation, room, response).await
            }
            HostMessageTarget::Direct(target) => {
                if invocation.kind == InvocationKind::ProviderWebhook {
                    return Err(ExtensionHostAdapterError::NotAuthorized);
                }
                self.authorize_direct_send(invocation, &target).await?;
                self.dispatch_direct(
                    invocation,
                    target,
                    DirectDispatchMessage {
                        stanza_id: request.stanza_id.clone(),
                        body: request.body,
                        thread_id: request.thread_id,
                        reply_to: request.reply_to,
                        markup: request.markup,
                    },
                )
                .await?;
                Ok(request.stanza_id)
            }
        }
    }

    fn spaces_jid(&self) -> Result<BareJid, ExtensionHostAdapterError> {
        self.state
            .deps
            .service_domains
            .spaces
            .parse()
            .map_err(|error: jid::Error| ExtensionHostAdapterError::Protocol(error.to_string()))
    }

    fn extension_manifest(
        &self,
        plugin_id: &PluginId,
    ) -> Option<waddle_extensions::ExtensionManifest> {
        self.state
            .deps
            .protocol
            .extension_manager
            .manifest_for_plugin(plugin_id.as_str())
    }

    fn extension_bot_nick(&self, plugin_id: &PluginId) -> String {
        self.extension_manifest(plugin_id)
            .map(|manifest| {
                manifest
                    .profile
                    .as_ref()
                    .map(|profile| profile.display_name.as_str())
                    .unwrap_or_else(|| manifest.name.as_str())
                    .trim()
                    .to_string()
            })
            .filter(|nick| !nick.is_empty())
            .unwrap_or_else(|| plugin_id.as_str().to_string())
    }

    fn extension_bot_hat_label(&self, plugin_id: &PluginId) -> Option<DisplayText> {
        self.extension_manifest(plugin_id)
            .and_then(|manifest| manifest.profile)
            .and_then(|profile| profile.bot_hat_label)
    }

    fn interpret_deps<'a>(&'a self, session: Option<&'a Session>) -> Deps<'a> {
        Deps {
            dispatch_probe_budget: None,
            delivery_execution_context:
                crate::server::routes::interpret::DeliveryExecutionContext::Live,
            effects: &crate::server::routes::interpret::effects::ImmediateSink,
            connection_registry: &self.state.deps.protocol.connection_registry,
            user_registry: Some(&self.state.deps.protocol.user_registry),
            sm_session_registry: Some(&self.state.deps.protocol.sm_session_registry),
            mam_storage: Some(&self.state.deps.protocol.mam_storage),
            inbox_storage: Some(&self.state.deps.protocol.inbox_storage),
            extension_manager: Some(&self.state.deps.protocol.extension_manager),
            room_registry: Some(&self.state.deps.protocol.room_registry),
            web_socket_state: Some(&self.state),
            authenticated_principal: session.map(
                crate::server::routes::websocket::ResolvedPrincipal::from_authenticated_session,
            ),
            local_domain: self.state.deps.auth_state.xmpp_domain.as_str(),
            blocking_storage: Some(&self.state.deps.protocol.blocking_storage),
            message_dispatcher: Some(&self.state.deps.protocol.dispatcher),
            pending_delivery_storage: Some(&self.state.deps.protocol.pending_delivery_storage),
            ordered_relay_origin: None,
            sfu: self.state.deps.protocol.sfu.as_deref(),
            ingress_effect_capture: None,
            direct_route_identity: None,
            host_sender: None,
            ingress_append_context: None,
        }
    }

    async fn roster_storage(&self) -> Result<DatabaseRosterStorage, ExtensionHostAdapterError> {
        Ok(DatabaseRosterStorage::new(
            self.state.deps.app_state.db_pool.global().clone(),
        ))
    }

    async fn roster_item(
        &self,
        owner: &BareJid,
        contact: &BareJid,
    ) -> Result<Option<RosterItem>, ExtensionHostAdapterError> {
        let storage = self.roster_storage().await?;
        let row = storage
            .get_roster_item(owner, contact)
            .await
            .map_err(|error| ExtensionHostAdapterError::Storage(error.to_string()))?;
        row.map(|row| {
            Ok(RosterItem {
                jid: row.contact_jid.parse().map_err(|error: jid::Error| {
                    ExtensionHostAdapterError::Protocol(error.to_string())
                })?,
                name: row.name,
                subscription: row.subscription.parse().map_err(
                    |error: waddle_xmpp::CoreError| {
                        ExtensionHostAdapterError::Protocol(error.to_string())
                    },
                )?,
                ask: row.ask.map(|ask| ask.parse()).transpose().map_err(
                    |error: waddle_xmpp::CoreError| {
                        ExtensionHostAdapterError::Protocol(error.to_string())
                    },
                )?,
                approved: row.approved,
                groups: row.groups,
            })
        })
        .transpose()
    }

    async fn room_actor(
        &self,
        room: &BareJid,
    ) -> Result<ActorRef<RoomActor>, ExtensionHostAdapterError> {
        self.state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: room.clone(),
            })
            .await
            .map_err(|error| ExtensionHostAdapterError::RoomActor(format!("{error:?}")))?
            .ok_or_else(|| ExtensionHostAdapterError::RoomNotFound(room.clone()))
    }

    async fn room_snapshot(
        &self,
        room: &BareJid,
    ) -> Result<waddle_xmpp::muc::room_actor::RoomSnapshot, ExtensionHostAdapterError> {
        self.room_actor(room)
            .await?
            .ask(GetSnapshot)
            .await
            .map_err(|error| ExtensionHostAdapterError::RoomActor(format!("{error:?}")))
    }

    async fn authorize_archive(
        &self,
        invocation: &ExtensionInvocation,
        archive: &BareJid,
    ) -> Result<(), ExtensionHostAdapterError> {
        if archive == &invocation.actor_jid.to_bare() {
            return Ok(());
        }
        if archive.domain().as_str() == self.state.deps.service_domains.muc {
            return self
                .authorize_room(invocation, archive, Permission::View)
                .await;
        }
        Err(ExtensionHostAdapterError::NotAuthorized)
    }

    async fn authorize_room(
        &self,
        invocation: &ExtensionInvocation,
        room: &BareJid,
        permission: Permission,
    ) -> Result<(), ExtensionHostAdapterError> {
        self.ensure_not_outcast(invocation, room).await?;
        let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room) else {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        };
        if self
            .allowed(
                invocation,
                Object::new(ObjectType::Channel, channel_id),
                permission,
            )
            .await?
        {
            Ok(())
        } else {
            Err(ExtensionHostAdapterError::NotAuthorized)
        }
    }

    async fn ensure_not_outcast(
        &self,
        invocation: &ExtensionInvocation,
        room: &BareJid,
    ) -> Result<(), ExtensionHostAdapterError> {
        let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room) else {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        };
        let Some(session) = invocation.session.as_ref() else {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        };
        let subject = Subject::user(&session.user_jid);
        if self
            .permission_allowed(
                subject,
                Object::new(ObjectType::Channel, channel_id),
                Permission::Custom("outcast".into()),
            )
            .await?
        {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        }
        Ok(())
    }

    async fn permission_allowed(
        &self,
        subject: Subject,
        object: Object,
        permission: Permission,
    ) -> Result<bool, ExtensionHostAdapterError> {
        self.state
            .deps
            .app_state
            .permission_actor
            .ask(CheckPermission {
                subject,
                permission,
                object,
            })
            .await
            .map(|response| response.allowed)
            .map_err(|error| ExtensionHostAdapterError::Protocol(format!("{error:?}")))
    }

    async fn authorize_direct_send(
        &self,
        invocation: &ExtensionInvocation,
        target: &Jid,
    ) -> Result<(), ExtensionHostAdapterError> {
        let target_bare = target.to_bare();
        if target_bare.domain().as_str() != self.state.deps.auth_state.xmpp_domain {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        }
        let roster = self
            .roster_item(&invocation.actor_jid.to_bare(), &target_bare)
            .await?;
        if roster
            .as_ref()
            .is_some_and(|item| matches!(item.subscription, Subscription::To | Subscription::Both))
        {
            Ok(())
        } else {
            Err(ExtensionHostAdapterError::NotAuthorized)
        }
    }

    async fn allowed(
        &self,
        invocation: &ExtensionInvocation,
        object: Object,
        permission: Permission,
    ) -> Result<bool, ExtensionHostAdapterError> {
        let Some(session) = invocation.session.as_ref() else {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        };
        let subject = Subject::user(&session.user_jid);
        if self
            .permission_allowed(subject.clone(), object, permission)
            .await?
        {
            return Ok(true);
        }

        self.permission_allowed(
            subject,
            Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
            Permission::Owner,
        )
        .await
    }

    async fn authorize_provider_room(
        &self,
        invocation: &ExtensionInvocation,
        room: &BareJid,
    ) -> Result<(), ExtensionHostAdapterError> {
        if !invocation
            .provider_room_grants
            .iter()
            .any(|granted| granted == room)
        {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        }
        let Some(_channel_id) = waddle_xmpp::parse_managed_room_jid(room) else {
            return Err(ExtensionHostAdapterError::NotAuthorized);
        };
        // The planner resolves room ownership and local existence before joining.
        // A provider room owned remotely must reach its typed Phase-A refusal.
        Ok(())
    }

    pub(super) fn plugin_actor_jid(
        &self,
        plugin_id: &PluginId,
    ) -> Result<FullJid, ExtensionHostAdapterError> {
        let bare: BareJid = format!(
            "{}@{}",
            plugin_id.as_str(),
            self.state.deps.service_domains.extensions
        )
        .parse()
        .map_err(|error: jid::Error| ExtensionHostAdapterError::Protocol(error.to_string()))?;
        bare.with_resource_str("bot")
            .map_err(|error| ExtensionHostAdapterError::Protocol(error.to_string()))
    }
}

#[cfg(test)]
mod tests;
