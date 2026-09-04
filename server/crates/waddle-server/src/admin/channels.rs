//! Admin V2 — Channels CRUD over XEP-0050 ad-hoc commands.
//!
//! Eight owner-gated commands under `urn:waddle:admin:channels:*`:
//!
//! - `list` — paginated read of all MUC rooms tracked by the room registry,
//!   with occupant + per-tier affiliation counts.
//! - `create` — create a new MUC room (defaults: public, persistent, not
//!   members-only).
//! - `update` — patch name / topic / XEP-0045 visibility policy on an
//!   existing room's `RoomConfig`.
//! - `delete` — destroy a MUC room via the room registry.
//! - `occupants` — list live occupants (nick, real_jid, role, affiliation).
//! - `affiliations` — list every persistent affiliation, optionally
//!   filtered to a single tier.
//! - `set-affiliation` — grant/revoke owner/admin/member/none/outcast;
//!   `outcast` is the XEP-0045 §10.2 ban.
//! - `kick` — XEP-0045 §9.1 role-change to `none`; for Waddle-managed
//!   members-only channels this also revokes an explicit `member` affiliation.
//!
//! All handlers delegate to the typed dependencies on [`AppState`]:
//! `room_registry` (`waddle_xmpp::muc::room_registry_actor::*`), and
//! `muc_domain` (used to construct fresh room JIDs on `create`).

use std::collections::HashSet;
use std::{
    collections::BTreeSet,
    sync::{Arc, OnceLock},
};

use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use waddle_xmpp::commands::{CommandContext, CommandResult};
use waddle_xmpp::muc::room_registry_actor::{
    CreateRoom, CreateRoomWithInitialAffiliations, DestroyRoom, DestroyRoomOutcome,
    DestroyRoomReason, GetOrCreateRoom, GetOrCreateRoomWithLiveRoster, GetRoom, ListRooms,
    RetryPendingRoomReleases,
};
use waddle_xmpp::muc::{
    affiliation::FederatedAffiliationConfig,
    room_actor::{
        ApplyAdminItems, ApplyAffiliationChange, ChangeAffiliation, EnforceMembersOnlyAffiliations,
        GetAffiliation, GetConfig, GetSnapshot, LeaveByRealJid, ListAffiliations, ListOccupants,
        OccupantCount, RoomActor, UpdateConfig,
    },
    AdminItem, PinPermission, RoomConfig,
};
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::registry::ConnectionRegistry;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::xep::xep0004::{DataForm, Field, FieldType, FormType};
use waddle_xmpp::xep::xep0421::OccupantIdentity;
use waddle_xmpp::XmppError;
use waddle_xmpp::{Affiliation, ChannelInfo, ChannelType, Role, Stanza};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::minidom::Element;

use crate::admin::is_community_owner;
use crate::auth::local_account_exists;
use crate::channel_space_links::{ChannelSpaceLink, ChannelSpaceLinkError};
use crate::db::actor::{DbExecute, DbQuery};
use crate::db::{row_value, ValueExt};
use crate::permissions::{
    CheckPermission, DeleteTuple, Object, ObjectType, Permission, PermissionError, Relation,
    Subject, SubjectType, Tuple, WriteTuple,
};
use crate::server::routes::websocket::WebSocketState;
use crate::server::xmpp_state::{
    delete_xmpp_channel, get_xmpp_channel, upsert_xmpp_channel, XmppChannelRecord,
    XmppChannelUpsert,
};
use crate::server::AppState;
use crate::space_identity::{canonical_space_projection, SpaceNode, SpaceProjectionError};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const NODE_LIST: &str = waddle_xmpp::admin::NS_ADMIN_CHANNELS_LIST;
pub const NODE_CREATE: &str = waddle_xmpp::admin::NS_ADMIN_CHANNELS_CREATE;
pub const NODE_UPDATE: &str = waddle_xmpp::admin::NS_ADMIN_CHANNELS_UPDATE;
pub const NODE_DELETE: &str = waddle_xmpp::admin::NS_ADMIN_CHANNELS_DELETE;
pub const NODE_OCCUPANTS: &str = waddle_xmpp::admin::NS_ADMIN_CHANNELS_OCCUPANTS;
pub const NODE_AFFILIATIONS: &str = waddle_xmpp::admin::NS_ADMIN_CHANNELS_AFFILIATIONS;
pub const NODE_SET_AFFILIATION: &str = waddle_xmpp::admin::NS_ADMIN_CHANNELS_SET_AFFILIATION;
pub const NODE_KICK: &str = waddle_xmpp::admin::NS_ADMIN_CHANNELS_KICK;
pub const NODE_GROUP_DM_CREATE: &str = waddle_xmpp::admin::NS_GROUP_DM_CREATE;
pub const NODE_GROUP_DM_LEAVE: &str = waddle_xmpp::admin::NS_GROUP_DM_LEAVE;
pub const NODE_GROUP_DM_RENAME: &str = waddle_xmpp::admin::NS_GROUP_DM_RENAME;

pub const DEFAULT_PAGE_SIZE: u32 = 50;
pub const MAX_PAGE_SIZE: u32 = 200;
const MAX_NAME_LEN: usize = 80;
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";

/// Upper bound on the admin-kick room-actor asks. The room actor
/// awaits the durable-membership source inside its affiliation
/// handlers, so a wedged (not dead) permission actor can stall the
/// room's mailbox; a reply timeout keeps that stall from propagating
/// into the admin command handler (timeout maps to the existing
/// catch-all `Err` arm). Magnitude matches the `REAPER_ASK_TIMEOUT`
/// precedent in `session_janitors.rs`.
const ADMIN_ROOM_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
type RoomConfigLockMap = dashmap::DashMap<BareJid, Arc<Semaphore>>;

// ---------------------------------------------------------------------------
// Affiliation wire vocabulary
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireAffiliation {
    Owner,
    Admin,
    Member,
    None,
    Outcast,
}

impl WireAffiliation {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::None => "none",
            Self::Outcast => "outcast",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "owner" => Ok(Self::Owner),
            "admin" => Ok(Self::Admin),
            "member" => Ok(Self::Member),
            "none" => Ok(Self::None),
            "outcast" => Ok(Self::Outcast),
            other => Err(format!(
                "affiliation must be owner|admin|member|none|outcast, got '{other}'"
            )),
        }
    }

    pub fn to_muc(self) -> Affiliation {
        match self {
            Self::Owner => Affiliation::Owner,
            Self::Admin => Affiliation::Admin,
            Self::Member => Affiliation::Member,
            Self::None => Affiliation::None,
            Self::Outcast => Affiliation::Outcast,
        }
    }

    pub fn from_muc(aff: Affiliation) -> Self {
        match aff {
            Affiliation::Owner => Self::Owner,
            Affiliation::Admin => Self::Admin,
            Affiliation::Member => Self::Member,
            Affiliation::None => Self::None,
            Affiliation::Outcast => Self::Outcast,
        }
    }
}

// MUC role wire vocabulary (read-only — used by `occupants`).
fn role_as_wire(role: waddle_xmpp::Role) -> &'static str {
    match role {
        waddle_xmpp::Role::Moderator => "moderator",
        waddle_xmpp::Role::Participant => "participant",
        waddle_xmpp::Role::Visitor => "visitor",
        waddle_xmpp::Role::None => "none",
    }
}

