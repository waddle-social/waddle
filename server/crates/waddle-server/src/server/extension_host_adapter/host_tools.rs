use async_trait::async_trait;
use jid::{BareJid, Jid};
use waddle_extensions::types::PubSubNode as ExtensionPubSubNode;
use waddle_extensions::{host_tools as ext_host, DisplayText};
use waddle_xmpp::mam::MamQuery;

use crate::{
    auth::Session,
    db::{actor::DbQueryOne, row_value, ValueExt},
};

use super::{
    conversions::*, ExtensionHostAdapter, ExtensionHostAdapterError, ExtensionInvocation,
    HostMessageTarget, HostSendMessage,
};

#[async_trait]
impl ext_host::ExtensionHostTools for ExtensionHostAdapter {
    async fn list_channels(
        &self,
        context: &ext_host::InvocationContext,
        _request: ext_host::ListChannelsRequest,
    ) -> Result<ext_host::ListChannelsResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let channels = self
            .list_channels(&invocation, 500, 0)
            .await
            .map_err(host_tool_error)?
            .into_iter()
            .map(|channel| ext_host::ChannelSummary {
                room: channel.room,
                name: DisplayText::new(channel.name).ok(),
                description: None,
            })
            .collect();
        Ok(ext_host::ListChannelsResponse { channels })
    }

    async fn list_spaces(
        &self,
        context: &ext_host::InvocationContext,
        _request: ext_host::ListSpacesRequest,
    ) -> Result<ext_host::ListSpacesResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let spaces = self
            .list_spaces(&invocation)
            .await
            .map_err(host_tool_error)?
            .into_iter()
            .filter_map(|space| {
                Some(ext_host::SpaceSummary {
                    service: space.service,
                    node: ExtensionPubSubNode::new(space.node).ok()?,
                    name: None,
                    description: None,
                    channels: Vec::new(),
                })
            })
            .collect();
        Ok(ext_host::ListSpacesResponse { spaces })
    }

    async fn list_room_members(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::ListRoomMembersRequest,
    ) -> Result<ext_host::ListRoomMembersResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let occupants = self
            .list_muc_members(&invocation, &request.room)
            .await
            .map_err(host_tool_error)?;
        let members = occupants
            .into_iter()
            .map(|member| ext_host::RoomMember {
                room: request.room.clone(),
                jid: member.occupant_jid,
                nick: DisplayText::new(member.nick).ok(),
                role: ext_muc_role(member.role),
                affiliation: ext_muc_affiliation(member.affiliation),
            })
            .collect();
        Ok(ext_host::ListRoomMembersResponse { members })
    }

    async fn get_presence(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::GetPresenceRequest,
    ) -> Result<ext_host::GetPresenceResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let resources = self
            .presence(&invocation, &request.subject)
            .await
            .map_err(host_tool_error)?
            .into_iter()
            .map(|presence| ext_host::PresenceState {
                jid: presence.jid,
                availability: ext_host::PresenceAvailability::Available,
                show: ext_presence_show(presence.show),
                status: presence
                    .status
                    .and_then(|status| DisplayText::new(status).ok()),
                priority: i32::from(presence.priority),
            })
            .collect();
        Ok(ext_host::GetPresenceResponse { resources })
    }

    async fn get_roster(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::GetRosterRequest,
    ) -> Result<ext_host::GetRosterResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let entries = self
            .roster(&invocation, &request.owner)
            .await
            .map_err(host_tool_error)?
            .into_iter()
            .map(|entry| ext_host::RosterEntry {
                jid: entry.jid,
                name: entry.name.and_then(|name| DisplayText::new(name).ok()),
                subscription: ext_roster_subscription(entry.subscription),
                ask: entry.ask.map(|_| ext_host::RosterAsk::Subscribe),
                groups: entry
                    .groups
                    .into_iter()
                    .filter_map(|group| DisplayText::new(group).ok())
                    .collect(),
            })
            .collect();
        Ok(ext_host::GetRosterResponse { entries })
    }

    async fn query_mam(
        &self,
        context: &ext_host::InvocationContext,
        query: ext_host::MamQuery,
    ) -> Result<ext_host::MamQueryResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let requester = invocation.actor_jid.to_bare();
        let (archive, with) = match query.target {
            ext_host::MamTarget::Room(jid) => (jid, query.sender),
            ext_host::MamTarget::Conversation(peer) => (requester, Some(peer)),
        };
        self.authorize_archive(&invocation, &archive)
            .await
            .map_err(host_tool_error)?;
        let xmpp_query = MamQuery {
            start: query.start,
            end: query.end,
            with: with.map(jid::Jid::from),
            thread_id: query
                .thread_id
                .and_then(|thread_id| waddle_xmpp::mam::ThreadId::new(thread_id.as_str())),
            fulltext: query
                .text
                .and_then(|text| waddle_xmpp::mam::RichText::new(text.into_string())),
            max: Some(query.max_results),
            filter_before_id: None,
            filter_after_id: None,
            ids: Vec::new(),
            before_id: Some(String::new()),
            after_id: None,
        };
        let result = self
            .state
            .deps
            .protocol
            .mam_storage
            .query_messages(&archive, &xmpp_query)
            .await
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Storage(error.to_string()))
            })?;
        let messages = result
            .messages
            .into_iter()
            .filter_map(ext_archived_message)
            .collect();
        Ok(ext_host::MamQueryResponse {
            messages,
            complete: result.complete,
        })
    }

    async fn send_message(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::SendMessageRequest,
    ) -> Result<ext_host::SendMessageResponse, ext_host::HostToolError> {
        let invocation = self.invocation_for_context(context).await?;
        let target = match request.target {
            ext_host::MessageTarget::Muc(room) => HostMessageTarget::Room(room),
            ext_host::MessageTarget::Direct(jid) => HostMessageTarget::Direct(Jid::from(jid)),
        };
        let stanza_id = waddle_extensions::StanzaId::new(uuid::Uuid::new_v4().to_string())
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Protocol(error.to_string()))
            })?;
        let stanza_id = self
            .send_message(
                &invocation,
                HostSendMessage {
                    target,
                    stanza_id: stanza_id.clone(),
                    body: request.body.into_string(),
                    thread_id: request.thread_id,
                    reply_to: request.reply_to,
                    extensions: request.extensions,
                },
            )
            .await
            .map_err(host_tool_error)?;
        Ok(ext_host::SendMessageResponse { stanza_id })
    }
}

