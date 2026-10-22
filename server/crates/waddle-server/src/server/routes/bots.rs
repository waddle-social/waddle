//! `/api/bots`: owners create, list and delete their bot accounts and mint
//! and revoke the bots' API tokens.
//!
//! This is the HTTP API exception for bots scoped in the repository
//! guidance: it manages accounts and credentials only and carries no XMPP
//! semantics. Requests authenticate with the browser session
//! ([`SessionPrincipal`], including the D20 CSRF guard), and every response
//! is `Cache-Control: no-store`.
//!
//! Authorization goes through the permission backend only (D5): creating
//! needs `create_bot` on the server, and every bot-scoped route needs
//! `manage` on `bot:<jid>`, which only the owner tuple grants. A bot the
//! caller does not manage is indistinguishable from a missing one (D19).
//! A minted secret appears in exactly one response and is never logged.

use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, FromRef, Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use jid::{BareJid, DomainPart};
use kameo::actor::ActorRef;
use kameo::error::SendError;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use waddle_xmpp::telemetry::attributes::{BotApiOp, RequestOutcome};

use super::auth::AuthState;
use super::session_auth::{ApiError, SessionPrincipal};
use super::websocket::WebSocketState;
use crate::admin::bot_affiliations::{clear_member_affiliations, ClearAffiliationsError};
use crate::auth::bots::{
    BotIdError, BotJid, BotName, BotRecord, BotStore, BotTokenError, BotTokenId, BotTokenLabel,
    BotTokenSummary, BotTokenTtl, RevokeOutcome, MAX_ACTIVE_TOKENS_PER_BOT,
};
use crate::db::actor::CreateBotOutcome;
use crate::permissions::{
    CheckPermission, DeleteSubjectTuples, DeleteTuple, Object, ObjectType, Permission,
    PermissionActor, PermissionError, Relation, Subject, Tuple, WriteTuple,
};
use crate::server::AppState;

const NO_STORE: &str = "no-store";
/// Longest display name, in characters, after trimming.
const DISPLAY_NAME_MAX_LEN: usize = 64;

#[derive(Clone)]
pub struct BotsState {
    auth: Arc<AuthState>,
    app: Arc<AppState>,
    websocket: Arc<WebSocketState>,
    bots_domain: DomainPart,
    limit: u32,
}

impl BotsState {
    pub fn new(
        auth: Arc<AuthState>,
        app: Arc<AppState>,
        websocket: Arc<WebSocketState>,
        bots_domain: &DomainPart,
        limit: u32,
    ) -> Self {
        Self {
            auth,
            app,
            websocket,
            bots_domain: bots_domain.clone(),
            limit,
        }
    }
}

impl FromRef<BotsState> for Arc<AuthState> {
    fn from_ref(state: &BotsState) -> Self {
        Arc::clone(&state.auth)
    }
}

pub fn router(state: Arc<BotsState>) -> Router {
    Router::new()
        .route("/api/bots", get(list_bots).post(create_bot))
        .route("/api/bots/{jid}", delete(delete_bot))
        .route("/api/bots/{jid}/tokens", post(mint_token))
        .route("/api/bots/{jid}/tokens/{token_id}", delete(revoke_token))
        .with_state(Arc::unwrap_or_clone(state))
}

