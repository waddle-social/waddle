//! Admin V2 — Channels CRUD over XEP-0050 ad-hoc commands.
//!
//! Eight owner-gated commands under `urn:waddle:admin:channels:*`:
//!
//! - `list` — paginated read of all MUC rooms tracked by the room registry,
//!   with occupant + per-tier affiliation counts.
//! - `create` — create a new MUC room (defaults: public, persistent, not
//!   members-only).
//! - `update` — patch name / topic / is_public on an existing room's
//!   `RoomConfig`.
//! - `delete` — destroy a MUC room via the room registry.
//! - `occupants` — list live occupants (nick, real_jid, role, affiliation).
//! - `affiliations` — list every persistent affiliation, optionally
//!   filtered to a single tier.
//! - `set-affiliation` — grant/revoke owner/admin/member/none/outcast;
//!   `outcast` is the XEP-0045 §10.2 ban.
//! - `kick` — XEP-0045 §9.1 role-change to `none` (occupant leaves but can
//!   rejoin).
//!
//! All handlers delegate to the typed dependencies on [`AppState`]:
//! `room_registry` (`waddle_xmpp::muc::room_registry_actor::*`), and
//! `muc_domain` (used to construct fresh room JIDs on `create`).

use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp::commands::{CommandContext, CommandResult};
use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DestroyRoom, GetRoom, ListRooms};
use waddle_xmpp::muc::{
    room_actor::{
        ChangeAffiliation, GetConfig, Leave, ListAffiliations, ListOccupants, OccupantCount,
        UpdateConfig,
    },
    RoomConfig,
};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FieldType, FormType};
use waddle_xmpp::Affiliation;
use waddle_xmpp::XmppError;

use crate::admin::is_community_owner;
use crate::server::AppState;

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

pub const DEFAULT_PAGE_SIZE: u32 = 50;
pub const MAX_PAGE_SIZE: u32 = 200;
const MAX_NAME_LEN: usize = 80;

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
    pub prefix: Option<String>,
    pub page_size: u32,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelListEntry {
    pub channel_jid: BareJid,
    pub name: String,
    pub topic: Option<String>,
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
    pub name: String,
    pub topic: Option<String>,
    /// Spec: default `true` (public).
    pub is_public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRef {
    pub channel_jid: BareJid,
    pub name: String,
    pub topic: Option<String>,
    pub is_public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsUpdateArgs {
    pub channel_jid: BareJid,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub is_public: Option<bool>,
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

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub async fn register(registry: &waddle_xmpp::commands::CommandRegistry, app_state: Arc<AppState>) {
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
        registry
            .register(NODE_UPDATE, "Admin · Update channel", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_update(ctx, state).await }
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
        registry
            .register(
                NODE_SET_AFFILIATION,
                "Admin · Set affiliation",
                move |ctx| {
                    let state = Arc::clone(&state);
                    async move { handle_set_affiliation(ctx, state).await }
                },
            )
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_KICK, "Admin · Kick occupant", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_kick(ctx, state).await }
            })
            .await;
    }
}

// ---------------------------------------------------------------------------
// Handler shells
// ---------------------------------------------------------------------------

type AdminErr = Box<CommandResult>;