impl ExtensionHostAdapter {
    async fn invocation_for_context(
        &self,
        context: &ext_host::InvocationContext,
    ) -> Result<ExtensionInvocation, ext_host::HostToolError> {
        if context.kind == ext_host::InvocationKind::ProviderWebhook {
            let actor_jid = self
                .plugin_actor_jid(&context.plugin_id)
                .map_err(host_tool_error)?;
            return Ok(ExtensionInvocation {
                session: None,
                actor_jid,
                plugin_id: context.plugin_id.clone(),
                source_room: context.source_room.clone(),
                kind: context.kind,
                provider_room_grants: context.provider_room_grants.clone(),
            });
        }
        let Some(requester) = context.requester.as_ref() else {
            return Err(host_tool_error(ExtensionHostAdapterError::NotAuthorized));
        };
        self.invocation_for_requester(
            requester,
            context.plugin_id.clone(),
            context.source_room.clone(),
            context.kind,
        )
        .await
    }

    async fn invocation_for_requester(
        &self,
        requester: &BareJid,
        plugin_id: waddle_extensions::PluginId,
        source_room: Option<BareJid>,
        kind: ext_host::InvocationKind,
    ) -> Result<ExtensionInvocation, ext_host::HostToolError> {
        let Some(localpart) = requester.node() else {
            return Err(host_tool_error(ExtensionHostAdapterError::NotAuthorized));
        };
        if requester.domain().as_str() != self.state.deps.auth_state.xmpp_domain {
            return Err(host_tool_error(ExtensionHostAdapterError::NotAuthorized));
        }
        let row = self
            .state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbQueryOne {
                sql: "SELECT id, username, xmpp_localpart FROM users WHERE xmpp_localpart = ? LIMIT 1"
                    .to_string(),
                params: vec![localpart.as_str().into()],
            })
            .await
            .map_err(|error| host_tool_error(ExtensionHostAdapterError::Storage(format!("{error:?}"))))?
            .ok_or_else(|| host_tool_error(ExtensionHostAdapterError::NotAuthorized))?;
        let user_id = row_value(&row, 0)
            .and_then(ValueExt::as_string)
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Storage(error.to_string()))
            })?;
        let username = row_value(&row, 1)
            .and_then(ValueExt::as_string)
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Storage(error.to_string()))
            })?;
        let xmpp_localpart = row_value(&row, 2)
            .and_then(ValueExt::as_string)
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Storage(error.to_string()))
            })?;
        let actor_jid = requester
            .clone()
            .with_resource_str("extension-host")
            .map_err(|error| {
                host_tool_error(ExtensionHostAdapterError::Protocol(error.to_string()))
            })?;
        Ok(ExtensionInvocation {
            session: Some(Session::new(&user_id, &username, &xmpp_localpart)),
            actor_jid,
            plugin_id,
            source_room,
            kind,
            provider_room_grants: Vec::new(),
        })
    }
}