// ---------------------------------------------------------------------------
// Typed argument / result structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelsListArgs {
    pub space_jid: Option<BareJid>,
    pub space_node: Option<SpaceNode>,
    pub prefix: Option<String>,
    pub page_size: u32,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelListEntry {
    pub channel_jid: BareJid,
    pub name: String,
    pub topic: Option<String>,
    pub channel_type: ChannelType,
    pub is_public: bool,
    pub members_only: bool,
    pub occupant_count: u32,
    pub affiliation_owner_count: u32,
    pub affiliation_admin_count: u32,
    pub affiliation_member_count: u32,
    pub affiliation_outcast_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelsListResult {
    pub entries: Vec<ChannelListEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsCreateArgs {
    pub space_jid: Option<BareJid>,
    pub space_node: Option<SpaceNode>,
    pub name: String,
    pub topic: Option<String>,
    pub channel_type: ChannelType,
    /// XEP-0045 `muc#roomconfig_publicroom`; default `true`.
    pub is_public: bool,
    /// XEP-0045 `muc#roomconfig_membersonly`. When omitted during create,
    /// hidden rooms default to members-only and public rooms default to open.
    pub members_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRef {
    pub channel_jid: BareJid,
    pub name: String,
    pub topic: Option<String>,
    pub channel_type: ChannelType,
    pub is_public: bool,
    pub members_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsUpdateArgs {
    pub channel_jid: BareJid,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub channel_type: Option<ChannelType>,
    pub is_public: Option<bool>,
    pub members_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsDeleteArgs {
    pub channel_jid: BareJid,
    pub confirm: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsOccupantsArgs {
    pub channel_jid: BareJid,
    pub page_size: u32,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelOccupantEntry {
    pub nick: String,
    pub real_jid: jid::FullJid,
    pub role: waddle_xmpp::Role,
    pub affiliation: WireAffiliation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsOccupantsResult {
    pub entries: Vec<ChannelOccupantEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsAffiliationsArgs {
    pub channel_jid: BareJid,
    pub filter: Option<WireAffiliation>,
    pub page_size: u32,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelAffiliationEntry {
    pub jid: BareJid,
    pub affiliation: WireAffiliation,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsAffiliationsResult {
    pub entries: Vec<ChannelAffiliationEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsSetAffiliationArgs {
    pub channel_jid: BareJid,
    pub member_jid: BareJid,
    pub affiliation: WireAffiliation,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsSetAffiliationResult {
    pub member_jid: BareJid,
    pub affiliation: WireAffiliation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsKickArgs {
    pub channel_jid: BareJid,
    pub occupant_jid: BareJid,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsKickResult {
    pub occupant_jid: BareJid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmCreateArgs {
    pub name: String,
    pub member_jids: Vec<BareJid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmLeaveArgs {
    pub room_jid: BareJid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmRenameArgs {
    pub room_jid: BareJid,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmRef {
    pub room_jid: BareJid,
    pub name: String,
    pub is_public: bool,
    pub members_only: bool,
    pub persistent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmLeaveResult {
    pub room_jid: BareJid,
    pub left: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmRenameResult {
    pub room_jid: BareJid,
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub async fn register(
    registry: &waddle_xmpp::commands::CommandRegistry,
    app_state: Arc<AppState>,
    websocket_state: Arc<WebSocketState>,
    connection_registry: Arc<ConnectionRegistry>,
    user_registry: ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    sm_session_registry: Arc<InMemorySmSessionRegistry>,
    sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
) {
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_LIST, "Admin · List channels", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_list(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_CREATE, "Admin · Create channel", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_create(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        let websocket_state = Arc::clone(&websocket_state);
        let connections = Arc::clone(&connection_registry);
        let sfu = sfu.clone();
        registry
            .register(NODE_UPDATE, "Admin · Update channel", move |ctx| {
                let state = Arc::clone(&state);
                let websocket_state = Arc::clone(&websocket_state);
                let connections = Arc::clone(&connections);
                let sfu = sfu.clone();
                async move { handle_update(ctx, state, websocket_state, connections, sfu).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_DELETE, "Admin · Delete channel", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_delete(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_OCCUPANTS, "Admin · List occupants", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_occupants(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_AFFILIATIONS, "Admin · List affiliations", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_affiliations(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        let websocket_state = Arc::clone(&websocket_state);
        let connections = Arc::clone(&connection_registry);
        let sfu = sfu.clone();
        registry
            .register(
                NODE_SET_AFFILIATION,
                "Admin · Set affiliation",
                move |ctx| {
                    let state = Arc::clone(&state);
                    let websocket_state = Arc::clone(&websocket_state);
                    let connections = Arc::clone(&connections);
                    let sfu = sfu.clone();
                    async move {
                        handle_set_affiliation(ctx, state, websocket_state, connections, sfu).await
                    }
                },
            )
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        let websocket_state = Arc::clone(&websocket_state);
        let connections = Arc::clone(&connection_registry);
        let sfu = sfu.clone();
        registry
            .register(NODE_KICK, "Admin · Kick occupant", move |ctx| {
                let state = Arc::clone(&state);
                let websocket_state = Arc::clone(&websocket_state);
                let connections = Arc::clone(&connections);
                let sfu = sfu.clone();
                async move { handle_kick(ctx, state, websocket_state, connections, sfu).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_GROUP_DM_CREATE, "Create group DM", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_group_dm_create(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        let websocket_state = Arc::clone(&websocket_state);
        let connections = Arc::clone(&connection_registry);
        let user_registry = user_registry.clone();
        let sm_sessions = Arc::clone(&sm_session_registry);
        registry
            .register(NODE_GROUP_DM_LEAVE, "Leave group DM", move |ctx| {
                let state = Arc::clone(&state);
                let websocket_state = Arc::clone(&websocket_state);
                let connections = Arc::clone(&connections);
                let user_registry = user_registry.clone();
                let sm_sessions = Arc::clone(&sm_sessions);
                async move {
                    handle_group_dm_leave(
                        ctx,
                        state,
                        websocket_state,
                        connections,
                        user_registry,
                        sm_sessions,
                    )
                    .await
                }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        let websocket_state = Arc::clone(&websocket_state);
        let connections = Arc::clone(&connection_registry);
        registry
            .register(NODE_GROUP_DM_RENAME, "Rename group DM", move |ctx| {
                let state = Arc::clone(&state);
                let websocket_state = Arc::clone(&websocket_state);
                let connections = Arc::clone(&connections);
                async move {
                    handle_group_dm_rename(ctx, state, websocket_state, connections).await
                }
            })
            .await;
    }
}

// ---------------------------------------------------------------------------
// Handler shells
// ---------------------------------------------------------------------------

type AdminErr = Box<CommandResult>;

async fn caller_or_forbidden(ctx: &CommandContext, state: &AppState) -> Result<BareJid, AdminErr> {
    let bare = ctx.from.to_bare();
    if !is_community_owner(state, &bare).await {
        return Err(Box::new(CommandResult::Error(XmppError::forbidden(Some(
            "Admin commands require the community owner role".to_string(),
        )))));
    }
    Ok(bare)
}

fn bad_request(text: impl Into<String>) -> AdminErr {
    Box::new(CommandResult::Error(XmppError::bad_request(Some(
        text.into(),
    ))))
}

fn internal_err(text: impl Into<String>) -> AdminErr {
    Box::new(CommandResult::Error(XmppError::internal(text.into())))
}

fn unavailable(text: impl Into<String>) -> AdminErr {
    Box::new(CommandResult::Error(XmppError::service_unavailable(Some(
        text.into(),
    ))))
}

async fn destroy_room_for_rollback(
    state: &AppState,
    room_jid: &BareJid,
    rollback_context: &str,
) -> Result<(), AdminErr> {
    let mut outcome = state
        .room_registry
        .ask(DestroyRoom {
            room_jid: room_jid.clone(),
            reason: DestroyRoomReason::Destroy,
        })
        .await
        .map_err(send_err("room_registry ask DestroyRoom during rollback"))?;
    if outcome == DestroyRoomOutcome::ReleaseBacklogFull {
        state
            .room_registry
            .ask(RetryPendingRoomReleases { limit: 1 })
            .await
            .map_err(send_err(
                "room_registry ask RetryPendingRoomReleases during rollback",
            ))?;
        outcome = state
            .room_registry
            .ask(DestroyRoom {
                room_jid: room_jid.clone(),
                reason: DestroyRoomReason::Destroy,
            })
            .await
            .map_err(send_err("room_registry retry DestroyRoom during rollback"))?;
    }
    match outcome {
        DestroyRoomOutcome::Destroyed | DestroyRoomOutcome::NotRegistered => Ok(()),
        DestroyRoomOutcome::DurableWipeFailed => Err(internal_err(format!(
            "{rollback_context}: durable room-state wipe failed for {room_jid}; rollback is incomplete"
        ))),
        DestroyRoomOutcome::ReleaseBacklogFull => Err(internal_err(format!(
            "{rollback_context}: exact-release retry backlog is still full for {room_jid} after bounded redrive; room remains registered and rollback must be retried"
        ))),
    }
}

async fn handle_list(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state).await {
        return *forbidden;
    }
    let args = match parse_list_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_list(&state, &args).await {
        Ok(result) => CommandResult::Completed {
            session_id: None,
            form: Some(build_list_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_create(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state).await {
        return *forbidden;
    }
    let args = match parse_create_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_create(&state, &args).await {
        Ok(channel) => CommandResult::Completed {
            session_id: None,
            form: Some(build_channel_form(&channel)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_group_dm_create(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    let args = match parse_group_dm_create_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_group_dm_create(&state, &ctx.from.to_bare(), &args).await {
        Ok(group_dm) => CommandResult::Completed {
            session_id: None,
            form: Some(build_group_dm_form(&group_dm)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_group_dm_leave(
    ctx: CommandContext,
    state: Arc<AppState>,
    websocket_state: Arc<WebSocketState>,
    connections: Arc<ConnectionRegistry>,
    user_registry: ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    sm_sessions: Arc<InMemorySmSessionRegistry>,
) -> CommandResult {
    let caller_bare = ctx.from.to_bare();
    let caller_full = match ctx.from.clone().try_into_full() {
        Ok(full) => full,
        Err(_) => match caller_bare.with_resource_str("group-dm-leave") {
            Ok(full) => full,
            Err(error) => {
                return *internal_err(format!(
                    "group-DM leave caller JID '{caller_bare}' is not a valid full JID base: {error}"
                ));
            }
        },
    };
    let args = match parse_group_dm_leave_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(e) => return CommandResult::Error(XmppError::bad_request(Some(e))),
    };
    match run_group_dm_leave(
        &state,
        &websocket_state.deps.protocol.pending_local_muc_departures,
        &connections,
        &user_registry,
        &sm_sessions,
        &caller_full,
        &args,
    )
    .await
    {
        Ok(result) => CommandResult::Completed {
            session_id: None,
            form: Some(build_group_dm_leave_form(&result)),
            notes: vec![],
        },
        Err(err) => *err,
    }
}

async fn handle_group_dm_rename(
    ctx: CommandContext,
    state: Arc<AppState>,
    websocket_state: Arc<WebSocketState>,
    connections: Arc<ConnectionRegistry>,
) -> CommandResult {
    let caller_full = match ctx.from.clone().try_into_full() {
        Ok(full) => full,
        Err(_) => {
            return *internal_err("group-DM rename requires a bound full JID");
        }
    };
    let args = match parse_group_dm_rename_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(e) => return CommandResult::Error(XmppError::bad_request(Some(e))),
    };
    if ctx.iq.to().map(|jid| jid.to_bare()) != Some(args.room_jid.clone()) {
        return CommandResult::Error(XmppError::bad_request(Some(
            "group-dm:rename must be addressed to the room_jid".to_string(),
        )));
    }
    match run_group_dm_rename(&state, &websocket_state, &connections, &caller_full, &args).await {
        Ok(result) => CommandResult::Completed {
            session_id: None,
            form: Some(build_group_dm_rename_form(&result)),
            notes: vec![],
        },
        Err(err) => *err,
    }
}

async fn handle_update(
    ctx: CommandContext,
    state: Arc<AppState>,
    websocket_state: Arc<WebSocketState>,
    connections: Arc<ConnectionRegistry>,
    sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state).await {
        return *forbidden;
    }
    let args = match parse_update_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_update(&state, &websocket_state, &connections, &args, sfu.as_ref()).await {
        Ok(channel) => CommandResult::Completed {
            session_id: None,
            form: Some(build_channel_form(&channel)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_delete(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state).await {
        return *forbidden;
    }
    let args = match parse_delete_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_delete(&state, &args).await {
        Ok(()) => CommandResult::Completed {
            session_id: None,
            form: None,
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_occupants(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state).await {
        return *forbidden;
    }
    let args = match parse_occupants_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_occupants(&state, &args).await {
        Ok(result) => CommandResult::Completed {
            session_id: None,
            form: Some(build_occupants_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_affiliations(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state).await {
        return *forbidden;
    }
    let args = match parse_affiliations_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_affiliations(&state, &args).await {
        Ok(result) => CommandResult::Completed {
            session_id: None,
            form: Some(build_affiliations_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_set_affiliation(
    ctx: CommandContext,
    state: Arc<AppState>,
    websocket_state: Arc<WebSocketState>,
    connections: Arc<ConnectionRegistry>,
    sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
) -> CommandResult {
    let caller_bare = match caller_or_forbidden(&ctx, &state).await {
        Ok(caller) => caller,
        Err(forbidden) => return *forbidden,
    };
    let args = match parse_set_affiliation_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_set_affiliation(
        &state,
        Some(websocket_state.as_ref()),
        &connections,
        &caller_bare,
        &args,
        sfu.as_ref(),
    )
    .await
    {
        Ok(result) => CommandResult::Completed {
            session_id: None,
            form: Some(build_set_affiliation_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_kick(
    ctx: CommandContext,
    state: Arc<AppState>,
    websocket_state: Arc<WebSocketState>,
    connections: Arc<ConnectionRegistry>,
    sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
) -> CommandResult {
    let caller_bare = match caller_or_forbidden(&ctx, &state).await {
        Ok(bare) => bare,
        Err(forbidden) => return *forbidden,
    };
    let caller_full = match ctx.from.clone().try_into_full() {
        Ok(full) => full,
        Err(_) => {
            // Synthesize a full JID with a fixed "admin-v2" resource so
            // the `ApplyAdminItems` actor message has the FullJid it
            // requires — only `.to_bare()` is read off it for the
            // §9.1.1 `<actor jid='…'/>` stamp, which collapses to
            // the same bare JID either way.
            match caller_bare.with_resource_str("admin-v2") {
                Ok(full) => full,
                Err(error) => {
                    return *internal_err(format!(
                        "admin caller JID '{caller_bare}' is not a valid full JID base: {error}"
                    ));
                }
            }
        }
    };
    let args = match parse_kick_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_kick(
        &state,
        Some(websocket_state.as_ref()),
        &connections,
        &caller_full,
        &args,
        sfu.as_ref(),
    )
    .await
    {
        Ok(result) => CommandResult::Completed {
            session_id: None,
            form: Some(build_kick_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

// ---------------------------------------------------------------------------
// Argument parsers
// ---------------------------------------------------------------------------

fn parse_optional_text(form: &DataForm, var: &str) -> Option<String> {
    form.get_value(var)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn parse_required_text(form: &DataForm, var: &str) -> Result<String, String> {
    parse_optional_text(form, var).ok_or_else(|| format!("'{var}' is required"))
}

fn parse_required_bare_jid(form: &DataForm, var: &str) -> Result<BareJid, String> {
    let raw = parse_required_text(form, var)?;
    raw.parse()
        .map_err(|e| format!("'{var}' is not a valid bare JID '{raw}': {e}"))
}

fn parse_optional_bool(form: &DataForm, var: &str) -> Result<Option<bool>, String> {
    let Some(raw) = form.get_value(var) else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    match raw {
        "1" | "true" | "TRUE" | "yes" | "YES" => Ok(Some(true)),
        "0" | "false" | "FALSE" | "no" | "NO" => Ok(Some(false)),
        other => Err(format!("'{var}' must be a boolean, got '{other}'")),
    }
}

fn parse_optional_channel_type(form: &DataForm, var: &str) -> Result<Option<ChannelType>, String> {
    let Some(raw) = parse_optional_text(form, var) else {
        return Ok(None);
    };
    let Some(channel_type) = ChannelType::parse(&raw) else {
        return Err(format!("'{var}' has unsupported channel type '{raw}'"));
    };
    if matches!(channel_type, ChannelType::GroupDm) {
        return Err(format!("'{var}' group-dm is managed by group-dm:create"));
    }
    Ok(Some(channel_type))
}

fn parse_page_size(form: &DataForm) -> Result<u32, String> {
    match form.get_value("page_size") {
        Some(raw) if !raw.is_empty() => {
            let parsed: u32 = raw
                .parse()
                .map_err(|_| format!("page_size must be a positive integer, got '{raw}'"))?;
            Ok(parsed.clamp(1, MAX_PAGE_SIZE))
        }
        _ => Ok(DEFAULT_PAGE_SIZE),
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    let len = name.chars().count();
    if len == 0 {
        return Err("name must be at least 1 character".to_string());
    }
    if len > MAX_NAME_LEN {
        return Err(format!(
            "name must be at most {MAX_NAME_LEN} characters, got {len}"
        ));
    }
    Ok(())
}

pub fn parse_list_args(form: Option<&DataForm>) -> Result<ChannelsListArgs, String> {
    let Some(form) = form else {
        return Ok(ChannelsListArgs {
            space_jid: None,
            space_node: None,
            prefix: None,
            page_size: DEFAULT_PAGE_SIZE,
            after_cursor: None,
        });
    };
    if !matches!(form.form_type, FormType::Submit) {
        return Ok(ChannelsListArgs {
            space_jid: None,
            space_node: None,
            prefix: None,
            page_size: DEFAULT_PAGE_SIZE,
            after_cursor: None,
        });
    }
    let space_jid = match form.get_value("space_jid") {
        Some(raw) if !raw.is_empty() => Some(
            raw.parse::<BareJid>()
                .map_err(|e| format!("space_jid invalid: {e}"))?,
        ),
        _ => None,
    };
    let prefix = form
        .get_value("prefix")
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    let space_node = parse_optional_text(form, "space_node").map(SpaceNode::from);
    let page_size = parse_page_size(form)?;
    let after_cursor = parse_optional_text(form, "after_cursor");
    Ok(ChannelsListArgs {
        space_jid,
        space_node,
        prefix,
        page_size,
        after_cursor,
    })
}

pub fn parse_create_args(form: Option<&DataForm>) -> Result<ChannelsCreateArgs, String> {
    let form = form.ok_or_else(|| "channels:create requires an args form".to_string())?;
    let name = parse_required_text(form, "name")?;
    validate_name(&name)?;
    let topic = parse_optional_text(form, "topic");
    let space_jid = match form.get_value("space_jid") {
        Some(raw) if !raw.is_empty() => Some(
            raw.parse::<BareJid>()
                .map_err(|e| format!("space_jid invalid: {e}"))?,
        ),
        _ => None,
    };
    let space_node = parse_optional_text(form, "space_node").map(SpaceNode::from);
    let channel_type =
        parse_optional_channel_type(form, "channel_type")?.unwrap_or(ChannelType::Text);
    // XEP-0045 public-room discovery visibility defaults true.
    let is_public = parse_optional_bool(form, "is_public")?.unwrap_or(true);
    let members_only = parse_optional_bool(form, "members_only")?;
    Ok(ChannelsCreateArgs {
        space_jid,
        space_node,
        name,
        topic,
        channel_type,
        is_public,
        members_only,
    })
}

pub fn parse_group_dm_create_args(form: Option<&DataForm>) -> Result<GroupDmCreateArgs, String> {
    let form = form.ok_or_else(|| "group-dm:create requires an args form".to_string())?;
    let name = parse_required_text(form, "name")?;
    validate_name(&name)?;
    let member_values = form
        .get_values("member_jids")
        .ok_or_else(|| "member_jids is required".to_string())?;
    let mut member_jids = Vec::with_capacity(member_values.len());
    for value in member_values
        .iter()
        .filter(|value| !value.trim().is_empty())
    {
        member_jids.push(
            value
                .trim()
                .parse::<BareJid>()
                .map_err(|e| format!("member_jids contains invalid JID '{value}': {e}"))?,
        );
    }
    member_jids.sort();
    member_jids.dedup();
    if member_jids.len() < 2 {
        return Err("group-dm:create requires at least two member_jids".to_string());
    }
    Ok(GroupDmCreateArgs { name, member_jids })
}

pub fn parse_group_dm_leave_args(form: Option<&DataForm>) -> Result<GroupDmLeaveArgs, String> {
    let form = form.ok_or_else(|| "group-dm:leave requires an args form".to_string())?;
    let room_jid = parse_required_bare_jid(form, "room_jid")?;
    Ok(GroupDmLeaveArgs { room_jid })
}

pub fn parse_group_dm_rename_args(form: Option<&DataForm>) -> Result<GroupDmRenameArgs, String> {
    let form = form.ok_or_else(|| "group-dm:rename requires an args form".to_string())?;
    let room_jid = parse_required_bare_jid(form, "room_jid")?;
    let name = parse_optional_text(form, "name").and_then(|value| {
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });
    if let Some(ref name) = name {
        validate_name(name)?;
    }
    Ok(GroupDmRenameArgs { room_jid, name })
}

pub fn parse_update_args(form: Option<&DataForm>) -> Result<ChannelsUpdateArgs, String> {
    let form = form.ok_or_else(|| "channels:update requires an args form".to_string())?;
    let channel_jid = parse_required_bare_jid(form, "channel_jid")?;
    let name = parse_optional_text(form, "name");
    if let Some(ref name) = name {
        validate_name(name)?;
    }
    let topic = parse_optional_text(form, "topic");
    let channel_type = parse_optional_channel_type(form, "channel_type")?;
    let is_public = parse_optional_bool(form, "is_public")?;
    let members_only = parse_optional_bool(form, "members_only")?;
    Ok(ChannelsUpdateArgs {
        channel_jid,
        name,
        topic,
        channel_type,
        is_public,
        members_only,
    })
}

pub fn parse_delete_args(form: Option<&DataForm>) -> Result<ChannelsDeleteArgs, String> {
    let form = form.ok_or_else(|| "channels:delete requires an args form".to_string())?;
    let channel_jid = parse_required_bare_jid(form, "channel_jid")?;
    let confirm = parse_required_text(form, "confirm")?;
    if confirm != "yes" {
        return Err("channels:delete requires confirm='yes'".to_string());
    }
    Ok(ChannelsDeleteArgs {
        channel_jid,
        confirm,
    })
}

pub fn parse_occupants_args(form: Option<&DataForm>) -> Result<ChannelsOccupantsArgs, String> {
    let form = form.ok_or_else(|| "channels:occupants requires an args form".to_string())?;
    let channel_jid = parse_required_bare_jid(form, "channel_jid")?;
    let page_size = parse_page_size(form)?;
    let after_cursor = parse_optional_text(form, "after_cursor");
    Ok(ChannelsOccupantsArgs {
        channel_jid,
        page_size,
        after_cursor,
    })
}

pub fn parse_affiliations_args(
    form: Option<&DataForm>,
) -> Result<ChannelsAffiliationsArgs, String> {
    let form = form.ok_or_else(|| "channels:affiliations requires an args form".to_string())?;
    let channel_jid = parse_required_bare_jid(form, "channel_jid")?;
    let filter = match form.get_value("filter") {
        Some(raw) if !raw.is_empty() => Some(WireAffiliation::parse(raw)?),
        _ => None,
    };
    let page_size = parse_page_size(form)?;
    let after_cursor = parse_optional_text(form, "after_cursor");
    Ok(ChannelsAffiliationsArgs {
        channel_jid,
        filter,
        page_size,
        after_cursor,
    })
}

pub fn parse_set_affiliation_args(
    form: Option<&DataForm>,
) -> Result<ChannelsSetAffiliationArgs, String> {
    let form = form.ok_or_else(|| "channels:set-affiliation requires an args form".to_string())?;
    let channel_jid = parse_required_bare_jid(form, "channel_jid")?;
    let member_jid = parse_required_bare_jid(form, "member_jid")?;
    let affiliation = WireAffiliation::parse(&parse_required_text(form, "affiliation")?)?;
    let reason = parse_optional_text(form, "reason");
    Ok(ChannelsSetAffiliationArgs {
        channel_jid,
        member_jid,
        affiliation,
        reason,
    })
}

pub fn parse_kick_args(form: Option<&DataForm>) -> Result<ChannelsKickArgs, String> {
    let form = form.ok_or_else(|| "channels:kick requires an args form".to_string())?;
    let channel_jid = parse_required_bare_jid(form, "channel_jid")?;
    let occupant_jid = parse_required_bare_jid(form, "occupant_jid")?;
    let reason = parse_optional_text(form, "reason");
    Ok(ChannelsKickArgs {
        channel_jid,
        occupant_jid,
        reason,
    })
}

// ---------------------------------------------------------------------------
// Delegating handlers
// ---------------------------------------------------------------------------

fn send_err<E: std::fmt::Display>(prefix: &'static str) -> impl FnOnce(E) -> AdminErr {
    move |error| internal_err(format!("{prefix}: {error}"))
}

async fn run_list(
    state: &AppState,
    args: &ChannelsListArgs,
) -> Result<ChannelsListResult, AdminErr> {
    let mut rooms = state
        .room_registry
        .ask(ListRooms)
        .await
        .map_err(send_err("room_registry ask ListRooms"))?;
    rooms.sort();

    // Narrow by the exact Spaces PubSub node against the channel↔space link projection.
    // A channel belongs to at most one space; rooms with no link row
    // are not in any space, so they are filtered out when a
    // space filter is supplied.
    if let Some(space_jid) = args.space_jid.as_ref() {
        let (space_node, _) =
            canonical_space_jid(&state.spaces_jid, space_jid, args.space_node.as_ref())?;
        let in_space = state
            .channel_space_link_store
            .list_channels_in_space_node(&space_node)
            .await
            .map_err(map_link_err)?;
        let permitted: std::collections::HashSet<BareJid> = in_space.into_iter().collect();
        rooms.retain(|jid| permitted.contains(jid));
    } else if let Some(space_node) = args.space_node.as_ref() {
        let in_space = state
            .channel_space_link_store
            .list_channels_in_space_node(space_node)
            .await
            .map_err(map_link_err)?;
        let permitted: std::collections::HashSet<BareJid> = in_space.into_iter().collect();
        rooms.retain(|jid| permitted.contains(jid));
    }

    if let Some(cursor) = args.after_cursor.as_deref() {
        rooms.retain(|jid| jid.to_string().as_str() > cursor);
    }
    // Filter by name prefix if requested. Requires asking each room's
    // config — bounded by the list size, which is small (single-tenant).
    let limit = args.page_size as usize;
    let mut entries = Vec::with_capacity(limit.min(rooms.len()));
    let mut iter_count = 0usize;
    for room_jid in &rooms {
        // Honour the prefix filter on the canonical name.
        let actor = match state
            .room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) | Err(_) => continue,
        };
        let config = actor
            .ask(GetConfig)
            .await
            .map_err(send_err("room actor GetConfig"))?;
        if config.group_dm {
            continue;
        }
        if let Some(prefix) = args.prefix.as_deref() {
            if !config.name.to_lowercase().starts_with(prefix) {
                continue;
            }
        }
        let channel_id = match waddle_xmpp::parse_managed_room_jid(room_jid) {
            Some(channel_id) => channel_id,
            None => continue,
        };
        let catalog_snapshot = get_xmpp_channel(state.db_pool.global_actor().clone(), &channel_id)
            .await
            .map_err(|error| internal_err(format!("channel catalog lookup failed: {error}")))?;
        let channel_type = channel_type_from_catalog_or_config(catalog_snapshot.as_ref(), &config);
        let occupant_count = actor
            .ask(OccupantCount)
            .await
            .map_err(send_err("room actor OccupantCount"))?;
        let affiliations = actor
            .ask(ListAffiliations { filter: None })
            .await
            .map_err(send_err("room actor ListAffiliations"))?;
        let (mut owner_n, mut admin_n, mut member_n, mut outcast_n) = (0u32, 0u32, 0u32, 0u32);
        for entry in &affiliations {
            match entry.affiliation {
                Affiliation::Owner => owner_n += 1,
                Affiliation::Admin => admin_n += 1,
                Affiliation::Member => member_n += 1,
                Affiliation::Outcast => outcast_n += 1,
                Affiliation::None => {}
            }
        }
        iter_count += 1;
        if iter_count > limit {
            break;
        }
        entries.push(ChannelListEntry {
            channel_jid: room_jid.clone(),
            name: config.name,
            topic: config.description,
            channel_type,
            is_public: config.public_room,
            members_only: config.members_only,
            occupant_count: u32::try_from(occupant_count).unwrap_or(u32::MAX),
            affiliation_owner_count: owner_n,
            affiliation_admin_count: admin_n,
            affiliation_member_count: member_n,
            affiliation_outcast_count: outcast_n,
        });
        if entries.len() >= limit {
            break;
        }
    }
    let next_cursor = if iter_count > limit && !entries.is_empty() {
        entries.last().map(|entry| entry.channel_jid.to_string())
    } else {
        None
    };
    Ok(ChannelsListResult {
        entries,
        next_cursor,
    })
}

fn map_link_err(error: ChannelSpaceLinkError) -> AdminErr {
    internal_err(format!("channel-space link storage: {error}"))
}

fn now_unix_seconds() -> i64 {
    chrono::Utc::now().timestamp()
}

fn mint_channel_localpart(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-');
    let base = if trimmed.is_empty() {
        "channel"
    } else {
        trimmed
    };
    let tail = uuid::Uuid::new_v4().simple().to_string();
    let short_tail: String = tail.chars().take(8).collect();
    format!("{base}-{short_tail}")
}

fn canonical_space_jid(
    spaces_jid: &BareJid,
    space_jid: &BareJid,
    space_node: Option<&SpaceNode>,
) -> Result<(SpaceNode, BareJid), AdminErr> {
    canonical_space_projection(spaces_jid, space_jid, space_node).map_err(|error| match error {
        SpaceProjectionError::WrongDomain => {
            bad_request(format!("space_jid must target {}", spaces_jid.domain()))
        }
        SpaceProjectionError::MissingLocalpart => bad_request("space_jid must have a localpart"),
        SpaceProjectionError::InvalidNode(node) => {
            internal_err(format!("invalid space node '{node}'"))
        }
        SpaceProjectionError::MismatchedProjection => {
            bad_request("space_jid must match the space_node projection")
        }
    })
}

async fn existing_space_node(
    state: &AppState,
    space_jid: &BareJid,
    space_node: Option<&SpaceNode>,
) -> Result<(SpaceNode, BareJid), AdminErr> {
    let (node, canonical_jid) = canonical_space_jid(&state.spaces_jid, space_jid, space_node)?;
    let exists = state
        .pubsub_storage
        .get_node(&state.spaces_jid, &node)
        .await
        .map_err(|e| internal_err(format!("pubsub get_node failed: {e}")))?;
    if exists.is_none() {
        return Err(Box::new(CommandResult::Error(XmppError::item_not_found(
            Some(format!("no space '{}'", space_jid)),
        ))));
    }
    Ok((node, canonical_jid))
}

async fn publish_channel_space_bookmark(
    state: &AppState,
    node: &str,
    channel_id: &str,
    name: &str,
    channel_type: &str,
) -> Result<bool, AdminErr> {
    let item_id = format!("{}@{}", channel_id, state.muc_domain);
    let item_filter = [item_id.clone()];
    let previous_item = state
        .pubsub_storage
        .get_items(&state.spaces_jid, node, Some(1), &item_filter)
        .await
        .map_err(|e| internal_err(format!("pubsub read existing channel bookmark failed: {e}")))?
        .into_iter()
        .next()
        .map(|stored| stored.to_pubsub_item());
    let item = waddle_xmpp::xep::build_channel_item(
        &ChannelInfo {
            id: channel_id.to_string(),
            name: name.to_string(),
            channel_type: channel_type.to_string(),
        },
        &state.muc_domain.to_string(),
    )
    .map_err(|e| internal_err(format!("failed to build XEP-0503 channel bookmark: {e}")))?;
    state
        .pubsub_storage
        .publish_item(&state.spaces_jid, node, &item, None, false)
        .await
        .map_err(|e| internal_err(format!("pubsub publish channel bookmark failed: {e}")))?;
    match write_channel_parent_tuple_if_absent(state, channel_id, node).await {
        Ok(created) => Ok(created),
        Err(error) => {
            if let Some(previous_item) = previous_item.as_ref() {
                let _ = state
                    .pubsub_storage
                    .publish_item(&state.spaces_jid, node, previous_item, None, false)
                    .await;
            } else {
                let _ = state
                    .pubsub_storage
                    .retract_item(&state.spaces_jid, node, &item_id)
                    .await;
            }
            Err(error)
        }
    }
}

async fn restore_channel_space_bookmark(
    state: &AppState,
    node: &str,
    item_id: &str,
    channel_id: &str,
    previous_item: Option<&PubSubItem>,
    parent_tuple_created: bool,
) {
    if let Some(previous_item) = previous_item {
        if parent_tuple_created {
            match delete_channel_parent_tuple(state, channel_id, node).await {
                Ok(_) => {}
                Err(_error) => {
                    tracing::warn!(
                        node = %node,
                        item_id = %item_id,
                        "channels:update rollback failed to delete operation-created parent tuple; preserving newly-published Spaces bookmark",
                    );
                    return;
                }
            }
        }
        match state
            .pubsub_storage
            .publish_item(&state.spaces_jid, node, previous_item, None, false)
            .await
        {
            Ok(_) => {}
            Err(error) => {
                if parent_tuple_created {
                    let _ = write_channel_parent_tuple(state, channel_id, node).await;
                }
                tracing::warn!(
                    node = %node,
                    item_id = %item_id,
                    error = %error,
                    "channels:update rollback failed to restore previous Spaces bookmark",
                );
            }
        }
    } else {
        if parent_tuple_created {
            match delete_channel_parent_tuple(state, channel_id, node).await {
                Ok(_) => {}
                Err(_error) => {
                    tracing::warn!(
                        node = %node,
                        item_id = %item_id,
                        "channels:update rollback failed to delete operation-created parent tuple; preserving newly-published Spaces bookmark",
                    );
                    return;
                }
            }
        }
        if let Err(error) = state
            .pubsub_storage
            .retract_item(&state.spaces_jid, node, item_id)
            .await
        {
            if parent_tuple_created {
                let _ = write_channel_parent_tuple(state, channel_id, node).await;
            }
            tracing::warn!(
                node = %node,
                item_id = %item_id,
                error = %error,
                "channels:update rollback failed to retract newly-published Spaces bookmark",
            );
        }
    }
}

async fn write_channel_parent_tuple(
    state: &AppState,
    channel_id: &str,
    space_node: &str,
) -> Result<(), AdminErr> {
    write_channel_parent_tuple_if_absent(state, channel_id, space_node)
        .await
        .map(|_| ())
}

async fn write_channel_parent_tuple_if_absent(
    state: &AppState,
    channel_id: &str,
    space_node: &str,
) -> Result<bool, AdminErr> {
    let tuple = Tuple::new(
        Object::new(ObjectType::Channel, channel_id),
        Relation::new("parent"),
        Subject::userset(SubjectType::Space, space_node, ""),
    );
    match state.permission_actor.ask(WriteTuple { tuple }).await {
        Ok(()) => Ok(true),
        Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => {
            Ok(false)
        }
        Err(error) => Err(internal_err(format!(
            "permission actor failed writing channel parent tuple: {error}"
        ))),
    }
}

async fn delete_channel_parent_tuple(
    state: &AppState,
    channel_id: &str,
    space_node: &str,
) -> Result<bool, AdminErr> {
    let tuple = Tuple::new(
        Object::new(ObjectType::Channel, channel_id),
        Relation::new("parent"),
        Subject::userset(SubjectType::Space, space_node, ""),
    );
    match state.permission_actor.ask(DeleteTuple { tuple }).await {
        Ok(()) => Ok(true),
        Err(kameo::error::SendError::HandlerError(PermissionError::TupleNotFound)) => Ok(false),
        Err(error) => Err(internal_err(format!(
            "permission actor failed deleting channel parent tuple: {error}"
        ))),
    }
}

fn channel_affiliation_relation(affiliation: Affiliation) -> Option<&'static str> {
    match affiliation {
        Affiliation::Owner => Some("owner"),
        Affiliation::Admin => Some("admin"),
        Affiliation::Member => Some("member"),
        Affiliation::Outcast => Some("outcast"),
        Affiliation::None => None,
    }
}

pub(crate) async fn explicit_channel_affiliations_for_jids(
    state: &AppState,
    channel_id: &str,
    jids: impl IntoIterator<Item = BareJid>,
) -> Result<Vec<(BareJid, Affiliation)>, String> {
    let object = Object::new(ObjectType::Channel, channel_id);
    let mut seen = BTreeSet::new();
    let mut affiliations = Vec::new();
    for jid in jids {
        if !seen.insert(jid.clone()) {
            continue;
        }
        let subject = Subject::user(jid.to_string());
        let affiliation = if check_explicit_channel_permission(
            state,
            object.clone(),
            subject.clone(),
            Permission::Custom("outcast".into()),
        )
        .await?
        {
            Affiliation::Outcast
        } else if check_explicit_channel_permission(
            state,
            object.clone(),
            subject.clone(),
            Permission::Owner,
        )
        .await?
        {
            Affiliation::Owner
        } else if check_explicit_channel_permission(
            state,
            object.clone(),
            subject.clone(),
            Permission::Admin,
        )
        .await?
        {
            Affiliation::Admin
        } else if check_explicit_channel_permission(
            state,
            object.clone(),
            subject,
            Permission::Member,
        )
        .await?
        {
            Affiliation::Member
        } else {
            Affiliation::None
        };
        affiliations.push((jid, affiliation));
    }
    Ok(affiliations)
}

async fn check_explicit_channel_permission(
    state: &AppState,
    object: Object,
    subject: Subject,
    permission: Permission,
) -> Result<bool, String> {
    state
        .permission_actor
        .ask(CheckPermission {
            object,
            subject,
            permission,
        })
        .await
        .map(|response| response.allowed)
        .map_err(|error| format!("permission actor failed checking channel affiliation: {error}"))
}

async fn persist_channel_affiliation(
    state: &AppState,
    channel_id: &str,
    jid: &BareJid,
    affiliation: Affiliation,
) -> Result<(), AdminErr> {
    let object = Object::new(ObjectType::Channel, channel_id);
    let subject = Subject::user(jid.to_string());

    for relation in ["owner", "admin", "member", "outcast"] {
        let tuple = Tuple::new(object.clone(), Relation::new(relation), subject.clone());
        match state.permission_actor.ask(DeleteTuple { tuple }).await {
            Ok(()) | Err(kameo::error::SendError::HandlerError(PermissionError::TupleNotFound)) => {
            }
            Err(error) => {
                return Err(internal_err(format!(
                    "permission actor failed deleting channel affiliation tuple: {error}"
                )));
            }
        }
    }

    let Some(relation) = channel_affiliation_relation(affiliation) else {
        return Ok(());
    };
    let tuple = Tuple::new(object, Relation::new(relation), subject);
    match state.permission_actor.ask(WriteTuple { tuple }).await {
        Ok(())
        | Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => Ok(()),
        Err(error) => Err(internal_err(format!(
            "permission actor failed writing channel affiliation tuple: {error}"
        ))),
    }
}

async fn persist_channel_affiliation_or_restore(
    state: &AppState,
    channel_id: &str,
    jid: &BareJid,
    previous_affiliation: Affiliation,
    next_affiliation: Affiliation,
) -> Result<(), AdminErr> {
    match persist_channel_affiliation(state, channel_id, jid, next_affiliation).await {
        Ok(()) => Ok(()),
        Err(error) => {
            if persist_channel_affiliation(state, channel_id, jid, previous_affiliation)
                .await
                .is_err()
            {
                tracing::warn!(
                    channel_id = channel_id,
                    jid = %jid,
                    "failed to restore channel affiliation after persistence error"
                );
            }
            Err(error)
        }
    }
}

struct RemovedChannelBookmark {
    node: String,
    item: Option<PubSubItem>,
    fallback_item: Option<PubSubItem>,
    parent_tuple_deleted: bool,
}

async fn retract_duplicate_channel_bookmarks(
    state: &AppState,
    keep_node: &str,
    channel_id: &str,
    channel_jid: &BareJid,
) -> Result<(), AdminErr> {
    let item_id = channel_jid.to_string();
    let nodes = state
        .pubsub_storage
        .list_node_names_for_item(&state.spaces_jid, &item_id)
        .await
        .map_err(|e| internal_err(format!("pubsub list channel bookmark nodes failed: {e}")))?;
    let mut removed = Vec::new();
    for node in nodes.into_iter().filter(|node| node != keep_node) {
        let snapshot = match snapshot_channel_bookmark(state, &node, &item_id).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                restore_removed_channel_bookmarks(state, &removed, &item_id, Some(channel_id))
                    .await;
                return Err(error);
            }
        };
        let fallback_item = if snapshot.is_none() {
            match rollback_channel_bookmark_item(state, channel_jid, channel_id).await {
                Some(item) => Some(item),
                None => {
                    restore_removed_channel_bookmarks(state, &removed, &item_id, Some(channel_id))
                        .await;
                    return Err(internal_err(format!(
                        "could not snapshot missing Spaces bookmark for linked channel {channel_jid}"
                    )));
                }
            }
        } else {
            None
        };
        match retract_channel_bookmark_and_parent(state, &node, &item_id, Some(channel_id)).await {
            Ok(parent_tuple_deleted) => removed.push(RemovedChannelBookmark {
                node,
                item: snapshot,
                fallback_item,
                parent_tuple_deleted,
            }),
            Err(error) => {
                restore_removed_channel_bookmarks(state, &removed, &item_id, Some(channel_id))
                    .await;
                return Err(error);
            }
        }
    }
    Ok(())
}

fn channel_type_from_config(config: &RoomConfig) -> &'static str {
    if config.group_dm {
        return waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM;
    }
    if config.forum {
        "forum"
    } else if config.moderated {
        "announcement"
    } else {
        "text"
    }
}

fn channel_type_from_catalog_or_config(
    existing: Option<&XmppChannelRecord>,
    config: &RoomConfig,
) -> ChannelType {
    existing
        .and_then(|row| ChannelType::parse(&row.channel_type))
        .unwrap_or_else(|| {
            ChannelType::parse(channel_type_from_config(config)).unwrap_or(ChannelType::Text)
        })
}

fn apply_channel_type(config: &mut RoomConfig, channel_type: ChannelType) {
    config.group_dm = matches!(channel_type, ChannelType::GroupDm);
    config.forum = matches!(channel_type, ChannelType::Forum);
    config.moderated = matches!(channel_type, ChannelType::Announcement);
    if config.group_dm {
        config.members_only = true;
    }
}

async fn upsert_channel_catalog(
    state: &AppState,
    channel_id: &str,
    config: &RoomConfig,
    channel_type: ChannelType,
) -> Result<(), AdminErr> {
    let db_actor = state.db_pool.global_actor().clone();
    let existing = get_xmpp_channel(db_actor.clone(), channel_id)
        .await
        .map_err(|error| internal_err(format!("channel catalog lookup failed: {error}")))?;
    let record = XmppChannelUpsert {
        id: channel_id.to_string(),
        name: config.name.clone(),
        description: config.description.clone(),
        channel_type: channel_type.as_str().to_string(),
        position: existing.as_ref().map(|row| row.position).unwrap_or(0),
        is_default: existing.as_ref().map(|row| row.is_default).unwrap_or(false),
        pin_permission: config.pin_permission,
        members_only: config.members_only,
        public_room: config.public_room,
    };
    upsert_xmpp_channel(db_actor, &record)
        .await
        .map_err(|error| internal_err(format!("channel catalog upsert failed: {error}")))
}

async fn upsert_group_dm_catalog(
    state: &AppState,
    group_dm_id: &str,
    config: &RoomConfig,
) -> Result<(), AdminErr> {
    let record = XmppChannelUpsert {
        id: group_dm_id.to_string(),
        name: config.name.clone(),
        description: config.description.clone(),
        channel_type: waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.to_string(),
        position: 0,
        is_default: false,
        pin_permission: config.pin_permission,
        members_only: config.members_only,
        public_room: config.public_room,
    };
    upsert_xmpp_channel(state.db_pool.global_actor().clone(), &record)
        .await
        .map_err(|error| internal_err(format!("group-DM catalog upsert failed: {error}")))
}

async fn restore_channel_catalog_record(state: &AppState, record: &XmppChannelRecord) {
    let db_actor = state.db_pool.global_actor().clone();
    let upsert = XmppChannelUpsert {
        id: record.id.clone(),
        name: record.name.clone(),
        description: record.description.clone(),
        channel_type: record.channel_type.clone(),
        position: record.position,
        is_default: record.is_default,
        pin_permission: record.pin_permission,
        members_only: record.members_only,
        public_room: record.public_room,
    };
    if let Err(error) = upsert_xmpp_channel(db_actor, &upsert).await {
        tracing::warn!(
            channel = %record.id,
            error = %error,
            "failed to restore channel catalog row during rollback",
        );
    }
}

async fn restore_channel_catalog_snapshot(
    state: &AppState,
    channel_id: &str,
    snapshot: Option<&XmppChannelRecord>,
) {
    if let Some(record) = snapshot {
        restore_channel_catalog_record(state, record).await;
    } else if let Err(error) =
        delete_xmpp_channel(state.db_pool.global_actor().clone(), channel_id).await
    {
        tracing::warn!(
            channel = %channel_id,
            error = %error,
            "failed to delete operation-created channel catalog row during rollback",
        );
    }
}

async fn rollback_channel_bookmark_item(
    state: &AppState,
    room_jid: &BareJid,
    channel_id: &str,
) -> Option<PubSubItem> {
    let actor = match state
        .room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                error = %error,
                room = %room_jid,
                "channels rollback could not snapshot room for missing Spaces bookmark",
            );
            return None;
        }
    };
    let config = match actor.ask(GetConfig).await {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(
                error = %error,
                room = %room_jid,
                "channels rollback could not snapshot room config for missing Spaces bookmark",
            );
            return None;
        }
    };
    let catalog_snapshot =
        match get_xmpp_channel(state.db_pool.global_actor().clone(), channel_id).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room = %room_jid,
                    channel_id,
                    "channels rollback could not load channel catalog for missing Spaces bookmark",
                );
                return None;
            }
        };
    let channel_type = channel_type_from_catalog_or_config(catalog_snapshot.as_ref(), &config);
    waddle_xmpp::xep::build_channel_item(
        &ChannelInfo {
            id: channel_id.to_string(),
            name: config.name,
            channel_type: channel_type.as_str().to_string(),
        },
        &state.muc_domain.to_string(),
    )
    .map_err(|error| {
        tracing::warn!(
            error = %error,
            room = %room_jid,
            "channels rollback could not build missing Spaces bookmark",
        );
    })
    .ok()
}

async fn run_create(state: &AppState, args: &ChannelsCreateArgs) -> Result<ChannelRef, AdminErr> {
    let localpart = mint_channel_localpart(&args.name);
    let muc_domain = state.muc_domain.to_string();
    let channel_jid: BareJid = format!("{localpart}@{muc_domain}")
        .parse()
        .map_err(|e| internal_err(format!("constructed channel JID is invalid: {e}")))?;
    let space_ref = match args.space_jid.as_ref() {
        Some(space_jid) => {
            Some(existing_space_node(state, space_jid, args.space_node.as_ref()).await?)
        }
        None if args.space_node.is_some() => {
            return Err(bad_request("space_node requires space_jid"));
        }
        None => None,
    };

    let members_only = args.members_only.unwrap_or(!args.is_public);
    let mut config = RoomConfig {
        name: args.name.clone(),
        description: args.topic.clone(),
        persistent: true,
        members_only,
        public_room: args.is_public,
        ..RoomConfig::default()
    };
    apply_channel_type(&mut config, args.channel_type);
    config.max_occupants = 0;
    config.enable_logging = true;

    state
        .room_registry
        .ask(CreateRoom {
            room_jid: channel_jid.clone(),
            waddle_id: "admin-v2".to_string(),
            channel_id: localpart.clone(),
            config: config.clone(),
        })
        .await
        .map_err(send_err("room_registry ask CreateRoom"))?;

    if let Err(error) = upsert_channel_catalog(state, &localpart, &config, args.channel_type).await
    {
        destroy_room_for_rollback(state, &channel_jid, "channel catalog creation failed").await?;
        return Err(error);
    }

    // Persist the channel↔space link when the caller supplied a
    // `space_jid`. The link drives `channels:list` filtering and
    // `spaces:delete` cascade behavior; the room itself lives in the
    // MUC registry independent of any space. The matching XEP-0503
    // bookmark is the public discovery source for native clients.
    if let Some((node, space_jid)) = space_ref.as_ref() {
        let parent_tuple_created = match publish_channel_space_bookmark(
            state,
            node,
            &localpart,
            &args.name,
            args.channel_type.as_str(),
        )
        .await
        {
            Ok(created) => created,
            Err(error) => {
                let _ = delete_xmpp_channel(state.db_pool.global_actor().clone(), &localpart).await;
                destroy_room_for_rollback(
                    state,
                    &channel_jid,
                    "channel-space bookmark creation failed",
                )
                .await?;
                return Err(error);
            }
        };
        if let Err(error) = state
            .channel_space_link_store
            .set(&ChannelSpaceLink {
                channel_jid: channel_jid.clone(),
                space_jid: space_jid.clone(),
                space_node: node.clone(),
                created_at: now_unix_seconds(),
            })
            .await
        {
            let tuple_ready_for_retract = if parent_tuple_created {
                match delete_channel_parent_tuple(state, &localpart, node).await {
                    Ok(_) => true,
                    Err(_delete_error) => {
                        tracing::warn!(
                            node = %node,
                            channel = %channel_jid,
                            "channels:create failed to persist channel-space link and could not delete operation-created parent tuple; preserving room and Spaces bookmark",
                        );
                        false
                    }
                }
            } else {
                true
            };
            if tuple_ready_for_retract {
                match state
                    .pubsub_storage
                    .retract_item(&state.spaces_jid, node, &channel_jid.to_string())
                    .await
                {
                    Ok(_) => {
                        let _ =
                            delete_xmpp_channel(state.db_pool.global_actor().clone(), &localpart)
                                .await;
                        destroy_room_for_rollback(
                            state,
                            &channel_jid,
                            "channel-space link creation failed",
                        )
                        .await?;
                    }
                    Err(retract_error) => {
                        if parent_tuple_created {
                            let _ = write_channel_parent_tuple(state, &localpart, node).await;
                        }
                        tracing::warn!(
                            node = %node,
                            channel = %channel_jid,
                            error = %retract_error,
                            "channels:create failed to persist channel-space link and could not retract Spaces bookmark; preserving room and parent tuple for consistency",
                        );
                    }
                }
            }
            return Err(map_link_err(error));
        }
    }

    Ok(ChannelRef {
        channel_jid,
        name: args.name.clone(),
        topic: args.topic.clone(),
        channel_type: args.channel_type,
        is_public: args.is_public,
        members_only: config.members_only,
    })
}

async fn run_group_dm_create(
    state: &AppState,
    creator_jid: &BareJid,
    args: &GroupDmCreateArgs,
) -> Result<GroupDmRef, AdminErr> {
    validate_group_dm_members(state, creator_jid, &args.member_jids).await?;
    let localpart = format!("group-dm-{}", mint_channel_localpart(&args.name));
    let muc_domain = state.muc_domain.to_string();
    let room_jid: BareJid = format!("{localpart}@{muc_domain}")
        .parse()
        .map_err(|e| internal_err(format!("constructed group-DM JID is invalid: {e}")))?;
    let config = RoomConfig {
        name: args.name.clone(),
        description: None,
        persistent: true,
        members_only: true,
        public_room: false,
        moderated: false,
        enable_logging: true,
        group_dm: true,
        pin_permission: PinPermission::Anyone,
        federated_affiliation_config: FederatedAffiliationConfig::open_none(),
        ..RoomConfig::default()
    };

    let mut members = args.member_jids.clone();
    members.push(creator_jid.clone());
    members.sort();
    members.dedup();
    let initial_affiliations = members
        .iter()
        .cloned()
        .map(|jid| waddle_xmpp::muc::DurableAffiliationEntry::new(jid, Some(Affiliation::Member)))
        .collect();

    state
        .room_registry
        .ask(CreateRoomWithInitialAffiliations {
            room_jid: room_jid.clone(),
            waddle_id: waddle_xmpp::muc::durable::WaddleId::new(
                waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.to_string(),
            ),
            channel_id: waddle_xmpp::muc::durable::ChannelId::new(localpart.clone()),
            config: config.clone(),
            initial_affiliations,
        })
        .await
        .map_err(send_err(
            "room_registry ask CreateRoomWithInitialAffiliations",
        ))?;

    if let Err(error) = upsert_group_dm_catalog(state, &localpart, &config).await {
        destroy_room_for_rollback(state, &room_jid, "group-DM catalog creation failed").await?;
        return Err(error);
    }

    let mut persisted_members: Vec<BareJid> = Vec::with_capacity(members.len());
    for member_jid in members {
        if let Err(error) = persist_group_dm_member_tuple(state, &localpart, &member_jid).await {
            rollback_group_dm_create(state, &localpart, &room_jid, &persisted_members).await?;
            return Err(Box::new(CommandResult::Error(error)));
        }
        persisted_members.push(member_jid.clone());
        if let Err(error) =
            publish_group_dm_bookmark(state, &member_jid, &room_jid, Some(&args.name)).await
        {
            rollback_group_dm_create(state, &localpart, &room_jid, &persisted_members).await?;
            return Err(error);
        }
    }

    Ok(GroupDmRef {
        room_jid,
        name: args.name.clone(),
        is_public: false,
        members_only: true,
        persistent: true,
    })
}

async fn run_group_dm_leave(
    state: &AppState,
    pending_local_muc_departures: &Arc<crate::server::routes::websocket::PendingLocalMucDepartures>,
    connections: &ConnectionRegistry,
    user_registry: &ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    sm_sessions: &InMemorySmSessionRegistry,
    caller_full_jid: &FullJid,
    args: &GroupDmLeaveArgs,
) -> Result<GroupDmLeaveResult, AdminErr> {
    let channel_id = waddle_xmpp::parse_managed_room_jid(&args.room_jid).ok_or_else(|| {
        Box::new(CommandResult::Error(XmppError::bad_request(Some(
            "room_jid must be a managed group-DM room JID".to_string(),
        ))))
    })?;
    let record = get_xmpp_channel(state.db_pool.global_actor().clone(), &channel_id)
        .await
        .map_err(|error| internal_err(format!("failed to load group-DM channel: {error}")))?
        .ok_or_else(|| {
            Box::new(CommandResult::Error(XmppError::item_not_found(Some(
                format!("no group DM '{}'", args.room_jid),
            ))))
        })?;
    if record.channel_type != waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM {
        return Err(Box::new(CommandResult::Error(XmppError::bad_request(
            Some("room_jid is not a group DM".to_string()),
        ))));
    }

    let caller_bare = caller_full_jid.to_bare();
    let mut actor = state
        .room_registry
        .ask(GetRoom {
            room_jid: args.room_jid.clone(),
        })
        .await
        .map_err(send_err("room_registry ask GetRoom"))?
        .ok_or_else(|| {
            Box::new(CommandResult::Error(XmppError::item_not_found(Some(
                format!("no group DM '{}'", args.room_jid),
            ))))
        })?;

    let pre_leave_affiliation = actor
        .ask(GetAffiliation {
            jid: caller_bare.clone(),
        })
        .await
        .map_err(send_err("room actor GetAffiliation"))?;
    let left = pre_leave_affiliation >= Affiliation::Member;
    if !left {
        return Ok(GroupDmLeaveResult {
            room_jid: args.room_jid.clone(),
            left,
        });
    }

    let live_resources =
        waddle_xmpp::registry::get_resources_for_user(user_registry, &caller_bare).await;
    let live_resource_set = live_resources.iter().cloned().collect::<BTreeSet<_>>();
    let mut resources = live_resources;
    match sm_sessions.detached_resources_for_user(&caller_bare).await {
        Ok(detached_resources) => resources.extend(detached_resources),
        Err(error) => {
            return Err(internal_err(format!(
                "failed to list detached group-DM leave resources: {error}"
            )));
        }
    }
    if !resources.iter().any(|resource| resource == caller_full_jid) {
        resources.push(caller_full_jid.clone());
    }
    resources.sort();
    resources.dedup();
    delete_group_dm_member_tuple(state, &channel_id, &caller_bare)
        .await
        .map_err(|error| Box::new(CommandResult::Error(error)))?;
    if let Err(error) = retract_group_dm_bookmark(state, &caller_bare, &args.room_jid).await {
        let _ = persist_group_dm_member_tuple(state, &channel_id, &caller_bare).await;
        return Err(error);
    }
    if let Err(error) = actor
        .ask(ChangeAffiliation {
            jid: caller_bare.clone(),
            affiliation: Affiliation::None,
        })
        .await
    {
        let should_restore_membership = match &error {
            kameo::error::SendError::HandlerError(
                waddle_xmpp::muc::room_actor::AffiliationMutationError::CommitOutcomeUnknown,
            ) => {
                match reconcile_ambiguous_group_dm_leave(
                    state,
                    &args.room_jid,
                    &channel_id,
                    &record,
                    &actor,
                    &caller_bare,
                )
                .await
                {
                    Some(recovered) => {
                        // The successor carries the transplanted roster, so
                        // the ordinary occupancy-leave loop below removes the
                        // leaver's sessions with the full retained-retry
                        // machinery — no synthesized effects.
                        actor = recovered;
                        false
                    }
                    None => true,
                }
            }
            _ => true,
        };
        if should_restore_membership {
            let _ = persist_group_dm_member_tuple(state, &channel_id, &caller_bare).await;
            let _ = publish_group_dm_bookmark(
                state,
                &caller_bare,
                &args.room_jid,
                group_dm_shared_name(&record.name),
            )
            .await;
        }
        if should_restore_membership {
            return Err(send_err("room actor ChangeAffiliation")(error));
        }
    }
    {
        for resource in resources {
            let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
            let in_flight = crate::server::routes::websocket::LocalDepartureItem::InFlight {
                room: args.room_jid.clone(),
                jid: resource.clone(),
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Administrative,
                selector: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                attempt,
                notified: HashSet::new(),
            };
            pending_local_muc_departures.record_in_flight(in_flight.clone());
            let _in_flight_lease = crate::server::routes::websocket::InFlightLease::hold(
                Arc::clone(pending_local_muc_departures),
                in_flight.clone(),
            );
            match crate::server::routes::websocket::ask_leave_bounded(
                &actor,
                LeaveByRealJid {
                    sender_jid: resource.clone(),
                    cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Administrative,
                    session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                    attempt,
                    origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                },
            )
            .await
            {
                Ok(waddle_xmpp::muc::room_actor::LeaveDisposition::Left(outcome)) => {
                    broadcast_group_dm_leave(
                        state,
                        connections,
                        &resource,
                        live_resource_set.contains(&resource),
                        &GroupDmLeaveEffect::from(outcome.as_ref()),
                    );
                    crate::server::routes::websocket::acknowledge_in_flight(
                        pending_local_muc_departures,
                        &actor,
                        &in_flight,
                        outcome.acknowledge,
                    );
                }
                Ok(waddle_xmpp::muc::room_actor::LeaveDisposition::Suppressed {
                    attempt: acknowledge,
                    ..
                }) => {
                    crate::server::routes::websocket::acknowledge_in_flight(
                        pending_local_muc_departures,
                        &actor,
                        &in_flight,
                        acknowledge,
                    );
                }
                Ok(
                    waddle_xmpp::muc::room_actor::LeaveDisposition::NotOccupant
                    | waddle_xmpp::muc::room_actor::LeaveDisposition::Superseded,
                ) => {
                    pending_local_muc_departures.complete_in_flight(&in_flight);
                }
                Ok(waddle_xmpp::muc::room_actor::LeaveDisposition::Deferred { .. }) => {
                    pending_local_muc_departures.complete_in_flight(&in_flight);
                    pending_local_muc_departures.record(
                        crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                            room: args.room_jid.clone(),
                            jid: resource,
                            cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Administrative,
                            selector: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                            attempt,
                            notified: HashSet::new(),
                        },
                    );
                }
                Err(crate::server::routes::websocket::LeaveAskFailure::Timeout) => {
                    // A timeout proves nothing about the actor's seal: retain
                    // the administrative departure itself for the janitor.
                    pending_local_muc_departures.complete_in_flight(&in_flight);
                    pending_local_muc_departures.record(
                        crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                            room: args.room_jid.clone(),
                            jid: resource,
                            cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Administrative,
                            selector: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                            attempt,
                            notified: HashSet::new(),
                        },
                    );
                }
                Err(crate::server::routes::websocket::LeaveAskFailure::Handler(
                    waddle_xmpp::muc::room_actor::RoomActorError::RoomSealed,
                )) => {
                    pending_local_muc_departures.complete_in_flight(&in_flight);
                    pending_local_muc_departures.record(
                        crate::server::routes::websocket::LocalDepartureItem::ConfirmRetired {
                            room: args.room_jid.clone(),
                            jid: resource,
                            actor: actor.id(),
                            cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Administrative,
                            selector: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                            attempt,
                            notified: HashSet::new(),
                        },
                    );
                }
                Err(_) => {
                    pending_local_muc_departures.complete_in_flight(&in_flight);
                    pending_local_muc_departures.record(
                        crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                            room: args.room_jid.clone(),
                            jid: resource,
                            cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Administrative,
                            selector: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                            attempt,
                            notified: HashSet::new(),
                        },
                    );
                }
            }
        }
    }

    Ok(GroupDmLeaveResult {
        room_jid: args.room_jid.clone(),
        left,
    })
}

async fn run_group_dm_rename(
    state: &AppState,
    websocket_state: &WebSocketState,
    connections: &ConnectionRegistry,
    caller_full_jid: &FullJid,
    args: &GroupDmRenameArgs,
) -> Result<GroupDmRenameResult, AdminErr> {
    let channel_id = waddle_xmpp::parse_managed_room_jid(&args.room_jid).ok_or_else(|| {
        bad_request(format!(
            "room_jid '{}' is not a managed group-DM room",
            args.room_jid
        ))
    })?;
    let record = get_xmpp_channel(state.db_pool.global_actor().clone(), &channel_id)
        .await
        .map_err(|error| internal_err(format!("channel lookup failed: {error}")))?
        .ok_or_else(|| {
            Box::new(CommandResult::Error(XmppError::item_not_found(Some(
                format!("no group DM '{}'", args.room_jid),
            ))))
        })?;
    if record.channel_type != waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM {
        return Err(bad_request(format!(
            "room_jid '{}' is not a group DM",
            args.room_jid
        )));
    }
    let _config_guard = acquire_room_config_lock(&args.room_jid).await;
    let mut actor = state
        .room_registry
        .ask(GetOrCreateRoom {
            room_jid: args.room_jid.clone(),
            waddle_id: waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.to_string(),
            channel_id: channel_id.clone(),
            config: group_dm_record_config(&record),
        })
        .await
        .map_err(send_err("room_registry ask GetOrCreateRoom"))?
        .actor_ref;
    hydrate_group_dm_member_affiliations(state, &actor, &channel_id).await?;
    let snapshot = actor
        .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
        .await
        .map_err(send_err("room actor GetSnapshot"))?;
    if !snapshot.room.config.group_dm {
        return Err(bad_request(format!(
            "room_jid '{}' is not a group DM",
            args.room_jid
        )));
    }
    let previous_config = snapshot.room.config.clone();
    let mut config = previous_config.clone();
    config.name = args.name.clone().unwrap_or_default();
    let intended_config = config.normalized();
    let mut reconciled_broadcast_snapshot: Option<waddle_xmpp::muc::room_actor::RoomSnapshot> =
        None;
    let mut cancelled_commit_recovery = CancelledConfigAskRecoveryGuard::arm_only(
        websocket_state,
        &actor,
        &args.room_jid,
        &intended_config,
        snapshot.config_revision.saturating_add(1),
    );
    let (updated_snapshot, config_reservation) = match actor
        .ask(waddle_xmpp::muc::room_actor::UpdateGroupDmConfigByMember {
            config: intended_config.clone(),
            sender_jid: caller_full_jid.clone(),
        })
        .await
    {
        Ok(applied) => {
            cancelled_commit_recovery.disarm();
            (applied.snapshot, applied.reservation)
        }
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::UpdateGroupDmConfigByMemberError::CommitOutcomeUnknown,
        )) => {
            cancelled_commit_recovery.disarm();
            let Some(recovered) = reconcile_ambiguous_group_dm_rename_commit(
                state,
                websocket_state,
                &args.room_jid,
                &channel_id,
                &record,
                &actor,
                &intended_config,
            )
            .await?
            else {
                return Err(unavailable(
                    "This room's rename outcome is being reconciled; please retry.",
                ));
            };
            actor = recovered.actor;
            reconciled_broadcast_snapshot = Some(snapshot.clone());
            (recovered.snapshot, recovered.reservation)
        }
        Err(error) => {
            cancelled_commit_recovery.disarm();
            return Err(group_dm_rename_update_error(state, &args.room_jid, &actor, error).await);
        }
    };
    // A committed config row must be registered with the producer supervisor
    // before any later await.  The rename path has several catalog and
    // bookmark awaits below, and a cancelled handler must not strand its
    // staged reservation while this node remains a live producer.
    let mut config_reservation =
        CommittedAdminConfigReservationGuard::new(websocket_state, config_reservation);
    let expected_revision = updated_snapshot.config_revision;
    if reconciled_broadcast_snapshot.is_none()
        && find_occupant_for_full_jid(&updated_snapshot, caller_full_jid).is_none()
    {
        return Err(internal_err(
            "group-DM rename updated without sender occupant",
        ));
    }
    if !group_dm_room_config_revision_is_current(&actor, expected_revision).await {
        return Err(Box::new(CommandResult::Error(XmppError::conflict(Some(
            "group-DM rename was superseded by a newer update".to_string(),
        )))));
    }
    if let Err(error) = upsert_group_dm_catalog(state, &channel_id, &intended_config).await {
        let _ = rollback_room_config_or_arm(
            websocket_state,
            &actor,
            expected_revision,
            previous_config,
            config_reservation.reservation().cloned(),
        )
        .await;
        return Err(error);
    }

    let members = match actor
        .ask(ListAffiliations {
            filter: Some(Affiliation::Member),
        })
        .await
    {
        Ok(members) => members,
        Err(error) => {
            if rollback_room_config_or_arm(
                websocket_state,
                &actor,
                expected_revision,
                previous_config.clone(),
                config_reservation.reservation().cloned(),
            )
            .await
            {
                let _ = upsert_group_dm_catalog(state, &channel_id, &previous_config).await;
            }
            return Err(send_err("room actor ListAffiliations")(error));
        }
    };
    let mut updated_bookmark_members = Vec::with_capacity(members.len());
    for member in &members {
        if !group_dm_room_config_revision_is_current(&actor, expected_revision).await {
            repair_group_dm_rename_side_effects_after_conflict(
                state,
                websocket_state,
                &actor,
                GroupDmRenameRepair {
                    channel_id: &channel_id,
                    room_jid: &args.room_jid,
                    previous_config: &previous_config,
                    expected_revision,
                    config_reservation: config_reservation.reservation().cloned(),
                    updated_members: &updated_bookmark_members,
                },
            )
            .await;
            return Err(Box::new(CommandResult::Error(XmppError::conflict(Some(
                "group-DM rename was superseded by a newer update".to_string(),
            )))));
        }
        if let Err(error) = publish_group_dm_bookmark(
            state,
            &member.jid,
            &args.room_jid,
            group_dm_shared_name(&intended_config.name),
        )
        .await
        {
            if rollback_room_config_or_arm(
                websocket_state,
                &actor,
                expected_revision,
                previous_config.clone(),
                config_reservation.reservation().cloned(),
            )
            .await
            {
                let _ = upsert_group_dm_catalog(state, &channel_id, &previous_config).await;
                restore_group_dm_bookmarks(
                    state,
                    &args.room_jid,
                    group_dm_shared_name(&previous_config.name),
                    &updated_bookmark_members,
                )
                .await;
            }
            return Err(error);
        }
        updated_bookmark_members.push(member.jid.clone());
    }

    if !group_dm_room_config_revision_is_current(&actor, expected_revision).await {
        repair_group_dm_rename_side_effects_after_conflict(
            state,
            websocket_state,
            &actor,
            GroupDmRenameRepair {
                channel_id: &channel_id,
                room_jid: &args.room_jid,
                previous_config: &previous_config,
                expected_revision,
                config_reservation: config_reservation.reservation().cloned(),
                updated_members: &updated_bookmark_members,
            },
        )
        .await;
        return Err(Box::new(CommandResult::Error(XmppError::conflict(Some(
            "group-DM rename was superseded by a newer update".to_string(),
        )))));
    }
    if let Some(reservation) = config_reservation.reservation() {
        arm_config_effect_reservation(websocket_state, reservation);
    } else if let Some(broadcast_snapshot) = reconciled_broadcast_snapshot.as_ref() {
        broadcast_group_dm_config_change(connections, &args.room_jid, broadcast_snapshot);
    } else {
        let broadcast_snapshot = actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .map_err(send_err("room actor GetSnapshot"))?;
        broadcast_group_dm_config_change(connections, &args.room_jid, &broadcast_snapshot);
    }
    config_reservation.clear();

    Ok(GroupDmRenameResult {
        room_jid: args.room_jid.clone(),
        name: args.name.clone(),
    })
}

fn room_config_lock(room_jid: &BareJid) -> Arc<Semaphore> {
    static LOCKS: OnceLock<RoomConfigLockMap> = OnceLock::new();
    // Retained process-local locks trade a small per-written-room map for no weak-lock race.
    let locks = LOCKS.get_or_init(RoomConfigLockMap::new);
    if let Some(existing) = locks.get(room_jid) {
        return Arc::clone(existing.value());
    }
    let lock = Arc::new(Semaphore::new(1));
    match locks.entry(room_jid.clone()) {
        dashmap::mapref::entry::Entry::Occupied(existing) => Arc::clone(existing.get()),
        dashmap::mapref::entry::Entry::Vacant(vacant) => {
            vacant.insert(Arc::clone(&lock));
            lock
        }
    }
}

pub(crate) async fn acquire_room_config_lock(room_jid: &BareJid) -> OwnedSemaphorePermit {
    let lock = room_config_lock(room_jid);
    lock.acquire_owned()
        .await
        .expect("room config lock semaphore is never closed")
}

async fn hydrate_group_dm_member_affiliations(
    state: &AppState,
    actor: &ActorRef<RoomActor>,
    group_dm_id: &str,
) -> Result<(), AdminErr> {
    let members = list_durable_group_dm_members(state, group_dm_id).await?;
    for member_jid in members {
        actor
            .ask(ChangeAffiliation {
                jid: member_jid,
                affiliation: Affiliation::Member,
            })
            .await
            .map_err(send_err("room actor ChangeAffiliation"))?;
    }
    Ok(())
}

async fn list_durable_group_dm_members(
    state: &AppState,
    group_dm_id: &str,
) -> Result<Vec<BareJid>, AdminErr> {
    let rows = state
        .db_pool
        .global_actor()
        .ask(DbQuery {
            sql: r#"
                SELECT DISTINCT subject_id
                FROM permission_tuples
                WHERE object_type = 'channel'
                  AND object_id = ?
                  AND relation = 'member'
                  AND subject_type = 'user'
                  AND subject_relation IS NULL
                ORDER BY subject_id ASC
            "#
            .to_string(),
            params: vec![group_dm_id.into()],
        })
        .await
        .map_err(|error| internal_err(format!("group-DM member lookup failed: {error}")))?;
    rows.into_iter()
        .map(|row| {
            row_value(&row, 0)
                .and_then(ValueExt::as_string)
                .map_err(|error| internal_err(format!("invalid group-DM member row: {error}")))
                .and_then(|jid| {
                    jid.parse::<BareJid>().map_err(|error| {
                        internal_err(format!("invalid durable group-DM member JID: {error}"))
                    })
                })
        })
        .collect()
}

fn find_occupant_for_full_jid<'a>(
    snapshot: &'a waddle_xmpp::muc::room_actor::RoomSnapshot,
    full_jid: &FullJid,
) -> Option<&'a waddle_xmpp::muc::Occupant> {
    snapshot.room.occupants.values().find(|occupant| {
        occupant.real_jid == *full_jid
            || snapshot
                .room
                .get_occupant_sessions(&occupant.nick)
                .iter()
                .any(|session| session == full_jid)
    })
}

pub(crate) struct GroupDmLeaveEffect {
    affiliation: Affiliation,
    leaving_room_jid: FullJid,
    remaining_occupants: Vec<FullJid>,
    removed_last_session: bool,
}

impl From<&waddle_xmpp::muc::room_actor::LeaveOutcome> for GroupDmLeaveEffect {
    fn from(outcome: &waddle_xmpp::muc::room_actor::LeaveOutcome) -> Self {
        Self {
            affiliation: outcome.affiliation,
            leaving_room_jid: outcome.leaving_room_jid.clone(),
            remaining_occupants: outcome.remaining_occupants.clone(),
            removed_last_session: outcome.removed_last_session,
        }
    }
}

pub(crate) fn broadcast_group_dm_leave(
    state: &AppState,
    connections: &ConnectionRegistry,
    leaving_real_jid: &FullJid,
    notify_self: bool,
    outcome: &GroupDmLeaveEffect,
) {
    let from_jid = outcome.leaving_room_jid.clone();
    let sender_bare = leaving_real_jid.to_bare();
    let identity = OccupantIdentity {
        bare_jid: &sender_bare,
        real_jid: Some(leaving_real_jid),
        secret: &state.occupant_id_secret,
    };
    if notify_self {
        let presence = waddle_xmpp::muc::build_leave_presence(
            &from_jid,
            leaving_real_jid,
            outcome.affiliation,
            waddle_xmpp::muc::MucPresenceStatus::new(true, false),
            &identity,
        );
        let _ = connections.try_send_to(leaving_real_jid, Stanza::Presence(presence));
    }
    if !outcome.removed_last_session {
        return;
    }
    for occupant_jid in &outcome.remaining_occupants {
        let presence = waddle_xmpp::muc::build_leave_presence(
            &from_jid,
            occupant_jid,
            outcome.affiliation,
            waddle_xmpp::muc::MucPresenceStatus::new(false, false),
            &identity,
        );
        let _ = connections.try_send_to(occupant_jid, Stanza::Presence(presence));
    }
}

fn broadcast_group_dm_config_change(
    connections: &ConnectionRegistry,
    room_jid: &BareJid,
    snapshot: &waddle_xmpp::muc::room_actor::RoomSnapshot,
) {
    for occupant in snapshot.room.occupants.values() {
        for recipient_jid in snapshot.room.get_occupant_sessions(&occupant.nick) {
            let message = build_group_dm_config_change_message(room_jid, &recipient_jid);
            let _ = connections.try_send_to(&recipient_jid, Stanza::Message(message));
        }
    }
}

fn build_group_dm_config_change_message(room_jid: &BareJid, to_jid: &FullJid) -> Message {
    let status = Element::builder("status", NS_MUC_USER)
        .attr(minidom::rxml::xml_ncname!("code").to_owned(), "104")
        .build();
    let x = Element::builder("x", NS_MUC_USER).append(status).build();
    let mut message = Message::new(Some(jid::Jid::from(to_jid.clone())));
    message.from = Some(jid::Jid::from(room_jid.clone()));
    message.type_ = MessageType::Groupchat;
    message.payloads.push(x);
    message
}

async fn restore_group_dm_bookmarks(
    state: &AppState,
    room_jid: &BareJid,
    previous_name: Option<&str>,
    updated_members: &[BareJid],
) {
    for member_jid in updated_members {
        let _ = publish_group_dm_bookmark(state, member_jid, room_jid, previous_name).await;
    }
}

/// Everything a superseded group-DM rename needs to undo: the config to
/// restore (or arm), the staged effect reservation that guarded it, and the
/// bookmark audience to repair.
struct GroupDmRenameRepair<'a> {
    channel_id: &'a str,
    room_jid: &'a BareJid,
    previous_config: &'a RoomConfig,
    expected_revision: u64,
    config_reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    updated_members: &'a [BareJid],
}

async fn repair_group_dm_rename_side_effects_after_conflict(
    state: &AppState,
    websocket_state: &WebSocketState,
    actor: &ActorRef<RoomActor>,
    repair: GroupDmRenameRepair<'_>,
) {
    let GroupDmRenameRepair {
        channel_id,
        room_jid,
        previous_config,
        expected_revision,
        config_reservation,
        updated_members,
    } = repair;
    let target_config = if rollback_room_config_or_arm(
        websocket_state,
        actor,
        expected_revision,
        previous_config.clone(),
        config_reservation.clone(),
    )
    .await
    {
        previous_config.clone()
    } else {
        actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .map(|snapshot| snapshot.room.config)
            .unwrap_or_else(|_| previous_config.clone())
    };
    let _ = upsert_group_dm_catalog(state, channel_id, &target_config).await;
    restore_group_dm_bookmarks(
        state,
        room_jid,
        group_dm_shared_name(&target_config.name),
        updated_members,
    )
    .await;
}

async fn rollback_room_config_or_arm(
    websocket_state: &WebSocketState,
    actor: &ActorRef<RoomActor>,
    expected_revision: u64,
    previous_config: RoomConfig,
    reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
) -> bool {
    let rolled_back = actor
        .ask(waddle_xmpp::muc::room_actor::RollbackConfigIfRevision {
            expected_revision,
            config: previous_config,
            reservation: reservation.clone(),
        })
        .await
        .unwrap_or(false);
    if !rolled_back {
        if let Some(reservation) = reservation {
            arm_config_effect_reservation(websocket_state, &reservation);
        }
    }
    rolled_back
}

/// XEP-0045 §10.2.1: broadcast `<message><x xmlns='muc#user'><status/></x></message>`
/// config-change codes to every occupant session after an admin-path
/// room configuration change (#1265 item 15).
async fn broadcast_admin_config_change(
    connections: &ConnectionRegistry,
    actor: &ActorRef<RoomActor>,
    room_jid: &BareJid,
    previous: &RoomConfig,
    updated: &RoomConfig,
) {
    let status_codes = waddle_xmpp::muc::config_change_status_codes(previous, updated);
    if status_codes.is_empty() {
        return;
    }
    let Ok(snapshot) = actor
        .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
    else {
        tracing::warn!(room = %room_jid, "Failed to snapshot room for config-change broadcast");
        return;
    };
    for occupant in snapshot.room.occupants.values() {
        for recipient_jid in snapshot.room.get_occupant_sessions(&occupant.nick) {
            let message = waddle_xmpp::muc::build_config_change_message(
                room_jid,
                &recipient_jid,
                &status_codes,
            );
            let _ = connections
                .send_to(&recipient_jid, Stanza::Message(message))
                .await;
        }
    }
}

/// Config producer paths register staged reservations with the shared arm
/// supervisor. Admin command paths use its separate non-blocking exact-drain
/// nudge because their handler-window reservations are already eligible.
fn arm_config_effect_reservation(
    websocket_state: &WebSocketState,
    reservation: &waddle_xmpp::muc::RoomEffectReservation,
) {
    websocket_state
        .deps
        .protocol
        .room_effect_arm_supervisor
        .arm(reservation.clone());
}

#[derive(Clone)]
enum CancelledConfigAskRecoveryAction {
    ArmReservation {
        arm_supervisor: crate::room_effect_outbox::RoomEffectArmSupervisor,
    },
    DeferMembersOnly(PendingAdminMembersOnlyEnforcementSeed),
}

#[derive(Clone)]
struct PendingAdminMembersOnlyEnforcementSeed {
    affiliations: Vec<(BareJid, Affiliation)>,
    config_status_codes: Vec<waddle_xmpp::muc::MucConfigStatusCode>,
    room_jid: BareJid,
    sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
    arm_supervisor: crate::room_effect_outbox::RoomEffectArmSupervisor,
}

impl PendingAdminMembersOnlyEnforcementSeed {
    async fn run(
        self,
        actor: ActorRef<RoomActor>,
        fallback_reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    ) {
        PendingAdminMembersOnlyEnforcement {
            actor,
            affiliations: self.affiliations,
            fallback_reservation,
            config_status_codes: self.config_status_codes,
            room_jid: self.room_jid,
            sfu: self.sfu,
            arm_supervisor: self.arm_supervisor,
        }
        .run()
        .await;
    }
}

struct CancelledConfigAskRecoveryGuard {
    actor: ActorRef<RoomActor>,
    room_jid: BareJid,
    intended_config: RoomConfig,
    expected_revision: u64,
    outbox: Arc<crate::room_effect_outbox::RoomEffectOutboxStore>,
    action: CancelledConfigAskRecoveryAction,
    disarmed: bool,
}

impl CancelledConfigAskRecoveryGuard {
    fn arm_only(
        websocket_state: &WebSocketState,
        actor: &ActorRef<RoomActor>,
        room_jid: &BareJid,
        intended_config: &RoomConfig,
        expected_revision: u64,
    ) -> Self {
        Self {
            actor: actor.clone(),
            room_jid: room_jid.clone(),
            intended_config: intended_config.clone(),
            expected_revision,
            outbox: Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
            action: CancelledConfigAskRecoveryAction::ArmReservation {
                arm_supervisor: websocket_state
                    .deps
                    .protocol
                    .room_effect_arm_supervisor
                    .clone(),
            },
            disarmed: false,
        }
    }

    fn defer_members_only(
        websocket_state: &WebSocketState,
        actor: &ActorRef<RoomActor>,
        room_jid: &BareJid,
        intended_config: &RoomConfig,
        expected_revision: u64,
        seed: PendingAdminMembersOnlyEnforcementSeed,
    ) -> Self {
        Self {
            actor: actor.clone(),
            room_jid: room_jid.clone(),
            intended_config: intended_config.clone(),
            expected_revision,
            outbox: Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
            action: CancelledConfigAskRecoveryAction::DeferMembersOnly(seed),
            disarmed: false,
        }
    }

    fn disarm(&mut self) {
        self.disarmed = true;
    }

    async fn recover(self) {
        // The original actor may be busy past the ask bound: keep retrying
        // with backoff while it is alive (a committed reservation left inert
        // would block the lifecycle FIFO, and a members-only flip would stay
        // applied without its enforcement).
        let mut timeout_attempt = 0_i64;
        let snapshot = loop {
            match self
                .actor
                .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
                .mailbox_timeout(ADMIN_ROOM_ASK_TIMEOUT)
                .reply_timeout(ADMIN_ROOM_ASK_TIMEOUT)
                .await
            {
                Ok(snapshot) => break snapshot,
                Err(kameo::error::SendError::Timeout(_)) => {
                    timeout_attempt = timeout_attempt.saturating_add(1);
                    let backoff_ms = crate::room_effect_outbox::retry_delay_ms(timeout_attempt);
                    tracing::warn!(
                        room = %self.room_jid,
                        timeout_attempt,
                        backoff_ms,
                        "cancelled admin/group-DM config ask recovery snapshot timed out; retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms.max(0) as u64))
                        .await;
                }
                Err(error) => {
                    tracing::warn!(
                        room = %self.room_jid,
                        ?error,
                        "cancelled admin/group-DM config ask recovery could not snapshot the room"
                    );
                    return;
                }
            }
        };
        let exact_intended_config = snapshot.config_revision == self.expected_revision
            && snapshot.room.config == self.intended_config;
        let recovered_reservations: Vec<waddle_xmpp::muc::RoomEffectReservation> = if let Some(
            coordinates,
        ) =
            snapshot.config_durable_coordinates
        {
            // The durable config coordinates identify the LAST config commit
            // exactly. If a later config commit already superseded the
            // intended one, no arithmetic on lifecycle revisions can single
            // out the intended row (projections, subject and affiliation
            // commits also advance the head) — so arm EVERY still-inert row
            // of this lifecycle up to the latest config commit: each such
            // row describes a durably committed config (arm-by-default) and
            // an unarmed one would head-of-line-block the lifecycle FIFO.
            let exact = snapshot.config_revision == self.expected_revision;
            if !exact {
                tracing::warn!(
                    room = %self.room_jid,
                    config_revision = snapshot.config_revision,
                    expected_revision = self.expected_revision,
                    "cancelled admin/group-DM config ask recovery: the intended config was superseded; arming every committed inert row up to the latest config commit"
                );
            }
            // A transient outbox lookup failure must not strand committed inert rows
            // at the lifecycle FIFO head (nothing else arms live-origin rows): retry
            // with backoff before giving up.
            let mut lookup_attempt = 0_i64;
            loop {
                match if exact {
                    self.outbox
                        .staged_reservation_for(coordinates.lifecycle, coordinates.revision)
                        .await
                        .map(|reservation| reservation.into_iter().collect())
                } else {
                    self.outbox
                        .staged_reservations_up_to(coordinates.lifecycle, coordinates.revision)
                        .await
                } {
                    Ok(reservations) => break reservations,
                    // Retry until the lookup succeeds: the producing process is still
                    // alive, so no other supervisor will ever arm these rows; the backoff
                    // is capped at MAX_RETRY_DELAY_MS and this task is detached.
                    Err(error) => {
                        lookup_attempt += 1;
                        let backoff_ms = crate::room_effect_outbox::retry_delay_ms(lookup_attempt);
                        tracing::warn!(
                            room = %self.room_jid,
                            %error,
                            lookup_attempt,
                            backoff_ms,
                            "cancelled admin/group-DM config ask recovery: staged reservation lookup failed; retrying"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(
                            backoff_ms.max(0) as u64
                        ))
                        .await;
                    }
                }
            }
        } else {
            Vec::new()
        };
        match &self.action {
            CancelledConfigAskRecoveryAction::ArmReservation { arm_supervisor } => {
                for reservation in recovered_reservations {
                    arm_supervisor.clone().arm(reservation);
                }
            }
            CancelledConfigAskRecoveryAction::DeferMembersOnly(seed) => {
                if exact_intended_config {
                    seed.clone()
                        .run(
                            self.actor.clone(),
                            recovered_reservations.into_iter().next(),
                        )
                        .await;
                } else {
                    for reservation in recovered_reservations {
                        seed.arm_supervisor.clone().arm(reservation);
                    }
                }
            }
        }
    }
}

impl Drop for CancelledConfigAskRecoveryGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let recovery = Self {
            actor: self.actor.clone(),
            room_jid: self.room_jid.clone(),
            intended_config: self.intended_config.clone(),
            expected_revision: self.expected_revision,
            outbox: Arc::clone(&self.outbox),
            action: self.action.clone(),
            disarmed: true,
        };
        tokio::spawn(recovery.recover());
    }
}

struct CommittedAdminConfigReservationGuard<'a> {
    websocket_state: &'a WebSocketState,
    reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    pending_members_only_enforcement: Option<PendingAdminMembersOnlyEnforcement>,
}

struct PendingAdminMembersOnlyEnforcement {
    actor: ActorRef<RoomActor>,
    affiliations: Vec<(BareJid, Affiliation)>,
    fallback_reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    config_status_codes: Vec<waddle_xmpp::muc::MucConfigStatusCode>,
    room_jid: BareJid,
    sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
    arm_supervisor: crate::room_effect_outbox::RoomEffectArmSupervisor,
}

impl PendingAdminMembersOnlyEnforcement {
    async fn run(self) {
        let Self {
            actor,
            affiliations,
            fallback_reservation,
            config_status_codes,
            room_jid,
            sfu,
            arm_supervisor,
        } = self;
        let fallback_on_failure = fallback_reservation.clone();
        match actor
            .ask(EnforceMembersOnlyAffiliations {
                affiliations,
                fallback_reservation,
                config_status_codes,
            })
            .await
        {
            Ok(applied) => {
                if let Some(reservation) = applied.outbox_reservation.as_ref() {
                    arm_supervisor.arm(reservation.clone());
                }
                crate::server::routes::websocket::muc_call_sfu::converge_moderation_deltas_via_sfu(
                    sfu.as_ref(),
                    &room_jid,
                    &applied.removed_by_moderation,
                    &applied.voice_changes,
                );
            }
            Err(error) => {
                if let Some(reservation) = fallback_on_failure {
                    arm_supervisor.arm(reservation);
                }
                tracing::warn!(
                    room = %room_jid,
                    ?error,
                    "cancelled admin channel config recovery could not enforce members-only"
                );
            }
        }
    }
}

impl<'a> CommittedAdminConfigReservationGuard<'a> {
    fn new(
        websocket_state: &'a WebSocketState,
        reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    ) -> Self {
        Self {
            websocket_state,
            reservation,
            pending_members_only_enforcement: None,
        }
    }

    fn reservation(&self) -> Option<&waddle_xmpp::muc::RoomEffectReservation> {
        self.reservation.as_ref()
    }

    fn replace(
        &mut self,
        reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    ) -> Option<waddle_xmpp::muc::RoomEffectReservation> {
        std::mem::replace(&mut self.reservation, reservation)
    }

    fn clear(&mut self) {
        self.reservation = None;
        self.pending_members_only_enforcement = None;
    }

    fn defer_to_members_only_enforcement(
        &mut self,
        pending_members_only_enforcement: PendingAdminMembersOnlyEnforcement,
    ) {
        self.pending_members_only_enforcement = Some(pending_members_only_enforcement);
    }

    fn clear_members_only_enforcement(&mut self) {
        self.pending_members_only_enforcement = None;
    }
}

impl Drop for CommittedAdminConfigReservationGuard<'_> {
    fn drop(&mut self) {
        if let Some(pending) = self.pending_members_only_enforcement.take() {
            tokio::spawn(pending.run());
            return;
        }
        if let Some(reservation) = self.reservation() {
            arm_config_effect_reservation(self.websocket_state, reservation);
        }
    }
}

async fn broadcast_presence_updates(
    connections: &ConnectionRegistry,
    updates: Vec<(FullJid, xmpp_parsers::presence::Presence)>,
) {
    for (recipient, presence) in updates {
        let _ = connections
            .send_to(&recipient, Stanza::Presence(presence))
            .await;
    }
}

async fn deliver_admin_affiliation_updates(
    websocket_state: Option<&WebSocketState>,
    connections: &ConnectionRegistry,
    room_jid: &BareJid,
    presence_updates: Vec<(FullJid, xmpp_parsers::presence::Presence)>,
    reservation: Option<&waddle_xmpp::muc::RoomEffectReservation>,
) {
    let Some(reservation) = reservation else {
        broadcast_presence_updates(connections, presence_updates).await;
        return;
    };
    let Some(websocket_state) = websocket_state else {
        tracing::warn!(room = %room_jid, "admin affiliation effect awaits janitor without websocket state");
        return;
    };
    websocket_state
        .deps
        .protocol
        .room_effect_arm_supervisor
        .spawn_reservation_drain(reservation.clone());
}

async fn group_dm_room_config_revision_is_current(
    actor: &ActorRef<RoomActor>,
    expected: u64,
) -> bool {
    actor
        .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
        .await
        .map(|snapshot| snapshot.config_revision == expected)
        .unwrap_or(false)
}

fn group_dm_record_config(record: &XmppChannelRecord) -> RoomConfig {
    RoomConfig {
        name: record.name.clone(),
        description: record.description.clone(),
        persistent: true,
        members_only: true,
        public_room: false,
        moderated: false,
        enable_logging: true,
        group_dm: true,
        pin_permission: record.pin_permission,
        federated_affiliation_config: FederatedAffiliationConfig::open_none(),
        ..RoomConfig::default()
    }
}

fn group_dm_shared_name(name: &str) -> Option<&str> {
    let trimmed = name.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(test)]
type RecoveryPublicationHook = std::sync::Arc<
    dyn Fn(
            ActorRef<RoomActor>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>>
        + Send
        + Sync,
>;

#[cfg(test)]
#[derive(Clone)]
struct RecoveryPublicationHookEntry {
    room_jid: BareJid,
    hook: RecoveryPublicationHook,
}

#[cfg(test)]
static RECOVERY_PUBLICATION_HOOK: OnceLock<std::sync::Mutex<Option<RecoveryPublicationHookEntry>>> =
    OnceLock::new();

#[cfg(test)]
fn install_recovery_publication_hook(
    room_jid: BareJid,
    hook: RecoveryPublicationHook,
) -> Option<RecoveryPublicationHookEntry> {
    let mut slot = RECOVERY_PUBLICATION_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (*slot).replace(RecoveryPublicationHookEntry { room_jid, hook })
}

#[cfg(test)]
fn restore_recovery_publication_hook(previous: Option<RecoveryPublicationHookEntry>) {
    let mut slot = RECOVERY_PUBLICATION_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = previous;
}

#[cfg(test)]
async fn run_recovery_publication_hook(room_jid: &BareJid, actor: &ActorRef<RoomActor>) {
    let hook = RECOVERY_PUBLICATION_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .filter(|entry| entry.room_jid == *room_jid)
        .map(|entry| entry.hook);
    if let Some(hook) = hook {
        hook(actor.clone()).await;
    }
}

#[cfg(not(test))]
async fn run_recovery_publication_hook(_room_jid: &BareJid, _actor: &ActorRef<RoomActor>) {}

struct RecoveredActorSpec<'a> {
    waddle_id: &'a str,
    channel_id: &'a str,
    config: RoomConfig,
}

async fn recover_actor_with_merged_live_roster(
    state: &AppState,
    room_jid: &BareJid,
    stale_actor: &ActorRef<RoomActor>,
    spec: RecoveredActorSpec<'_>,
    get_or_create_context: &'static str,
    snapshot_context: &'static str,
) -> Result<
    (
        ActorRef<RoomActor>,
        waddle_xmpp::muc::room_actor::RoomSnapshot,
    ),
    AdminErr,
> {
    // Exact demotion retires (and may immediately kill) the old actor, so a
    // snapshot after it is inherently racy. Capture the live roster first;
    // unlike the prior fallback, an unreadable actor is a visible recovery
    // failure rather than permission to replay an older caller snapshot.
    let stale_snapshot = stale_actor
        .ask(GetSnapshot)
        .await
        .map_err(send_err(snapshot_context))?;
    // Demote the exact stale actor and publish the successor in ONE registry
    // turn: no cleanup or janitor lookup can observe the room as absent in
    // between and mistake the handoff for convergence.
    let fallback_config = spec.config.clone();
    let recovered = match state
        .room_registry
        .ask(GetOrCreateRoomWithLiveRoster {
            room_jid: room_jid.clone(),
            waddle_id: waddle_xmpp::muc::durable::WaddleId::new(spec.waddle_id.to_string()),
            channel_id: waddle_xmpp::muc::durable::ChannelId::new(spec.channel_id.to_string()),
            config: spec.config,
            live_room_restore: stale_snapshot.room,
            occupancy_revision: stale_snapshot.occupancy_revision,
            departures: stale_snapshot.departures.clone(),
            demote_first: Some(stale_actor.clone()),
        })
        .await
    {
        Ok(acquisition) => acquisition.actor_ref,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_registry_actor::RoomRegistryError::StaleActorNotCurrent(_),
        )) => {
            // A successor was already published (a concurrent join reaped the
            // sealed stale actor and re-created the room). Follow the CURRENT
            // actor instead of failing recovery: bailing out here would
            // leave the committed staged row with no arming owner — the
            // caller has already disarmed its cancellation guard — blocking
            // the room's effect FIFO until this process dies (#1647, codex
            // round 29). Mirrors the group-DM and owner-config fallbacks.
            state
                .room_registry
                .ask(GetOrCreateRoom {
                    room_jid: room_jid.clone(),
                    waddle_id: spec.waddle_id.to_string(),
                    channel_id: spec.channel_id.to_string(),
                    config: fallback_config,
                })
                .await
                .map_err(send_err(get_or_create_context))?
                .actor_ref
        }
        Err(error) => return Err(send_err(get_or_create_context)(error)),
    };
    run_recovery_publication_hook(room_jid, &recovered).await;
    let snapshot = recovered
        .ask(GetSnapshot)
        .await
        .map_err(send_err(snapshot_context))?;
    Ok((recovered, snapshot))
}

async fn recover_group_dm_actor_after_demote(
    state: &AppState,
    room_jid: &BareJid,
    channel_id: &str,
    record: &XmppChannelRecord,
    stale_actor: &ActorRef<RoomActor>,
) -> Result<ActorRef<RoomActor>, AdminErr> {
    // Same atomic demote-and-publish as the merged-roster recovery: the
    // sealed stale actor's final roster is transplanted (its occupancy is
    // the truth for the successor), affiliations are rehydrated below.
    let stale_snapshot = stale_actor.ask(GetSnapshot).await.map_err(send_err(
        "room actor GetSnapshot during group-DM reconciliation",
    ))?;
    let actor = match state
        .room_registry
        .ask(GetOrCreateRoomWithLiveRoster {
            room_jid: room_jid.clone(),
            waddle_id: waddle_xmpp::muc::durable::WaddleId::new(
                waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.to_string(),
            ),
            channel_id: waddle_xmpp::muc::durable::ChannelId::new(channel_id.to_string()),
            config: group_dm_record_config(record),
            live_room_restore: stale_snapshot.room,
            occupancy_revision: stale_snapshot.occupancy_revision,
            departures: stale_snapshot.departures,
            demote_first: Some(stale_actor.clone()),
        })
        .await
    {
        Ok(acquisition) => acquisition.actor_ref,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_registry_actor::RoomRegistryError::StaleActorNotCurrent(_),
        )) => {
            // A successor was already published: a concurrent join reaped the
            // sealed stale actor and re-created the room. Follow the CURRENT
            // actor instead of failing recovery — its durable hydration is
            // the affiliation truth the caller inspects, and failing here
            // would repersist a membership whose removal committed. Mirrors
            // the owner-config recovery's StaleActorNotCurrent fallback.
            state
                .room_registry
                .ask(GetOrCreateRoom {
                    room_jid: room_jid.clone(),
                    waddle_id: waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.to_string(),
                    channel_id: channel_id.to_string(),
                    config: group_dm_record_config(record),
                })
                .await
                .map_err(send_err(
                    "room_registry ask GetOrCreateRoom following the published successor",
                ))?
                .actor_ref
        }
        Err(error) => {
            return Err(send_err(
                "room_registry ask GetOrCreateRoomWithLiveRoster during reconciliation",
            )(error))
        }
    };
    hydrate_group_dm_member_affiliations(state, &actor, channel_id).await?;
    Ok(actor)
}

/// `Some(successor)` when the ambiguous leave commit is proven durable: the
/// successor carries the transplanted live roster (the leaver's sessions
/// included), so the caller MUST still run the ordinary occupancy-leave path
/// against it — reconciliation only settles the affiliation.
async fn reconcile_ambiguous_group_dm_leave(
    state: &AppState,
    room_jid: &BareJid,
    channel_id: &str,
    record: &XmppChannelRecord,
    stale_actor: &ActorRef<RoomActor>,
    caller_bare: &BareJid,
) -> Option<ActorRef<RoomActor>> {
    let Ok(actor) =
        recover_group_dm_actor_after_demote(state, room_jid, channel_id, record, stale_actor).await
    else {
        return None;
    };
    let committed = actor
        .ask(GetAffiliation {
            jid: caller_bare.clone(),
        })
        .await
        .map(|affiliation| affiliation == Affiliation::None)
        .unwrap_or(false);
    committed.then_some(actor)
}

struct RecoveredGroupDmRenameCommit {
    actor: ActorRef<RoomActor>,
    snapshot: waddle_xmpp::muc::room_actor::RoomSnapshot,
    reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
}

async fn reconcile_ambiguous_group_dm_rename_commit(
    state: &AppState,
    websocket_state: &WebSocketState,
    room_jid: &BareJid,
    channel_id: &str,
    record: &XmppChannelRecord,
    stale_actor: &ActorRef<RoomActor>,
    intended_config: &RoomConfig,
) -> Result<Option<RecoveredGroupDmRenameCommit>, AdminErr> {
    let (actor, snapshot) = recover_actor_with_merged_live_roster(
        state,
        room_jid,
        stale_actor,
        RecoveredActorSpec {
            waddle_id: waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM,
            channel_id,
            config: group_dm_record_config(record),
        },
        "room_registry ask GetOrCreateRoom during reconciliation",
        "room actor GetSnapshot during group-DM rename reconciliation",
    )
    .await?;
    if snapshot.room.config == *intended_config {
        let reservation = if let Some(coordinates) = snapshot.config_durable_coordinates {
            // The shared outbox store (no per-request schema round-trip that
            // could fail ahead of the retained-arming safety net below).
            websocket_state
                .deps
                .protocol
                .room_effect_outbox
                .staged_reservation_for(coordinates.lifecycle, coordinates.revision)
                .await
                .map_err(|error| {
                    // Durable commit with known coordinates: hand the arming
                    // to a retained retry rather than stranding the inert row.
                    websocket_state
                        .deps
                        .protocol
                        .room_effect_arm_supervisor
                        .retain_staged_reservation_arming(room_jid.clone(), coordinates);
                    internal_err(format!("group-DM reservation recovery failed: {error}"))
                })?
        } else {
            None
        };
        Ok(Some(RecoveredGroupDmRenameCommit {
            actor,
            snapshot,
            reservation,
        }))
    } else {
        Ok(None)
    }
}

async fn group_dm_rename_update_error(
    state: &AppState,
    room_jid: &BareJid,
    actor: &ActorRef<RoomActor>,
    error: kameo::error::SendError<
        waddle_xmpp::muc::room_actor::UpdateGroupDmConfigByMember,
        waddle_xmpp::muc::room_actor::UpdateGroupDmConfigByMemberError,
    >,
) -> AdminErr {
    use waddle_xmpp::muc::room_actor::UpdateGroupDmConfigByMemberError;

    match error {
        kameo::error::SendError::HandlerError(error) => match error {
            UpdateGroupDmConfigByMemberError::NotGroupDm => {
                bad_request("room_jid is not a group DM")
            }
            UpdateGroupDmConfigByMemberError::NotMember => {
                unavailable("Command is not available on this service")
            }
            UpdateGroupDmConfigByMemberError::NotOccupant => {
                Box::new(CommandResult::Error(XmppError::forbidden(Some(
                    "Only joined group-DM occupants can rename the room".to_string(),
                ))))
            }
            // A definitive ownership loss rejects the rename before any
            // actor-memory projection, so exact-demote and let the caller retry.
            UpdateGroupDmConfigByMemberError::NotOwner => {
                let _ = state
                    .room_registry
                    .ask(
                        waddle_xmpp::muc::room_registry_actor::DemoteRoomIfExactActor {
                            room_jid: room_jid.clone(),
                            actor_ref: actor.clone(),
                        },
                    )
                    .await;
                unavailable("This room is temporarily unavailable; please retry.")
            }
            UpdateGroupDmConfigByMemberError::OwnershipUnavailable => {
                unavailable("This room's ownership cannot be verified right now; please retry.")
            }
            UpdateGroupDmConfigByMemberError::PersistFailed => {
                internal_err("group-DM rename durable commit failed")
            }
            UpdateGroupDmConfigByMemberError::CommitOutcomeUnknown => {
                unavailable("This room's rename outcome is being reconciled; please retry.")
            }
        },
        error => internal_err(format!("room actor UpdateGroupDmConfigByMember: {error}")),
    }
}

pub(crate) async fn publish_group_dm_bookmark(
    state: &AppState,
    member_jid: &BareJid,
    room_jid: &BareJid,
    name: Option<&str>,
) -> Result<(), AdminErr> {
    let mut bookmark = existing_group_dm_bookmark(state, member_jid, room_jid).await?;
    bookmark.name = name.map(str::to_string);
    bookmark.autojoin = true;
    let item = waddle_xmpp::xep::xep0402::build_bookmark_item(&bookmark);
    state
        .pubsub_storage
        .publish_item(
            member_jid,
            waddle_xmpp::xep::xep0402::PEP_NODE,
            &item,
            Some(member_jid),
            true,
        )
        .await
        .map(|_| ())
        .map_err(|error| internal_err(format!("failed to publish group-DM bookmark: {error}")))
}

async fn existing_group_dm_bookmark(
    state: &AppState,
    member_jid: &BareJid,
    room_jid: &BareJid,
) -> Result<waddle_xmpp::xep::xep0402::Bookmark, AdminErr> {
    let item_id = room_jid.to_string();
    let item_filter = [item_id.clone()];
    let existing = state
        .pubsub_storage
        .get_items(
            member_jid,
            waddle_xmpp::xep::xep0402::PEP_NODE,
            Some(1),
            &item_filter,
        )
        .await
        .map_err(|error| internal_err(format!("failed to read group-DM bookmark: {error}")))?;
    Ok(existing
        .into_iter()
        .next()
        .and_then(|item| item.to_pubsub_item().payload)
        .and_then(|payload| waddle_xmpp::xep::xep0402::parse_bookmark(&item_id, &payload).ok())
        .unwrap_or_else(|| waddle_xmpp::xep::xep0402::Bookmark::new(room_jid.clone())))
}

pub(crate) async fn retract_group_dm_bookmark(
    state: &AppState,
    member_jid: &BareJid,
    room_jid: &BareJid,
) -> Result<(), AdminErr> {
    state
        .pubsub_storage
        .retract_item(
            member_jid,
            waddle_xmpp::xep::xep0402::PEP_NODE,
            &room_jid.to_string(),
        )
        .await
        .map(|_| ())
        .map_err(|error| internal_err(format!("failed to retract group-DM bookmark: {error}")))
}

async fn rollback_group_dm_create(
    state: &AppState,
    group_dm_id: &str,
    room_jid: &BareJid,
    persisted_members: &[BareJid],
) -> Result<(), AdminErr> {
    destroy_room_for_rollback(state, room_jid, "group-DM creation failed").await?;
    for persisted_member in persisted_members {
        let _ = retract_group_dm_bookmark(state, persisted_member, room_jid).await;
        let _ = delete_group_dm_member_tuple(state, group_dm_id, persisted_member).await;
    }
    let _ = delete_xmpp_channel(state.db_pool.global_actor().clone(), group_dm_id).await;
    Ok(())
}

pub(crate) async fn persist_group_dm_member_tuple(
    state: &AppState,
    group_dm_id: &str,
    member_jid: &BareJid,
) -> Result<(), XmppError> {
    persist_group_dm_member_actor_tuple(state, group_dm_id, member_jid).await?;
    if let Err(error) = persist_durable_group_dm_member_tuple(state, group_dm_id, member_jid).await
    {
        let _ = delete_group_dm_member_actor_tuple(state, group_dm_id, member_jid).await;
        return Err(error);
    }
    Ok(())
}

async fn persist_group_dm_member_actor_tuple(
    state: &AppState,
    group_dm_id: &str,
    member_jid: &BareJid,
) -> Result<(), XmppError> {
    let tuple = Tuple::new(
        Object::new(ObjectType::Channel, group_dm_id),
        Relation::new("member"),
        Subject::user(member_jid.to_string()),
    );
    match state.permission_actor.ask(WriteTuple { tuple }).await {
        Ok(()) => {}
        Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => {}
        Err(error) => {
            return Err(XmppError::internal(format!(
                "Failed to persist group-DM member: {error}"
            )));
        }
    }
    Ok(())
}

async fn persist_durable_group_dm_member_tuple(
    state: &AppState,
    group_dm_id: &str,
    member_jid: &BareJid,
) -> Result<(), XmppError> {
    state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: r#"
                INSERT INTO permission_tuples
                    (id, object_type, object_id, relation, subject_type, subject_id, subject_relation)
                SELECT ?, 'channel', ?, 'member', 'user', ?, NULL
                WHERE NOT EXISTS (
                    SELECT 1 FROM permission_tuples
                    WHERE object_type = 'channel'
                      AND object_id = ?
                      AND relation = 'member'
                      AND subject_type = 'user'
                      AND subject_id = ?
                      AND subject_relation IS NULL
                )
            "#
            .to_string(),
            params: vec![
                format!("group-dm-member:{group_dm_id}:{member_jid}").into(),
                group_dm_id.into(),
                member_jid.to_string().into(),
                group_dm_id.into(),
                member_jid.to_string().into(),
            ],
        })
        .await
        .map(|_| ())
        .map_err(|error| {
            XmppError::internal(format!("Failed to persist durable group-DM member: {error}"))
        })
}

async fn delete_group_dm_member_tuple(
    state: &AppState,
    group_dm_id: &str,
    member_jid: &BareJid,
) -> Result<(), XmppError> {
    delete_group_dm_member_actor_tuple(state, group_dm_id, member_jid).await?;
    match delete_durable_group_dm_member_tuple(state, group_dm_id, member_jid).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = persist_group_dm_member_actor_tuple(state, group_dm_id, member_jid).await;
            Err(error)
        }
    }
}

async fn delete_group_dm_member_actor_tuple(
    state: &AppState,
    group_dm_id: &str,
    member_jid: &BareJid,
) -> Result<(), XmppError> {
    let tuple = Tuple::new(
        Object::new(ObjectType::Channel, group_dm_id),
        Relation::new("member"),
        Subject::user(member_jid.to_string()),
    );
    match state.permission_actor.ask(DeleteTuple { tuple }).await {
        Ok(()) => {}
        Err(kameo::error::SendError::HandlerError(PermissionError::TupleNotFound)) => {}
        Err(error) => {
            return Err(XmppError::internal(format!(
                "Failed to roll back group-DM member: {error}"
            )));
        }
    }
    Ok(())
}

async fn delete_durable_group_dm_member_tuple(
    state: &AppState,
    group_dm_id: &str,
    member_jid: &BareJid,
) -> Result<(), XmppError> {
    state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: r#"
                DELETE FROM permission_tuples
                WHERE object_type = 'channel'
                  AND object_id = ?
                  AND relation = 'member'
                  AND subject_type = 'user'
                  AND subject_id = ?
                  AND subject_relation IS NULL
            "#
            .to_string(),
            params: vec![group_dm_id.into(), member_jid.to_string().into()],
        })
        .await
        .map(|_| ())
        .map_err(|error| {
            XmppError::internal(format!("Failed to delete durable group-DM member: {error}"))
        })
}

pub(crate) async fn rollback_group_dm_member_tuple(
    state: &AppState,
    group_dm_id: &str,
    member_jid: &BareJid,
) {
    let _ = delete_group_dm_member_tuple(state, group_dm_id, member_jid).await;
}

/// Fallible group-DM member-tuple removal for callers that must
/// propagate the failure — the XEP-0045 §10.9 destroy wipe (#1261)
/// refuses to acknowledge a destruction whose durable authorization
/// survived.
pub(crate) async fn remove_group_dm_member_tuple(
    state: &AppState,
    group_dm_id: &str,
    member_jid: &BareJid,
) -> Result<(), XmppError> {
    delete_group_dm_member_tuple(state, group_dm_id, member_jid).await
}

pub(crate) async fn validate_group_dm_invitee(
    state: &AppState,
    inviter_jid: &BareJid,
    invitee_jid: &BareJid,
) -> Result<(), XmppError> {
    if invitee_jid.domain() != inviter_jid.domain() {
        return Err(XmppError::bad_request(Some(format!(
            "invitee must be local to {}",
            inviter_jid.domain()
        ))));
    }
    let Some(localpart) = invitee_jid.node().map(|node| node.to_string()) else {
        return Err(XmppError::bad_request(Some(
            "invitee must be a user JID with a localpart".to_string(),
        )));
    };
    let exists = local_account_exists(
        state.db_pool.global_actor(),
        &localpart,
        invitee_jid.domain().as_str(),
    )
    .await
    .map_err(|error| XmppError::internal(format!("user lookup failed: {error}")))?;
    if !exists {
        return Err(XmppError::item_not_found(Some(format!(
            "group-DM invitee does not exist: {invitee_jid}"
        ))));
    }
    Ok(())
}

async fn validate_group_dm_members(
    state: &AppState,
    creator_jid: &BareJid,
    member_jids: &[BareJid],
) -> Result<(), AdminErr> {
    for member_jid in member_jids {
        if member_jid.domain() != creator_jid.domain() {
            return Err(bad_request(format!(
                "member_jids must be local to {}",
                creator_jid.domain()
            )));
        }
        let Some(localpart) = member_jid.node().map(|node| node.to_string()) else {
            return Err(bad_request("member_jids must be user JIDs with localparts"));
        };
        let exists = local_account_exists(
            state.db_pool.global_actor(),
            &localpart,
            member_jid.domain().as_str(),
        )
        .await
        .map_err(|error| internal_err(format!("user lookup failed: {error}")))?;
        if !exists {
            return Err(Box::new(CommandResult::Error(XmppError::item_not_found(
                Some(format!("group-DM member does not exist: {member_jid}")),
            ))));
        }
    }
    Ok(())
}

async fn run_update(
    state: &AppState,
    websocket_state: &WebSocketState,
    connections: &ConnectionRegistry,
    args: &ChannelsUpdateArgs,
    sfu: Option<&Arc<dyn waddle_sfu::SfuService>>,
) -> Result<ChannelRef, AdminErr> {
    let mut actor = state
        .room_registry
        .ask(GetRoom {
            room_jid: args.channel_jid.clone(),
        })
        .await
        .map_err(send_err("room_registry ask GetRoom"))?
        .ok_or_else(|| {
            Box::new(CommandResult::Error(XmppError::item_not_found(Some(
                format!("no channel '{}'", args.channel_jid),
            ))))
        })?;

    let _config_guard = acquire_room_config_lock(&args.channel_jid).await;
    let pre_update_snapshot = actor
        .ask(GetSnapshot)
        .await
        .map_err(send_err("room actor GetSnapshot before channel update"))?;
    let existing = actor
        .ask(GetConfig)
        .await
        .map_err(send_err("room actor GetConfig"))?;

    let new_name = args.name.clone().unwrap_or_else(|| existing.name.clone());
    let new_topic = args.topic.clone().or(existing.description.clone());
    let new_members_only = args.members_only.unwrap_or(existing.members_only);
    let new_public_room = args.is_public.unwrap_or(existing.public_room);
    let channel_id = waddle_xmpp::parse_managed_room_jid(&args.channel_jid)
        .ok_or_else(|| bad_request("channel_jid must be a managed MUC room JID"))?;
    let catalog_snapshot = get_xmpp_channel(state.db_pool.global_actor().clone(), &channel_id)
        .await
        .map_err(|error| internal_err(format!("channel catalog lookup failed: {error}")))?;
    let existing_channel_type =
        channel_type_from_catalog_or_config(catalog_snapshot.as_ref(), &existing);
    let new_channel_type = args.channel_type.unwrap_or(existing_channel_type);

    let mut updated = RoomConfig {
        name: new_name.clone(),
        description: new_topic.clone(),
        members_only: new_members_only,
        public_room: new_public_room,
        ..existing.clone()
    };
    apply_channel_type(&mut updated, new_channel_type);
    let linked_bookmark = if let Some(link) = state
        .channel_space_link_store
        .get(&args.channel_jid)
        .await
        .map_err(map_link_err)?
    {
        let node = link.space_node.clone();
        let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&args.channel_jid) else {
            return Err(internal_err(format!(
                "linked channel JID is not managed: {}",
                args.channel_jid
            )));
        };
        let item_id = args.channel_jid.to_string();
        let previous_bookmark = snapshot_channel_bookmark(state, &node, &item_id).await?;
        Some((node, channel_id, item_id, previous_bookmark))
    } else {
        None
    };

    let members_only_enforcement_affiliations =
        if !existing.requires_membership() && updated.requires_membership() {
            let snapshot = actor
                .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
                .await
                .map_err(send_err("room actor GetSnapshot"))?;
            let occupant_jids: Vec<BareJid> = snapshot
                .room
                .occupants
                .values()
                .map(|occupant| occupant.real_jid.to_bare())
                .collect();
            Some(
                explicit_channel_affiliations_for_jids(state, &channel_id, occupant_jids)
                    .await
                    .map_err(internal_err)?,
            )
        } else {
            None
        };

    let effect_plan = if !existing.requires_membership() && updated.requires_membership() {
        waddle_xmpp::muc::room_actor::ConfigEffectPlan::ManagedMembersOnlyFallback
    } else {
        waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience
    };
    let mut cancelled_commit_recovery =
        if let Some(affiliations) = members_only_enforcement_affiliations.as_ref() {
            CancelledConfigAskRecoveryGuard::defer_members_only(
                websocket_state,
                &actor,
                &args.channel_jid,
                &updated,
                pre_update_snapshot.config_revision.saturating_add(1),
                PendingAdminMembersOnlyEnforcementSeed {
                    affiliations: affiliations.clone(),
                    config_status_codes: waddle_xmpp::muc::config_change_status_codes(
                        &existing, &updated,
                    ),
                    room_jid: args.channel_jid.clone(),
                    sfu: sfu.cloned(),
                    arm_supervisor: websocket_state
                        .deps
                        .protocol
                        .room_effect_arm_supervisor
                        .clone(),
                },
            )
        } else {
            CancelledConfigAskRecoveryGuard::arm_only(
                websocket_state,
                &actor,
                &args.channel_jid,
                &updated,
                pre_update_snapshot.config_revision.saturating_add(1),
            )
        };
    let applied_config = match actor
        .ask(UpdateConfig {
            config: updated.clone(),
            effect_plan,
        })
        .await
    {
        Ok(applied) => {
            cancelled_commit_recovery.disarm();
            applied
        }
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::RoomMutationError::CommitOutcomeUnknown,
        )) => {
            cancelled_commit_recovery.disarm();
            let (recovered, snapshot) = recover_actor_with_merged_live_roster(
                state,
                &args.channel_jid,
                &actor,
                RecoveredActorSpec {
                    waddle_id: &pre_update_snapshot.room.waddle_id,
                    channel_id: &pre_update_snapshot.room.channel_id,
                    config: pre_update_snapshot.room.config.clone(),
                },
                "room_registry ask GetOrCreateRoom during channel reconciliation",
                "room actor GetSnapshot during channel reconciliation",
            )
            .await?;
            if snapshot.room.config != updated {
                return Err(unavailable(
                    "This channel update outcome is being reconciled; please retry.",
                ));
            }
            let reservation = if let Some(coordinates) = snapshot.config_durable_coordinates {
                websocket_state
                    .deps
                    .protocol
                    .room_effect_outbox
                    .staged_reservation_for(coordinates.lifecycle, coordinates.revision)
                    .await
                    .map_err(|error| {
                        websocket_state
                            .deps
                            .protocol
                            .room_effect_arm_supervisor
                            .retain_staged_reservation_arming(
                                args.channel_jid.clone(),
                                coordinates,
                            );
                        internal_err(format!("channel reservation recovery failed: {error}"))
                    })?
            } else {
                None
            };
            actor = recovered;
            waddle_xmpp::muc::room_actor::ConfigMutationApplied {
                revision: snapshot.config_revision,
                notification: None,
                reservation,
            }
        }
        Err(error) => {
            cancelled_commit_recovery.disarm();
            return Err(send_err("room actor UpdateConfig")(error));
        }
    };
    let expected_revision = applied_config.revision;
    let mut config_reservation =
        CommittedAdminConfigReservationGuard::new(websocket_state, applied_config.reservation);
    if let Some(affiliations) = members_only_enforcement_affiliations.as_ref() {
        config_reservation.defer_to_members_only_enforcement(PendingAdminMembersOnlyEnforcement {
            actor: actor.clone(),
            affiliations: affiliations.clone(),
            fallback_reservation: config_reservation.reservation().cloned(),
            config_status_codes: waddle_xmpp::muc::config_change_status_codes(&existing, &updated),
            room_jid: args.channel_jid.clone(),
            sfu: sfu.cloned(),
            arm_supervisor: websocket_state
                .deps
                .protocol
                .room_effect_arm_supervisor
                .clone(),
        });
    }

    if let Err(error) = upsert_channel_catalog(state, &channel_id, &updated, new_channel_type).await
    {
        config_reservation.clear_members_only_enforcement();
        let _ = rollback_room_config_or_arm(
            websocket_state,
            &actor,
            expected_revision,
            existing.clone(),
            config_reservation.reservation().cloned(),
        )
        .await;
        config_reservation.clear();
        return Err(error);
    }

    if let Some((node, channel_id, item_id, previous_bookmark)) = linked_bookmark {
        let bookmark_channel_type = new_channel_type.as_str().to_string();
        let parent_tuple_created = match publish_channel_space_bookmark(
            state,
            &node,
            &channel_id,
            &new_name,
            &bookmark_channel_type,
        )
        .await
        {
            Ok(created) => created,
            Err(error) => {
                config_reservation.clear_members_only_enforcement();
                if rollback_room_config_or_arm(
                    websocket_state,
                    &actor,
                    expected_revision,
                    existing.clone(),
                    config_reservation.reservation().cloned(),
                )
                .await
                {
                    restore_channel_catalog_snapshot(state, &channel_id, catalog_snapshot.as_ref())
                        .await;
                }
                config_reservation.clear();
                return Err(error);
            }
        };
        if let Err(error) =
            retract_duplicate_channel_bookmarks(state, &node, &channel_id, &args.channel_jid).await
        {
            config_reservation.clear_members_only_enforcement();
            if rollback_room_config_or_arm(
                websocket_state,
                &actor,
                expected_revision,
                existing.clone(),
                config_reservation.reservation().cloned(),
            )
            .await
            {
                restore_channel_catalog_snapshot(state, &channel_id, catalog_snapshot.as_ref())
                    .await;
                restore_channel_space_bookmark(
                    state,
                    &node,
                    &item_id,
                    &channel_id,
                    previous_bookmark.as_ref(),
                    parent_tuple_created,
                )
                .await;
            }
            config_reservation.clear();
            return Err(error);
        }
    }

    let mut arm_reservation = config_reservation.reservation().cloned();
    if let Some(explicit_affiliations) = members_only_enforcement_affiliations {
        let enforcement = actor
            .ask(EnforceMembersOnlyAffiliations {
                affiliations: explicit_affiliations,
                fallback_reservation: config_reservation.reservation().cloned(),
                config_status_codes: waddle_xmpp::muc::config_change_status_codes(
                    &existing, &updated,
                ),
            })
            .await;
        config_reservation.clear_members_only_enforcement();
        let applied = enforcement.map_err(|error| {
            if let Some(reservation) = config_reservation.reservation() {
                arm_config_effect_reservation(websocket_state, reservation);
            }
            send_err("room actor EnforceMembersOnlyAffiliations")(error)
        })?;
        let enforcement_has_reservation = applied.outbox_reservation.is_some();
        arm_reservation = applied.outbox_reservation;
        if enforcement_has_reservation {
            let _ = config_reservation.replace(arm_reservation.clone());
        }
        // A status-322 ejection ends room membership, so it ends call
        // participation; a surviving occupant who lost voice loses
        // publish rights.
        crate::server::routes::websocket::muc_call_sfu::converge_moderation_deltas_via_sfu(
            sfu,
            &args.channel_jid,
            &applied.removed_by_moderation,
            &applied.voice_changes,
        );
        if !enforcement_has_reservation {
            broadcast_presence_updates(connections, applied.presence_updates).await;
        }
    }

    // A channel-type change flips `moderated` in both directions
    // (`apply_channel_type`), which silently re-decides XEP-0045 voice
    // for every seated visitor without touching any role. Converge the
    // live SFU grants or a Chat -> Announcement switch mid-call leaves
    // every non-moderator publishing.
    if existing.moderated != updated.moderated {
        crate::server::routes::websocket::muc_call_sfu::converge_room_voice_after_moderation_flip(
            sfu,
            &actor,
            &args.channel_jid,
            None,
        )
        .await;
    }

    // XEP-0045 §10.2.1 (#1265 item 15): a config change applied through
    // the admin channels path notifies occupants exactly like the
    // muc#owner IQ path — status 104 (plus 170/171 when the logging
    // knob flips).
    if let Some(reservation) = arm_reservation.as_ref() {
        arm_config_effect_reservation(websocket_state, reservation);
    } else {
        broadcast_admin_config_change(connections, &actor, &args.channel_jid, &existing, &updated)
            .await;
    }
    config_reservation.clear();

    Ok(ChannelRef {
        channel_jid: args.channel_jid.clone(),
        name: new_name,
        topic: new_topic,
        channel_type: new_channel_type,
        is_public: new_public_room,
        members_only: updated.members_only,
    })
}

async fn run_delete(state: &AppState, args: &ChannelsDeleteArgs) -> Result<(), AdminErr> {
    let link = state
        .channel_space_link_store
        .get(&args.channel_jid)
        .await
        .map_err(map_link_err)?;
    let linked_node = link.as_ref().map(|link| link.space_node.clone());
    let linked_node_text = linked_node.as_ref().map(ToString::to_string);
    let item_id = args.channel_jid.to_string();
    let channel_id = waddle_xmpp::parse_managed_room_jid(&args.channel_jid);
    let catalog_snapshot = match channel_id.as_deref() {
        Some(channel_id) => Some(
            get_xmpp_channel(state.db_pool.global_actor().clone(), channel_id)
                .await
                .map_err(|error| internal_err(format!("channel catalog lookup failed: {error}")))?,
        ),
        None => None,
    };
    let mut bookmark_nodes: std::collections::BTreeSet<String> = state
        .pubsub_storage
        .list_node_names_for_item(&state.spaces_jid, &item_id)
        .await
        .map_err(|e| internal_err(format!("pubsub list channel bookmark nodes failed: {e}")))?
        .into_iter()
        .collect();
    if let Some(node) = linked_node.as_ref() {
        bookmark_nodes.insert(node.to_string());
    }

    let mut bookmark_snapshots: std::collections::BTreeMap<String, Option<PubSubItem>> =
        std::collections::BTreeMap::new();
    for node in &bookmark_nodes {
        bookmark_snapshots.insert(
            node.clone(),
            snapshot_channel_bookmark(state, node, &item_id).await?,
        );
    }

    let mut removed_bookmarks: Vec<RemovedChannelBookmark> = Vec::new();

    for node in bookmark_nodes
        .iter()
        .filter(|node| Some(node.as_str()) != linked_node_text.as_deref())
    {
        let snapshot = bookmark_snapshots.get(node).cloned().unwrap_or(None);
        let fallback_item = if snapshot.is_none() {
            if let Some(channel_id) = channel_id.as_deref() {
                match rollback_channel_bookmark_item(state, &args.channel_jid, channel_id).await {
                    Some(item) => Some(item),
                    None => {
                        restore_removed_channel_bookmarks(
                            state,
                            &removed_bookmarks,
                            &item_id,
                            Some(channel_id),
                        )
                        .await;
                        return Err(internal_err(format!(
                            "could not snapshot missing Spaces bookmark for linked channel {}",
                            args.channel_jid
                        )));
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        match retract_channel_bookmark_and_parent(state, node, &item_id, channel_id.as_deref())
            .await
        {
            Ok(parent_tuple_deleted) => removed_bookmarks.push(RemovedChannelBookmark {
                node: node.clone(),
                item: snapshot,
                fallback_item,
                parent_tuple_deleted,
            }),
            Err(error) => {
                restore_removed_channel_bookmarks(
                    state,
                    &removed_bookmarks,
                    &item_id,
                    channel_id.as_deref(),
                )
                .await;
                return Err(error);
            }
        }
    }

    if link.is_some() {
        if let Err(error) = state
            .channel_space_link_store
            .clear(&args.channel_jid)
            .await
        {
            restore_removed_channel_bookmarks(
                state,
                &removed_bookmarks,
                &item_id,
                channel_id.as_deref(),
            )
            .await;
            return Err(map_link_err(error));
        }
    }

    if let Some(node) = linked_node.as_ref() {
        let node = node.to_string();
        let snapshot = bookmark_snapshots.get(&node).cloned().unwrap_or(None);
        let fallback_item = if snapshot.is_none() {
            if let Some(channel_id) = channel_id.as_deref() {
                match rollback_channel_bookmark_item(state, &args.channel_jid, channel_id).await {
                    Some(item) => Some(item),
                    None => {
                        if let Some(link) = link.as_ref() {
                            if let Err(rollback_error) =
                                state.channel_space_link_store.set(link).await
                            {
                                tracing::warn!(
                                    error = %rollback_error,
                                    channel = %args.channel_jid,
                                    space = %link.space_jid,
                                    "channels:delete rollback failed to restore channel-space link",
                                );
                            }
                        }
                        restore_removed_channel_bookmarks(
                            state,
                            &removed_bookmarks,
                            &item_id,
                            Some(channel_id),
                        )
                        .await;
                        return Err(internal_err(format!(
                            "could not snapshot missing Spaces bookmark for linked channel {}",
                            args.channel_jid
                        )));
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        match retract_channel_bookmark_and_parent(state, &node, &item_id, channel_id.as_deref())
            .await
        {
            Ok(parent_tuple_deleted) => removed_bookmarks.push(RemovedChannelBookmark {
                node,
                item: snapshot,
                fallback_item,
                parent_tuple_deleted,
            }),
            Err(error) => {
                if let Some(link) = link.as_ref() {
                    if let Err(rollback_error) = state.channel_space_link_store.set(link).await {
                        tracing::warn!(
                            error = %rollback_error,
                            channel = %args.channel_jid,
                            space = %link.space_jid,
                            "channels:delete rollback failed to restore channel-space link",
                        );
                    }
                }
                restore_removed_channel_bookmarks(
                    state,
                    &removed_bookmarks,
                    &item_id,
                    channel_id.as_deref(),
                )
                .await;
                return Err(error);
            }
        }
    }

    if let Some(channel_id) = channel_id.as_deref() {
        if let Err(error) =
            delete_xmpp_channel(state.db_pool.global_actor().clone(), channel_id).await
        {
            if let Some(link) = link.as_ref() {
                if let Err(rollback_error) = state.channel_space_link_store.set(link).await {
                    tracing::warn!(
                        error = %rollback_error,
                        channel = %args.channel_jid,
                        space = %link.space_jid,
                        "channels:delete rollback failed to restore channel-space link",
                    );
                }
            }
            restore_removed_channel_bookmarks(
                state,
                &removed_bookmarks,
                &item_id,
                Some(channel_id),
            )
            .await;
            return Err(internal_err(format!(
                "channel catalog delete failed: {error}"
            )));
        }
    }

    // `NotRegistered` is fine (a dormant channel has no live actor);
    // `DurableWipeFailed` means the registry deliberately kept the room
    // because its fenced clustering wipe failed — the deletion must
    // fail and roll back rather than record a destruction that will
    // resurrect (#1261).
    let destroy_result = state
        .room_registry
        .ask(DestroyRoom {
            room_jid: args.channel_jid.clone(),
            reason: DestroyRoomReason::Destroy,
        })
        .await;
    let failure = match destroy_result {
        Ok(
            waddle_xmpp::muc::room_registry_actor::DestroyRoomOutcome::Destroyed
            | waddle_xmpp::muc::room_registry_actor::DestroyRoomOutcome::NotRegistered,
        ) => None,
        Ok(waddle_xmpp::muc::room_registry_actor::DestroyRoomOutcome::DurableWipeFailed) => {
            Some(internal_err(format!(
                "room destroy refused for {}: durable room-state wipe failed",
                args.channel_jid
            )))
        }
        Ok(waddle_xmpp::muc::room_registry_actor::DestroyRoomOutcome::ReleaseBacklogFull) => {
            Some(internal_err(format!(
                "room destroy refused for {}: exact-release retry backlog is full; retry deletion",
                args.channel_jid
            )))
        }
        Err(error) => Some(send_err("room_registry ask DestroyRoom")(error)),
    };
    if let Some(error) = failure {
        restore_removed_channel_bookmarks(
            state,
            &removed_bookmarks,
            &item_id,
            channel_id.as_deref(),
        )
        .await;
        if let (Some(channel_id), Some(snapshot)) =
            (channel_id.as_deref(), catalog_snapshot.as_ref())
        {
            restore_channel_catalog_snapshot(state, channel_id, snapshot.as_ref()).await;
        }
        if let Some(link) = link.as_ref() {
            if let Err(rollback_error) = state.channel_space_link_store.set(link).await {
                tracing::warn!(
                    error = %rollback_error,
                    channel = %args.channel_jid,
                    space = %link.space_jid,
                    "channels:delete rollback failed to restore channel-space link",
                );
            }
        }
        return Err(error);
    }
    Ok(())
}

async fn snapshot_channel_bookmark(
    state: &AppState,
    node: &str,
    item_id: &str,
) -> Result<Option<PubSubItem>, AdminErr> {
    let item_filter = [item_id.to_string()];
    Ok(state
        .pubsub_storage
        .get_items(&state.spaces_jid, node, Some(1), &item_filter)
        .await
        .map_err(|e| internal_err(format!("pubsub read channel bookmark failed: {e}")))?
        .into_iter()
        .next()
        .map(|stored| stored.to_pubsub_item()))
}

async fn restore_removed_channel_bookmarks(
    state: &AppState,
    removed: &[RemovedChannelBookmark],
    item_id: &str,
    channel_id: Option<&str>,
) {
    for removed in removed.iter().rev() {
        if let Some(item) = removed.item.as_ref() {
            match state
                .pubsub_storage
                .publish_item(&state.spaces_jid, &removed.node, item, None, false)
                .await
            {
                Ok(_) => {
                    if removed.parent_tuple_deleted {
                        if let Some(channel_id) = channel_id {
                            let _ =
                                write_channel_parent_tuple(state, channel_id, &removed.node).await;
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        node = %removed.node,
                        item_id = %item_id,
                        "channels:delete failed to restore Spaces bookmark",
                    );
                }
            }
        } else if removed.parent_tuple_deleted {
            if let Some(item) = removed.fallback_item.as_ref() {
                match state
                    .pubsub_storage
                    .publish_item(&state.spaces_jid, &removed.node, item, None, false)
                    .await
                {
                    Ok(_) => {
                        if removed.parent_tuple_deleted {
                            if let Some(channel_id) = channel_id {
                                let _ =
                                    write_channel_parent_tuple(state, channel_id, &removed.node)
                                        .await;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            node = %removed.node,
                            item_id = %item_id,
                            "channels:delete failed to restore fallback Spaces bookmark",
                        );
                    }
                }
            } else {
                tracing::warn!(
                    node = %removed.node,
                    item_id = %item_id,
                    "channels:delete skipped parent tuple restore because no Spaces bookmark item was restored",
                );
            }
        }
    }
}

async fn retract_channel_bookmark_and_parent(
    state: &AppState,
    node: &str,
    item_id: &str,
    channel_id: Option<&str>,
) -> Result<bool, AdminErr> {
    let parent_tuple_deleted = if let Some(channel_id) = channel_id {
        delete_channel_parent_tuple(state, channel_id, node).await?
    } else {
        false
    };
    if let Err(error) = state
        .pubsub_storage
        .retract_item(&state.spaces_jid, node, item_id)
        .await
    {
        if parent_tuple_deleted {
            if let Some(channel_id) = channel_id {
                let _ = write_channel_parent_tuple(state, channel_id, node).await;
            }
        }
        tracing::warn!(
            node = %node,
            item_id = %item_id,
            error = %error,
            "channels:delete failed to retract a Spaces bookmark",
        );
        return Err(internal_err(format!(
            "pubsub retract channel bookmark failed: {error}"
        )));
    }
    Ok(parent_tuple_deleted)
}

async fn run_occupants(
    state: &AppState,
    args: &ChannelsOccupantsArgs,
) -> Result<ChannelsOccupantsResult, AdminErr> {
    let actor = state
        .room_registry
        .ask(GetRoom {
            room_jid: args.channel_jid.clone(),
        })
        .await
        .map_err(send_err("room_registry ask GetRoom"))?
        .ok_or_else(|| {
            Box::new(CommandResult::Error(XmppError::item_not_found(Some(
                format!("no channel '{}'", args.channel_jid),
            ))))
        })?;
    let mut occupants = actor
        .ask(ListOccupants)
        .await
        .map_err(send_err("room actor ListOccupants"))?;
    occupants.sort_by(|a, b| a.nick.cmp(&b.nick));

    if let Some(cursor) = args.after_cursor.as_deref() {
        occupants.retain(|info| info.nick.as_str() > cursor);
    }
    let limit = args.page_size as usize;
    let total = occupants.len();
    let entries: Vec<ChannelOccupantEntry> = occupants
        .iter()
        .take(limit)
        .map(|info| ChannelOccupantEntry {
            nick: info.nick.clone(),
            real_jid: info.real_jid.clone(),
            role: info.role,
            affiliation: WireAffiliation::from_muc(info.affiliation),
        })
        .collect();
    let next_cursor = if total > limit {
        entries.last().map(|entry| entry.nick.clone())
    } else {
        None
    };
    Ok(ChannelsOccupantsResult {
        entries,
        next_cursor,
    })
}

async fn run_affiliations(
    state: &AppState,
    args: &ChannelsAffiliationsArgs,
) -> Result<ChannelsAffiliationsResult, AdminErr> {
    let actor = state
        .room_registry
        .ask(GetRoom {
            room_jid: args.channel_jid.clone(),
        })
        .await
        .map_err(send_err("room_registry ask GetRoom"))?
        .ok_or_else(|| {
            Box::new(CommandResult::Error(XmppError::item_not_found(Some(
                format!("no channel '{}'", args.channel_jid),
            ))))
        })?;

    let filter_aff = args.filter.map(|w| w.to_muc());
    let mut affiliations = actor
        .ask(ListAffiliations { filter: filter_aff })
        .await
        .map_err(send_err("room actor ListAffiliations"))?;
    affiliations.sort_by(|a, b| a.jid.cmp(&b.jid));

    if let Some(cursor) = args.after_cursor.as_deref() {
        affiliations.retain(|entry| entry.jid.to_string().as_str() > cursor);
    }
    let limit = args.page_size as usize;
    let total = affiliations.len();
    let entries: Vec<ChannelAffiliationEntry> = affiliations
        .iter()
        .take(limit)
        .map(|e| ChannelAffiliationEntry {
            jid: e.jid.clone(),
            affiliation: WireAffiliation::from_muc(e.affiliation),
            reason: e.reason.clone(),
        })
        .collect();
    let next_cursor = if total > limit {
        entries.last().map(|entry| entry.jid.to_string())
    } else {
        None
    };
    Ok(ChannelsAffiliationsResult {
        entries,
        next_cursor,
    })
}

async fn run_set_affiliation(
    state: &AppState,
    websocket_state: Option<&WebSocketState>,
    connections: &ConnectionRegistry,
    caller_bare: &BareJid,
    args: &ChannelsSetAffiliationArgs,
    sfu: Option<&Arc<dyn waddle_sfu::SfuService>>,
) -> Result<ChannelsSetAffiliationResult, AdminErr> {
    let channel_id = waddle_xmpp::parse_managed_room_jid(&args.channel_jid)
        .ok_or_else(|| bad_request("channel_jid must be a managed MUC room JID"))?;
    let _config_guard = acquire_room_config_lock(&args.channel_jid).await;
    let actor = state
        .room_registry
        .ask(GetRoom {
            room_jid: args.channel_jid.clone(),
        })
        .await
        .map_err(send_err("room_registry ask GetRoom"))?
        .ok_or_else(|| {
            Box::new(CommandResult::Error(XmppError::item_not_found(Some(
                format!("no channel '{}'", args.channel_jid),
            ))))
        })?;
    let previous_affiliation = actor
        .ask(GetAffiliation {
            jid: args.member_jid.clone(),
        })
        .await
        .map_err(send_err("room actor GetAffiliation"))?;
    let next_affiliation = args.affiliation.to_muc();
    if next_affiliation != Affiliation::Owner && previous_affiliation == Affiliation::Owner {
        let owners = actor
            .ask(ListAffiliations {
                filter: Some(Affiliation::Owner),
            })
            .await
            .map_err(send_err("room actor ListAffiliations"))?;
        if owners.len() == 1 && owners[0].jid == args.member_jid {
            return Err(Box::new(CommandResult::Error(XmppError::conflict(Some(
                "cannot remove the last owner from a room".to_string(),
            )))));
        }
    }
    let durable_previous_affiliation =
        explicit_channel_affiliations_for_jids(state, &channel_id, [args.member_jid.clone()])
            .await
            .map_err(internal_err)?
            .into_iter()
            .next()
            .map(|(_, affiliation)| affiliation)
            .unwrap_or(Affiliation::None);
    persist_channel_affiliation_or_restore(
        state,
        &channel_id,
        &args.member_jid,
        durable_previous_affiliation,
        next_affiliation,
    )
    .await?;
    let applied = match actor
        .ask(ApplyAffiliationChange {
            actor: Some(caller_bare.clone()),
            jid: args.member_jid.clone(),
            affiliation: next_affiliation,
        })
        .await
    {
        Ok(applied) => applied,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::CannotRemoveLastOwner,
        )) => {
            let _ = persist_channel_affiliation(
                state,
                &channel_id,
                &args.member_jid,
                durable_previous_affiliation,
            )
            .await;
            return Err(Box::new(CommandResult::Error(XmppError::conflict(Some(
                "cannot remove the last owner from a room".to_string(),
            )))));
        }
        Err(error) => {
            let _ = persist_channel_affiliation(
                state,
                &channel_id,
                &args.member_jid,
                durable_previous_affiliation,
            )
            .await;
            return Err(send_err("room actor ApplyAffiliationChange")(error));
        }
    };
    let presence_updates = applied.presence_updates;
    let removed_by_moderation = applied.removed_by_moderation;
    let voice_changes = applied.voice_changes;
    let outbox_reservation = applied.outbox_reservation;
    // Membership-scoped visibility (#935): an admin-V2 ban (Outcast)
    // ends the occupant's room membership, so their live SFU call
    // participation ends with it. Fire-and-forget inside the SFU
    // layer; the moderation result is never blocked on LiveKit.
    // A demotion that keeps the occupant in the room can still cost
    // them voice (e.g. `admin -> none` in a moderated room), which
    // must revoke their SFU publish rights.
    if outbox_reservation.is_none() {
        crate::server::routes::websocket::muc_call_sfu::converge_moderation_deltas_via_sfu(
            sfu,
            &args.channel_jid,
            &removed_by_moderation,
            &voice_changes,
        );
    } else {
        evict_moderation_removals(sfu, &args.channel_jid, &removed_by_moderation);
    }
    deliver_admin_affiliation_updates(
        websocket_state,
        connections,
        &args.channel_jid,
        presence_updates,
        outbox_reservation.as_ref(),
    )
    .await;
    Ok(ChannelsSetAffiliationResult {
        member_jid: args.member_jid.clone(),
        affiliation: args.affiliation,
    })
}

/// Identifies one private-kick revocation: which channel member loses their
/// affiliation, on whose authority, and what durable affiliation to restore
/// if the in-room revocation fails.
struct PrivateKickRevocation<'a> {
    channel_id: &'a str,
    caller_bare: &'a BareJid,
    occupant_jid: &'a BareJid,
    durable_previous_affiliation: Affiliation,
}

async fn sync_private_kick_affiliation_revocation(
    state: &AppState,
    websocket_state: Option<&WebSocketState>,
    connections: &ConnectionRegistry,
    room_jid: &BareJid,
    actor: &ActorRef<RoomActor>,
    revocation: PrivateKickRevocation<'_>,
) -> Result<(), AdminErr> {
    let PrivateKickRevocation {
        channel_id,
        caller_bare,
        occupant_jid,
        durable_previous_affiliation,
    } = revocation;
    match actor
        .ask(ApplyAffiliationChange {
            actor: Some(caller_bare.clone()),
            jid: occupant_jid.clone(),
            affiliation: Affiliation::None,
        })
        .await
    {
        Ok(applied) => {
            deliver_admin_affiliation_updates(
                websocket_state,
                connections,
                room_jid,
                applied.presence_updates,
                applied.outbox_reservation.as_ref(),
            )
            .await;
            Ok(())
        }
        Err(error) => {
            let _ = persist_channel_affiliation(
                state,
                channel_id,
                occupant_jid,
                durable_previous_affiliation,
            )
            .await;
            Err(send_err("room actor ApplyAffiliationChange (kick)")(error))
        }
    }
}

async fn run_kick(
    state: &AppState,
    websocket_state: Option<&WebSocketState>,
    connections: &ConnectionRegistry,
    caller_full: &FullJid,
    args: &ChannelsKickArgs,
    sfu: Option<&Arc<dyn waddle_sfu::SfuService>>,
) -> Result<ChannelsKickResult, AdminErr> {
    // XEP-0045 §9.1.1 — kicking an occupant is a role-change to "none";
    // the service MUST send `<presence type='unavailable'>` carrying
    // `<status code='307'/>` to every occupant, including the kicked
    // one (which additionally receives `<status code='110'/>` per
    // §6.6). The regular admin-IQ path (#680) does this by routing
    // through `RoomActor::ApplyAdminItems`, which builds the per-
    // occupant presence stanzas via `build_kick_presence`. Admin V2
    // reuses the same actor message so the broadcast shape is
    // exactly identical to the IQ path.
    let channel_id = waddle_xmpp::parse_managed_room_jid(&args.channel_jid)
        .ok_or_else(|| bad_request("channel_jid must be a managed MUC room JID"))?;
    let _config_guard = acquire_room_config_lock(&args.channel_jid).await;
    let actor = state
        .room_registry
        .ask(GetRoom {
            room_jid: args.channel_jid.clone(),
        })
        .await
        .map_err(send_err("room_registry ask GetRoom"))?
        .ok_or_else(|| {
            Box::new(CommandResult::Error(XmppError::item_not_found(Some(
                format!("no channel '{}'", args.channel_jid),
            ))))
        })?;
    let config = actor
        .ask(GetConfig)
        .await
        .map_err(send_err("room actor GetConfig"))?;
    let caller_bare = caller_full.to_bare();
    let durable_previous_affiliation = if config.members_only {
        explicit_channel_affiliations_for_jids(state, &channel_id, [args.occupant_jid.clone()])
            .await
            .map_err(internal_err)?
            .into_iter()
            .next()
            .map(|(_, affiliation)| affiliation)
            .unwrap_or(Affiliation::None)
    } else {
        Affiliation::None
    };
    let revoke_members_only_member =
        config.members_only && durable_previous_affiliation == Affiliation::Member;
    if revoke_members_only_member {
        persist_channel_affiliation_or_restore(
            state,
            &channel_id,
            &args.occupant_jid,
            durable_previous_affiliation,
            Affiliation::None,
        )
        .await?;
    }
    // Resolve the occupant's nick — `ApplyAdminItems` looks up by nick
    // in the role-change branch, so we must translate the caller's
    // bare-JID handle into the room-nick first. If the target is not
    // currently joined, there is no role-kick presence to broadcast.
    // Private managed-channel kicks still keep the durable membership
    // revocation above and synchronize the actor affiliation list.
    let occupants = actor
        .ask(ListOccupants)
        .reply_timeout(ADMIN_ROOM_ASK_TIMEOUT)
        .await
        .map_err(send_err("room actor ListOccupants"))?;
    let Some(target_nick) = occupants
        .into_iter()
        .find(|info| info.real_jid.to_bare() == args.occupant_jid)
        .map(|info| info.nick)
    else {
        if revoke_members_only_member {
            sync_private_kick_affiliation_revocation(
                state,
                websocket_state,
                connections,
                &args.channel_jid,
                &actor,
                PrivateKickRevocation {
                    channel_id: &channel_id,
                    caller_bare: &caller_bare,
                    occupant_jid: &args.occupant_jid,
                    durable_previous_affiliation,
                },
            )
            .await?;
        }
        return Ok(ChannelsKickResult {
            occupant_jid: args.occupant_jid.clone(),
        });
    };

    let items = vec![AdminItem {
        jid: None,
        nick: Some(target_nick),
        affiliation: None,
        role: Some(Role::None),
        reason: args.reason.clone(),
    }];
    let applied = match actor
        .ask(ApplyAdminItems {
            sender_jid: caller_full.clone(),
            // Community-owner admin V2 callers are not necessarily
            // joined to the room; declare them as `Affiliation::Owner`
            // so `ApplyAdminItems` short-circuits the can-modify gate.
            // §9.1 doesn't require the kicker to be in the room — the
            // gate is "MAY be performed by an admin", which the V2
            // owner ACL has already established via
            // `caller_or_forbidden`.
            sender_affiliation: Affiliation::Owner,
            sender_role: Role::Moderator,
            items,
        })
        .reply_timeout(ADMIN_ROOM_ASK_TIMEOUT)
        .await
    {
        Ok(applied) => applied,
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::AdminApplyError::OccupantNotFound(_),
        )) if revoke_members_only_member => {
            sync_private_kick_affiliation_revocation(
                state,
                websocket_state,
                connections,
                &args.channel_jid,
                &actor,
                PrivateKickRevocation {
                    channel_id: &channel_id,
                    caller_bare: &caller_bare,
                    occupant_jid: &args.occupant_jid,
                    durable_previous_affiliation,
                },
            )
            .await?;
            return Ok(ChannelsKickResult {
                occupant_jid: args.occupant_jid.clone(),
            });
        }
        Err(kameo::error::SendError::HandlerError(error)) => {
            if revoke_members_only_member {
                let _ = persist_channel_affiliation(
                    state,
                    &channel_id,
                    &args.occupant_jid,
                    durable_previous_affiliation,
                )
                .await;
            }
            return Err(internal_err(format!(
                "room actor ApplyAdminItems (kick) rejected: {error}"
            )));
        }
        Err(error) => {
            if revoke_members_only_member {
                let _ = persist_channel_affiliation(
                    state,
                    &channel_id,
                    &args.occupant_jid,
                    durable_previous_affiliation,
                )
                .await;
            }
            return Err(internal_err(format!(
                "room actor ApplyAdminItems (kick) failed: {error}"
            )));
        }
    };

    // The kick is durably applied in the room actor at this point:
    // broadcast the 307 presences and run the SFU eviction BEFORE the
    // fallible affiliation-revocation ask below, so an actor-
    // infrastructure failure there can't strand an applied kick with
    // no occupant notification and a live call session (#935 review).
    for (recipient, presence) in applied.presence_updates {
        let _ = connections
            .send_to(&recipient, Stanza::Presence(presence))
            .await;
    }
    // Membership-scoped visibility (#935): a kick (307) ends the
    // occupant's room membership, so their live SFU call
    // participation ends with it.
    crate::server::routes::websocket::muc_call_sfu::converge_moderation_deltas_via_sfu(
        sfu,
        &args.channel_jid,
        &applied.removed_by_moderation,
        &applied.voice_changes,
    );

    if revoke_members_only_member {
        sync_private_kick_affiliation_revocation(
            state,
            websocket_state,
            connections,
            &args.channel_jid,
            &actor,
            PrivateKickRevocation {
                channel_id: &channel_id,
                caller_bare: &caller_bare,
                occupant_jid: &args.occupant_jid,
                durable_previous_affiliation,
            },
        )
        .await?;
    }

    Ok(ChannelsKickResult {
        occupant_jid: args.occupant_jid.clone(),
    })
}

/// Evict every session involuntarily removed by moderation (kick 307
/// / ban 301) from the room's SFU call, when an SFU is configured.
fn evict_moderation_removals(
    sfu: Option<&Arc<dyn waddle_sfu::SfuService>>,
    room_jid: &BareJid,
    removed: &[FullJid],
) {
    let Some(sfu) = sfu else {
        return;
    };
    for jid in removed {
        crate::server::routes::websocket::muc_call_sfu::unregister_participant_via_sfu_ungated(
            sfu, room_jid, jid,
        );
    }
}

// ---------------------------------------------------------------------------
// Response builders
// ---------------------------------------------------------------------------

fn bool_str(b: bool) -> &'static str {
    if b {
        "1"
    } else {
        "0"
    }
}

pub fn build_list_form(result: &ChannelsListResult) -> DataForm {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_LIST))
        .add_reported(Field::new("channel_jid", FieldType::JidSingle).with_label("Channel JID"))
        .add_reported(Field::new("name", FieldType::TextSingle).with_label("Name"))
        .add_reported(Field::new("topic", FieldType::TextSingle).with_label("Topic"))
        .add_reported(Field::new("channel_type", FieldType::TextSingle).with_label("Type"))
        .add_reported(Field::new("is_public", FieldType::Boolean).with_label("Public"))
        .add_reported(Field::new("members_only", FieldType::Boolean).with_label("Members only"))
        .add_reported(Field::new("occupant_count", FieldType::TextSingle).with_label("Occupants"))
        .add_reported(Field::new("owner_count", FieldType::TextSingle).with_label("Owners"))
        .add_reported(Field::new("admin_count", FieldType::TextSingle).with_label("Admins"))
        .add_reported(Field::new("member_count", FieldType::TextSingle).with_label("Members"))
        .add_reported(Field::new("outcast_count", FieldType::TextSingle).with_label("Outcasts"));
    for entry in &result.entries {
        let row = vec![
            Field::new("channel_jid", FieldType::JidSingle)
                .with_value(entry.channel_jid.to_string()),
            Field::new("name", FieldType::TextSingle).with_value(entry.name.clone()),
            Field::new("topic", FieldType::TextSingle)
                .with_value(entry.topic.clone().unwrap_or_default()),
            Field::new("channel_type", FieldType::TextSingle)
                .with_value(entry.channel_type.as_str()),
            Field::boolean("is_public", entry.is_public),
            Field::boolean("members_only", entry.members_only),
            Field::new("occupant_count", FieldType::TextSingle)
                .with_value(entry.occupant_count.to_string()),
            Field::new("owner_count", FieldType::TextSingle)
                .with_value(entry.affiliation_owner_count.to_string()),
            Field::new("admin_count", FieldType::TextSingle)
                .with_value(entry.affiliation_admin_count.to_string()),
            Field::new("member_count", FieldType::TextSingle)
                .with_value(entry.affiliation_member_count.to_string()),
            Field::new("outcast_count", FieldType::TextSingle)
                .with_value(entry.affiliation_outcast_count.to_string()),
        ];
        form = form.add_item(row);
    }
    if let Some(cursor) = result.next_cursor.as_ref() {
        form = form.add_field(Field::text_single("next_cursor", cursor));
    }
    form
}

pub fn build_channel_form(channel: &ChannelRef) -> DataForm {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_CREATE))
        .add_field(
            Field::new("channel_jid", FieldType::JidSingle)
                .with_value(channel.channel_jid.to_string()),
        )
        .add_field(Field::text_single("name", channel.name.clone()))
        .add_field(Field::text_single(
            "channel_type",
            channel.channel_type.as_str(),
        ))
        .add_field(Field::text_single("is_public", bool_str(channel.is_public)))
        .add_field(Field::text_single(
            "members_only",
            bool_str(channel.members_only),
        ));
    if let Some(topic) = channel.topic.as_ref() {
        form = form.add_field(Field::text_single("topic", topic));
    }
    form
}

pub fn build_group_dm_form(group_dm: &GroupDmRef) -> DataForm {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_GROUP_DM_CREATE))
        .add_field(
            Field::new("room_jid", FieldType::JidSingle).with_value(group_dm.room_jid.to_string()),
        )
        .add_field(Field::text_single("name", group_dm.name.clone()))
        .add_field(Field::text_single(
            "is_public",
            bool_str(group_dm.is_public),
        ))
        .add_field(Field::text_single(
            "members_only",
            bool_str(group_dm.members_only),
        ))
        .add_field(Field::text_single(
            "persistent",
            bool_str(group_dm.persistent),
        ))
}

pub fn build_group_dm_leave_form(result: &GroupDmLeaveResult) -> DataForm {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_GROUP_DM_LEAVE))
        .add_field(
            Field::new("room_jid", FieldType::JidSingle).with_value(result.room_jid.to_string()),
        )
        .add_field(Field::text_single("left", bool_str(result.left)))
}

pub fn build_group_dm_rename_form(result: &GroupDmRenameResult) -> DataForm {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_GROUP_DM_RENAME))
        .add_field(
            Field::new("room_jid", FieldType::JidSingle).with_value(result.room_jid.to_string()),
        )
        .add_field(Field::text_single(
            "name",
            result.name.clone().unwrap_or_default(),
        ))
}

pub fn build_occupants_form(result: &ChannelsOccupantsResult) -> DataForm {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_OCCUPANTS))
        .add_reported(Field::new("nick", FieldType::TextSingle).with_label("Nick"))
        .add_reported(Field::new("real_jid", FieldType::JidSingle).with_label("Real JID"))
        .add_reported(Field::new("role", FieldType::TextSingle).with_label("Role"))
        .add_reported(Field::new("affiliation", FieldType::TextSingle).with_label("Affiliation"));
    for entry in &result.entries {
        let row = vec![
            Field::new("nick", FieldType::TextSingle).with_value(entry.nick.clone()),
            Field::new("real_jid", FieldType::JidSingle).with_value(entry.real_jid.to_string()),
            Field::new("role", FieldType::TextSingle).with_value(role_as_wire(entry.role)),
            Field::new("affiliation", FieldType::TextSingle)
                .with_value(entry.affiliation.as_wire()),
        ];
        form = form.add_item(row);
    }
    if let Some(cursor) = result.next_cursor.as_ref() {
        form = form.add_field(Field::text_single("next_cursor", cursor));
    }
    form
}

pub fn build_affiliations_form(result: &ChannelsAffiliationsResult) -> DataForm {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_AFFILIATIONS))
        .add_reported(Field::new("jid", FieldType::JidSingle).with_label("JID"))
        .add_reported(Field::new("affiliation", FieldType::TextSingle).with_label("Affiliation"))
        .add_reported(Field::new("reason", FieldType::TextSingle).with_label("Reason"));
    for entry in &result.entries {
        let row = vec![
            Field::new("jid", FieldType::JidSingle).with_value(entry.jid.to_string()),
            Field::new("affiliation", FieldType::TextSingle)
                .with_value(entry.affiliation.as_wire()),
            Field::new("reason", FieldType::TextSingle)
                .with_value(entry.reason.clone().unwrap_or_default()),
        ];
        form = form.add_item(row);
    }
    if let Some(cursor) = result.next_cursor.as_ref() {
        form = form.add_field(Field::text_single("next_cursor", cursor));
    }
    form
}

pub fn build_set_affiliation_form(result: &ChannelsSetAffiliationResult) -> DataForm {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_SET_AFFILIATION))
        .add_field(
            Field::new("member_jid", FieldType::JidSingle)
                .with_value(result.member_jid.to_string()),
        )
        .add_field(Field::text_single(
            "affiliation",
            result.affiliation.as_wire(),
        ))
}

pub fn build_kick_form(result: &ChannelsKickResult) -> DataForm {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_KICK))
        .add_field(
            Field::new("occupant_jid", FieldType::JidSingle)
                .with_value(result.occupant_jid.to_string()),
        )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room_effect_outbox::drain::drain_due_effects;
    use crate::room_effect_outbox::{
        RoomEffectEnqueue, RoomEffectKey, RoomEffectOriginInstanceId, RoomEffectProducingNode,
    };
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state, register_test_connection,
    };
    use std::str::FromStr;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::{
        MucConfigStatusCode, RoomEffectReservation, RoomLifecycleId, RoomLifecycleState,
        RoomMutationEffects, RoomRevision,
    };
    use waddle_xmpp::ownership::NodeIdentity;
    use waddle_xmpp::registry::OutboundStanza;

    fn admin_effect_room_jid() -> BareJid {
        BareJid::from_str("admin-effects@muc.example.com").expect("room JID")
    }

    fn admin_effect_origin() -> RoomEffectOriginInstanceId {
        RoomEffectOriginInstanceId::new("admin-channel-test-origin".to_owned())
            .expect("origin instance")
    }

    fn admin_effect_producing_node() -> RoomEffectProducingNode {
        RoomEffectProducingNode::from_node_identity(NodeIdentity::new(
            "admin-channel-test-node",
            "admin-channel-test-epoch",
        ))
    }

    async fn create_owned_admin_effect_room(
        state: &crate::server::routes::websocket::WebSocketState,
        room_jid: &BareJid,
        lifecycle: RoomLifecycleId,
    ) {
        state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "admin-channel-test".to_owned(),
                channel_id: "admin-channel-test".to_owned(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create local room");
        let connection = state
            .deps
            .protocol
            .room_effect_outbox
            .database()
            .guard()
            .await
            .expect("effect database connection");
        connection
            .execute(
                "CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (lifecycle_id TEXT NOT NULL, room_jid TEXT NOT NULL, revision BIGINT NOT NULL, state TEXT NOT NULL)",
                (),
            )
            .await
            .expect("create lifecycle table");
        connection
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    RoomRevision::initial().as_i64(),
                    RoomLifecycleState::Active.as_db_str(),
                ],
            )
            .await
            .expect("insert active lifecycle");
    }

    async fn stage_admin_config_effect(
        state: &crate::server::routes::websocket::WebSocketState,
        lifecycle: RoomLifecycleId,
        revision: RoomRevision,
        room_jid: &BareJid,
        recipient: FullJid,
    ) -> RoomEffectReservation {
        let effects = RoomMutationEffects::config(
            room_jid.clone(),
            vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
            vec![recipient],
        );
        let store = state.deps.protocol.room_effect_outbox.as_ref();
        let mut tx = store.database().begin().await.expect("effect transaction");
        let reservation = store
            .enqueue_in_tx(
                &mut tx,
                RoomEffectEnqueue {
                    lifecycle,
                    revision,
                    effects: &effects,
                    origin: &admin_effect_origin(),
                    producing_node: &admin_effect_producing_node(),
                    now_ms: 0,
                },
            )
            .await
            .expect("stage config effect");
        tx.commit().await.expect("commit staged effect");
        reservation
    }

    async fn wait_for_admin_reservation_to_arm(
        state: &crate::server::routes::websocket::WebSocketState,
        reservation: &RoomEffectReservation,
    ) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let key = RoomEffectKey {
                    lifecycle: reservation.lifecycle,
                    revision: reservation.revision,
                    ordinal: reservation.ordinals[0],
                };
                let row = state
                    .deps
                    .protocol
                    .room_effect_outbox
                    .find(&key)
                    .await
                    .expect("find staged row")
                    .expect("staged row exists");
                if row.available_at_ms != i64::MAX {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("admin arm supervisor completes without a correctness sleep");
    }

    async fn drain_admin_effect_and_ack(
        state: std::sync::Arc<crate::server::routes::websocket::WebSocketState>,
        receiver: &mut mpsc::Receiver<OutboundStanza>,
    ) {
        let drain_state = std::sync::Arc::clone(&state);
        let now_ms = crate::time::now_ms();
        let drain =
            tokio::spawn(async move { drain_due_effects(drain_state.as_ref(), now_ms, 8).await });
        let outbound = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("effect delivery reaches recipient")
            .expect("recipient connection remains open");
        outbound
            .write_acceptance
            .as_ref()
            .expect("durable admin effect waits for write acceptance")
            .acknowledge();
        assert_eq!(
            drain
                .await
                .expect("admin effect drain joins")
                .expect("admin effect drain succeeds")
                .drained,
            1,
            "the armed admin config effect drains once"
        );
    }

    #[test]
    fn wire_affiliation_round_trips() {
        for wire in [
            WireAffiliation::Owner,
            WireAffiliation::Admin,
            WireAffiliation::Member,
            WireAffiliation::None,
            WireAffiliation::Outcast,
        ] {
            assert_eq!(WireAffiliation::parse(wire.as_wire()), Ok(wire));
        }
    }

    #[test]
    fn create_args_default_is_public_true() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_CREATE))
            .add_field(Field::text_single("name", "general"));
        let args = parse_create_args(Some(&form)).expect("ok");
        assert!(args.is_public, "public is the spec-mandated default");
    }

    #[test]
    fn create_args_honours_explicit_is_public_false() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_CREATE))
            .add_field(Field::text_single("name", "secret"))
            .add_field(Field::text_single("is_public", "false"));
        let args = parse_create_args(Some(&form)).expect("ok");
        assert!(!args.is_public);
    }

    #[test]
    fn create_args_rejects_overlong_name() {
        let long = "a".repeat(81);
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_CREATE))
            .add_field(Field::text_single("name", &long));
        let err = parse_create_args(Some(&form)).expect_err("overlong");
        assert!(err.contains("80"), "got: {err}");
    }

    #[test]
    fn delete_args_requires_confirm_yes() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_DELETE))
            .add_field(Field::text_single("channel_jid", "general@muc.localhost"))
            .add_field(Field::text_single("confirm", "no"));
        let err = parse_delete_args(Some(&form)).expect_err("confirm=no rejected");
        assert!(err.contains("confirm"));
    }

    #[test]
    fn set_affiliation_args_validate_affiliation() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_SET_AFFILIATION))
            .add_field(Field::text_single("channel_jid", "general@muc.localhost"))
            .add_field(Field::text_single("member_jid", "alice@localhost"))
            .add_field(Field::text_single("affiliation", "ceo"));
        let err = parse_set_affiliation_args(Some(&form)).expect_err("ceo rejected");
        assert!(err.contains("affiliation"));
    }

    #[test]
    fn affiliations_args_accept_filter() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_AFFILIATIONS))
            .add_field(Field::text_single("channel_jid", "general@muc.localhost"))
            .add_field(Field::text_single("filter", "outcast"));
        let args = parse_affiliations_args(Some(&form)).expect("ok");
        assert_eq!(args.filter, Some(WireAffiliation::Outcast));
    }

    #[test]
    fn kick_args_require_occupant_jid() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_KICK))
            .add_field(Field::text_single("channel_jid", "general@muc.localhost"));
        let err = parse_kick_args(Some(&form)).expect_err("missing occupant_jid");
        assert!(err.contains("occupant_jid"));
    }

    #[test]
    fn build_list_form_reports_all_columns() {
        let result = ChannelsListResult {
            entries: vec![ChannelListEntry {
                channel_jid: "general@muc.localhost".parse().expect("jid"),
                name: "General".to_string(),
                topic: Some("All things".to_string()),
                channel_type: ChannelType::Text,
                is_public: true,
                members_only: false,
                occupant_count: 7,
                affiliation_owner_count: 1,
                affiliation_admin_count: 2,
                affiliation_member_count: 3,
                affiliation_outcast_count: 0,
            }],
            next_cursor: None,
        };
        let form = build_list_form(&result);
        assert!(matches!(form.form_type, FormType::Result));
        assert_eq!(form.reported.len(), 11);
        assert_eq!(form.items.len(), 1);
    }

    #[test]
    fn build_channel_form_reports_visibility_policy() {
        let channel = ChannelRef {
            channel_jid: "general@muc.localhost".parse().expect("jid"),
            name: "General".to_string(),
            topic: None,
            channel_type: ChannelType::Text,
            is_public: true,
            members_only: true,
        };
        let form = build_channel_form(&channel);
        assert_eq!(form.get_value("is_public"), Some("1"));
        assert_eq!(form.get_value("members_only"), Some("1"));
    }

    #[test]
    fn catalog_channel_type_takes_precedence_over_room_config() {
        let record = XmppChannelRecord {
            id: "announcements".to_string(),
            name: "Announcements".to_string(),
            description: None,
            channel_type: "announcement".to_string(),
            position: 0,
            is_default: false,
            pin_permission: PinPermission::Anyone,
            members_only: false,
            public_room: true,
            created_at: "2026-06-21T00:00:00Z".to_string(),
            updated_at: None,
        };
        let config = RoomConfig {
            moderated: false,
            ..RoomConfig::default()
        };

        assert_eq!(
            channel_type_from_catalog_or_config(Some(&record), &config),
            ChannelType::Announcement
        );
    }

    #[test]
    fn group_dm_channel_type_forces_members_only_config() {
        let mut config = RoomConfig {
            members_only: false,
            ..RoomConfig::default()
        };

        apply_channel_type(&mut config, ChannelType::GroupDm);

        assert!(config.group_dm);
        assert!(config.members_only);
    }

    #[test]
    fn mint_channel_localpart_fallback() {
        assert!(mint_channel_localpart("???").starts_with("channel-"));
        assert!(mint_channel_localpart("Hello World").starts_with("hello-world-"));
    }

    #[tokio::test]
    async fn admin_config_reservation_stages_then_arms_and_drains_after_outer_success() {
        let state = create_test_websocket_state().await;
        let room_jid = admin_effect_room_jid();
        let lifecycle = RoomLifecycleId::generate();
        let recipient: FullJid = "alice@example.com/admin".parse().expect("recipient JID");
        create_owned_admin_effect_room(state.as_ref(), &room_jid, lifecycle).await;
        let (sender, mut receiver) = mpsc::channel(1);
        register_test_connection(state.as_ref(), &recipient, sender).await;
        let reservation = stage_admin_config_effect(
            state.as_ref(),
            lifecycle,
            RoomRevision::initial(),
            &room_jid,
            recipient,
        )
        .await;

        let staged = state
            .deps
            .protocol
            .room_effect_outbox
            .find(&RoomEffectKey {
                lifecycle,
                revision: reservation.revision,
                ordinal: reservation.ordinals[0],
            })
            .await
            .expect("find staged row")
            .expect("staged row");
        assert_eq!(
            staged.available_at_ms,
            i64::MAX,
            "outer work gates delivery"
        );

        arm_config_effect_reservation(state.as_ref(), &reservation);
        wait_for_admin_reservation_to_arm(state.as_ref(), &reservation).await;
        drain_admin_effect_and_ack(state, &mut receiver).await;
    }

    #[tokio::test]
    async fn admin_config_abort_without_rollback_arms_the_committed_reservation() {
        let state = create_test_websocket_state().await;
        let room_jid = admin_effect_room_jid();
        let lifecycle = RoomLifecycleId::generate();
        create_owned_admin_effect_room(state.as_ref(), &room_jid, lifecycle).await;
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("room lookup")
            .expect("room actor");
        let recipient: FullJid = "alice@example.com/admin".parse().expect("recipient JID");
        let (sender, mut receiver) = mpsc::channel(1);
        register_test_connection(state.as_ref(), &recipient, sender).await;
        let reservation = stage_admin_config_effect(
            state.as_ref(),
            lifecycle,
            RoomRevision::initial(),
            &room_jid,
            recipient,
        )
        .await;

        assert!(
            !rollback_room_config_or_arm(
                state.as_ref(),
                &actor,
                1,
                RoomConfig::default(),
                Some(reservation.clone()),
            )
            .await,
            "a revision mismatch models an abort path that cannot prove rollback"
        );
        wait_for_admin_reservation_to_arm(state.as_ref(), &reservation).await;
        drain_admin_effect_and_ack(state, &mut receiver).await;
    }

    #[tokio::test]
    async fn admin_commit_recovery_arms_the_reservation_recovered_from_coordinates() {
        let state = create_test_websocket_state().await;
        let room_jid = admin_effect_room_jid();
        let lifecycle = RoomLifecycleId::generate();
        create_owned_admin_effect_room(state.as_ref(), &room_jid, lifecycle).await;
        let recipient: FullJid = "alice@example.com/admin".parse().expect("recipient JID");
        let (sender, mut receiver) = mpsc::channel(1);
        register_test_connection(state.as_ref(), &recipient, sender).await;
        let reservation = stage_admin_config_effect(
            state.as_ref(),
            lifecycle,
            RoomRevision::initial(),
            &room_jid,
            recipient,
        )
        .await;

        let recovered = state
            .deps
            .protocol
            .room_effect_outbox
            .staged_reservation_for(lifecycle, reservation.revision)
            .await
            .expect("recover staged reservation from reconciled coordinates")
            .expect("committed config retains its staged reservation");
        assert_eq!(recovered, reservation);

        arm_config_effect_reservation(state.as_ref(), &recovered);
        wait_for_admin_reservation_to_arm(state.as_ref(), &recovered).await;
        drain_admin_effect_and_ack(state, &mut receiver).await;
    }

    #[tokio::test]
    async fn failed_cancelled_members_only_enforcement_arms_its_config_fallback() {
        let state = create_test_websocket_state().await;
        let room_jid = admin_effect_room_jid();
        let lifecycle = RoomLifecycleId::generate();
        create_owned_admin_effect_room(state.as_ref(), &room_jid, lifecycle).await;
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("room lookup")
            .expect("room actor");
        let recipient: FullJid = "alice@example.com/admin".parse().expect("recipient JID");
        let reservation = stage_admin_config_effect(
            state.as_ref(),
            lifecycle,
            RoomRevision::initial(),
            &room_jid,
            recipient,
        )
        .await;
        let key = RoomEffectKey {
            lifecycle,
            revision: reservation.revision,
            ordinal: reservation.ordinals[0],
        };
        actor.kill();
        let mut guard =
            CommittedAdminConfigReservationGuard::new(state.as_ref(), Some(reservation.clone()));
        guard.defer_to_members_only_enforcement(PendingAdminMembersOnlyEnforcement {
            actor,
            affiliations: Vec::new(),
            fallback_reservation: Some(reservation),
            config_status_codes: vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
            room_jid,
            sfu: None,
            arm_supervisor: state.deps.protocol.room_effect_arm_supervisor.clone(),
        });
        drop(guard);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if state
                    .deps
                    .protocol
                    .room_effect_outbox
                    .find(&key)
                    .await
                    .expect("fallback row lookup")
                    .expect("fallback row")
                    .available_at_ms
                    != i64::MAX
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed detached enforcement arms the staged config fallback");
    }
}

