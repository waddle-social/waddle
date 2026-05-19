//! Admin V2 — Spaces CRUD over XEP-0050 ad-hoc commands.
//!
//! Six owner-gated commands under `urn:waddle:admin:spaces:*`:
//!
//! - `list`   — paginated read of spaces (name/description/icon + counts).
//! - `create` — create a new space (name 1–80, optional description/icon).
//! - `update` — edit name/description/icon for an existing space.
//! - `delete` — destroy a space and cascade-destroy its channels.
//! - `members` — paginated read of a space's pubsub-affiliation roster.
//! - `set-role` — change a member's pubsub affiliation (owner/admin/member/none).
//!
//! All handlers follow the same shape as admin V1's `users_list`:
//!
//! 1. ACL-check via [`crate::admin::is_community_owner`].
//! 2. Parse the typed `<x type='submit'>` data form into a Rust struct.
//! 3. Delegate to the typed dependencies on `AppState`
//!    ([`crate::spaces_metadata::SpacesMetadataStore`] + the pubsub storage
//!    trait + the MUC room registry actor for cascade-destroy).
//! 4. Return a typed `<x type='result'>` data form.
//!
//! ## Vocabulary mapping
//!
//! The wire vocabulary for space membership (`owner`/`admin`/`member`) maps
//! onto XEP-0060 PubSub affiliations on the space's PubSub node (the space
//! JID is owned by [`crate::server::AppState::spaces_jid`] and the node name
//! is the space's localpart). `owner` ↔ [`waddle_xmpp_core::pubsub::Affiliation::Owner`],
//! `admin` ↔ [`waddle_xmpp_core::pubsub::Affiliation::Publisher`] (the
//! highest read+write tier short of Owner), `member` ↔
//! [`waddle_xmpp_core::pubsub::Affiliation::Member`], `none` removes the row.

use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp::commands::{CommandContext, CommandResult};
use waddle_xmpp::muc::room_actor::GetConfig;
use waddle_xmpp::muc::room_registry_actor::GetRoom;
use waddle_xmpp::pubsub::{Affiliation as PubSubAffiliation, PubSubItem};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FieldType, FormType};
use waddle_xmpp::{ChannelInfo, XmppError};

use crate::admin::is_community_owner;
use crate::channel_space_links::ChannelSpaceLink;
use crate::permissions::{
    DeleteTuple, Object, ObjectType, PermissionError, Relation, Subject, SubjectType, Tuple,
    WriteTuple,
};
use crate::server::AppState;
use crate::spaces_metadata::{SpaceMetadata, SpacesMetadataError};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// XEP-0050 node + XEP-0004 FORM_TYPE for `spaces:list`.
pub const NODE_LIST: &str = waddle_xmpp::admin::NS_ADMIN_SPACES_LIST;
/// XEP-0050 node + XEP-0004 FORM_TYPE for `spaces:create`.
pub const NODE_CREATE: &str = waddle_xmpp::admin::NS_ADMIN_SPACES_CREATE;
/// XEP-0050 node + XEP-0004 FORM_TYPE for `spaces:update`.
pub const NODE_UPDATE: &str = waddle_xmpp::admin::NS_ADMIN_SPACES_UPDATE;
/// XEP-0050 node + XEP-0004 FORM_TYPE for `spaces:delete`.
pub const NODE_DELETE: &str = waddle_xmpp::admin::NS_ADMIN_SPACES_DELETE;
/// XEP-0050 node + XEP-0004 FORM_TYPE for `spaces:members`.
pub const NODE_MEMBERS: &str = waddle_xmpp::admin::NS_ADMIN_SPACES_MEMBERS;
/// XEP-0050 node + XEP-0004 FORM_TYPE for `spaces:set-role`.
pub const NODE_SET_ROLE: &str = waddle_xmpp::admin::NS_ADMIN_SPACES_SET_ROLE;

/// Default page size when callers omit `page_size`.
pub const DEFAULT_PAGE_SIZE: u32 = 50;
/// Hard cap on `page_size` regardless of the requested value.
pub const MAX_PAGE_SIZE: u32 = 200;

/// Spec-mandated max length on a space name (1–80).
const MAX_NAME_LEN: usize = 80;

// ---------------------------------------------------------------------------
// Wire role vocabulary
// ---------------------------------------------------------------------------

/// Typed role vocabulary the wire commands accept on `set-role` and emit on
/// `members`. The mapping onto XEP-0060 affiliations is explicit so the
/// boundary never carries an untyped `String`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceRole {
    Owner,
    Admin,
    Member,
    None,
}

