//! Removing a member from every channel and space it is affiliated with.
//!
//! Deleting a bot (D8) must take it out of room state that lives outside
//! the permission tuples: the room actor's affiliation list, its durable
//! inbox recipients and, under clustering, the
//! `clustering_muc_room_affiliations` mirror. Each channel goes through the
//! same core as the admin `channels:set-affiliation` command, and each space
//! through the PubSub affiliation the admin spaces commands write. The
//! caller is responsible for authorization; for bots that is `bot#manage`.
//!
//! XEP-0045 never lets a room lose its last owner, and deletion does not
//! override that: when the member is the only owner of any room, nothing is
//! cleared and the caller gets [`ClearAffiliationsError::SoleRoomOwner`], so
//! someone can make another owner first.

use std::collections::BTreeSet;

use jid::BareJid;
use kameo::actor::ActorRef;
use waddle_xmpp::commands::CommandResult;
use waddle_xmpp::muc::room_actor::{ListAffiliations, RoomActor};
use waddle_xmpp::muc::room_registry_actor::GetRoom;
use waddle_xmpp::pubsub::Affiliation as PubSubAffiliation;
use waddle_xmpp::{Affiliation, XmppError};

use super::channels::{
    acquire_room_config_lock, apply_channel_affiliation, AdminErr, ChannelAffiliationChange,
    ChannelAffiliationSinks,
};
use crate::permissions::{
    DeleteTuple, ListSubjectTuples, ObjectType, PermissionError, Subject, Tuple,
};
use crate::server::routes::websocket::handlers::presence::{
    get_managed_channel_for_room, parse_room_jid_context,
};
use crate::server::routes::websocket::{get_or_create_room_actor, WebSocketState};
use crate::server::AppState;

/// Why [`clear_member_affiliations`] stopped.
pub(crate) enum ClearAffiliationsError {
    /// The member is the only owner of `room`. Removing it would leave the
    /// room ownerless, so nothing was cleared.
    SoleRoomOwner {
        room: BareJid,
    },
    Failed(AdminErr),
}

impl From<AdminErr> for ClearAffiliationsError {
    fn from(error: AdminErr) -> Self {
        Self::Failed(error)
    }
}

/// A channel the member holds a tuple on, with its live room actor.
struct HeldChannel {
    channel_id: String,
    room_jid: BareJid,
    actor: ActorRef<RoomActor>,
}

/// Clear every channel and space affiliation `member` holds. Idempotent: a
/// retry after a partial failure finishes the job.
pub(crate) async fn clear_member_affiliations(
    state: &AppState,
    websocket_state: &WebSocketState,
    member: &BareJid,
) -> Result<(), ClearAffiliationsError> {
    let tuples = state
        .permission_actor
        .ask(ListSubjectTuples {
            subject: Subject::user(member.to_string()),
        })
        .await
        .map_err(|error| internal_err(format!("listing member tuples failed: {error}")))?;

    let mut channel_ids = BTreeSet::new();
    let mut space_tuples = Vec::new();
    for tuple in tuples {
        match tuple.object.object_type {
            ObjectType::Channel => {
                channel_ids.insert(tuple.object.id.clone());
            }
            ObjectType::Space => space_tuples.push(tuple),
            _ => {}
        }
    }

    // Check every room before touching any, so a sole-owner refusal leaves
    // the member's other memberships intact.
    let mut channels = Vec::with_capacity(channel_ids.len());
    for channel_id in channel_ids {
        let room_jid = waddle_xmpp::managed_room_jid(&channel_id, state.muc_domain.as_str())
            .map_err(|error| {
                internal_err(format!("channel {channel_id} has no room JID: {error}"))
            })?;
        // No room to clear: its tuples go with the caller's subject purge.
        let Some(actor) = room_actor(state, websocket_state, &room_jid).await? else {
            continue;
        };
        if is_sole_owner(&actor, member).await? {
            return Err(ClearAffiliationsError::SoleRoomOwner { room: room_jid });
        }
        channels.push(HeldChannel {
            channel_id,
            room_jid,
            actor,
        });
    }

    for channel in &channels {
        clear_channel_affiliation(state, websocket_state, channel, member).await?;
    }
    clear_space_affiliations(state, member, space_tuples).await?;
    Ok(())
}