#[cfg(test)]
mod sfu_eviction_tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::server::routes::websocket::tests::RecordingSfu;
    use crate::server::AppState;
    use waddle_xmpp::muc::room_actor::{JoinAffiliationGrant, JoinWithAffiliation};
    use waddle_xmpp::muc::RoomConfig;

    async fn fresh_state() -> AppState {
        let db_pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("db pool");
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .expect("migrations");
        AppState::new(Arc::new(db_pool))
    }

    async fn room_with_bob(state: &AppState, room_jid: &BareJid) -> FullJid {
        let actor = state
            .room_registry
            .ask(GetOrCreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "test-waddle".to_string(),
                channel_id: room_jid.node().expect("localpart").to_string(),
                config: RoomConfig {
                    members_only: false,
                    ..RoomConfig::default()
                },
            })
            .await
            .expect("room actor")
            .actor_ref;
        let bob: FullJid = "bob@localhost/web".parse().expect("bob jid");
        actor
            .ask(JoinWithAffiliation {
                sender_jid: bob.clone(),
                nick: "bob".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "localhost".to_string(),
                admission_revision: 0,
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("bob joins");
        bob
    }

    #[tokio::test]
    async fn admin_v2_kick_evicts_target_sessions_from_room_call() {
        let state = fresh_state().await;
        let connections = ConnectionRegistry::new();
        let room_jid: BareJid = "kick-evict-chan@muc.localhost".parse().expect("room jid");
        let bob = room_with_bob(&state, &room_jid).await;

        let recorder = Arc::new(RecordingSfu::default());
        let sfu: Arc<dyn waddle_sfu::SfuService> = recorder.clone();
        let caller: FullJid = "owner@localhost/admin".parse().expect("caller jid");
        run_kick(
            &state,
            None,
            &connections,
            &caller,
            &ChannelsKickArgs {
                channel_jid: room_jid.clone(),
                occupant_jid: bob.to_bare(),
                reason: None,
            },
            Some(&sfu),
        )
        .await
        .unwrap_or_else(|_| panic!("kick succeeds"));

        let evicted = recorder.snapshot();
        assert_eq!(evicted.len(), 1, "kicked session evicted: {evicted:?}");
        assert_eq!(evicted[0].0.as_str(), "kick-evict-chan@muc.localhost");
        assert_eq!(evicted[0].1.as_livekit_identity(), "bob@localhost/web");
    }

    #[tokio::test]
    async fn admin_v2_ban_evicts_target_sessions_from_room_call() {
        let state = fresh_state().await;
        let connections = ConnectionRegistry::new();
        let room_jid: BareJid = "ban-evict-chan@muc.localhost".parse().expect("room jid");
        let bob = room_with_bob(&state, &room_jid).await;

        let recorder = Arc::new(RecordingSfu::default());
        let sfu: Arc<dyn waddle_sfu::SfuService> = recorder.clone();
        let caller: BareJid = "owner@localhost".parse().expect("caller jid");
        run_set_affiliation(
            &state,
            None,
            &connections,
            &caller,
            &ChannelsSetAffiliationArgs {
                channel_jid: room_jid.clone(),
                member_jid: bob.to_bare(),
                affiliation: WireAffiliation::Outcast,
                reason: None,
            },
            Some(&sfu),
        )
        .await
        .unwrap_or_else(|_| panic!("ban succeeds"));

        let evicted = recorder.snapshot();
        assert_eq!(evicted.len(), 1, "banned session evicted: {evicted:?}");
        assert_eq!(evicted[0].0.as_str(), "ban-evict-chan@muc.localhost");
        assert_eq!(evicted[0].1.as_livekit_identity(), "bob@localhost/web");
    }

    #[tokio::test]
    async fn admin_v2_member_promotion_does_not_evict() {
        let state = fresh_state().await;
        let connections = ConnectionRegistry::new();
        let room_jid: BareJid = "promote-chan@muc.localhost".parse().expect("room jid");
        let bob = room_with_bob(&state, &room_jid).await;

        let recorder = Arc::new(RecordingSfu::default());
        let sfu: Arc<dyn waddle_sfu::SfuService> = recorder.clone();
        let caller: BareJid = "owner@localhost".parse().expect("caller jid");
        run_set_affiliation(
            &state,
            None,
            &connections,
            &caller,
            &ChannelsSetAffiliationArgs {
                channel_jid: room_jid.clone(),
                member_jid: bob.to_bare(),
                affiliation: WireAffiliation::Admin,
                reason: None,
            },
            Some(&sfu),
        )
        .await
        .unwrap_or_else(|_| panic!("promotion succeeds"));

        assert!(
            recorder.snapshot().is_empty(),
            "an affiliation change that keeps the occupant must not evict"
        );
    }
}