// ---------------------------------------------------------------------------
// JSON contract
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBotRequest {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MintTokenRequest {
    label: String,
    #[serde(default)]
    expires_in_days: Option<u32>,
}

#[derive(Debug, Serialize)]
struct BotListView {
    bots: Vec<BotView>,
    limit: u32,
    token_limit: u32,
}

#[derive(Debug, Serialize)]
struct BotView {
    jid: String,
    name: String,
    display_name: Option<String>,
    owner_jid: String,
    created_at: DateTime<Utc>,
    disabled_at: Option<DateTime<Utc>>,
    disabled_reason: Option<&'static str>,
    tokens: Vec<BotTokenView>,
}

impl BotView {
    fn new(record: &BotRecord, tokens: &[BotTokenSummary]) -> Self {
        Self {
            jid: record.jid.to_string(),
            name: record.name.to_string(),
            display_name: record.display_name.clone(),
            owner_jid: record.owner.to_string(),
            created_at: record.created_at,
            disabled_at: record.disabled.map(|(at, _)| at),
            disabled_reason: record.disabled.map(|(_, reason)| reason.as_str()),
            tokens: tokens.iter().map(BotTokenView::new).collect(),
        }
    }
}

/// A token's metadata. Never carries the hash or the secret.
#[derive(Debug, Serialize)]
struct BotTokenView {
    id: String,
    label: String,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    expires_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

impl BotTokenView {
    fn new(summary: &BotTokenSummary) -> Self {
        Self {
            id: summary.id.to_string(),
            label: summary.label.to_string(),
            created_at: summary.created_at,
            last_used_at: summary.last_used_at,
            expires_at: summary.expires_at,
            revoked_at: summary.revoked_at,
        }
    }
}

/// The only response body that ever carries a token secret.
#[derive(Serialize)]
struct MintedTokenView<'a> {
    token: BotTokenView,
    secret: &'a str,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

const UNSUPPORTED_MEDIA_TYPE: ApiError = ApiError::new(
    StatusCode::UNSUPPORTED_MEDIA_TYPE,
    "unsupported_media_type",
    "the request body must be application/json",
);
const INVALID_REQUEST: ApiError = ApiError::new(
    StatusCode::BAD_REQUEST,
    "invalid_request",
    "the request body is malformed",
);
const INVALID_NAME: ApiError = ApiError::new(
    StatusCode::BAD_REQUEST,
    "invalid_name",
    "bot names are 1-32 characters of a-z, 0-9, '_' or '-', starting and ending with a letter or digit",
);
const INVALID_DISPLAY_NAME: ApiError = ApiError::new(
    StatusCode::BAD_REQUEST,
    "invalid_request",
    "display_name must be at most 64 characters with no control characters",
);
const INVALID_TOKEN_REQUEST: ApiError = ApiError::new(
    StatusCode::BAD_REQUEST,
    "invalid_token_request",
    "label must be 1-64 characters and expires_in_days between 1 and 365",
);
const INVALID_JID: ApiError = ApiError::new(
    StatusCode::BAD_REQUEST,
    "invalid_jid",
    "not a bot JID on this server",
);
const FORBIDDEN: ApiError = ApiError::new(
    StatusCode::FORBIDDEN,
    "forbidden",
    "you may not create bots on this server",
);
const OWNER_INELIGIBLE: ApiError = ApiError::new(
    StatusCode::FORBIDDEN,
    "owner_ineligible",
    "your account name cannot own bots",
);
const BOT_LIMIT_REACHED: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "bot_limit_reached",
    "you already have the maximum number of bots",
);
const BOT_EXISTS: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "bot_exists",
    "a bot with that name already exists",
);
const BOT_DISABLED: ApiError =
    ApiError::new(StatusCode::CONFLICT, "bot_disabled", "the bot is disabled");
const BOT_TOKEN_LIMIT_REACHED: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "bot_token_limit_reached",
    "the bot already has the maximum number of active tokens",
);
const BOT_SOLE_ROOM_OWNER: ApiError = ApiError::new(
    StatusCode::CONFLICT,
    "bot_sole_room_owner",
    "the bot is the only owner of a channel; make someone else an owner first",
);
const NOT_FOUND: ApiError = ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found");
const UNAVAILABLE: ApiError = ApiError::new(
    StatusCode::SERVICE_UNAVAILABLE,
    "bots_unavailable",
    "bot management is temporarily unavailable",
);

fn unavailable(context: &'static str, error: impl std::fmt::Display) -> ApiError {
    warn!(%error, "{context}");
    UNAVAILABLE
}

fn json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    match body {
        Ok(Json(body)) => Ok(body),
        Err(JsonRejection::MissingJsonContentType(_)) => Err(UNSUPPORTED_MEDIA_TYPE),
        Err(_) => Err(INVALID_REQUEST),
    }
}

// ---------------------------------------------------------------------------
// Responses and telemetry
// ---------------------------------------------------------------------------

fn no_store(status: StatusCode, body: impl Serialize) -> Response {
    (status, [(header::CACHE_CONTROL, NO_STORE)], Json(body)).into_response()
}

