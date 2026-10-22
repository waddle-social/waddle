//! Removing a member from every room and space it is affiliated with.
//!
//! Deleting a bot (D8) must take it out of room state that lives outside
//! the permission tuples: the room actor's affiliation list, its durable
//! inbox recipients and, under clustering, the
//! `clustering_muc_room_affiliations` mirror. A room's affiliation can exist
//! without a channel tuple, so rooms are found three ways: the member's
//! channel tuples, the live rooms on this node that list the member, and,
//! under clustering, the durable mirror, which covers rooms other nodes own
//! and dormant rooms. Each room goes through the same core as the admin
//! `channels:set-affiliation` command, and each space through the PubSub
//! affiliation the admin spaces commands write. The caller is responsible
//! for authorization; for bots that is `bot#manage`.
//!
//! Under clustering a room's actor lives on the node holding its claim. A
//! room another node owns is checked and cleared by that node, over the
//! `muc_member_clear` relay message, through the same core; any node can
//! therefore complete a delete.
//!
//! XEP-0045 never lets a room lose its last owner, and deletion does not
//! override that: when the member is the only owner of any room, nothing is
//! cleared and the caller gets [`ClearAffiliationsError::SoleRoomOwner`], so
//! someone can make another owner first.

use std::collections::{BTreeMap, BTreeSet};

use jid::BareJid;
use kameo::actor::ActorRef;
use waddle_xmpp::commands::CommandResult;
use waddle_xmpp::muc::room_actor::{GetAffiliation, ListAffiliations, RoomActor};
use waddle_xmpp::muc::room_registry_actor::{GetRoom, ListRooms};
use waddle_xmpp::muc::RoomConfig;
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

/// Where a held channel's room actor lives.
enum RoomLocation {
    Local(ActorRef<RoomActor>),
    /// Another node holds the room's claim.
    #[cfg(feature = "clustering")]
    Remote(waddle_xmpp::ownership::NodeIdentity),
}

/// A room the member is affiliated with, with where its room actor lives.
struct HeldChannel {
    channel_id: String,
    room_jid: BareJid,
    location: RoomLocation,
    /// The durable mirror holds the room, so a room no node holds live is
    /// loaded from it even without a managed channel record.
    #[cfg(feature = "clustering")]
    durable: bool,
}

#[derive(Clone, Copy)]
enum Step {
    /// Refuse a sole owner; change nothing.
    Check,
    /// Set the member's affiliation to `none`, refusing a sole owner.
    Clear,
}

enum StepOutcome {
    Done,
    SoleOwner,
    /// The room's owner no longer holds a live actor for it.
    #[cfg(feature = "clustering")]
    Moved,
}

/// Clear every room and space affiliation `member` holds. Idempotent: a
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

    // Room JID to whether the durable mirror holds it.
    let mut rooms = BTreeMap::new();
    let mut space_tuples = Vec::new();
    for tuple in tuples {
        match tuple.object.object_type {
            ObjectType::Channel => {
                let channel_id = &tuple.object.id;
                let room_jid = waddle_xmpp::managed_room_jid(channel_id, state.muc_domain.as_str())
                    .map_err(|error| {
                        internal_err(format!("channel {channel_id} has no room JID: {error}"))
                    })?;
                rooms.insert(room_jid, false);
            }
            ObjectType::Space => space_tuples.push(tuple),
            _ => {}
        }
    }
    for room_jid in live_affiliated_rooms(state, member).await? {
        rooms.entry(room_jid).or_insert(false);
    }
    #[cfg(feature = "clustering")]
    if let Some(store) = state.clustering_claims.muc_durable_store.as_ref() {
        let durable = store
            .list_member_affiliated_rooms(member)
            .await
            .map_err(|error| {
                internal_err(format!("listing durable affiliations failed: {error}"))
            })?;
        for room_jid in durable {
            rooms.insert(room_jid, true);
        }
    }

    // Check every room before touching any, so a sole-owner refusal leaves
    // the member's other memberships intact.
    let mut channels = Vec::with_capacity(rooms.len());
    for (room_jid, durable) in rooms {
        let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
            return Err(internal_err(format!("room {room_jid} has no channel id")).into());
        };
        // No room to clear: its tuples go with the caller's subject purge.
        let Some(location) = locate_room(state, websocket_state, &room_jid, durable).await? else {
            continue;
        };
        let mut channel = HeldChannel {
            channel_id,
            room_jid,
            location,
            #[cfg(feature = "clustering")]
            durable,
        };
        if run_step(state, websocket_state, &mut channel, member, Step::Check).await? {
            channels.push(channel);
        }
    }

    for channel in &mut channels {
        run_step(state, websocket_state, channel, member, Step::Clear).await?;
    }
    clear_space_affiliations(state, member, space_tuples).await?;
    Ok(())
}