#[cfg(test)]
mod group_dm_durable_reconciliation_tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::server::AppState;
    use kameo::actor::Spawn;
    use std::collections::HashMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use tokio::sync::mpsc;
    use waddle_xmpp::muc::durable::{
        DurableRoomState, MucDurableFuture, MucDurableStore, RoomCommitDatabaseError,
        RoomCommitError, RoomCommitFuture, RoomCommittedCoordinates, RoomDurableMutation,
        RoomLifecycleId, RoomRevision,
    };
    use waddle_xmpp::muc::room_actor::{
        GetSnapshot, Join, JoinAffiliationGrant, JoinWithAffiliation,
    };
    use waddle_xmpp::muc::room_registry_actor::{
        CreateRoomWithInitialAffiliations, WireClusteringClaims,
    };
    use waddle_xmpp::ownership::{
        ClaimStore, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };
    use waddle_xmpp::registry::{
        ConnectionEntry, ConnectionRegistry, RegisterUserResource, UserRegistryActor,
    };
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum DurableMode {
        CommitSucceeds,
        DestroyFails,
        AffiliationCommitUnknown,
        ConfigCommitUnknown,
        ProjectionLeaveSecondOwnershipUnavailableOnce,
        ProjectionLeaveSecondDelayed,
    }

    #[derive(Clone)]
    struct CommitPause {
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl CommitPause {
        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

        fn new() -> Self {
            Self {
                reached: Arc::new(tokio::sync::Notify::new()),
                release: Arc::new(tokio::sync::Notify::new()),
            }
        }

        async fn wait_until_reached(&self) {
            tokio::time::timeout(Self::TIMEOUT, self.reached.notified())
                .await
                .expect("timed out waiting for the paused admin config commit reply; the commit was never attempted");
        }

        async fn wait_until_released(&self) {
            tokio::time::timeout(Self::TIMEOUT, self.release.notified())
                .await
                .expect("timed out waiting to release the paused admin config commit reply");
        }

        fn release(&self) {
            self.release.notify_one();
        }
    }

    struct TestGroupDmDurableStore {
        mode: DurableMode,
        outbox: Option<Arc<crate::room_effect_outbox::RoomEffectOutboxStore>>,
        states: Mutex<HashMap<BareJid, DurableRoomState>>,
        fences: Mutex<HashMap<BareJid, waddle_xmpp::muc::RoomClaimFenceContext>>,
        coordinates: Mutex<HashMap<BareJid, (RoomLifecycleId, i64)>>,
        commit_pause: Mutex<Option<CommitPause>>,
        projection_leave_attempts: AtomicUsize,
    }

    impl TestGroupDmDurableStore {
        fn new(mode: DurableMode) -> Arc<Self> {
            Self::with_optional_outbox(mode, None)
        }

        fn with_outbox(
            mode: DurableMode,
            outbox: Arc<crate::room_effect_outbox::RoomEffectOutboxStore>,
        ) -> Arc<Self> {
            Self::with_optional_outbox(mode, Some(outbox))
        }

        fn with_optional_outbox(
            mode: DurableMode,
            outbox: Option<Arc<crate::room_effect_outbox::RoomEffectOutboxStore>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                mode,
                outbox,
                states: Mutex::new(HashMap::new()),
                fences: Mutex::new(HashMap::new()),
                coordinates: Mutex::new(HashMap::new()),
                commit_pause: Mutex::new(None),
                projection_leave_attempts: AtomicUsize::new(0),
            })
        }

        fn next_coordinates(&self, room_jid: &BareJid) -> RoomCommittedCoordinates {
            let mut coordinates = self.coordinates.lock().expect("coordinates lock");
            let entry = coordinates
                .entry(room_jid.clone())
                .or_insert_with(|| (RoomLifecycleId::generate(), 0));
            entry.1 += 1;
            RoomCommittedCoordinates {
                lifecycle: entry.0,
                revision: RoomRevision::from_stored(entry.1).expect("positive revision"),
            }
        }

        fn exact_fence_matches(
            &self,
            room_jid: &BareJid,
            fence: &waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> bool {
            self.fences.lock().expect("fences lock").get(room_jid) == Some(fence)
        }

        fn apply_mutation(&self, room_jid: &BareJid, intent: RoomDurableMutation) {
            match intent {
                RoomDurableMutation::Create {
                    waddle_id,
                    channel_id,
                    config,
                    initial_affiliations,
                } => {
                    self.states.lock().expect("states lock").insert(
                        room_jid.clone(),
                        DurableRoomState {
                            coordinates: None,
                            config_coordinates: None,
                            waddle_id: waddle_id.into_string(),
                            channel_id: channel_id.into_string(),
                            config,
                            subject: None,
                            affiliations: initial_affiliations
                                .into_iter()
                                .filter_map(|entry| {
                                    entry.affiliation.map(|affiliation| {
                                        waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                                            entry.jid,
                                            affiliation,
                                        )
                                    })
                                })
                                .collect(),
                        },
                    );
                }
                RoomDurableMutation::Affiliation(entry) => {
                    let mut states = self.states.lock().expect("states lock");
                    let state = states.get_mut(room_jid).expect("room state present");
                    state
                        .affiliations
                        .retain(|current| current.jid != entry.jid);
                    if let Some(affiliation) = entry.affiliation {
                        state.affiliations.push(
                            waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                                entry.jid,
                                affiliation,
                            ),
                        );
                    }
                }
                RoomDurableMutation::Config { config, .. }
                | RoomDurableMutation::MembersOnlyEnforcement { config, .. } => {
                    let mut states = self.states.lock().expect("states lock");
                    let state = states.get_mut(room_jid).expect("room state present");
                    state.config = config;
                }
                RoomDurableMutation::Destroy { .. } => {
                    self.states.lock().expect("states lock").remove(room_jid);
                }
                _ => {}
            }
        }

        fn record_coordinates(
            &self,
            room_jid: &BareJid,
            coordinates: RoomCommittedCoordinates,
            is_config_commit: bool,
        ) {
            if let Some(state) = self.states.lock().expect("states lock").get_mut(room_jid) {
                state.coordinates = Some(coordinates);
                if is_config_commit {
                    state.config_coordinates = Some(coordinates);
                }
            }
        }

        fn pause_next_commit_reply(&self) -> CommitPause {
            let pause = CommitPause::new();
            *self.commit_pause.lock().expect("commit pause lock") = Some(pause.clone());
            pause
        }
    }

    impl MucDurableStore for TestGroupDmDurableStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            let exact = self.exact_fence_matches(room_jid, fence);
            let state = self
                .states
                .lock()
                .expect("states lock")
                .get(room_jid)
                .cloned();
            Box::pin(async move {
                if exact {
                    Ok(state)
                } else {
                    Err(waddle_xmpp::XmppError::OwnershipLost {
                        entity: fence.entity.clone(),
                    })
                }
            })
        }

        fn commit_room_mutation<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
            intent: RoomDurableMutation,
            effects: waddle_xmpp::muc::RoomMutationEffects,
        ) -> RoomCommitFuture<'a> {
            let exact = self.exact_fence_matches(room_jid, fence);
            let mode = self.mode;
            let coordinates = self.next_coordinates(room_jid);
            let projection_leave_attempt = matches!(
                &intent,
                RoomDurableMutation::Projection(
                    waddle_xmpp::muc::durable::RoomProjection::OccupancyLeave { .. }
                )
            )
            .then(|| {
                self.projection_leave_attempts
                    .fetch_add(1, Ordering::SeqCst)
                    + 1
            });
            Box::pin(async move {
                if !exact {
                    return Err(RoomCommitError::NotOwner);
                }
                let effect_origin = crate::room_effect_outbox::RoomEffectOriginInstanceId::new(
                    "admin-channel-test-origin".to_owned(),
                )
                .expect("test origin instance");
                let producing_node =
                    crate::room_effect_outbox::RoomEffectProducingNode::from_node_identity(
                        NodeIdentity::new("admin-channel-test-node", "admin-channel-test-epoch"),
                    );
                let is_affiliation = matches!(intent, RoomDurableMutation::Affiliation(_));
                let is_config = matches!(intent, RoomDurableMutation::Config { .. });
                let is_destroy = matches!(intent, RoomDurableMutation::Destroy { .. });
                let reservation = if let Some(outbox) = self.outbox.as_ref() {
                    let mut tx = outbox.database().begin().await.map_err(|_| {
                        RoomCommitError::Database(RoomCommitDatabaseError::sanitized())
                    })?;
                    if let Some(superseded) = effects.superseding_reservation() {
                        // Mirror production tolerance (muc_durable.rs): rows
                        // already consumed by arming/drain or an interleaved
                        // supersession are not an error.
                        let _ = outbox
                            .supersede_reservation_in_tx(&mut tx, superseded)
                            .await
                            .map_err(|_| {
                                RoomCommitError::Database(RoomCommitDatabaseError::sanitized())
                            })?;
                    }
                    let reservation = if effects.effects().is_empty() {
                        None
                    } else {
                        outbox
                            .enqueue_in_tx(
                                &mut tx,
                                crate::room_effect_outbox::RoomEffectEnqueue {
                                    lifecycle: coordinates.lifecycle,
                                    revision: coordinates.revision,
                                    effects: &effects,
                                    origin: &effect_origin,
                                    producing_node: &producing_node,
                                    now_ms: 0,
                                },
                            )
                            .await
                            .map(Some)
                            .map_err(|_| {
                                RoomCommitError::Database(RoomCommitDatabaseError::sanitized())
                            })?
                    };
                    tx.commit().await.map_err(|_| {
                        RoomCommitError::Database(RoomCommitDatabaseError::sanitized())
                    })?;
                    reservation
                } else {
                    None
                };
                let is_config_commit = matches!(
                    &intent,
                    RoomDurableMutation::Create { .. }
                        | RoomDurableMutation::Config { .. }
                        | RoomDurableMutation::MembersOnlyEnforcement { .. }
                );
                if mode == DurableMode::ProjectionLeaveSecondOwnershipUnavailableOnce
                    && projection_leave_attempt == Some(2)
                {
                    return Err(RoomCommitError::OwnershipUnavailable);
                }
                self.apply_mutation(room_jid, intent);
                self.record_coordinates(room_jid, coordinates, is_config_commit);
                if mode == DurableMode::ProjectionLeaveSecondDelayed
                    && projection_leave_attempt == Some(2)
                {
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                }
                let pause = { self.commit_pause.lock().expect("commit pause lock").take() };
                if let Some(pause) = pause {
                    // Each pause has exactly one producer and one test waiter. `notify_one`
                    // retains a permit if the waiter has not reached `.notified()` yet.
                    pause.reached.notify_one();
                    pause.wait_until_released().await;
                }
                if is_destroy && mode == DurableMode::DestroyFails {
                    return Err(RoomCommitError::Database(
                        RoomCommitDatabaseError::sanitized(),
                    ));
                }
                if is_affiliation && mode == DurableMode::AffiliationCommitUnknown {
                    return Err(RoomCommitError::CommitOutcomeUnknown);
                }
                if is_config && mode == DurableMode::ConfigCommitUnknown {
                    return Err(RoomCommitError::CommitOutcomeUnknown);
                }
                Ok(waddle_xmpp::muc::RoomCommitOutcome {
                    coordinates,
                    reservation,
                })
            })
        }

        fn establish_claim_fence(
            &self,
            room_jid: &BareJid,
            fence: waddle_xmpp::muc::RoomClaimFenceContext,
        ) {
            self.fences
                .lock()
                .expect("fences lock")
                .insert(room_jid.clone(), fence);
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, bool> {
            let exact = self.exact_fence_matches(room_jid, fence);
            Box::pin(async move { Ok(exact) })
        }
    }

    async fn fresh_state_with_room_registry(
        mode: DurableMode,
    ) -> (AppState, Arc<TestGroupDmDurableStore>) {
        let db_pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("db pool");
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .expect("migrations");
        let mut state = AppState::new(Arc::new(db_pool));
        let store = TestGroupDmDurableStore::new(mode);
        let registry = waddle_xmpp::muc::room_registry_actor::RoomRegistryActor::spawn(
            waddle_xmpp::muc::room_registry_actor::RoomRegistryActor::new(
                state.muc_domain.to_string(),
                state.occupant_id_secret.clone(),
            ),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let _ = registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "group-dm-test-node",
                    "group-dm-test-epoch",
                )),
                durable_store: Some(store.clone()),
                rollout_backoff: None,
            })
            .await;
        state.room_registry = registry;
        state.clustering_enabled = true;
        (state, store)
    }

    async fn seed_group_dm(
        state: &AppState,
        room_jid: &BareJid,
        name: &str,
        members: &[BareJid],
    ) -> (String, ActorRef<RoomActor>) {
        let channel_id = waddle_xmpp::parse_managed_room_jid(room_jid).expect("managed room jid");
        let config = RoomConfig {
            name: name.to_string(),
            persistent: true,
            members_only: true,
            public_room: false,
            enable_logging: true,
            group_dm: true,
            federated_affiliation_config: FederatedAffiliationConfig::open_none(),
            ..RoomConfig::default()
        };
        let actor = state
            .room_registry
            .ask(CreateRoomWithInitialAffiliations {
                room_jid: room_jid.clone(),
                waddle_id: waddle_xmpp::muc::durable::WaddleId::new(
                    waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.to_string(),
                ),
                channel_id: waddle_xmpp::muc::durable::ChannelId::new(channel_id.clone()),
                config: config.clone(),
                initial_affiliations: members
                    .iter()
                    .cloned()
                    .map(|jid| {
                        waddle_xmpp::muc::DurableAffiliationEntry::new(
                            jid,
                            Some(Affiliation::Member),
                        )
                    })
                    .collect(),
            })
            .await
            .expect("create room");
        upsert_group_dm_catalog(state, &channel_id, &config)
            .await
            .unwrap_or_else(|_| panic!("catalog row"));
        for member in members {
            persist_group_dm_member_tuple(state, &channel_id, member)
                .await
                .expect("member tuple");
            publish_group_dm_bookmark(state, member, room_jid, Some(name))
                .await
                .unwrap_or_else(|_| panic!("member bookmark"));
        }
        (channel_id, actor)
    }

    async fn seed_managed_channel(
        state: &AppState,
        room_jid: &BareJid,
        name: &str,
        affiliations: &[(BareJid, Affiliation)],
    ) -> (String, ActorRef<RoomActor>) {
        let channel_id = waddle_xmpp::parse_managed_room_jid(room_jid).expect("managed room jid");
        let config = RoomConfig {
            name: name.to_string(),
            persistent: true,
            public_room: true,
            enable_logging: true,
            federated_affiliation_config: FederatedAffiliationConfig::open_none(),
            ..RoomConfig::default()
        };
        let actor = state
            .room_registry
            .ask(CreateRoomWithInitialAffiliations {
                room_jid: room_jid.clone(),
                waddle_id: waddle_xmpp::muc::durable::WaddleId::new("admin-v2".to_string()),
                channel_id: waddle_xmpp::muc::durable::ChannelId::new(channel_id.clone()),
                config: config.clone(),
                initial_affiliations: affiliations
                    .iter()
                    .cloned()
                    .map(|(jid, affiliation)| {
                        waddle_xmpp::muc::DurableAffiliationEntry::new(jid, Some(affiliation))
                    })
                    .collect(),
            })
            .await
            .expect("create managed channel");
        upsert_channel_catalog(state, &channel_id, &config, ChannelType::Text)
            .await
            .unwrap_or_else(|_| panic!("catalog row"));
        (channel_id, actor)
    }

    struct RecoveryPublicationHookReset {
        previous: Option<RecoveryPublicationHookEntry>,
        _serial: tokio::sync::OwnedMutexGuard<()>,
    }

    impl Drop for RecoveryPublicationHookReset {
        fn drop(&mut self) {
            restore_recovery_publication_hook(self.previous.take());
        }
    }

    /// The hook slot is process-global, so tests that install one must not
    /// overlap — serialize them on a shared async mutex for the hook's
    /// lifetime.
    async fn install_test_recovery_publication_hook(
        room_jid: BareJid,
        hook: RecoveryPublicationHook,
    ) -> RecoveryPublicationHookReset {
        static HOOK_SERIAL: std::sync::OnceLock<std::sync::Arc<tokio::sync::Mutex<()>>> =
            std::sync::OnceLock::new();
        let serial = std::sync::Arc::clone(
            HOOK_SERIAL.get_or_init(|| std::sync::Arc::new(tokio::sync::Mutex::new(()))),
        )
        .lock_owned()
        .await;
        RecoveryPublicationHookReset {
            previous: install_recovery_publication_hook(room_jid, hook),
            _serial: serial,
        }
    }

    async fn insert_active_lifecycle_row(
        websocket_state: &crate::server::routes::websocket::WebSocketState,
        room_jid: &BareJid,
        lifecycle: RoomLifecycleId,
        revision: i64,
    ) {
        let connection = websocket_state
            .deps
            .protocol
            .room_effect_outbox
            .database()
            .guard()
            .await
            .expect("effect database connection");
        connection
            .execute(
                "CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (lifecycle_id TEXT NOT NULL, room_jid TEXT NOT NULL, revision BIGINT NOT NULL, state TEXT NOT NULL)",
                (),
            )
            .await
            .expect("create lifecycle table");
        connection
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    revision,
                    waddle_xmpp::muc::RoomLifecycleState::Active.as_db_str(),
                ],
            )
            .await
            .expect("insert lifecycle row");
    }

    async fn wait_for_room_effect_to_arm(
        websocket_state: &crate::server::routes::websocket::WebSocketState,
        key: &crate::room_effect_outbox::RoomEffectKey,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if websocket_state
                    .deps
                    .protocol
                    .room_effect_outbox
                    .find(key)
                    .await
                    .expect("find staged effect")
                    .is_some_and(|row| row.available_at_ms != i64::MAX)
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("effect arms");
    }

    async fn room_snapshot(
        state: &AppState,
        room_jid: &BareJid,
    ) -> waddle_xmpp::muc::room_actor::RoomSnapshot {
        state
            .room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("room lookup")
            .expect("room actor")
            .ask(GetSnapshot)
            .await
            .expect("room snapshot")
    }

    #[tokio::test]
    async fn rollback_group_dm_create_keeps_catalog_and_membership_until_destroy_succeeds() {
        let (state, _store) = fresh_state_with_room_registry(DurableMode::DestroyFails).await;
        let room_jid: BareJid = "group-dm-rollback@muc.localhost".parse().expect("room jid");
        let member: BareJid = "alice@localhost".parse().expect("member jid");
        let (channel_id, _actor) =
            seed_group_dm(&state, &room_jid, "Rollback", std::slice::from_ref(&member)).await;

        let error = rollback_group_dm_create(
            &state,
            &channel_id,
            &room_jid,
            std::slice::from_ref(&member),
        )
        .await
        .expect_err("destroy failure must surface");
        let _ = error;

        assert!(
            get_xmpp_channel(state.db_pool.global_actor().clone(), &channel_id)
                .await
                .unwrap_or_else(|_| panic!("channel lookup"))
                .is_some(),
            "catalog must remain until destroy commits"
        );
        assert_eq!(
            list_durable_group_dm_members(&state, &channel_id)
                .await
                .unwrap_or_else(|_| panic!("member lookup")),
            vec![member.clone()],
            "membership tuples must remain until destroy commits"
        );
        let bookmark = existing_group_dm_bookmark(&state, &member, &room_jid)
            .await
            .unwrap_or_else(|_| panic!("bookmark lookup"));
        assert_eq!(
            bookmark.jid, room_jid,
            "bookmark must remain until destroy commits"
        );
        assert!(bookmark.autojoin, "bookmark must still be published");
    }

    #[tokio::test]
    async fn group_dm_leave_ambiguous_commit_does_not_restore_membership_when_leave_committed() {
        let (state, _store) =
            fresh_state_with_room_registry(DurableMode::AffiliationCommitUnknown).await;
        let room_jid: BareJid = "group-dm-leave@muc.localhost".parse().expect("room jid");
        let member: BareJid = "alice@localhost".parse().expect("member jid");
        let caller_full: FullJid = "alice@localhost/web".parse().expect("caller jid");
        let (channel_id, actor) =
            seed_group_dm(&state, &room_jid, "Leave", std::slice::from_ref(&member)).await;
        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: caller_full.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join room");
        let connections = ConnectionRegistry::new();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
        let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
        let connection = ConnectionEntry::new(outbound_tx);
        connections.register_entry(caller_full.clone(), connection.clone());
        user_registry
            .ask(RegisterUserResource {
                jid: caller_full.clone(),
                entry: connection,
            })
            .await
            .expect("register live caller resource");
        let sm_sessions = InMemorySmSessionRegistry::new();
        let pending_local_muc_departures =
            Arc::new(crate::server::routes::websocket::PendingLocalMucDepartures::default());

        let result = run_group_dm_leave(
            &state,
            &pending_local_muc_departures,
            &connections,
            &user_registry,
            &sm_sessions,
            &caller_full,
            &GroupDmLeaveArgs {
                room_jid: room_jid.clone(),
            },
        )
        .await
        .unwrap_or_else(|_| panic!("leave result"));
        assert!(result.left, "the committed leave must still report success");
        assert!(
            !list_durable_group_dm_members(&state, &channel_id)
                .await
                .unwrap_or_else(|_| panic!("member lookup"))
                .contains(&member),
            "a committed ambiguous leave must not restore the permission tuple"
        );
        let bookmark_items = state
            .pubsub_storage
            .get_items(
                &member,
                waddle_xmpp::xep::xep0402::PEP_NODE,
                Some(1),
                &[room_jid.to_string()],
            )
            .await
            .expect("bookmark items");
        assert!(
            bookmark_items.is_empty(),
            "a committed ambiguous leave must not republish the bookmark"
        );
        assert!(
            outbound_rx.try_recv().is_ok(),
            "leave presence should be sent even when ambiguous leave commit is reconciled"
        );
        // The recovered successor carries the transplanted roster: the
        // ordinary leave loop must have removed the leaver's session, not
        // just the affiliation (the ghost-occupant regression).
        let successor = state
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("successor lookup")
            .expect("successor registered");
        let successor_snapshot = successor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("successor snapshot");
        assert!(
            successor_snapshot
                .room
                .find_occupant_by_real_jid(&caller_full)
                .is_none(),
            "the committed ambiguous leave must remove the leaver's session from the successor"
        );
    }

    #[tokio::test]
    async fn admin_removal_reply_timeout_replays_via_janitor_exactly_once() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state_sharing_app_room_registry().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::new(DurableMode::ProjectionLeaveSecondDelayed);
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "group-dm-admin-partial-node",
                    "group-dm-admin-partial-epoch",
                )),
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable group-DM store");

        let room_jid: BareJid = "group-dm-admin-partial@muc.localhost"
            .parse()
            .expect("room JID");
        let member: BareJid = "alice@localhost".parse().expect("member JID");
        let recipient_bare: BareJid = "bob@localhost".parse().expect("recipient JID");
        let alice_phone: FullJid = "alice@localhost/phone".parse().expect("alice phone JID");
        let alice_web: FullJid = "alice@localhost/web".parse().expect("alice web JID");
        let bob_phone: FullJid = "bob@localhost/phone".parse().expect("bob phone JID");
        let (_channel_id, actor) = seed_group_dm(
            state,
            &room_jid,
            "Leave",
            &[member.clone(), recipient_bare.clone()],
        )
        .await;
        actor
            .ask(JoinWithAffiliation {
                sender_jid: alice_phone.clone(),
                nick: "alice".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "localhost".to_string(),
                admission_revision: actor
                    .ask(GetSnapshot)
                    .await
                    .expect("snapshot before alice phone join")
                    .admission_revision,
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("join alice phone");
        actor
            .ask(JoinWithAffiliation {
                sender_jid: alice_web.clone(),
                nick: "alice".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "localhost".to_string(),
                admission_revision: actor
                    .ask(GetSnapshot)
                    .await
                    .expect("snapshot before alice web join")
                    .admission_revision,
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("join alice web");
        actor
            .ask(JoinWithAffiliation {
                sender_jid: bob_phone.clone(),
                nick: "bob".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "localhost".to_string(),
                admission_revision: actor
                    .ask(GetSnapshot)
                    .await
                    .expect("snapshot before bob join")
                    .admission_revision,
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("join bob");

        let (alice_phone_tx, mut alice_phone_rx) = mpsc::channel(8);
        crate::server::routes::websocket::tests::register_test_connection(
            websocket_state.as_ref(),
            &alice_phone,
            alice_phone_tx,
        )
        .await;
        let (alice_web_tx, mut alice_web_rx) = mpsc::channel(8);
        crate::server::routes::websocket::tests::register_test_connection(
            websocket_state.as_ref(),
            &alice_web,
            alice_web_tx,
        )
        .await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        crate::server::routes::websocket::tests::register_test_connection(
            websocket_state.as_ref(),
            &bob_phone,
            bob_tx,
        )
        .await;

        let result = run_group_dm_leave(
            state,
            &websocket_state.deps.protocol.pending_local_muc_departures,
            websocket_state.deps.protocol.connection_registry.as_ref(),
            &websocket_state.deps.protocol.user_registry,
            websocket_state.deps.protocol.sm_session_registry.as_ref(),
            &alice_web,
            &GroupDmLeaveArgs {
                room_jid: room_jid.clone(),
            },
        )
        .await
        .unwrap_or_else(|_| panic!("leave result"));
        assert!(result.left, "membership removal still succeeds");

        let alice_phone_leave = alice_phone_rx
            .try_recv()
            .expect("the first resource receives its self unavailable");
        let waddle_xmpp::Stanza::Presence(alice_phone_presence) = alice_phone_leave.stanza else {
            panic!("expected presence broadcast for the first resource");
        };
        assert_eq!(
            alice_phone_presence.type_,
            xmpp_parsers::presence::Type::Unavailable
        );
        assert!(
            alice_web_rx.try_recv().is_err(),
            "the deferred second resource must not receive an unavailable yet"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "the room must not fan out the final leave before the deferred retry converges"
        );

        let mut pending = websocket_state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_due(std::time::Instant::now());
        assert_eq!(
            pending.len(),
            1,
            "exactly one deferred administrative item is retained"
        );
        let retained = pending.pop().expect("retained departure");
        assert!(matches!(
            retained.item,
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                ref room,
                ref jid,
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Administrative,
                selector: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                ..
            } if room == &room_jid && jid == &alice_web
        ));
        websocket_state
            .deps
            .protocol
            .pending_local_muc_departures
            .requeue_with_backoff(retained);
        assert!(
            websocket_state
                .deps
                .protocol
                .connection_registry
                .get_entry(&alice_web)
                .is_some(),
            "the deferred administrative resource stays live while retained for janitor retry"
        );

        // The requeue backoff is 2s plus up to 25% jitter: wait past the maximum.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        crate::server::session_janitors::run_local_muc_departure_sweep(websocket_state.as_ref())
            .await;

        let alice_web_leave = alice_web_rx
            .try_recv()
            .expect("the janitor must self-notify the deferred live resource");
        let waddle_xmpp::Stanza::Presence(alice_web_presence) = alice_web_leave.stanza else {
            panic!("expected janitor self unavailable");
        };
        assert_eq!(
            alice_web_presence.type_,
            xmpp_parsers::presence::Type::Unavailable
        );
        let bob_leave = bob_rx
            .try_recv()
            .expect("the janitor must fan out the final administrative leave once");
        let waddle_xmpp::Stanza::Presence(bob_presence) = bob_leave.stanza else {
            panic!("expected janitor fan-out unavailable");
        };
        assert_eq!(
            bob_presence.type_,
            xmpp_parsers::presence::Type::Unavailable
        );
        assert!(
            alice_phone_rx.try_recv().is_err(),
            "the already-left first resource must not be notified twice"
        );
        assert!(
            alice_web_rx.try_recv().is_err() && bob_rx.try_recv().is_err(),
            "the janitor retry must emit exactly one group-DM leave shape"
        );
        assert_eq!(
            websocket_state
                .deps
                .protocol
                .pending_local_muc_departures
                .len(),
            0,
            "the deferred administrative item converges once the retry commits"
        );

        let second = run_group_dm_leave(
            state,
            &websocket_state.deps.protocol.pending_local_muc_departures,
            websocket_state.deps.protocol.connection_registry.as_ref(),
            &websocket_state.deps.protocol.user_registry,
            websocket_state.deps.protocol.sm_session_registry.as_ref(),
            &alice_web,
            &GroupDmLeaveArgs { room_jid },
        )
        .await
        .unwrap_or_else(|_| panic!("second leave result"));
        assert!(
            !second.left,
            "retrying the command after convergence exits early"
        );
        assert!(
            alice_phone_rx.try_recv().is_err()
                && alice_web_rx.try_recv().is_err()
                && bob_rx.try_recv().is_err(),
            "the command retry must not refan out any leave presence"
        );
    }

    #[tokio::test]
    async fn group_dm_rename_ambiguous_commit_lookup_failure_hands_the_row_to_retained_recovery() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::ConfigCommitUnknown,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "group-dm-lookup-fail-node",
                    "group-dm-lookup-fail-epoch",
                )),
                durable_store: Some(durable_store.clone()),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable group-DM store");

        let room_jid: BareJid = "group-dm-lookup-fail@muc.localhost"
            .parse()
            .expect("room JID");
        let member: BareJid = "alice@localhost".parse().expect("member JID");
        let caller: FullJid = "alice@localhost/web".parse().expect("caller JID");
        let (channel_id, actor) =
            seed_group_dm(state, &room_jid, "Before", std::slice::from_ref(&member)).await;
        let initial_coordinates = *durable_store
            .coordinates
            .lock()
            .expect("coordinates lock")
            .get(&room_jid)
            .expect("initial committed coordinates");
        websocket_state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "group-dm-lookup-fail".to_owned(),
                channel_id: "group-dm-lookup-fail".to_owned(),
                config: RoomConfig::default(),
            })
            .await
            .expect("make recovered room locally drainable");
        let connection = websocket_state
            .deps
            .protocol
            .room_effect_outbox
            .database()
            .guard()
            .await
            .expect("effect database connection");
        connection
            .execute(
                "CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (lifecycle_id TEXT NOT NULL, room_jid TEXT NOT NULL, revision BIGINT NOT NULL, state TEXT NOT NULL)",
                (),
            )
            .await
            .expect("create lifecycle table");
        connection
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params![
                    initial_coordinates.0.to_string(),
                    room_jid.to_string(),
                    initial_coordinates.1,
                    waddle_xmpp::muc::RoomLifecycleState::Active.as_db_str(),
                ],
            )
            .await
            .expect("insert initial lifecycle");
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: caller.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join group DM");
        let (sender, _receiver) = mpsc::channel(1);
        crate::server::routes::websocket::tests::register_test_connection(
            websocket_state.as_ref(),
            &caller,
            sender,
        )
        .await;

        // The rename commits durably but its acknowledgement is lost, and the
        // exact reservation lookup during reconciliation fails transiently:
        // the committed row must still be armed by the retained recovery —
        // nothing else arms live-origin rows.
        let committed_before = *durable_store
            .coordinates
            .lock()
            .expect("coordinates lock")
            .get(&room_jid)
            .expect("committed coordinates after join");
        let config_lifecycle = committed_before.0;
        let config_revision = waddle_xmpp::muc::RoomRevision::from_stored(committed_before.1 + 1)
            .expect("next revision");
        websocket_state
            .deps
            .protocol
            .room_effect_outbox
            .fail_next_staged_reservation_lookup_for_test(config_lifecycle, config_revision);

        let connections = ConnectionRegistry::new();
        let result = run_group_dm_rename(
            state,
            websocket_state.as_ref(),
            &connections,
            &caller,
            &GroupDmRenameArgs {
                room_jid: room_jid.clone(),
                name: Some("After".to_owned()),
            },
        )
        .await;
        assert!(
            result.is_err(),
            "the transient lookup failure surfaces to the caller; the committed row is recovered \
             out of band"
        );
        let _ = channel_id;

        let key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: config_lifecycle,
            revision: config_revision,
            ordinal: waddle_xmpp::muc::RoomEffectOrdinal::first(),
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let row = websocket_state
                .deps
                .protocol
                .room_effect_outbox
                .find(&key)
                .await
                .expect("find staged effect");
            if row
                .as_ref()
                .is_some_and(|row| row.available_at_ms != i64::MAX)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the retained recovery must arm the committed rename reservation after the \
                 lookup failure; row = {row:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    #[tokio::test]
    async fn group_dm_rename_recovers_and_arms_the_committed_config_reservation() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::ConfigCommitUnknown,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "group-dm-recovery-node",
                    "group-dm-recovery-epoch",
                )),
                durable_store: Some(durable_store.clone()),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable group-DM store");

        let room_jid: BareJid = "group-dm-recovery@muc.localhost".parse().expect("room JID");
        let member: BareJid = "alice@localhost".parse().expect("member JID");
        let caller: FullJid = "alice@localhost/web".parse().expect("caller JID");
        let (channel_id, actor) =
            seed_group_dm(state, &room_jid, "Before", std::slice::from_ref(&member)).await;
        let initial_coordinates = *durable_store
            .coordinates
            .lock()
            .expect("coordinates lock")
            .get(&room_jid)
            .expect("initial committed coordinates");
        websocket_state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "group-dm-recovery".to_owned(),
                channel_id: "group-dm-recovery".to_owned(),
                config: RoomConfig::default(),
            })
            .await
            .expect("make recovered room locally drainable");
        let connection = websocket_state
            .deps
            .protocol
            .room_effect_outbox
            .database()
            .guard()
            .await
            .expect("effect database connection");
        connection
            .execute(
                "CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (lifecycle_id TEXT NOT NULL, room_jid TEXT NOT NULL, revision BIGINT NOT NULL, state TEXT NOT NULL)",
                (),
            )
            .await
            .expect("create lifecycle table");
        connection
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params![
                    initial_coordinates.0.to_string(),
                    room_jid.to_string(),
                    initial_coordinates.1,
                    waddle_xmpp::muc::RoomLifecycleState::Active.as_db_str(),
                ],
            )
            .await
            .expect("insert initial lifecycle");
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: caller.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join group DM");
        let (sender, mut receiver) = mpsc::channel(1);
        crate::server::routes::websocket::tests::register_test_connection(
            websocket_state.as_ref(),
            &caller,
            sender,
        )
        .await;

        let connections = ConnectionRegistry::new();
        let result = run_group_dm_rename(
            state,
            websocket_state.as_ref(),
            &connections,
            &caller,
            &GroupDmRenameArgs {
                room_jid: room_jid.clone(),
                name: Some("After".to_owned()),
            },
        )
        .await
        .unwrap_or_else(|_| panic!("ambiguous committed rename reconciles"));
        assert_eq!(result.room_jid, room_jid);
        assert_eq!(result.name, Some("After".to_owned()));
        assert!(
            get_xmpp_channel(state.db_pool.global_actor().clone(), &channel_id)
                .await
                .expect("updated catalog lookup")
                .is_some_and(|record| record.name == "After"),
            "handler proceeds with catalog work after reconciling the committed config"
        );

        let drain_state = Arc::clone(&websocket_state);
        let drain = tokio::spawn(async move {
            crate::room_effect_outbox::drain::drain_due_effects(
                drain_state.as_ref(),
                crate::time::now_ms(),
                8,
            )
            .await
        });
        let outbound = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .expect("reconciled effect reaches the recipient")
            .expect("recipient connection remains open");
        outbound
            .write_acceptance
            .as_ref()
            .expect("reconciled effect waits for write acceptance")
            .acknowledge();
        assert_eq!(
            drain
                .await
                .expect("reconciled drain joins")
                .expect("reconciled drain succeeds")
                .drained,
            1,
            "the recovered reservation drains exactly once"
        );
    }

    #[tokio::test]
    async fn cancelled_admin_channel_update_ask_recovers_and_arms_the_committed_reservation() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "admin-channel-cancel-node",
                    "admin-channel-cancel-epoch",
                )),
                durable_store: Some(durable_store.clone()),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable channel store");

        let room_jid: BareJid = "admin-channel-cancel@muc.localhost"
            .parse()
            .expect("room JID");
        let owner_bare: BareJid = "owner@localhost".parse().expect("owner JID");
        let _ = seed_managed_channel(
            state,
            &room_jid,
            "Before",
            &[(owner_bare, Affiliation::Owner)],
        )
        .await;

        let pause = durable_store.pause_next_commit_reply();
        let task_state = Arc::clone(&websocket_state);
        let task_room = room_jid.clone();
        let task = tokio::spawn(async move {
            let connections = ConnectionRegistry::new();
            let _ = run_update(
                task_state.deps.app_state.as_ref(),
                task_state.as_ref(),
                &connections,
                &ChannelsUpdateArgs {
                    channel_jid: task_room,
                    name: Some("After".to_owned()),
                    topic: None,
                    channel_type: None,
                    is_public: None,
                    members_only: None,
                },
                None,
            )
            .await;
        });

        pause.wait_until_reached().await;
        let (lifecycle, revision) = *durable_store
            .coordinates
            .lock()
            .expect("coordinates lock")
            .get(&room_jid)
            .expect("committed coordinates");
        let key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle,
            revision: RoomRevision::from_stored(revision).expect("revision"),
            ordinal: waddle_xmpp::muc::RoomEffectOrdinal::first(),
        };

        pause.release();
        task.abort();
        let _ = task.await;

        wait_for_room_effect_to_arm(websocket_state.as_ref(), &key).await;
    }

    #[tokio::test]
    async fn cancelled_config_recovery_arms_matching_reservations() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        websocket_state
            .deps
            .protocol
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "cancelled-config-recovery-node",
                    "cancelled-config-recovery-epoch",
                )),
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable room registry");

        let room_jid: BareJid = "cancelled-config-recovery@muc.example.com"
            .parse()
            .expect("room JID");
        let actor = websocket_state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoomWithInitialAffiliations {
                room_jid: room_jid.clone(),
                waddle_id: waddle_xmpp::muc::durable::WaddleId::new(
                    "admin-channel-test".to_owned(),
                ),
                channel_id: waddle_xmpp::muc::durable::ChannelId::new(
                    "cancelled-config-recovery".to_owned(),
                ),
                config: RoomConfig::default(),
                initial_affiliations: Vec::new(),
            })
            .await
            .expect("create durable room");
        let recipient: FullJid = "alice@example.com/admin".parse().expect("recipient JID");
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: recipient,
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join config-effect recipient");
        let updated_config = RoomConfig {
            name: "After".to_owned(),
            ..RoomConfig::default()
        };
        let applied = actor
            .ask(waddle_xmpp::muc::room_actor::UpdateConfig {
                config: updated_config.clone(),
                effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("commit config");
        let superseded_snapshot = actor.ask(GetSnapshot).await.expect("config snapshot");
        let superseded_reservation = applied.reservation.expect("staged config reservation");
        let superseded_key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: superseded_reservation.lifecycle,
            revision: superseded_reservation.revision,
            ordinal: superseded_reservation.ordinals[0],
        };
        let latest_config = RoomConfig {
            name: "Latest".to_owned(),
            ..RoomConfig::default()
        };
        let latest_applied = actor
            .ask(waddle_xmpp::muc::room_actor::UpdateConfig {
                config: latest_config.clone(),
                effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("commit latest config");
        let latest_reservation = latest_applied
            .reservation
            .expect("latest staged config reservation");
        let latest_key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: latest_reservation.lifecycle,
            revision: latest_reservation.revision,
            ordinal: latest_reservation.ordinals[0],
        };
        let store = websocket_state.deps.protocol.room_effect_outbox.as_ref();
        assert_eq!(
            store
                .find(&superseded_key)
                .await
                .expect("superseded staged row lookup")
                .expect("superseded staged row")
                .available_at_ms,
            i64::MAX,
            "the superseded config row starts inert"
        );
        assert_eq!(
            store
                .find(&latest_key)
                .await
                .expect("latest staged row lookup")
                .expect("latest staged row")
                .available_at_ms,
            i64::MAX,
            "the latest config row starts inert"
        );

        CancelledConfigAskRecoveryGuard::arm_only(
            websocket_state.as_ref(),
            &actor,
            &room_jid,
            &updated_config,
            superseded_snapshot.config_revision,
        )
        .recover()
        .await;
        wait_for_room_effect_to_arm(websocket_state.as_ref(), &superseded_key).await;
        wait_for_room_effect_to_arm(websocket_state.as_ref(), &latest_key).await;

        let exact_room_jid: BareJid = "cancelled-config-recovery-exact@muc.example.com"
            .parse()
            .expect("exact room JID");
        let exact_actor = websocket_state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoomWithInitialAffiliations {
                room_jid: exact_room_jid.clone(),
                waddle_id: waddle_xmpp::muc::durable::WaddleId::new(
                    "admin-channel-test-exact".to_owned(),
                ),
                channel_id: waddle_xmpp::muc::durable::ChannelId::new(
                    "cancelled-config-recovery-exact".to_owned(),
                ),
                config: RoomConfig::default(),
                initial_affiliations: Vec::new(),
            })
            .await
            .expect("create durable exact room");
        exact_actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: "alice@example.com/exact"
                    .parse()
                    .expect("exact recipient JID"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join exact config recipient");
        let previous_exact_config = RoomConfig {
            name: "Earlier".to_owned(),
            ..RoomConfig::default()
        };
        let previous_exact = exact_actor
            .ask(waddle_xmpp::muc::room_actor::UpdateConfig {
                config: previous_exact_config,
                effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("commit earlier exact config");
        let previous_exact_reservation = previous_exact
            .reservation
            .expect("earlier exact staged reservation");
        let previous_exact_key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: previous_exact_reservation.lifecycle,
            revision: previous_exact_reservation.revision,
            ordinal: previous_exact_reservation.ordinals[0],
        };
        let exact_config = RoomConfig {
            name: "Exact".to_owned(),
            ..RoomConfig::default()
        };
        let exact_applied = exact_actor
            .ask(waddle_xmpp::muc::room_actor::UpdateConfig {
                config: exact_config.clone(),
                effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("commit exact config");
        let exact_snapshot = exact_actor.ask(GetSnapshot).await.expect("exact snapshot");
        let exact_reservation = exact_applied
            .reservation
            .expect("exact staged config reservation");
        let exact_key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: exact_reservation.lifecycle,
            revision: exact_reservation.revision,
            ordinal: exact_reservation.ordinals[0],
        };
        CancelledConfigAskRecoveryGuard::arm_only(
            websocket_state.as_ref(),
            &exact_actor,
            &exact_room_jid,
            &exact_config,
            exact_snapshot.config_revision,
        )
        .recover()
        .await;
        wait_for_room_effect_to_arm(websocket_state.as_ref(), &exact_key).await;
        assert_eq!(
            store
                .find(&previous_exact_key)
                .await
                .expect("previous exact staged row lookup")
                .expect("previous exact staged row")
                .available_at_ms,
            i64::MAX,
            "the exact branch still arms only its exact config row"
        );
    }

    #[tokio::test]
    async fn cancelled_group_dm_member_config_arms_its_committed_reservation() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "group-dm-cancellation-node",
                    "group-dm-cancellation-epoch",
                )),
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable group-DM store");

        let room_jid: BareJid = "group-dm-cancellation@muc.localhost"
            .parse()
            .expect("room JID");
        let member: BareJid = "alice@localhost".parse().expect("member JID");
        let caller: FullJid = "alice@localhost/web".parse().expect("caller JID");
        let (_channel_id, actor) =
            seed_group_dm(state, &room_jid, "Before", std::slice::from_ref(&member)).await;
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: caller.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("seat group-DM member");
        let mut updated = actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .config;
        updated.name = "After".to_owned();
        let applied = actor
            .ask(waddle_xmpp::muc::room_actor::UpdateGroupDmConfigByMember {
                config: updated,
                sender_jid: caller,
            })
            .await
            .expect("member config commit");
        let reservation = applied.reservation.expect("staged config reservation");
        let key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: reservation.lifecycle,
            revision: reservation.revision,
            ordinal: reservation.ordinals[0],
        };
        let store = websocket_state.deps.protocol.room_effect_outbox.as_ref();
        assert_eq!(
            store
                .find(&key)
                .await
                .expect("staged row lookup")
                .expect("staged row")
                .available_at_ms,
            i64::MAX,
            "the config reservation starts inert"
        );

        // This drop is the cancellation window before `run_group_dm_rename`
        // reaches its catalog and bookmark awaits.
        drop(CommittedAdminConfigReservationGuard::new(
            websocket_state.as_ref(),
            Some(reservation),
        ));

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if store
                    .find(&key)
                    .await
                    .expect("armed row lookup")
                    .expect("armed row")
                    .available_at_ms
                    != i64::MAX
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled member-config producer arms its committed reservation");
    }

    #[tokio::test]
    async fn cancelled_config_recovery_retries_snapshot_timeout_then_arms() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "group-dm-cancel-ask-node",
                    "group-dm-cancel-ask-epoch",
                )),
                durable_store: Some(durable_store.clone()),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable group-DM store");

        let room_jid: BareJid = "group-dm-cancel-ask@muc.localhost"
            .parse()
            .expect("room JID");
        let member: BareJid = "alice@localhost".parse().expect("member JID");
        let caller: FullJid = "alice@localhost/web".parse().expect("caller JID");
        let (_channel_id, actor) =
            seed_group_dm(state, &room_jid, "Before", std::slice::from_ref(&member)).await;
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: caller.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join group DM");

        let pause = durable_store.pause_next_commit_reply();
        let task_state = Arc::clone(&websocket_state);
        let task_room = room_jid.clone();
        let task_caller = caller.clone();
        let task = tokio::spawn(async move {
            let connections = ConnectionRegistry::new();
            let _ = run_group_dm_rename(
                task_state.deps.app_state.as_ref(),
                task_state.as_ref(),
                &connections,
                &task_caller,
                &GroupDmRenameArgs {
                    room_jid: task_room,
                    name: Some("After".to_owned()),
                },
            )
            .await;
        });

        pause.wait_until_reached().await;
        let (lifecycle, revision) = *durable_store
            .coordinates
            .lock()
            .expect("coordinates lock")
            .get(&room_jid)
            .expect("committed coordinates");
        let key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle,
            revision: RoomRevision::from_stored(revision).expect("revision"),
            ordinal: waddle_xmpp::muc::RoomEffectOrdinal::first(),
        };

        task.abort();
        let _ = task.await;

        // The recovery guard now asks a still-paused actor. Let that first
        // snapshot ask exceed `ADMIN_ROOM_ASK_TIMEOUT`, then release the
        // actor so the retry loop can snapshot and arm the reservation.
        tokio::time::sleep(ADMIN_ROOM_ASK_TIMEOUT + std::time::Duration::from_secs(1)).await;
        pause.release();

        // The first snapshot ask timed out while the actor was paused; the
        // recovery retries after `retry_delay_ms(1)` (5s), so arming lands
        // around 10s after cancellation. Poll well past that.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
        loop {
            let armed = websocket_state
                .deps
                .protocol
                .room_effect_outbox
                .find(&key)
                .await
                .expect("find staged effect")
                .is_some_and(|row| row.available_at_ms != i64::MAX);
            if armed {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the retried recovery must arm the committed reservation"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    #[tokio::test]
    async fn cancelled_config_recovery_retries_a_transient_reservation_lookup_failure() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "group-dm-cancel-lookup-node",
                    "group-dm-cancel-lookup-epoch",
                )),
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable group-DM store");

        let room_jid: BareJid = "group-dm-cancel-lookup@muc.localhost"
            .parse()
            .expect("room JID");
        let member: BareJid = "alice@localhost".parse().expect("member JID");
        let caller: FullJid = "alice@localhost/web".parse().expect("caller JID");
        let (_channel_id, actor) =
            seed_group_dm(state, &room_jid, "Before", std::slice::from_ref(&member)).await;
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: caller.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join group DM");

        let mut updated = actor.ask(GetSnapshot).await.expect("snapshot").room.config;
        updated.name = "After".to_owned();
        let applied = actor
            .ask(waddle_xmpp::muc::room_actor::UpdateGroupDmConfigByMember {
                config: updated,
                sender_jid: caller,
            })
            .await
            .expect("config commit");
        let reservation = applied.reservation.expect("staged config reservation");
        let key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: reservation.lifecycle,
            revision: reservation.revision,
            ordinal: reservation.ordinals[0],
        };
        websocket_state
            .deps
            .protocol
            .room_effect_outbox
            .fail_next_staged_reservation_lookup_for_test(
                reservation.lifecycle,
                reservation.revision,
            );

        drop(CommittedAdminConfigReservationGuard::new(
            websocket_state.as_ref(),
            Some(reservation),
        ));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let armed = websocket_state
                .deps
                .protocol
                .room_effect_outbox
                .find(&key)
                .await
                .expect("find staged effect")
                .is_some_and(|row| row.available_at_ms != i64::MAX);
            if armed {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the retried recovery must arm the committed reservation after one lookup failure"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    #[tokio::test]
    async fn cancelled_config_recovery_keeps_retrying_lookup_failures_until_success() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        websocket_state
            .deps
            .protocol
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "cancelled-config-retry-node",
                    "cancelled-config-retry-epoch",
                )),
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable room registry");

        let room_jid: BareJid = "cancelled-config-retry@muc.example.com"
            .parse()
            .expect("room JID");
        let actor = websocket_state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoomWithInitialAffiliations {
                room_jid: room_jid.clone(),
                waddle_id: waddle_xmpp::muc::durable::WaddleId::new(
                    "admin-channel-retry-test".to_owned(),
                ),
                channel_id: waddle_xmpp::muc::durable::ChannelId::new(
                    "cancelled-config-retry".to_owned(),
                ),
                config: RoomConfig::default(),
                initial_affiliations: Vec::new(),
            })
            .await
            .expect("create durable room");
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: "alice@example.com/admin".parse().expect("recipient JID"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join config-effect recipient");

        let exact_config = RoomConfig {
            name: "Exact".to_owned(),
            ..RoomConfig::default()
        };
        let exact_applied = actor
            .ask(waddle_xmpp::muc::room_actor::UpdateConfig {
                config: exact_config.clone(),
                effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("commit exact config");
        let exact_snapshot = actor.ask(GetSnapshot).await.expect("exact snapshot");
        let exact_reservation = exact_applied.reservation.expect("exact staged reservation");
        let exact_key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: exact_reservation.lifecycle,
            revision: exact_reservation.revision,
            ordinal: exact_reservation.ordinals[0],
        };
        let store = Arc::clone(&websocket_state.deps.protocol.room_effect_outbox);
        store.fail_staged_reservation_lookup_times_for_test(
            exact_reservation.lifecycle,
            exact_reservation.revision,
            2,
        );
        let exact_recovery = tokio::spawn({
            let websocket_state = Arc::clone(&websocket_state);
            let actor = actor.clone();
            let room_jid = room_jid.clone();
            async move {
                CancelledConfigAskRecoveryGuard::arm_only(
                    websocket_state.as_ref(),
                    &actor,
                    &room_jid,
                    &exact_config,
                    exact_snapshot.config_revision,
                )
                .recover()
                .await;
            }
        });

        // retry_delay_ms(1) + retry_delay_ms(2) is 5s + 10s. Allow 25s for
        // those backoffs plus scheduler/database overhead before declaring recovery stuck.
        let exact_deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
        loop {
            if store
                .find(&exact_key)
                .await
                .expect("find exact staged row")
                .is_some_and(|row| row.available_at_ms != i64::MAX)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < exact_deadline,
                "the exact recovery must arm after two lookup failures"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        exact_recovery.await.expect("exact recovery joins");

        let superseded_config = RoomConfig {
            name: "Superseded".to_owned(),
            ..RoomConfig::default()
        };
        let superseded_applied = actor
            .ask(waddle_xmpp::muc::room_actor::UpdateConfig {
                config: superseded_config.clone(),
                effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("commit superseded config");
        let superseded_snapshot = actor
            .ask(GetSnapshot)
            .await
            .expect("superseded config snapshot");
        let superseded_reservation = superseded_applied
            .reservation
            .expect("superseded staged reservation");
        let superseded_key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: superseded_reservation.lifecycle,
            revision: superseded_reservation.revision,
            ordinal: superseded_reservation.ordinals[0],
        };
        let latest_config = RoomConfig {
            name: "Latest".to_owned(),
            ..RoomConfig::default()
        };
        let latest_applied = actor
            .ask(waddle_xmpp::muc::room_actor::UpdateConfig {
                config: latest_config,
                effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("commit latest config");
        let latest_reservation = latest_applied
            .reservation
            .expect("latest staged reservation");
        let latest_key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: latest_reservation.lifecycle,
            revision: latest_reservation.revision,
            ordinal: latest_reservation.ordinals[0],
        };
        let latest_snapshot = actor
            .ask(GetSnapshot)
            .await
            .expect("latest config snapshot");
        assert_eq!(
            latest_snapshot.config_revision,
            superseded_snapshot.config_revision + 1,
            "the latest config must force the superseded recovery branch"
        );
        store.fail_staged_reservations_up_to_lookup_times_for_test(
            latest_reservation.lifecycle,
            latest_reservation.revision,
            2,
        );
        let superseded_recovery = tokio::spawn({
            let websocket_state = Arc::clone(&websocket_state);
            let actor = actor.clone();
            let room_jid = room_jid.clone();
            async move {
                CancelledConfigAskRecoveryGuard::arm_only(
                    websocket_state.as_ref(),
                    &actor,
                    &room_jid,
                    &superseded_config,
                    superseded_snapshot.config_revision,
                )
                .recover()
                .await;
            }
        });

        let superseded_deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
        loop {
            let superseded_armed = store
                .find(&superseded_key)
                .await
                .expect("find superseded staged row")
                .is_some_and(|row| row.available_at_ms != i64::MAX);
            let latest_armed = store
                .find(&latest_key)
                .await
                .expect("find latest staged row")
                .is_some_and(|row| row.available_at_ms != i64::MAX);
            if superseded_armed && latest_armed {
                break;
            }
            assert!(
                std::time::Instant::now() < superseded_deadline,
                "the superseded recovery must arm every committed inert row after two lookup failures"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        superseded_recovery
            .await
            .expect("superseded recovery joins");
    }

    #[tokio::test]
    async fn cancelled_channel_members_only_flip_enforces_and_supersedes_config_fallback() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "members-only-cancellation-node",
                    "members-only-cancellation-epoch",
                )),
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable channel store");

        let room_jid: BareJid = "members-only-cancellation@muc.localhost"
            .parse()
            .expect("room JID");
        let owner_bare: BareJid = "owner@localhost".parse().expect("owner JID");
        let non_member_bare: BareJid = "bob@localhost".parse().expect("non-member JID");
        let owner: FullJid = "owner@localhost/web".parse().expect("owner session");
        let non_member: FullJid = "bob@localhost/web".parse().expect("non-member session");
        let (_channel_id, actor) = seed_managed_channel(
            state,
            &room_jid,
            "Before",
            &[(owner_bare.clone(), Affiliation::Owner)],
        )
        .await;
        for (nick, real_jid, affiliation) in [
            ("owner", owner.clone(), Affiliation::Owner),
            ("bob", non_member.clone(), Affiliation::None),
        ] {
            actor
                .ask(Join {
                    nick: nick.to_owned(),
                    real_jid,
                    role: Role::Participant,
                    affiliation,
                })
                .await
                .expect("seat test occupant");
        }
        let mut open_config = actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("initial snapshot")
            .room
            .config;
        open_config.members_only = false;
        let _ = actor
            .ask(UpdateConfig {
                config: open_config,
                effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("open the seeded channel");
        let previous = actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .config;
        let mut updated = previous.clone();
        updated.members_only = true;
        let config_status_codes = waddle_xmpp::muc::config_change_status_codes(&previous, &updated);
        let committed = actor
            .ask(UpdateConfig {
                config: updated,
                effect_plan:
                    waddle_xmpp::muc::room_actor::ConfigEffectPlan::ManagedMembersOnlyFallback,
            })
            .await
            .expect("members-only config commit");
        let fallback_reservation = committed.reservation.expect("staged fallback reservation");
        let lifecycle = fallback_reservation.lifecycle;
        let fallback_revision = fallback_reservation.revision;
        let mut guard = CommittedAdminConfigReservationGuard::new(
            websocket_state.as_ref(),
            Some(fallback_reservation.clone()),
        );
        guard.defer_to_members_only_enforcement(PendingAdminMembersOnlyEnforcement {
            actor: actor.clone(),
            affiliations: vec![
                (owner_bare, Affiliation::Owner),
                (non_member_bare, Affiliation::None),
            ],
            fallback_reservation: Some(fallback_reservation),
            config_status_codes,
            room_jid: room_jid.clone(),
            sfu: None,
            arm_supervisor: websocket_state
                .deps
                .protocol
                .room_effect_arm_supervisor
                .clone(),
        });

        // This drop models cancellation after the config commit but before the
        // caller reaches `EnforceMembersOnlyAffiliations`.
        drop(guard);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let snapshot = actor
                    .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
                    .await
                    .expect("post-cancellation snapshot");
                if snapshot
                    .room
                    .find_occupant_by_real_jid(&non_member)
                    .is_none()
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached enforcement evicts the non-member");

        let rows = websocket_state
            .deps
            .protocol
            .room_effect_outbox
            .list_for_lifecycle(lifecycle)
            .await
            .expect("list durable effects");
        let config_recipients: Vec<_> = rows
            .iter()
            .filter(|row| row.key.revision > fallback_revision)
            .filter_map(|row| match &row.effect {
                waddle_xmpp::muc::RoomEffect::ConfigChanged { recipients, .. } => {
                    Some(recipients.as_slice())
                }
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(
            config_recipients,
            vec![&owner],
            "the enforcement supersedes the fallback and leaves one config notification"
        );
        assert!(
            rows.iter().any(|row| matches!(
                &row.effect,
                waddle_xmpp::muc::RoomEffect::AdminSelfNotify { updates }
                    if row.key.revision > fallback_revision
                        && updates.iter().any(|update| update.recipient == non_member)
            )),
            "the replacement effect includes the non-member's status-322 removal"
        );
    }

    #[tokio::test]
    async fn rollback_clears_admin_channel_members_only_enforcement_before_guard_drop() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "admin-rollback-clear-node",
                    "admin-rollback-clear-epoch",
                )),
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable channel store");

        let room_jid: BareJid = "admin-rollback-clear@muc.localhost"
            .parse()
            .expect("room JID");
        let owner_bare: BareJid = "owner@localhost".parse().expect("owner JID");
        let non_member_bare: BareJid = "bob@localhost".parse().expect("non-member JID");
        let owner: FullJid = "owner@localhost/web".parse().expect("owner session");
        let non_member: FullJid = "bob@localhost/web".parse().expect("non-member session");
        let (_channel_id, actor) = seed_managed_channel(
            state,
            &room_jid,
            "Before",
            &[(owner_bare.clone(), Affiliation::Owner)],
        )
        .await;
        for (nick, real_jid, affiliation) in [
            ("owner", owner.clone(), Affiliation::Owner),
            ("bob", non_member.clone(), Affiliation::None),
        ] {
            actor
                .ask(Join {
                    nick: nick.to_owned(),
                    real_jid,
                    role: Role::Participant,
                    affiliation,
                })
                .await
                .expect("seat test occupant");
        }
        let mut open_config = actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("initial snapshot")
            .room
            .config;
        open_config.members_only = false;
        let _ = actor
            .ask(UpdateConfig {
                config: open_config,
                effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("open the seeded channel");
        let previous = actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("previous snapshot")
            .room
            .config;
        let mut updated = previous.clone();
        updated.members_only = true;
        let committed = actor
            .ask(UpdateConfig {
                config: updated.clone(),
                effect_plan:
                    waddle_xmpp::muc::room_actor::ConfigEffectPlan::ManagedMembersOnlyFallback,
            })
            .await
            .expect("members-only config commit");
        let fallback_reservation = committed.reservation.expect("staged fallback reservation");
        let mut guard = CommittedAdminConfigReservationGuard::new(
            websocket_state.as_ref(),
            Some(fallback_reservation.clone()),
        );
        guard.defer_to_members_only_enforcement(PendingAdminMembersOnlyEnforcement {
            actor: actor.clone(),
            affiliations: vec![
                (owner_bare, Affiliation::Owner),
                (non_member_bare, Affiliation::None),
            ],
            fallback_reservation: Some(fallback_reservation),
            config_status_codes: waddle_xmpp::muc::config_change_status_codes(&previous, &updated),
            room_jid: room_jid.clone(),
            sfu: None,
            arm_supervisor: websocket_state
                .deps
                .protocol
                .room_effect_arm_supervisor
                .clone(),
        });
        guard.clear_members_only_enforcement();
        assert!(
            rollback_room_config_or_arm(
                websocket_state.as_ref(),
                &actor,
                committed.revision,
                previous.clone(),
                guard.reservation().cloned(),
            )
            .await,
            "rollback must restore the pre-update config"
        );
        drop(guard);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let snapshot = actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("post-rollback snapshot");
        assert!(
            !snapshot.room.config.members_only,
            "rollback must restore the non-members-only config"
        );
        assert!(
            snapshot
                .room
                .find_occupant_by_real_jid(&non_member)
                .is_some(),
            "rollback must not detach stale deferred enforcement against a restored occupant"
        );
    }

    #[tokio::test]
    async fn group_dm_rename_recovery_preserves_post_publication_leave_and_join() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::ConfigCommitUnknown,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "group-dm-race-node",
                    "group-dm-race-epoch",
                )),
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable group-DM store");

        let room_jid: BareJid = "group-dm-race@muc.localhost".parse().expect("room JID");
        let member: BareJid = "alice@localhost".parse().expect("member JID");
        let caller: FullJid = "alice@localhost/web".parse().expect("caller JID");
        let joiner: FullJid = "bob@localhost/phone".parse().expect("joiner JID");
        let (_channel_id, actor) =
            seed_group_dm(state, &room_jid, "Before", std::slice::from_ref(&member)).await;
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: caller.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join group DM");
        let _hook = install_test_recovery_publication_hook(
            room_jid.clone(),
            std::sync::Arc::new({
                let caller = caller.clone();
                let joiner = joiner.clone();
                move |actor: ActorRef<RoomActor>| {
                    let caller = caller.clone();
                    let joiner = joiner.clone();
                    Box::pin(async move {
                        assert!(
                            matches!(
                                actor
                                    .ask(LeaveByRealJid {
                                        sender_jid: caller.clone(),
                                        cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Administrative,
                                        session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                                        attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                                        origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
})
                                    .await
                                    .expect("leave recovered room"),
                                waddle_xmpp::muc::room_actor::LeaveDisposition::Left(_)
                            ),
                            "seeded caller must be able to leave the recovered actor"
                        );
                        actor
                            .ask(Join {
                                nick: "bob".to_owned(),
                                real_jid: joiner.clone(),
                                role: Role::Participant,
                                affiliation: Affiliation::Member,
                            })
                            .await
                            .expect("join recovered room");
                    })
                }
            }),
        )
        .await;

        assert!(
            run_group_dm_rename(
                state,
                websocket_state.as_ref(),
                &ConnectionRegistry::new(),
                &caller,
                &GroupDmRenameArgs {
                    room_jid: room_jid.clone(),
                    name: Some("After".to_owned()),
                },
            )
            .await
            .is_ok(),
            "ambiguous committed rename reconciles"
        );

        let snapshot = room_snapshot(state, &room_jid).await;
        assert!(
            snapshot.room.find_occupant_by_real_jid(&caller).is_none(),
            "a post-publication leave must not be replayed from the stale actor"
        );
        assert!(
            snapshot.room.find_occupant_by_real_jid(&joiner).is_some(),
            "a post-publication join must survive recovery"
        );
    }

    #[tokio::test]
    async fn channel_update_recovery_preserves_post_publication_leave_and_join() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::ConfigCommitUnknown,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "channel-race-node",
                    "channel-race-epoch",
                )),
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable channel store");

        let room_jid: BareJid = "channel-race@muc.localhost".parse().expect("room JID");
        let owner_bare: BareJid = "owner@localhost".parse().expect("owner bare JID");
        let owner: FullJid = "owner@localhost/web".parse().expect("owner JID");
        let joiner: FullJid = "bob@localhost/phone".parse().expect("joiner JID");
        let (_channel_id, actor) = seed_managed_channel(
            state,
            &room_jid,
            "Before",
            &[(owner_bare.clone(), Affiliation::Owner)],
        )
        .await;
        actor
            .ask(Join {
                nick: "owner".to_owned(),
                real_jid: owner.clone(),
                role: Role::Moderator,
                affiliation: Affiliation::Owner,
            })
            .await
            .expect("join managed channel");
        let _hook = install_test_recovery_publication_hook(
            room_jid.clone(),
            std::sync::Arc::new({
                let owner = owner.clone();
                let joiner = joiner.clone();
                move |actor: ActorRef<RoomActor>| {
                    let owner = owner.clone();
                    let joiner = joiner.clone();
                    Box::pin(async move {
                        assert!(
                            matches!(
                                actor
                                    .ask(LeaveByRealJid {
                                        sender_jid: owner.clone(),
                                        cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Administrative,
                                        session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                                        attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                                        origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
})
                                    .await
                                    .expect("leave recovered channel"),
                                waddle_xmpp::muc::room_actor::LeaveDisposition::Left(_)
                            ),
                            "seeded owner must be able to leave the recovered actor"
                        );
                        actor
                            .ask(Join {
                                nick: "bob".to_owned(),
                                real_jid: joiner.clone(),
                                role: Role::Participant,
                                affiliation: Affiliation::Member,
                            })
                            .await
                            .expect("join recovered channel");
                    })
                }
            }),
        )
        .await;

        assert!(
            run_update(
                state,
                websocket_state.as_ref(),
                &ConnectionRegistry::new(),
                &ChannelsUpdateArgs {
                    channel_jid: room_jid.clone(),
                    name: Some("After".to_owned()),
                    topic: None,
                    channel_type: None,
                    is_public: None,
                    members_only: None,
                },
                None,
            )
            .await
            .is_ok(),
            "ambiguous committed channel update reconciles"
        );

        let snapshot = room_snapshot(state, &room_jid).await;
        assert!(
            snapshot.room.find_occupant_by_real_jid(&owner).is_none(),
            "a post-publication leave must not be replayed into the recovered channel"
        );
        assert!(
            snapshot.room.find_occupant_by_real_jid(&joiner).is_some(),
            "a post-publication join must survive the recovered channel publish"
        );
    }

    #[tokio::test]
    async fn admin_affiliation_command_returns_before_async_drain_and_effect_still_delivers() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        websocket_state
            .deps
            .protocol
            .room_effect_arm_supervisor
            .attach_drain_state(&websocket_state);
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "admin-affiliation-node",
                    "admin-affiliation-epoch",
                )),
                durable_store: Some(durable_store.clone()),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable store");

        let room_jid: BareJid = "admin-affiliation@muc.localhost".parse().expect("room JID");
        let owner_bare: BareJid = "owner@localhost".parse().expect("owner bare JID");
        let caller_bare: BareJid = "alice@localhost".parse().expect("caller bare JID");
        let caller: FullJid = "alice@localhost/web".parse().expect("caller JID");
        let (_channel_id, actor) = seed_managed_channel(
            state,
            &room_jid,
            "General",
            &[
                (owner_bare.clone(), Affiliation::Owner),
                (caller_bare.clone(), Affiliation::Admin),
            ],
        )
        .await;
        // The caller's join commits a #1647 occupancy projection revision, so
        // read the seeded coordinates only after it.
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: caller.clone(),
                role: Role::Moderator,
                affiliation: Affiliation::Admin,
            })
            .await
            .expect("join caller");
        let seeded_coordinates = *durable_store
            .coordinates
            .lock()
            .expect("coordinates lock")
            .get(&room_jid)
            .expect("seeded coordinates");
        insert_active_lifecycle_row(
            websocket_state.as_ref(),
            &room_jid,
            seeded_coordinates.0,
            seeded_coordinates.1 + 1,
        )
        .await;
        let (sender, mut receiver) = mpsc::channel(4);
        crate::server::routes::websocket::tests::register_test_connection(
            websocket_state.as_ref(),
            &caller,
            sender,
        )
        .await;

        // Promptness bound: anything comfortably below the 5s write-acceptance
        // timeout proves the command did not wait on its own socket; CI's
        // loaded nextest workers need headroom beyond 1s.
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            run_set_affiliation(
                state,
                Some(websocket_state.as_ref()),
                websocket_state.deps.protocol.connection_registry.as_ref(),
                &caller_bare,
                &ChannelsSetAffiliationArgs {
                    channel_jid: room_jid.clone(),
                    member_jid: caller_bare.clone(),
                    affiliation: WireAffiliation::Member,
                    reason: None,
                },
                None,
            ),
        )
        .await
        .expect("admin command must not wait for write acceptance")
        .unwrap_or_else(|_| panic!("affiliation command succeeds"));

        let key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: seeded_coordinates.0,
            revision: RoomRevision::from_stored(seeded_coordinates.1 + 1).expect("revision"),
            ordinal: waddle_xmpp::muc::RoomEffectOrdinal::first(),
        };
        wait_for_room_effect_to_arm(websocket_state.as_ref(), &key).await;

        // Drain-and-poll instead of one spawned drain + one recv: the
        // command's own detached drain can already hold (or have released
        // with backoff) the row's lease, so a single drain pass at a fixed
        // instant races it. Alternate recv attempts with fresh drain passes
        // (using a due-time far in the future so backoff never hides the
        // row) until the frame lands.
        let outbound = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if let Ok(frame) =
                    tokio::time::timeout(std::time::Duration::from_millis(200), receiver.recv())
                        .await
                {
                    return frame.expect("caller connection remains open");
                }
                let _ = crate::room_effect_outbox::drain::drain_due_effects(
                    websocket_state.as_ref(),
                    crate::time::now_ms()
                        .saturating_add(crate::room_effect_outbox::MAX_RETRY_DELAY_MS),
                    8,
                )
                .await;
            }
        })
        .await
        .expect("async drain reaches caller connection");
        outbound
            .write_acceptance
            .as_ref()
            .expect("admin effect retains write acceptance")
            .acknowledge();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if websocket_state
                    .deps
                    .protocol
                    .room_effect_outbox
                    .queue_depth()
                    .await
                    .expect("queue depth")
                    == 0
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("row completes after caller write acceptance");
    }

    #[tokio::test]
    async fn group_dm_rename_success_stages_arms_and_drains_its_config_effect() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "admin-success-node",
                    "admin-success-epoch",
                )),
                durable_store: Some(durable_store.clone()),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable store");

        let room_jid: BareJid = "admin-success@muc.localhost".parse().expect("room JID");
        let member: BareJid = "alice@localhost".parse().expect("member JID");
        let caller: FullJid = "alice@localhost/web".parse().expect("caller JID");
        let (_channel_id, actor) =
            seed_group_dm(state, &room_jid, "Before", std::slice::from_ref(&member)).await;
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: caller.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join group DM");
        let (sender, mut receiver) = mpsc::channel(1);
        crate::server::routes::websocket::tests::register_test_connection(
            websocket_state.as_ref(),
            &caller,
            sender,
        )
        .await;

        assert!(
            run_group_dm_rename(
                state,
                websocket_state.as_ref(),
                websocket_state.deps.protocol.connection_registry.as_ref(),
                &caller,
                &GroupDmRenameArgs {
                    room_jid: room_jid.clone(),
                    name: Some("After".to_owned()),
                },
            )
            .await
            .is_ok(),
            "normal admin group-DM rename succeeds"
        );

        let coordinates = *durable_store
            .coordinates
            .lock()
            .expect("coordinates lock")
            .get(&room_jid)
            .expect("committed coordinates");
        websocket_state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "admin-success".to_owned(),
                channel_id: "admin-success".to_owned(),
                config: RoomConfig::default(),
            })
            .await
            .expect("make room locally drainable");
        let connection = websocket_state
            .deps
            .protocol
            .room_effect_outbox
            .database()
            .guard()
            .await
            .expect("effect database connection");
        connection
            .execute(
                "CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (lifecycle_id TEXT NOT NULL, room_jid TEXT NOT NULL, revision BIGINT NOT NULL, state TEXT NOT NULL)",
                (),
            )
            .await
            .expect("create lifecycle table");
        connection
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params![
                    coordinates.0.to_string(),
                    room_jid.to_string(),
                    coordinates.1,
                    waddle_xmpp::muc::RoomLifecycleState::Active.as_db_str(),
                ],
            )
            .await
            .expect("insert committed lifecycle");
        let key = crate::room_effect_outbox::RoomEffectKey {
            lifecycle: coordinates.0,
            revision: RoomRevision::from_stored(coordinates.1).expect("revision"),
            ordinal: waddle_xmpp::muc::RoomEffectOrdinal::first(),
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if websocket_state
                    .deps
                    .protocol
                    .room_effect_outbox
                    .find(&key)
                    .await
                    .expect("find staged effect")
                    .is_some_and(|row| row.available_at_ms != i64::MAX)
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("successful handler arms its config effect");

        let drain_state = Arc::clone(&websocket_state);
        let drain = tokio::spawn(async move {
            crate::room_effect_outbox::drain::drain_due_effects(
                drain_state.as_ref(),
                crate::time::now_ms(),
                8,
            )
            .await
        });
        let outbound = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .expect("drain sends config message")
            .expect("recipient connection remains open");
        outbound
            .write_acceptance
            .as_ref()
            .expect("outbox delivery retains write acceptance")
            .acknowledge();
        assert_eq!(
            drain
                .await
                .expect("drain joins")
                .expect("drain succeeds")
                .drained,
            1,
            "the admin producer-created config row drains exactly once"
        );
    }

    #[tokio::test]
    async fn admin_config_outer_failure_rolls_back_the_exact_staged_reservation() {
        let websocket_state =
            crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let state = websocket_state.deps.app_state.as_ref();
        let durable_store = TestGroupDmDurableStore::with_outbox(
            DurableMode::CommitSucceeds,
            Arc::clone(&websocket_state.deps.protocol.room_effect_outbox),
        );
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "admin-rollback-node",
                    "admin-rollback-epoch",
                )),
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable store");

        let room_jid: BareJid = "admin-rollback@muc.localhost".parse().expect("room JID");
        let member: BareJid = "alice@localhost".parse().expect("member JID");
        let caller: FullJid = "alice@localhost/web".parse().expect("caller JID");
        let (_channel_id, actor) =
            seed_group_dm(state, &room_jid, "Before", std::slice::from_ref(&member)).await;
        actor
            .ask(Join {
                nick: "alice".to_owned(),
                real_jid: caller.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join group DM");
        let (sender, mut receiver) = mpsc::channel(1);
        crate::server::routes::websocket::tests::register_test_connection(
            websocket_state.as_ref(),
            &caller,
            sender,
        )
        .await;
        let previous = actor.ask(GetConfig).await.expect("original config");
        let applied = actor
            .ask(UpdateConfig {
                config: RoomConfig {
                    name: "After".to_owned(),
                    ..previous.clone()
                },
                effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("durably stage config before outer work fails");
        let reservation = applied.reservation.expect("staged config reservation");

        assert!(
            rollback_room_config_or_arm(
                websocket_state.as_ref(),
                &actor,
                applied.revision,
                previous,
                Some(reservation.clone()),
            )
            .await,
            "a simulated outer failure commits the compensating config rollback"
        );
        assert!(
            websocket_state
                .deps
                .protocol
                .room_effect_outbox
                .find(&crate::room_effect_outbox::RoomEffectKey {
                    lifecycle: reservation.lifecycle,
                    revision: reservation.revision,
                    ordinal: reservation.ordinals[0],
                })
                .await
                .expect("find rolled-back effect")
                .is_none(),
            "RollbackConfigIfRevision deletes only the reservation from the failed outer operation"
        );
        assert_eq!(
            crate::room_effect_outbox::drain::drain_due_effects(
                websocket_state.as_ref(),
                crate::time::now_ms(),
                8,
            )
            .await
            .expect("post-rollback drain succeeds")
            .drained,
            0,
            "the rolled-back reservation leaves no due effect to deliver"
        );
        assert!(
            receiver.try_recv().is_err(),
            "the failed outer operation sends no config message"
        );
    }
}