fn no_content() -> Response {
    (StatusCode::NO_CONTENT, [(header::CACHE_CONTROL, NO_STORE)]).into_response()
}

/// Finish a request: record its operation and outcome, then respond.
fn respond(op: BotApiOp, result: Result<Response, ApiError>) -> Response {
    let response = result.unwrap_or_else(IntoResponse::into_response);
    let outcome = if response.status().is_success() {
        RequestOutcome::Success
    } else {
        RequestOutcome::Failure
    };
    record_bot_api(op, outcome);
    response
}

fn record_bot_api(op: BotApiOp, outcome: RequestOutcome) {
    waddle_xmpp::counter_add!(
        "waddle.bots.api.requests",
        "{request}",
        "Bot management API requests by operation and outcome.",
        1,
        op,
        outcome,
    );
}

// ---------------------------------------------------------------------------
// Permission helpers
// ---------------------------------------------------------------------------

fn bot_object(bot: &BotJid) -> Object {
    Object::new(ObjectType::Bot, bot.to_string())
}

fn owner_tuple(bot: &BotJid, owner: &BareJid) -> Tuple {
    Tuple::new(
        bot_object(bot),
        Relation::new("owner"),
        Subject::user(owner.to_string()),
    )
}

async fn allowed(
    permissions: &ActorRef<PermissionActor>,
    subject: &BareJid,
    permission: Permission,
    object: Object,
) -> Result<bool, ApiError> {
    permissions
        .ask(CheckPermission {
            subject: Subject::user(subject.to_string()),
            permission,
            object,
        })
        .await
        .map(|response| response.allowed)
        .map_err(|error| unavailable("bot permission check failed", error))
}