/// Run `step` wherever the room lives, following a moved claim once.
/// `false` when the channel no longer exists.
async fn run_step(
    state: &AppState,
    websocket_state: &WebSocketState,
    channel: &mut HeldChannel,
    member: &BareJid,
    step: Step,
) -> Result<bool, ClearAffiliationsError> {
    let outcome = step_once(state, websocket_state, channel, member, step).await?;
    #[cfg(feature = "clustering")]
    let outcome = match outcome {
        StepOutcome::Moved => {
            match locate_room(state, websocket_state, &channel.room_jid, channel.durable).await? {
                Some(location) => channel.location = location,
                None => return Ok(false),
            }
            step_once(state, websocket_state, channel, member, step).await?
        }
        outcome => outcome,
    };
    match outcome {
        StepOutcome::Done => Ok(true),
        StepOutcome::SoleOwner => Err(ClearAffiliationsError::SoleRoomOwner {
            room: channel.room_jid.clone(),
        }),
        #[cfg(feature = "clustering")]
        StepOutcome::Moved => Err(internal_err(format!(
            "the owner of room {} kept moving",
            channel.room_jid
        ))
        .into()),
    }
}

async fn step_once(
    state: &AppState,
    websocket_state: &WebSocketState,
    channel: &HeldChannel,
    member: &BareJid,
    step: Step,
) -> Result<StepOutcome, AdminErr> {
    match &channel.location {
        RoomLocation::Local(actor) => {
            local_step(
                state,
                websocket_state,
                actor,
                &channel.room_jid,
                &channel.channel_id,
                member,
                step,
            )
            .await
        }
        #[cfg(feature = "clustering")]
        RoomLocation::Remote(owner) => {
            remote_step(websocket_state, owner, &channel.room_jid, member, step).await
        }
    }
}