impl SpaceRole {
    /// Wire spelling matching the spec (`owner` / `admin` / `member` / `none`).
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::None => "none",
        }
    }

    /// Parse from the wire spelling; returns `Err` for anything else so a
    /// typo doesn't silently demote a member.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "owner" => Ok(Self::Owner),
            "admin" => Ok(Self::Admin),
            "member" => Ok(Self::Member),
            "none" => Ok(Self::None),
            other => Err(format!(
                "role must be owner|admin|member|none, got '{other}'"
            )),
        }
    }

    /// Project onto the corresponding XEP-0060 PubSub affiliation. `None`
    /// has no on-the-wire affiliation equivalent — callers translate it to
    /// "remove the row" themselves before reaching storage.
    pub fn to_pubsub(self) -> PubSubAffiliation {
        match self {
            Self::Owner => PubSubAffiliation::Owner,
            Self::Admin => PubSubAffiliation::Publisher,
            Self::Member => PubSubAffiliation::Member,
            Self::None => PubSubAffiliation::None,
        }
    }

    /// Lift a PubSub affiliation into the wire role vocabulary. `Outcast`
    /// is collapsed to `None` because the spaces wire surface does not
    /// expose a banned tier (channels do).
    pub fn from_pubsub(aff: PubSubAffiliation) -> Self {
        match aff {
            PubSubAffiliation::Owner => Self::Owner,
            PubSubAffiliation::Publisher => Self::Admin,
            PubSubAffiliation::Member => Self::Member,
            PubSubAffiliation::PublishOnly => Self::Admin,
            PubSubAffiliation::None | PubSubAffiliation::Outcast => Self::None,
        }
    }
}