async fn is_sole_owner(actor: &ActorRef<RoomActor>, member: &BareJid) -> Result<bool, AdminErr> {
    let owners = actor
        .ask(ListAffiliations {
            filter: Some(Affiliation::Owner),
        })
        .await
        .map_err(|error| internal_err(format!("listing room owners failed: {error}")))?;
    Ok(matches!(owners.as_slice(), [only] if &only.jid == member))
}

async fn clear_channel_affiliation(
    state: &AppState,
    websocket_state: &WebSocketState,
    channel: &HeldChannel,
    member: &BareJid,
) -> Result<(), ClearAffiliationsError> {
    let _config_guard = acquire_room_config_lock(&channel.room_jid).await;
    // Ownership may have changed since the pre-check.
    if is_sole_owner(&channel.actor, member).await? {
        return Err(ClearAffiliationsError::SoleRoomOwner {
            room: channel.room_jid.clone(),
        });
    }
    let protocol = &websocket_state.deps.protocol;
    apply_channel_affiliation(
        state,
        ChannelAffiliationSinks {
            websocket_state: Some(websocket_state),
            connections: &protocol.connection_registry,
            sfu: protocol.sfu.as_ref(),
        },
        &channel.actor,
        ChannelAffiliationChange {
            channel_jid: &channel.room_jid,
            channel_id: &channel.channel_id,
            member_jid: member,
            affiliation: Affiliation::None,
            actor: None,
        },
    )
    .await?;
    Ok(())
}

/// The live room actor, loading a dormant channel the way a member join
/// does. `None` when the channel no longer exists.
async fn room_actor(
    state: &AppState,
    websocket_state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<Option<ActorRef<RoomActor>>, AdminErr> {
    let live = state
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .map_err(|error| internal_err(format!("room lookup for {room_jid} failed: {error}")))?;
    if live.is_some() {
        return Ok(live);
    }
    let channel = get_managed_channel_for_room(websocket_state, room_jid)
        .await
        .map_err(|error| internal_err(format!("channel lookup for {room_jid} failed: {error}")))?;
    let Some(channel) = channel else {
        return Ok(None);
    };
    let acquisition = get_or_create_room_actor(
        websocket_state,
        room_jid,
        channel.room_config(),
        parse_room_jid_context(room_jid).0,
        channel.id,
    )
    .await
    .map_err(|error| internal_err(format!("loading room {room_jid} failed: {error}")))?;
    Ok(Some(acquisition.actor_ref))
}

async fn clear_space_affiliations(
    state: &AppState,
    member: &BareJid,
    space_tuples: Vec<Tuple>,
) -> Result<(), AdminErr> {
    let mut nodes: BTreeSet<String> = space_tuples
        .iter()
        .map(|tuple| tuple.object.id.clone())
        .collect();
    let held = state
        .pubsub_storage
        .list_entity_affiliations(&state.spaces_jid, member)
        .await
        .map_err(|error| internal_err(format!("listing space affiliations failed: {error}")))?;
    nodes.extend(held.into_iter().map(|(node, _)| node));
    for node in &nodes {
        state
            .pubsub_storage
            .set_affiliation(&state.spaces_jid, node, member, PubSubAffiliation::None)
            .await
            .map_err(|error| {
                internal_err(format!("clearing space {node} affiliation failed: {error}"))
            })?;
    }
    for tuple in space_tuples {
        match state.permission_actor.ask(DeleteTuple { tuple }).await {
            Ok(()) | Err(kameo::error::SendError::HandlerError(PermissionError::TupleNotFound)) => {
            }
            Err(error) => {
                return Err(internal_err(format!(
                    "deleting space membership tuple failed: {error}"
                )));
            }
        }
    }
    Ok(())
}

fn internal_err(text: String) -> AdminErr {
    Box::new(CommandResult::Error(XmppError::internal(text)))
}