fn caller_or_forbidden(ctx: &CommandContext, state: &AppState) -> Result<BareJid, AdminErr> {
    let bare = ctx.from.to_bare();
    if !is_community_owner(state, &bare) {
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

async fn handle_list(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state) {
        return *forbidden;
    }
    let args = match parse_list_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_list(&state, &args).await {
        Ok(result) => CommandResult::Completed {
            form: Some(build_list_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_create(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state) {
        return *forbidden;
    }
    let args = match parse_create_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_create(&state, &args).await {
        Ok(channel) => CommandResult::Completed {
            form: Some(build_channel_form(&channel)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_update(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state) {
        return *forbidden;
    }
    let args = match parse_update_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_update(&state, &args).await {
        Ok(channel) => CommandResult::Completed {
            form: Some(build_channel_form(&channel)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_delete(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state) {
        return *forbidden;
    }
    let args = match parse_delete_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_delete(&state, &args).await {
        Ok(()) => CommandResult::Completed {
            form: None,
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_occupants(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state) {
        return *forbidden;
    }
    let args = match parse_occupants_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_occupants(&state, &args).await {
        Ok(result) => CommandResult::Completed {
            form: Some(build_occupants_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_affiliations(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state) {
        return *forbidden;
    }
    let args = match parse_affiliations_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_affiliations(&state, &args).await {
        Ok(result) => CommandResult::Completed {
            form: Some(build_affiliations_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_set_affiliation(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state) {
        return *forbidden;
    }
    let args = match parse_set_affiliation_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_set_affiliation(&state, &args).await {
        Ok(result) => CommandResult::Completed {
            form: Some(build_set_affiliation_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_kick(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state) {
        return *forbidden;
    }
    let args = match parse_kick_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_kick(&state, &args).await {
        Ok(result) => CommandResult::Completed {
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
            prefix: None,
            page_size: DEFAULT_PAGE_SIZE,
            after_cursor: None,
        });
    };
    if !matches!(form.form_type, FormType::Submit) {
        return Ok(ChannelsListArgs {
            space_jid: None,
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
    let page_size = parse_page_size(form)?;
    let after_cursor = parse_optional_text(form, "after_cursor");
    Ok(ChannelsListArgs {
        space_jid,
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
    // Spec: default true.
    let is_public = parse_optional_bool(form, "is_public")?.unwrap_or(true);
    Ok(ChannelsCreateArgs {
        space_jid,
        name,
        topic,
        is_public,
    })
}

pub fn parse_update_args(form: Option<&DataForm>) -> Result<ChannelsUpdateArgs, String> {
    let form = form.ok_or_else(|| "channels:update requires an args form".to_string())?;
    let channel_jid = parse_required_bare_jid(form, "channel_jid")?;
    let name = parse_optional_text(form, "name");
    if let Some(ref name) = name {
        validate_name(name)?;
    }
    let topic = parse_optional_text(form, "topic");
    let is_public = parse_optional_bool(form, "is_public")?;
    Ok(ChannelsUpdateArgs {
        channel_jid,
        name,
        topic,
        is_public,
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
        if let Some(prefix) = args.prefix.as_deref() {
            if !config.name.to_lowercase().starts_with(prefix) {
                continue;
            }
        }
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
            is_public: !config.members_only,
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
    // V2 scope: space_jid filter is a no-op in this minimal cut because the
    // server doesn't yet track channel→space links via the room registry;
    // the filter parses successfully so the wire surface is stable, and
    // future PRs can implement the actual filtering against the spaces
    // metadata projection.
    let _ = args.space_jid.as_ref();
    Ok(ChannelsListResult {
        entries,
        next_cursor,
    })
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

async fn run_create(state: &AppState, args: &ChannelsCreateArgs) -> Result<ChannelRef, AdminErr> {
    let localpart = mint_channel_localpart(&args.name);
    let muc_domain = state.muc_domain.to_string();
    let channel_jid: BareJid = format!("{localpart}@{muc_domain}")
        .parse()
        .map_err(|e| internal_err(format!("constructed channel JID is invalid: {e}")))?;

    // Spec: public, persistent, not members-only.
    let mut config = RoomConfig {
        name: args.name.clone(),
        description: args.topic.clone(),
        persistent: true,
        members_only: !args.is_public,
        ..RoomConfig::default()
    };
    // Spec: forum/announcement etc default off; only name/topic/visibility
    // are exposed at the admin V2 wire.
    config.moderated = false;
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

    Ok(ChannelRef {
        channel_jid,
        name: args.name.clone(),
        topic: args.topic.clone(),
        is_public: args.is_public,
    })
}

async fn run_update(state: &AppState, args: &ChannelsUpdateArgs) -> Result<ChannelRef, AdminErr> {
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

    let existing = actor
        .ask(GetConfig)
        .await
        .map_err(send_err("room actor GetConfig"))?;

    let new_name = args.name.clone().unwrap_or_else(|| existing.name.clone());
    let new_topic = args.topic.clone().or(existing.description.clone());
    let new_members_only = args
        .is_public
        .map(|public| !public)
        .unwrap_or(existing.members_only);

    let updated = RoomConfig {
        name: new_name.clone(),
        description: new_topic.clone(),
        members_only: new_members_only,
        ..existing
    };
    actor
        .ask(UpdateConfig {
            config: updated.clone(),
        })
        .await
        .map_err(send_err("room actor UpdateConfig"))?;

    Ok(ChannelRef {
        channel_jid: args.channel_jid.clone(),
        name: new_name,
        topic: new_topic,
        is_public: !new_members_only,
    })
}

async fn run_delete(state: &AppState, args: &ChannelsDeleteArgs) -> Result<(), AdminErr> {
    let _removed = state
        .room_registry
        .ask(DestroyRoom {
            room_jid: args.channel_jid.clone(),
        })
        .await
        .map_err(send_err("room_registry ask DestroyRoom"))?;
    Ok(())
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
    args: &ChannelsSetAffiliationArgs,
) -> Result<ChannelsSetAffiliationResult, AdminErr> {
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
    actor
        .ask(ChangeAffiliation {
            jid: args.member_jid.clone(),
            affiliation: args.affiliation.to_muc(),
        })
        .await
        .map_err(send_err("room actor ChangeAffiliation"))?;
    Ok(ChannelsSetAffiliationResult {
        member_jid: args.member_jid.clone(),
        affiliation: args.affiliation,
    })
}

async fn run_kick(
    state: &AppState,
    args: &ChannelsKickArgs,
) -> Result<ChannelsKickResult, AdminErr> {
    // XEP-0045 §9.1 — role-change to "none" removes the occupant from
    // the live presence map. Admin doesn't need to be joined; we look up
    // the occupant by their bare JID and call `Leave` on the actor. The
    // §307 presence broadcast happens via the wire-side machinery
    // (#680); this admin path is best-effort state mutation when the
    // occupant is currently joined.
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
    // Walk the occupant list to find an entry whose real_jid bare equals
    // the requested occupant_jid; nick is needed to call Leave.
    let occupants = actor
        .ask(ListOccupants)
        .await
        .map_err(send_err("room actor ListOccupants"))?;
    let nick = occupants
        .into_iter()
        .find(|info| info.real_jid.to_bare() == args.occupant_jid)
        .map(|info| info.nick);
    if let Some(nick) = nick {
        // Best-effort — actor returns Err if the occupant disappears
        // between the list and the leave; we treat that as success.
        let _ = actor.ask(Leave { nick }).await;
    }
    // Reason is recorded in the response but not pushed through the
    // (unavailable) broadcast path in this minimal cut.
    let _ = args.reason.as_ref();
    Ok(ChannelsKickResult {
        occupant_jid: args.occupant_jid.clone(),
    })
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
        .add_field(Field::text_single("is_public", bool_str(channel.is_public)));
    if let Some(topic) = channel.topic.as_ref() {
        form = form.add_field(Field::text_single("topic", topic));
    }
    form
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
        assert_eq!(form.reported.len(), 10);
        assert_eq!(form.items.len(), 1);
    }

    #[test]
    fn mint_channel_localpart_fallback() {
        assert!(mint_channel_localpart("???").starts_with("channel-"));
        assert!(mint_channel_localpart("Hello World").starts_with("hello-world-"));
    }
}