/// Write `bot#owner`; an existing tuple is success. The error is only
/// logged.
async fn write_owner_tuple(
    permissions: &ActorRef<PermissionActor>,
    bot: &BotJid,
    owner: &BareJid,
) -> Result<(), String> {
    match permissions
        .ask(WriteTuple {
            tuple: owner_tuple(bot, owner),
        })
        .await
    {
        Ok(()) | Err(SendError::HandlerError(PermissionError::TupleAlreadyExists)) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// D18: grant the owner `manage` on a freshly inserted bot. If the tuple
/// cannot be written the row is deleted again, so a bot never exists
/// without an owner able to manage it.
async fn write_owner_tuple_or_rollback(
    permissions: &ActorRef<PermissionActor>,
    store: &BotStore,
    bot: &BotJid,
    owner: &BareJid,
) -> Result<(), ApiError> {
    let Err(error) = write_owner_tuple(permissions, bot, owner).await else {
        return Ok(());
    };
    warn!(%error, %bot, "writing the bot owner tuple failed; removing the bot");
    if let Err(error) = store.delete(bot).await {
        warn!(%error, %bot, "removing a bot without an owner tuple failed");
    }
    Err(UNAVAILABLE)
}

/// The bot behind a bot-scoped path, if the caller manages it. Anything
/// else, including a bot that does not exist, is `not_found` (D19).
async fn managed_bot(
    state: &BotsState,
    caller: &BareJid,
    raw_jid: &str,
) -> Result<BotRecord, ApiError> {
    let bot = BotJid::parse(raw_jid, &state.bots_domain).map_err(|_| INVALID_JID)?;
    if !allowed(
        &state.auth.permission_actor,
        caller,
        Permission::Manage,
        bot_object(&bot),
    )
    .await?
    {
        return Err(NOT_FOUND);
    }
    state
        .auth
        .bot_store
        .get(&bot)
        .await
        .map_err(|error| unavailable("bot lookup failed", error))?
        .ok_or(NOT_FOUND)
}

fn token_error(error: BotTokenError) -> ApiError {
    match error {
        BotTokenError::TokenLimitReached => BOT_TOKEN_LIMIT_REACHED,
        BotTokenError::BotDisabled => BOT_DISABLED,
        BotTokenError::NotFound => NOT_FOUND,
        other => unavailable("bot token operation failed", other),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_bots(
    State(state): State<BotsState>,
    principal: Result<SessionPrincipal, ApiError>,
) -> Response {
    respond(BotApiOp::List, list(&state, principal).await)
}

async fn list(
    state: &BotsState,
    principal: Result<SessionPrincipal, ApiError>,
) -> Result<Response, ApiError> {
    let principal = principal?;
    let records = state
        .auth
        .bot_store
        .list_for_owner(&principal.user_jid)
        .await
        .map_err(|error| unavailable("listing bots failed", error))?;
    // At most `limit` bots per owner, so one token query per bot is fine.
    let mut bots = Vec::with_capacity(records.len());
    for record in &records {
        let tokens = state
            .auth
            .bot_tokens
            .list_for_bot(&record.jid)
            .await
            .map_err(token_error)?;
        bots.push(BotView::new(record, &tokens));
    }
    Ok(no_store(
        StatusCode::OK,
        BotListView {
            bots,
            limit: state.limit,
            token_limit: MAX_ACTIVE_TOKENS_PER_BOT,
        },
    ))
}

async fn create_bot(
    State(state): State<BotsState>,
    principal: Result<SessionPrincipal, ApiError>,
    body: Result<Json<CreateBotRequest>, JsonRejection>,
) -> Response {
    respond(BotApiOp::Create, create(&state, principal, body).await)
}

fn display_name(value: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.chars().count() > DISPLAY_NAME_MAX_LEN || trimmed.chars().any(char::is_control) {
        return Err(INVALID_DISPLAY_NAME);
    }
    Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
}

async fn create(
    state: &BotsState,
    principal: Result<SessionPrincipal, ApiError>,
    body: Result<Json<CreateBotRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let principal = principal?;
    let request = json_body(body)?;
    let name = BotName::try_from(request.name.as_str()).map_err(|_| INVALID_NAME)?;
    let display_name = display_name(request.display_name)?;
    let caller = &principal.user_jid;
    let permissions = &state.auth.permission_actor;

    let server = Object::new(
        ObjectType::Server,
        state.auth.bootstrap_membership.server_id.as_str(),
    );
    if !allowed(permissions, caller, Permission::CreateBot, server).await? {
        return Err(FORBIDDEN);
    }
    let bot = BotJid::new(caller, &name, &state.bots_domain).map_err(|error| match error {
        BotIdError::OwnerIneligible => OWNER_INELIGIBLE,
        _ => INVALID_NAME,
    })?;

    let store = &state.auth.bot_store;
    let outcome = store
        .create(&bot, caller, display_name, state.limit)
        .await
        .map_err(|error| unavailable("creating a bot failed", error))?;
    match outcome {
        CreateBotOutcome::Created => {}
        CreateBotOutcome::AlreadyExists { same_owner: true } => {
            // D18 repair: an earlier create may have died between the
            // insert and the tuple write, leaving a bot nobody can manage.
            write_owner_tuple(permissions, &bot, caller)
                .await
                .map_err(|error| unavailable("repairing a bot owner tuple failed", error))?;
            return Err(BOT_EXISTS);
        }
        CreateBotOutcome::AlreadyExists { same_owner: false } => return Err(BOT_EXISTS),
        CreateBotOutcome::LimitReached => return Err(BOT_LIMIT_REACHED),
        // A session only resolves for a human row, so this is a race with
        // that row's removal.
        CreateBotOutcome::OwnerMissing => return Err(FORBIDDEN),
    }
    write_owner_tuple_or_rollback(permissions, store, &bot, caller).await?;

    let record = store
        .get(&bot)
        .await
        .map_err(|error| unavailable("reading a created bot failed", error))?
        .ok_or(UNAVAILABLE)?;
    debug!(%bot, transport = ?principal.transport, "bot created");
    Ok(no_store(StatusCode::CREATED, BotView::new(&record, &[])))
}

async fn delete_bot(
    State(state): State<BotsState>,
    principal: Result<SessionPrincipal, ApiError>,
    Path(jid): Path<String>,
) -> Response {
    respond(BotApiOp::Delete, remove(&state, principal, &jid).await)
}

/// D8 hard delete. Each step is idempotent, so a retry after a partial
/// failure completes the delete; the owner tuple goes last so the owner
/// keeps `manage` until everything else is gone. A bot that is the only
/// owner of a room is refused with 409 `bot_sole_room_owner` before
/// anything changes: XEP-0045 keeps every room owned, so another owner has
/// to be made first, and a blind retry would only fail again.
async fn remove(
    state: &BotsState,
    principal: Result<SessionPrincipal, ApiError>,
    raw_jid: &str,
) -> Result<Response, ApiError> {
    let principal = principal?;
    let record = managed_bot(state, &principal.user_jid, raw_jid).await?;
    let bot = &record.jid;

    clear_member_affiliations(&state.app, &state.websocket, bot.as_bare())
        .await
        .map_err(|error| match error {
            ClearAffiliationsError::SoleRoomOwner { room } => {
                debug!(%bot, %room, "bot is the only owner of a room");
                BOT_SOLE_ROOM_OWNER
            }
            ClearAffiliationsError::Failed(error) => {
                let reason = match *error {
                    waddle_xmpp::commands::CommandResult::Error(error) => error.to_string(),
                    _ => "unexpected command result".to_string(),
                };
                unavailable("clearing bot affiliations failed", reason)
            }
        })?;
    state
        .auth
        .permission_actor
        .ask(DeleteSubjectTuples {
            subject: Subject::user(bot.to_string()),
        })
        .await
        .map_err(|error| unavailable("purging bot grants failed", error))?;
    state
        .auth
        .bot_store
        .delete(bot)
        .await
        .map_err(|error| unavailable("deleting a bot failed", error))?;
    if let Err(error) = state
        .auth
        .permission_actor
        .ask(DeleteTuple {
            tuple: owner_tuple(bot, &principal.user_jid),
        })
        .await
    {
        warn!(%error, %bot, "deleting the bot owner tuple failed");
    }
    debug!(%bot, transport = ?principal.transport, "bot deleted");
    Ok(no_content())
}

async fn mint_token(
    State(state): State<BotsState>,
    principal: Result<SessionPrincipal, ApiError>,
    Path(jid): Path<String>,
    body: Result<Json<MintTokenRequest>, JsonRejection>,
) -> Response {
    respond(
        BotApiOp::MintToken,
        mint(&state, principal, &jid, body).await,
    )
}

async fn mint(
    state: &BotsState,
    principal: Result<SessionPrincipal, ApiError>,
    raw_jid: &str,
    body: Result<Json<MintTokenRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let principal = principal?;
    let record = managed_bot(state, &principal.user_jid, raw_jid).await?;
    let request = json_body(body)?;
    let label =
        BotTokenLabel::try_from(request.label.as_str()).map_err(|_| INVALID_TOKEN_REQUEST)?;
    let ttl = match request.expires_in_days {
        None => BotTokenTtl::default(),
        Some(days) => u16::try_from(days)
            .ok()
            .and_then(|days| BotTokenTtl::days(days).ok())
            .ok_or(INVALID_TOKEN_REQUEST)?,
    };
    let minted = state
        .auth
        .bot_tokens
        .mint(&record.jid, label, ttl)
        .await
        .map_err(token_error)?;
    Ok(no_store(
        StatusCode::CREATED,
        MintedTokenView {
            token: BotTokenView::new(&minted.summary),
            secret: minted.secret.expose(),
        },
    ))
}

async fn revoke_token(
    State(state): State<BotsState>,
    principal: Result<SessionPrincipal, ApiError>,
    Path((jid, token_id)): Path<(String, String)>,
) -> Response {
    respond(
        BotApiOp::RevokeToken,
        revoke(&state, principal, &jid, &token_id).await,
    )
}

async fn revoke(
    state: &BotsState,
    principal: Result<SessionPrincipal, ApiError>,
    raw_jid: &str,
    raw_token_id: &str,
) -> Result<Response, ApiError> {
    let principal = principal?;
    let record = managed_bot(state, &principal.user_jid, raw_jid).await?;
    let token_id = BotTokenId::parse(raw_token_id).map_err(|_| NOT_FOUND)?;
    match state
        .auth
        .bot_tokens
        .revoke(&record.jid, token_id)
        .await
        .map_err(token_error)?
    {
        RevokeOutcome::Revoked | RevokeOutcome::AlreadyRevoked => Ok(no_content()),
        RevokeOutcome::NotFound => Err(NOT_FOUND),
    }
}

#[cfg(test)]
mod tests;