/// Check or clear through a room actor on this node. The caller resolved
/// `actor`; a remote owner runs this for the origin.
async fn local_step(
    state: &AppState,
    websocket_state: &WebSocketState,
    actor: &ActorRef<RoomActor>,
    room_jid: &BareJid,
    channel_id: &str,
    member: &BareJid,
    step: Step,
) -> Result<StepOutcome, AdminErr> {
    let _config_guard = match step {
        Step::Check => None,
        Step::Clear => Some(acquire_room_config_lock(room_jid).await),
    };
    // A clear re-checks under the lock: ownership may have changed since
    // the pre-check.
    if is_sole_owner(actor, member).await? {
        return Ok(StepOutcome::SoleOwner);
    }
    if let Step::Clear = step {
        let protocol = &websocket_state.deps.protocol;
        apply_channel_affiliation(
            state,
            ChannelAffiliationSinks {
                websocket_state: Some(websocket_state),
                connections: &protocol.connection_registry,
                sfu: protocol.sfu.as_ref(),
            },
            actor,
            ChannelAffiliationChange {
                channel_jid: room_jid,
                channel_id,
                member_jid: member,
                affiliation: Affiliation::None,
                actor: None,
            },
        )
        .await?;
    }
    Ok(StepOutcome::Done)
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

/// The live rooms on this node whose affiliation list names `member`.
async fn live_affiliated_rooms(
    state: &AppState,
    member: &BareJid,
) -> Result<Vec<BareJid>, AdminErr> {
    let live = state
        .room_registry
        .ask(ListRooms)
        .await
        .map_err(|error| internal_err(format!("listing live rooms failed: {error}")))?;
    let mut held = Vec::new();
    for room_jid in live {
        let actor = state
            .room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .map_err(|error| internal_err(format!("room lookup for {room_jid} failed: {error}")))?;
        // Gone since the listing: nothing left to clear.
        let Some(actor) = actor else {
            continue;
        };
        let affiliation = actor
            .ask(GetAffiliation {
                jid: member.clone(),
            })
            .await
            .map_err(|error| {
                internal_err(format!("affiliation lookup in {room_jid} failed: {error}"))
            })?;
        if affiliation != Affiliation::None {
            held.push(room_jid);
        }
    }
    Ok(held)
}

/// Where the room's actor lives: a live local actor, the node holding the
/// room's claim, or a dormant room loaded here the way a join does. A
/// dormant room is a channel's, or one the durable mirror holds (`durable`).
/// `None` when the room no longer exists.
async fn locate_room(
    state: &AppState,
    websocket_state: &WebSocketState,
    room_jid: &BareJid,
    durable: bool,
) -> Result<Option<RoomLocation>, AdminErr> {
    let live = state
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .map_err(|error| internal_err(format!("room lookup for {room_jid} failed: {error}")))?;
    if let Some(actor) = live {
        return Ok(Some(RoomLocation::Local(actor)));
    }
    #[cfg(feature = "clustering")]
    match crate::server::routes::interpret::extension_room_owner(websocket_state, room_jid).await {
        Ok(Some(owner)) => return Ok(Some(RoomLocation::Remote(owner))),
        Ok(None) => {}
        Err(error) => {
            return Err(internal_err(format!(
                "owner lookup for {room_jid} failed: {error}"
            )))
        }
    }
    let channel = get_managed_channel_for_room(websocket_state, room_jid)
        .await
        .map_err(|error| internal_err(format!("channel lookup for {room_jid} failed: {error}")))?;
    let (waddle_id, channel_id) = parse_room_jid_context(room_jid);
    let (config, waddle_id, channel_id) = match channel {
        Some(channel) => (channel.room_config(), waddle_id, channel.id),
        // A room without a channel record: the actor restores its config
        // and affiliations from the durable mirror, so this placeholder
        // config, the one a join creates such a room with, is replaced.
        None if durable => (
            RoomConfig {
                name: room_jid
                    .node()
                    .map(|node| node.to_string())
                    .unwrap_or_else(|| "Room".to_string()),
                members_only: false,
                ..Default::default()
            },
            waddle_id,
            channel_id,
        ),
        None => return Ok(None),
    };
    let acquisition =
        get_or_create_room_actor(websocket_state, room_jid, config, waddle_id, channel_id)
            .await
            .map_err(|error| internal_err(format!("loading room {room_jid} failed: {error}")))?;
    Ok(Some(RoomLocation::Local(acquisition.actor_ref)))
}

#[cfg(feature = "clustering")]
use relay::remote_step;
#[cfg(feature = "clustering")]
pub(crate) use relay::{
    relayed_member_clear, MUC_MEMBER_CLEAR_MAILBOX_TIMEOUT, MUC_MEMBER_CLEAR_REPLY_TIMEOUT,
};
#[cfg(all(test, feature = "clustering"))]
pub(crate) use relay::{TestMucMemberClearRelay, TEST_MUC_MEMBER_CLEAR_RELAY};

/// Rooms another node owns: that node checks and clears through its own
/// actor with [`local_step`].
#[cfg(feature = "clustering")]
mod relay {
    use std::time::Duration;

    use jid::BareJid;
    use kameo::actor::ActorRef;
    use waddle_xmpp::muc::room_actor::RoomActor;
    use waddle_xmpp::muc::room_registry_actor::GetRoom;
    use waddle_xmpp::ownership::NodeIdentity;

    use super::{internal_err, local_step, AdminErr, Step, StepOutcome};
    use crate::clustering::relay::{
        RelayAskError, RelayHandle, RelayMucMemberClear, RelayMucMemberClearReply,
        RelayMucMemberClearStep, RelaySendEffect, RelaySendFailure,
    };
    use crate::server::routes::websocket::WebSocketState;

    pub(crate) const MUC_MEMBER_CLEAR_MAILBOX_TIMEOUT: Duration = Duration::from_secs(1);
    /// Clearing persists tuples and the room's durable rows on the owner.
    pub(crate) const MUC_MEMBER_CLEAR_REPLY_TIMEOUT: Duration = Duration::from_secs(10);
    /// The origin's own bound on one ask, relay lookup included.
    const ASK_LIMIT: Duration = MUC_MEMBER_CLEAR_MAILBOX_TIMEOUT
        .saturating_add(MUC_MEMBER_CLEAR_REPLY_TIMEOUT)
        .saturating_add(Duration::from_millis(500));

    /// Replaces only the transport hop to the owner.
    #[cfg(test)]
    pub(crate) type TestMucMemberClearRelay = std::sync::Arc<
        dyn Fn(
                NodeIdentity,
                RelayMucMemberClear,
            ) -> futures::future::BoxFuture<
                'static,
                Result<RelayMucMemberClearReply, RelayAskError>,
            > + Send
            + Sync,
    >;

    #[cfg(test)]
    tokio::task_local! {
        pub(crate) static TEST_MUC_MEMBER_CLEAR_RELAY: TestMucMemberClearRelay;
    }

    /// Origin side: ask the room's owner to run `step`.
    pub(super) async fn remote_step(
        websocket_state: &WebSocketState,
        owner: &NodeIdentity,
        room_jid: &BareJid,
        member: &BareJid,
        step: Step,
    ) -> Result<StepOutcome, AdminErr> {
        let message = RelayMucMemberClear {
            room: room_jid.clone(),
            member: member.clone(),
            step: match step {
                Step::Check => RelayMucMemberClearStep::Check,
                Step::Clear => RelayMucMemberClearStep::Clear,
            },
            trace: Default::default(),
        };
        match ask_owner(websocket_state, owner, message).await {
            Ok(RelayMucMemberClearReply::Clearable | RelayMucMemberClearReply::Cleared) => {
                Ok(StepOutcome::Done)
            }
            Ok(RelayMucMemberClearReply::SoleOwner) => Ok(StepOutcome::SoleOwner),
            Ok(RelayMucMemberClearReply::NotOwner) => Ok(StepOutcome::Moved),
            Ok(RelayMucMemberClearReply::Failed) => Err(internal_err(format!(
                "the owner of room {room_jid} could not clear the member"
            ))),
            Err(error) => Err(internal_err(format!(
                "the owner of room {room_jid} was unreachable: {error}"
            ))),
        }
    }

    async fn ask_owner(
        websocket_state: &WebSocketState,
        owner: &NodeIdentity,
        message: RelayMucMemberClear,
    ) -> Result<RelayMucMemberClearReply, RelayAskError> {
        #[cfg(test)]
        if let Ok(relay) = TEST_MUC_MEMBER_CLEAR_RELAY.try_with(std::sync::Arc::clone) {
            return relay(owner.clone(), message).await;
        }
        let Some(bridge) = websocket_state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        else {
            return Err(RelayAskError::NotFound {
                node_id: crate::clustering::NodeId::new(owner.node_id.clone()),
            });
        };
        let mut handle: RelayHandle = bridge.muc_member_clear_handle(owner);
        tokio::time::timeout(ASK_LIMIT, handle.muc_member_clear(message))
            .await
            .unwrap_or_else(|_| {
                Err(RelayAskError::Send {
                    failure: RelaySendFailure::ReplyTimeout,
                    effect: RelaySendEffect::MaybeCommitted,
                    message: "the room owner did not answer in time".to_owned(),
                })
            })
    }

    /// Owner side: run a forwarded step against the local room actor.
    pub(crate) async fn relayed_member_clear(
        websocket_state: &WebSocketState,
        message: RelayMucMemberClear,
    ) -> RelayMucMemberClearReply {
        let state = &websocket_state.deps.app_state;
        let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&message.room) else {
            return RelayMucMemberClearReply::Failed;
        };
        let actor: Option<ActorRef<RoomActor>> = match state
            .room_registry
            .ask(GetRoom {
                room_jid: message.room.clone(),
            })
            .await
        {
            Ok(actor) => actor,
            Err(error) => {
                tracing::warn!(room = %message.room, %error, "relayed member clear room lookup failed");
                return RelayMucMemberClearReply::Failed;
            }
        };
        // No live actor here: the claim moved, so the origin re-resolves.
        let Some(actor) = actor else {
            return RelayMucMemberClearReply::NotOwner;
        };
        let step = match message.step {
            RelayMucMemberClearStep::Check => Step::Check,
            RelayMucMemberClearStep::Clear => Step::Clear,
        };
        match local_step(
            state,
            websocket_state,
            &actor,
            &message.room,
            &channel_id,
            &message.member,
            step,
        )
        .await
        {
            Ok(StepOutcome::SoleOwner) => RelayMucMemberClearReply::SoleOwner,
            Ok(StepOutcome::Done) => match message.step {
                RelayMucMemberClearStep::Check => RelayMucMemberClearReply::Clearable,
                RelayMucMemberClearStep::Clear => RelayMucMemberClearReply::Cleared,
            },
            Ok(StepOutcome::Moved) => RelayMucMemberClearReply::NotOwner,
            Err(error) => {
                tracing::warn!(
                    room = %message.room,
                    error = %super::admin_err_text(&error),
                    "relayed member clear failed"
                );
                RelayMucMemberClearReply::Failed
            }
        }
    }
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

#[cfg(feature = "clustering")]
fn admin_err_text(error: &AdminErr) -> String {
    match error.as_ref() {
        CommandResult::Error(error) => error.to_string(),
        _ => "unexpected command result".to_string(),
    }
}

fn internal_err(text: String) -> AdminErr {
    Box::new(CommandResult::Error(XmppError::internal(text)))
}