// ---------------------------------------------------------------------------
// Typed argument / result structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpacesListArgs {
    pub prefix: Option<String>,
    pub page_size: u32,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceListEntry {
    pub space_jid: BareJid,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    pub channel_count: u32,
    pub member_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpacesListResult {
    pub entries: Vec<SpaceListEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpacesCreateArgs {
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceRef {
    pub space_jid: BareJid,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpacesUpdateArgs {
    pub space_jid: BareJid,
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpacesDeleteArgs {
    pub space_jid: BareJid,
    pub confirm: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpacesMembersArgs {
    pub space_jid: BareJid,
    pub page_size: u32,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceMemberEntry {
    pub jid: BareJid,
    pub role: SpaceRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpacesMembersResult {
    pub entries: Vec<SpaceMemberEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpacesSetRoleArgs {
    pub space_jid: BareJid,
    pub member_jid: BareJid,
    pub role: SpaceRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpacesSetRoleResult {
    pub member_jid: BareJid,
    pub role: SpaceRole,
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register all six spaces ad-hoc commands on `registry`.
pub async fn register(registry: &waddle_xmpp::commands::CommandRegistry, app_state: Arc<AppState>) {
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_LIST, "Admin · List spaces", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_list(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_CREATE, "Admin · Create space", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_create(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_UPDATE, "Admin · Update space", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_update(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_DELETE, "Admin · Delete space", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_delete(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_MEMBERS, "Admin · List space members", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_members(ctx, state).await }
            })
            .await;
    }
    {
        let state = Arc::clone(&app_state);
        registry
            .register(NODE_SET_ROLE, "Admin · Set space role", move |ctx| {
                let state = Arc::clone(&state);
                async move { handle_set_role(ctx, state).await }
            })
            .await;
    }
}

// ---------------------------------------------------------------------------
// Handler shells (ACL + parse + delegate + build response)
// ---------------------------------------------------------------------------

/// Boxed `CommandResult::Error` carrier so handler internals can return a
/// compact `Result` without tripping `clippy::result_large_err`.
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

fn map_metadata_err(error: SpacesMetadataError) -> AdminErr {
    internal_err(format!("spaces metadata storage: {error}"))
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
        Ok(space) => CommandResult::Completed {
            form: Some(build_space_form(&space)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_update(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state).await {
        return *forbidden;
    }
    let args = match parse_update_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_update(&state, &args).await {
        Ok(space) => CommandResult::Completed {
            form: Some(build_space_form(&space)),
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
            form: None,
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_members(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state).await {
        return *forbidden;
    }
    let args = match parse_members_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_members(&state, &args).await {
        Ok(result) => CommandResult::Completed {
            form: Some(build_members_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

async fn handle_set_role(ctx: CommandContext, state: Arc<AppState>) -> CommandResult {
    if let Err(forbidden) = caller_or_forbidden(&ctx, &state).await {
        return *forbidden;
    }
    let args = match parse_set_role_args(ctx.command.form.as_ref()) {
        Ok(args) => args,
        Err(error) => return *bad_request(error),
    };
    match run_set_role(&state, &args).await {
        Ok(result) => CommandResult::Completed {
            form: Some(build_set_role_form(&result)),
            notes: vec![],
        },
        Err(result) => *result,
    }
}

// ---------------------------------------------------------------------------
// Argument parsers (XEP-0004 submit form → typed struct)
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

pub fn parse_list_args(form: Option<&DataForm>) -> Result<SpacesListArgs, String> {
    let Some(form) = form else {
        return Ok(SpacesListArgs {
            prefix: None,
            page_size: DEFAULT_PAGE_SIZE,
            after_cursor: None,
        });
    };
    if !matches!(form.form_type, FormType::Submit) {
        return Ok(SpacesListArgs {
            prefix: None,
            page_size: DEFAULT_PAGE_SIZE,
            after_cursor: None,
        });
    }
    let prefix = form
        .get_value("prefix")
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    let page_size = parse_page_size(form)?;
    let after_cursor = parse_optional_text(form, "after_cursor");
    Ok(SpacesListArgs {
        prefix,
        page_size,
        after_cursor,
    })
}

pub fn parse_create_args(form: Option<&DataForm>) -> Result<SpacesCreateArgs, String> {
    let form = form.ok_or_else(|| "spaces:create requires an args form".to_string())?;
    let name = parse_required_text(form, "name")?;
    validate_name(&name)?;
    let description = parse_optional_text(form, "description");
    let icon_url = parse_optional_text(form, "icon_url");
    Ok(SpacesCreateArgs {
        name,
        description,
        icon_url,
    })
}

pub fn parse_update_args(form: Option<&DataForm>) -> Result<SpacesUpdateArgs, String> {
    let form = form.ok_or_else(|| "spaces:update requires an args form".to_string())?;
    let space_jid = parse_required_bare_jid(form, "space_jid")?;
    let name = parse_optional_text(form, "name");
    if let Some(ref name) = name {
        validate_name(name)?;
    }
    let description = parse_optional_text(form, "description");
    let icon_url = parse_optional_text(form, "icon_url");
    Ok(SpacesUpdateArgs {
        space_jid,
        name,
        description,
        icon_url,
    })
}

pub fn parse_delete_args(form: Option<&DataForm>) -> Result<SpacesDeleteArgs, String> {
    let form = form.ok_or_else(|| "spaces:delete requires an args form".to_string())?;
    let space_jid = parse_required_bare_jid(form, "space_jid")?;
    let confirm = parse_required_text(form, "confirm")?;
    if confirm != "yes" {
        return Err("spaces:delete requires confirm='yes'".to_string());
    }
    Ok(SpacesDeleteArgs { space_jid, confirm })
}

pub fn parse_members_args(form: Option<&DataForm>) -> Result<SpacesMembersArgs, String> {
    let form = form.ok_or_else(|| "spaces:members requires an args form".to_string())?;
    let space_jid = parse_required_bare_jid(form, "space_jid")?;
    let page_size = parse_page_size(form)?;
    let after_cursor = parse_optional_text(form, "after_cursor");
    Ok(SpacesMembersArgs {
        space_jid,
        page_size,
        after_cursor,
    })
}

pub fn parse_set_role_args(form: Option<&DataForm>) -> Result<SpacesSetRoleArgs, String> {
    let form = form.ok_or_else(|| "spaces:set-role requires an args form".to_string())?;
    let space_jid = parse_required_bare_jid(form, "space_jid")?;
    let member_jid = parse_required_bare_jid(form, "member_jid")?;
    let role_raw = parse_required_text(form, "role")?;
    let role = SpaceRole::parse(&role_raw)?;
    Ok(SpacesSetRoleArgs {
        space_jid,
        member_jid,
        role,
    })
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

// ---------------------------------------------------------------------------
// Delegating handlers
// ---------------------------------------------------------------------------

fn now_unix_seconds() -> i64 {
    chrono::Utc::now().timestamp()
}

fn space_node_name(space_jid: &BareJid) -> Option<String> {
    space_jid.node().map(|n| n.to_string())
}

async fn run_list(state: &AppState, args: &SpacesListArgs) -> Result<SpacesListResult, AdminErr> {
    let mut metadata_rows = state
        .spaces_metadata_store
        .list_all()
        .await
        .map_err(map_metadata_err)?;

    if let Some(prefix) = args.prefix.as_deref() {
        metadata_rows.retain(|row| row.name.to_lowercase().starts_with(prefix));
    }

    if let Some(cursor) = args.after_cursor.as_deref() {
        metadata_rows.retain(|row| row.space_jid.to_string().as_str() > cursor);
    }
    metadata_rows.sort_by(|a, b| a.space_jid.cmp(&b.space_jid));

    let limit = args.page_size as usize;
    let total = metadata_rows.len();
    let mut entries = Vec::with_capacity(limit.min(total));

    for row in metadata_rows.iter().take(limit) {
        let (channel_count, member_count) = counts_for_space(state, &row.space_jid).await?;
        entries.push(SpaceListEntry {
            space_jid: row.space_jid.clone(),
            name: row.name.clone(),
            description: row.description.clone(),
            icon_url: row.icon_url.clone(),
            channel_count,
            member_count,
        });
    }

    let next_cursor = if total > limit {
        entries.last().map(|entry| entry.space_jid.to_string())
    } else {
        None
    };
    Ok(SpacesListResult {
        entries,
        next_cursor,
    })
}

async fn counts_for_space(state: &AppState, space_jid: &BareJid) -> Result<(u32, u32), AdminErr> {
    let Some(node) = space_node_name(space_jid) else {
        return Ok((0, 0));
    };
    let items = state
        .pubsub_storage
        .get_items(&state.spaces_jid, &node, None, &[])
        .await
        .map_err(|e| internal_err(format!("pubsub get_items failed: {e}")))?;
    let affiliations = state
        .pubsub_storage
        .list_node_affiliations(&state.spaces_jid, &node)
        .await
        .map_err(|e| internal_err(format!("pubsub list_node_affiliations failed: {e}")))?;
    let channel_count = u32::try_from(items.len()).unwrap_or(u32::MAX);
    let member_count = u32::try_from(
        affiliations
            .iter()
            .filter(|(_, aff)| !matches!(aff, PubSubAffiliation::Outcast | PubSubAffiliation::None))
            .count(),
    )
    .unwrap_or(u32::MAX);
    Ok((channel_count, member_count))
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

async fn write_channel_parent_tuple(
    state: &AppState,
    channel_id: &str,
    space_node: &str,
) -> Result<(), AdminErr> {
    let tuple = Tuple::new(
        Object::new(ObjectType::Channel, channel_id),
        Relation::new("parent"),
        Subject::userset(SubjectType::Space, space_node, ""),
    );
    match state.permission_actor.ask(WriteTuple { tuple }).await {
        Ok(()) => Ok(()),
        Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => Ok(()),
        Err(error) => Err(internal_err(format!(
            "permission actor failed writing channel parent tuple: {error}"
        ))),
    }
}

struct SpaceDeleteCleanup {
    room_jid: BareJid,
    channel_id: Option<String>,
    parent_tuple_deleted: bool,
    had_bookmark_item: bool,
    rollback_bookmark_item: Option<PubSubItem>,
    removed_link: Option<ChannelSpaceLink>,
}

async fn rollback_space_delete_cleanups(
    state: &AppState,
    cleanups: &[SpaceDeleteCleanup],
    space_node: &str,
    skip_rooms: &std::collections::BTreeSet<BareJid>,
    skip_parent_tuple_rooms: &std::collections::BTreeSet<BareJid>,
) {
    for cleanup in cleanups.iter().rev() {
        if skip_rooms.contains(&cleanup.room_jid) {
            continue;
        }
        let mut bookmark_available =
            cleanup.had_bookmark_item && !skip_parent_tuple_rooms.contains(&cleanup.room_jid);
        if !cleanup.had_bookmark_item {
            if let Some(item) = cleanup.rollback_bookmark_item.as_ref() {
                match state
                    .pubsub_storage
                    .publish_item(&state.spaces_jid, space_node, item, None, false)
                    .await
                {
                    Ok(_) => {
                        bookmark_available = true;
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            room = %cleanup.room_jid,
                            space_node = %space_node,
                            "spaces:delete rollback failed to restore missing channel bookmark",
                        );
                    }
                }
            }
        }
        if cleanup.parent_tuple_deleted && bookmark_available {
            if let Some(channel_id) = cleanup.channel_id.as_deref() {
                if write_channel_parent_tuple(state, channel_id, space_node)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        room = %cleanup.room_jid,
                        space_node = %space_node,
                        "spaces:delete rollback failed to restore channel parent tuple",
                    );
                }
            }
        }
        if let Some(link) = cleanup.removed_link.as_ref() {
            if let Err(error) = state.channel_space_link_store.set(link).await {
                tracing::warn!(
                    error = %error,
                    room = %cleanup.room_jid,
                    space = %link.space_jid,
                    "spaces:delete rollback failed to restore channel-space link",
                );
            }
        }
    }
}

async fn restore_space_node_items(
    state: &AppState,
    space_node: &str,
    items: &[(String, PubSubItem)],
    skip_rooms: &std::collections::BTreeSet<BareJid>,
) -> std::collections::BTreeSet<BareJid> {
    let mut failed_rooms = std::collections::BTreeSet::new();
    for (item_id, item) in items.iter().rev() {
        if item_id
            .parse::<BareJid>()
            .is_ok_and(|room_jid| skip_rooms.contains(&room_jid))
        {
            continue;
        }
        if let Err(error) = state
            .pubsub_storage
            .publish_item(&state.spaces_jid, space_node, item, None, false)
            .await
        {
            tracing::warn!(
                error = %error,
                item_id = %item_id,
                space_node = %space_node,
                "spaces:delete rollback failed to restore PubSub item",
            );
            if let Ok(room_jid) = item_id.parse::<BareJid>() {
                failed_rooms.insert(room_jid);
            }
        }
    }
    failed_rooms
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
                "spaces:delete rollback could not snapshot room for missing channel bookmark",
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
                "spaces:delete rollback could not snapshot room config for missing channel bookmark",
            );
            return None;
        }
    };
    let channel_type = if config.forum { "forum" } else { "text" };
    match waddle_xmpp::xep::build_channel_item(
        &ChannelInfo {
            id: channel_id.to_string(),
            name: config.name,
            channel_type: channel_type.to_string(),
        },
        &state.muc_domain.to_string(),
    ) {
        Ok(item) => Some(item),
        Err(error) => {
            tracing::warn!(
                error = %error,
                room = %room_jid,
                "spaces:delete rollback could not build missing channel bookmark",
            );
            None
        }
    }
}

async fn run_create(state: &AppState, args: &SpacesCreateArgs) -> Result<SpaceRef, AdminErr> {
    // Mint a fresh localpart for this space. Mirror the chat-side
    // convention of slugified-name + short-id by lowercasing+slugifying
    // the name and appending a short UUID tail.
    let localpart = mint_space_localpart(&args.name);
    let domain = state.spaces_jid.domain();
    let space_jid: BareJid = format!("{localpart}@{domain}")
        .parse()
        .map_err(|e| internal_err(format!("constructed space JID is invalid: {e}")))?;

    let now = now_unix_seconds();
    // Create the pubsub node that backs the space's channel list.
    let (_, created_node) = state
        .pubsub_storage
        .get_or_create_node(&state.spaces_jid, &localpart)
        .await
        .map_err(|e| internal_err(format!("pubsub create node failed: {e}")))?;
    if let Err(error) = state
        .pubsub_storage
        .update_node_config(
            &state.spaces_jid,
            &localpart,
            &waddle_xmpp::pubsub::NodeConfig::spaces_public(),
        )
        .await
    {
        if created_node {
            let _ = state
                .pubsub_storage
                .delete_node(&state.spaces_jid, &localpart)
                .await;
        }
        return Err(internal_err(format!(
            "pubsub configure space node failed: {error}"
        )));
    }

    // Seed server-owners as PubSub owners on the new node so they can
    // administer it. Mirrors `spaces_pubsub_seed::seed_owners_on_node`.
    crate::spaces_pubsub_seed::seed_owners_on_node(
        &state.pubsub_storage,
        &state.spaces_jid,
        &localpart,
        &state.server_owner_jids,
    )
    .await;

    let metadata = SpaceMetadata {
        space_jid: space_jid.clone(),
        name: args.name.clone(),
        description: args.description.clone(),
        icon_url: args.icon_url.clone(),
        created_at: now,
        updated_at: now,
    };
    if let Err(error) = state.spaces_metadata_store.upsert(&metadata).await {
        let _ = state
            .pubsub_storage
            .delete_node(&state.spaces_jid, &localpart)
            .await;
        return Err(map_metadata_err(error));
    }

    Ok(SpaceRef {
        space_jid,
        name: args.name.clone(),
        description: args.description.clone(),
        icon_url: args.icon_url.clone(),
    })
}

fn mint_space_localpart(name: &str) -> String {
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
    let base = if trimmed.is_empty() { "space" } else { trimmed };
    let tail = uuid::Uuid::new_v4().simple().to_string();
    let short_tail: String = tail.chars().take(8).collect();
    format!("{base}-{short_tail}")
}

async fn run_update(state: &AppState, args: &SpacesUpdateArgs) -> Result<SpaceRef, AdminErr> {
    let existing = state
        .spaces_metadata_store
        .get(&args.space_jid)
        .await
        .map_err(map_metadata_err)?
        .ok_or_else(|| {
            Box::new(CommandResult::Error(XmppError::item_not_found(Some(
                format!("no space '{}'", args.space_jid),
            ))))
        })?;

    let updated_name = args.name.clone().unwrap_or_else(|| existing.name.clone());
    let updated_description = args.description.clone().or(existing.description.clone());
    let updated_icon_url = args.icon_url.clone().or(existing.icon_url.clone());

    let metadata = SpaceMetadata {
        space_jid: existing.space_jid.clone(),
        name: updated_name.clone(),
        description: updated_description.clone(),
        icon_url: updated_icon_url.clone(),
        created_at: existing.created_at,
        updated_at: now_unix_seconds(),
    };
    state
        .spaces_metadata_store
        .upsert(&metadata)
        .await
        .map_err(map_metadata_err)?;

    Ok(SpaceRef {
        space_jid: existing.space_jid,
        name: updated_name,
        description: updated_description,
        icon_url: updated_icon_url,
    })
}

async fn run_delete(state: &AppState, args: &SpacesDeleteArgs) -> Result<(), AdminErr> {
    // Cascade destroy. There are two sources of "channels in this
    // space" we need to honor:
    //   1. The persistent channel↔space link projection
    //      (`channel_space_link_store`) — populated by admin V2's
    //      `channels:create`. This is the authoritative source for
    //      admin-managed channels.
    //   2. Legacy/bookmark items stored on the space's pubsub node
    //      (each item's `id` is a managed-room JID).
    // Both are unioned so the cascade is total.
    let Some(node_name) = space_node_name(&args.space_jid) else {
        return Err(bad_request("space_jid must have a localpart"));
    };

    // Collect channels-to-destroy from both sources.
    let mut targets: std::collections::BTreeSet<BareJid> = std::collections::BTreeSet::new();

    let linked = state
        .channel_space_link_store
        .list_channels_in_space(&args.space_jid)
        .await
        .map_err(|e| internal_err(format!("channel-space link storage: {e}")))?;
    for jid in linked {
        targets.insert(jid);
    }

    let items = state
        .pubsub_storage
        .get_items(&state.spaces_jid, &node_name, None, &[])
        .await
        .map_err(|e| internal_err(format!("pubsub get_items failed: {e}")))?;
    for stored in &items {
        if let Ok(room_jid) = stored.id.parse::<BareJid>() {
            targets.insert(room_jid);
        }
    }
    let bookmarked_rooms: std::collections::BTreeSet<BareJid> = items
        .iter()
        .filter_map(|stored| stored.id.parse::<BareJid>().ok())
        .collect();

    let mut cleanups: Vec<SpaceDeleteCleanup> = Vec::new();
    let mut destroy_targets: std::collections::BTreeSet<BareJid> =
        std::collections::BTreeSet::new();
    for room_jid in &targets {
        let mut cleanup = SpaceDeleteCleanup {
            room_jid: room_jid.clone(),
            channel_id: None,
            parent_tuple_deleted: false,
            had_bookmark_item: bookmarked_rooms.contains(room_jid),
            rollback_bookmark_item: None,
            removed_link: None,
        };
        if let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) {
            cleanup.channel_id = Some(channel_id.clone());
            match delete_channel_parent_tuple(state, &channel_id, &node_name).await {
                Ok(true) => {
                    cleanup.parent_tuple_deleted = true;
                    if !cleanup.had_bookmark_item {
                        cleanup.rollback_bookmark_item =
                            rollback_channel_bookmark_item(state, room_jid, &channel_id).await;
                        if cleanup.rollback_bookmark_item.is_none() {
                            let _ =
                                write_channel_parent_tuple(state, &channel_id, &node_name).await;
                            rollback_space_delete_cleanups(
                                state,
                                &cleanups,
                                &node_name,
                                &std::collections::BTreeSet::new(),
                                &std::collections::BTreeSet::new(),
                            )
                            .await;
                            return Err(internal_err(format!(
                                "could not snapshot missing Spaces bookmark for linked channel {room_jid}"
                            )));
                        }
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        room = %room_jid,
                        space_node = %node_name,
                        "cascade destroy: clearing channel parent tuple failed",
                    );
                    rollback_space_delete_cleanups(
                        state,
                        &cleanups,
                        &node_name,
                        &std::collections::BTreeSet::new(),
                        &std::collections::BTreeSet::new(),
                    )
                    .await;
                    return Err(error);
                }
            }
        }

        let existing_link = match state.channel_space_link_store.get(room_jid).await {
            Ok(link) => link,
            Err(error) => {
                cleanups.push(cleanup);
                rollback_space_delete_cleanups(
                    state,
                    &cleanups,
                    &node_name,
                    &std::collections::BTreeSet::new(),
                    &std::collections::BTreeSet::new(),
                )
                .await;
                return Err(internal_err(format!("channel-space link storage: {error}")));
            }
        };

        if existing_link
            .as_ref()
            .is_some_and(|link| link.space_jid != args.space_jid)
        {
            cleanups.push(cleanup);
            continue;
        }

        // Drop the link row only after the parent tuple is gone. If this
        // fails, the link remains so a retry can still find the target.
        match state.channel_space_link_store.clear(room_jid).await {
            Ok(true) => {
                cleanup.removed_link = existing_link.or_else(|| {
                    Some(ChannelSpaceLink {
                        channel_jid: room_jid.clone(),
                        space_jid: args.space_jid.clone(),
                        created_at: now_unix_seconds(),
                    })
                });
            }
            Ok(false) => {}
            Err(error) => {
                cleanups.push(cleanup);
                rollback_space_delete_cleanups(
                    state,
                    &cleanups,
                    &node_name,
                    &std::collections::BTreeSet::new(),
                    &std::collections::BTreeSet::new(),
                )
                .await;
                tracing::warn!(
                    error = %error,
                    room = %room_jid,
                    "cascade destroy: clearing channel-space link failed",
                );
                return Err(internal_err(format!(
                    "clearing channel-space link failed for {room_jid}: {error}"
                )));
            }
        }
        destroy_targets.insert(room_jid.clone());
        cleanups.push(cleanup);
    }

    let mut retracted_items: Vec<(String, PubSubItem)> = Vec::new();
    for stored in &items {
        let item_id = stored.id.clone();
        let item = stored.to_pubsub_item();
        if let Err(error) = state
            .pubsub_storage
            .retract_item(&state.spaces_jid, &node_name, &item_id)
            .await
        {
            let failed_restores = restore_space_node_items(
                state,
                &node_name,
                &retracted_items,
                &std::collections::BTreeSet::new(),
            )
            .await;
            rollback_space_delete_cleanups(
                state,
                &cleanups,
                &node_name,
                &std::collections::BTreeSet::new(),
                &failed_restores,
            )
            .await;
            return Err(internal_err(format!(
                "pubsub retract space item failed for {item_id}: {error}"
            )));
        }
        retracted_items.push((item_id, item));
    }

    let mut destroyed_rooms: std::collections::BTreeSet<BareJid> =
        std::collections::BTreeSet::new();
    for room_jid in &destroy_targets {
        if let Err(error) = state
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::DestroyRoom {
                room_jid: room_jid.clone(),
            })
            .await
        {
            let failed_restores =
                restore_space_node_items(state, &node_name, &retracted_items, &destroyed_rooms)
                    .await;
            rollback_space_delete_cleanups(
                state,
                &cleanups,
                &node_name,
                &destroyed_rooms,
                &failed_restores,
            )
            .await;
            tracing::warn!(
                error = %error,
                room = %room_jid,
                "cascade destroy: room registry ask failed",
            );
            return Err(internal_err(format!(
                "cascade destroy failed for room {room_jid}: {error}"
            )));
        }
        destroyed_rooms.insert(room_jid.clone());
    }

    state
        .pubsub_storage
        .purge_node(&state.spaces_jid, &node_name)
        .await
        .map_err(|e| internal_err(format!("pubsub purge_node failed: {e}")))?;

    let _deleted = state
        .pubsub_storage
        .delete_node(&state.spaces_jid, &node_name)
        .await
        .map_err(|e| internal_err(format!("pubsub delete_node failed: {e}")))?;

    let _existed = state
        .spaces_metadata_store
        .delete(&args.space_jid)
        .await
        .map_err(map_metadata_err)?;

    Ok(())
}

async fn run_members(
    state: &AppState,
    args: &SpacesMembersArgs,
) -> Result<SpacesMembersResult, AdminErr> {
    let Some(node) = space_node_name(&args.space_jid) else {
        return Err(bad_request("space_jid must have a localpart"));
    };

    let mut affiliations = state
        .pubsub_storage
        .list_node_affiliations(&state.spaces_jid, &node)
        .await
        .map_err(|e| internal_err(format!("pubsub list_node_affiliations failed: {e}")))?;

    affiliations
        .retain(|(_, aff)| !matches!(aff, PubSubAffiliation::Outcast | PubSubAffiliation::None));
    affiliations.sort_by(|a, b| a.0.cmp(&b.0));

    if let Some(cursor) = args.after_cursor.as_deref() {
        affiliations.retain(|(jid, _)| jid.to_string().as_str() > cursor);
    }

    let limit = args.page_size as usize;
    let total = affiliations.len();
    let entries: Vec<SpaceMemberEntry> = affiliations
        .iter()
        .take(limit)
        .map(|(jid, aff)| SpaceMemberEntry {
            jid: jid.clone(),
            role: SpaceRole::from_pubsub(*aff),
        })
        .collect();
    let next_cursor = if total > limit {
        entries.last().map(|entry| entry.jid.to_string())
    } else {
        None
    };
    Ok(SpacesMembersResult {
        entries,
        next_cursor,
    })
}

async fn run_set_role(
    state: &AppState,
    args: &SpacesSetRoleArgs,
) -> Result<SpacesSetRoleResult, AdminErr> {
    let Some(node) = space_node_name(&args.space_jid) else {
        return Err(bad_request("space_jid must have a localpart"));
    };
    // Refuse silently for unknown spaces: if there's no metadata row,
    // there's no space to grant a role on.
    let exists = state
        .spaces_metadata_store
        .get(&args.space_jid)
        .await
        .map_err(map_metadata_err)?
        .is_some();
    if !exists {
        return Err(Box::new(CommandResult::Error(XmppError::item_not_found(
            Some(format!("no space '{}'", args.space_jid)),
        ))));
    }
    let target_aff = args.role.to_pubsub();
    state
        .pubsub_storage
        .set_affiliation(&state.spaces_jid, &node, &args.member_jid, target_aff)
        .await
        .map_err(|e| internal_err(format!("pubsub set_affiliation failed: {e}")))?;

    Ok(SpacesSetRoleResult {
        member_jid: args.member_jid.clone(),
        role: args.role,
    })
}

// ---------------------------------------------------------------------------
// Response builders (typed struct → XEP-0004 result form)
// ---------------------------------------------------------------------------

pub fn build_list_form(result: &SpacesListResult) -> DataForm {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_LIST))
        .add_reported(Field::new("space_jid", FieldType::JidSingle).with_label("Space JID"))
        .add_reported(Field::new("name", FieldType::TextSingle).with_label("Name"))
        .add_reported(Field::new("description", FieldType::TextSingle).with_label("Description"))
        .add_reported(Field::new("icon_url", FieldType::TextSingle).with_label("Icon URL"))
        .add_reported(Field::new("channel_count", FieldType::TextSingle).with_label("Channels"))
        .add_reported(Field::new("member_count", FieldType::TextSingle).with_label("Members"));
    for entry in &result.entries {
        let row = vec![
            Field::new("space_jid", FieldType::JidSingle).with_value(entry.space_jid.to_string()),
            Field::new("name", FieldType::TextSingle).with_value(entry.name.clone()),
            Field::new("description", FieldType::TextSingle)
                .with_value(entry.description.clone().unwrap_or_default()),
            Field::new("icon_url", FieldType::TextSingle)
                .with_value(entry.icon_url.clone().unwrap_or_default()),
            Field::new("channel_count", FieldType::TextSingle)
                .with_value(entry.channel_count.to_string()),
            Field::new("member_count", FieldType::TextSingle)
                .with_value(entry.member_count.to_string()),
        ];
        form = form.add_item(row);
    }
    if let Some(cursor) = result.next_cursor.as_ref() {
        form = form.add_field(Field::text_single("next_cursor", cursor));
    }
    form
}

pub fn build_space_form(space: &SpaceRef) -> DataForm {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_CREATE))
        .add_field(
            Field::new("space_jid", FieldType::JidSingle).with_value(space.space_jid.to_string()),
        )
        .add_field(Field::text_single("name", space.name.clone()));
    if let Some(description) = space.description.as_ref() {
        form = form.add_field(Field::text_single("description", description));
    }
    if let Some(icon_url) = space.icon_url.as_ref() {
        form = form.add_field(Field::text_single("icon_url", icon_url));
    }
    form
}

pub fn build_members_form(result: &SpacesMembersResult) -> DataForm {
    let mut form = DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_MEMBERS))
        .add_reported(Field::new("jid", FieldType::JidSingle).with_label("JID"))
        .add_reported(Field::new("role", FieldType::TextSingle).with_label("Role"));
    for entry in &result.entries {
        let row = vec![
            Field::new("jid", FieldType::JidSingle).with_value(entry.jid.to_string()),
            Field::new("role", FieldType::TextSingle).with_value(entry.role.as_wire().to_string()),
        ];
        form = form.add_item(row);
    }
    if let Some(cursor) = result.next_cursor.as_ref() {
        form = form.add_field(Field::text_single("next_cursor", cursor));
    }
    form
}

pub fn build_set_role_form(result: &SpacesSetRoleResult) -> DataForm {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(NODE_SET_ROLE))
        .add_field(
            Field::new("member_jid", FieldType::JidSingle)
                .with_value(result.member_jid.to_string()),
        )
        .add_field(Field::text_single("role", result.role.as_wire()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn submit_form(extra: Vec<Field>) -> DataForm {
        let mut form = DataForm::new(FormType::Submit).add_field(Field::form_type(NODE_LIST));
        for field in extra {
            form = form.add_field(field);
        }
        form
    }

    #[test]
    fn role_wire_round_trips() {
        for role in [
            SpaceRole::Owner,
            SpaceRole::Admin,
            SpaceRole::Member,
            SpaceRole::None,
        ] {
            let wire = role.as_wire();
            assert_eq!(SpaceRole::parse(wire), Ok(role));
        }
    }

    #[test]
    fn role_parse_rejects_garbage() {
        assert!(SpaceRole::parse("outcast").is_err());
        assert!(SpaceRole::parse("OWNER").is_err());
        assert!(SpaceRole::parse("").is_err());
    }

    #[test]
    fn role_pubsub_mapping_is_lossy_for_outcast() {
        assert_eq!(
            SpaceRole::from_pubsub(PubSubAffiliation::Outcast),
            SpaceRole::None
        );
        assert_eq!(
            SpaceRole::from_pubsub(PubSubAffiliation::Owner),
            SpaceRole::Owner
        );
        assert_eq!(
            SpaceRole::from_pubsub(PubSubAffiliation::Publisher),
            SpaceRole::Admin
        );
        assert_eq!(
            SpaceRole::from_pubsub(PubSubAffiliation::Member),
            SpaceRole::Member
        );
    }

    #[test]
    fn parse_list_args_returns_defaults_on_missing_form() {
        let args = parse_list_args(None).expect("ok");
        assert_eq!(args.prefix, None);
        assert_eq!(args.page_size, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn parse_list_args_clamps_page_size() {
        let form = submit_form(vec![Field::text_single("page_size", "10000")]);
        let args = parse_list_args(Some(&form)).expect("ok");
        assert_eq!(args.page_size, MAX_PAGE_SIZE);
    }

    #[test]
    fn parse_create_args_requires_name() {
        let form = DataForm::new(FormType::Submit).add_field(Field::form_type(NODE_CREATE));
        let err = parse_create_args(Some(&form)).expect_err("missing name");
        assert!(err.contains("name"));
    }

    #[test]
    fn parse_create_args_rejects_overlong_name() {
        let long = "x".repeat(MAX_NAME_LEN + 1);
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_CREATE))
            .add_field(Field::text_single("name", &long));
        let err = parse_create_args(Some(&form)).expect_err("overlong");
        assert!(err.contains("80"));
    }

    #[test]
    fn parse_create_args_accepts_name_only() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_CREATE))
            .add_field(Field::text_single("name", "Engineering"));
        let args = parse_create_args(Some(&form)).expect("ok");
        assert_eq!(args.name, "Engineering");
        assert_eq!(args.description, None);
        assert_eq!(args.icon_url, None);
    }

    #[test]
    fn parse_delete_args_requires_confirm_yes() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_DELETE))
            .add_field(Field::text_single("space_jid", "eng@spaces.localhost"))
            .add_field(Field::text_single("confirm", "no"));
        let err = parse_delete_args(Some(&form)).expect_err("confirm=no rejected");
        assert!(err.contains("confirm"));
    }

    #[test]
    fn parse_set_role_args_validates_role() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::form_type(NODE_SET_ROLE))
            .add_field(Field::text_single("space_jid", "eng@spaces.localhost"))
            .add_field(Field::text_single("member_jid", "alice@localhost"))
            .add_field(Field::text_single("role", "ceo"));
        let err = parse_set_role_args(Some(&form)).expect_err("ceo rejected");
        assert!(err.contains("role"));
    }

    #[test]
    fn build_list_form_emits_columns_and_cursor() {
        let result = SpacesListResult {
            entries: vec![SpaceListEntry {
                space_jid: "eng@spaces.localhost".parse().expect("jid"),
                name: "Engineering".to_string(),
                description: Some("Hack".to_string()),
                icon_url: None,
                channel_count: 3,
                member_count: 5,
            }],
            next_cursor: Some("eng@spaces.localhost".to_string()),
        };
        let form = build_list_form(&result);
        assert!(matches!(form.form_type, FormType::Result));
        assert_eq!(form.reported.len(), 6);
        assert_eq!(form.items.len(), 1);
        assert_eq!(form.get_value("next_cursor"), Some("eng@spaces.localhost"));
    }

    #[test]
    fn mint_localpart_slugifies_uppercase_and_spaces() {
        let lp = mint_space_localpart("Hello World!");
        assert!(lp.starts_with("hello-world-"), "got: {lp}");
    }

    #[test]
    fn mint_localpart_falls_back_to_space_for_garbage_name() {
        let lp = mint_space_localpart("???");
        assert!(lp.starts_with("space-"), "got: {lp}");
    }
}
