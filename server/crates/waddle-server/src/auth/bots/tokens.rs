//! Bot API tokens: minting, validation, revocation and cleanup.
//!
//! A token is `wbt_` followed by 32 random bytes in unpadded base64url
//! (47 characters). Only its keyed hash is stored, and it is looked up by
//! that hash alone. Nothing here logs, formats or caches the token itself.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use jid::BareJid;
use kameo::actor::ActorRef;
use tracing::{instrument, warn};

use super::types::{BotIdError, BotJid, BotTokenId, BotTokenLabel};
use super::{parse_storage_timestamp, storage_timestamp};
use crate::auth::token_hash::TokenHasher;
use crate::db::actor::{DbActor, DbExecute, DbQuery, DbQueryOne, MintBotToken, MintOutcome};
use crate::db::{row_value, DbDecode, Value};
use crate::permissions::{
    CheckPermission, Object, ObjectType, Permission, PermissionActor, Subject,
};

pub const BOT_TOKEN_PREFIX: &str = "wbt_";
pub const MAX_ACTIVE_TOKENS_PER_BOT: u32 = 10;

/// Random bytes behind each token.
const BOT_TOKEN_SECRET_BYTES: usize = 32;
/// `wbt_` plus 43 characters of unpadded base64url.
const BOT_TOKEN_LEN: usize = 47;
/// `last_used_at` is rewritten at most this often per token.
const LAST_USED_TOUCH_INTERVAL_SECS: i64 = 60;
const DEFAULT_TOKEN_TTL_DAYS: u16 = 90;
const MAX_TOKEN_TTL_DAYS: u16 = 365;

/// A freshly minted token. Shown to the owner once; never stored, logged
/// or serialized by the server.
pub struct BotTokenSecret(String);

impl BotTokenSecret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

pub struct MintedBotToken {
    pub summary: BotTokenSummary,
    pub secret: BotTokenSecret,
}

/// A token's metadata, without the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotTokenSummary {
    pub id: BotTokenId,
    pub label: BotTokenLabel,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// The bot a valid token authenticates. Deliberately unrelated to
/// [`crate::auth::Session`]: a bot principal never backs a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotPrincipal {
    pub bot_jid: BareJid,
    pub owner_jid: BareJid,
    pub token_id: BotTokenId,
}

/// Token failures. No variant carries the presented token.
#[derive(thiserror::Error, Debug)]
pub enum BotTokenError {
    #[error("bot token is malformed")]
    Malformed,
    #[error("bot token is not recognised")]
    Unknown,
    #[error("bot token has been revoked")]
    Revoked,
    #[error("bot token has expired")]
    Expired,
    #[error("bot is disabled")]
    BotDisabled,
    #[error("bot owner is no longer a server member")]
    OwnerNotMember,
    #[error("bot already has the maximum number of active tokens")]
    TokenLimitReached,
    #[error("bot not found")]
    NotFound,
    #[error("permission backend unavailable")]
    Unavailable,
    #[error("bot token storage failed: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeOutcome {
    Revoked,
    AlreadyRevoked,
    NotFound,
}

/// Token lifetime in days, 1..=365. Defaults to 90.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotTokenTtl(u16);

impl BotTokenTtl {
    pub fn days(days: u16) -> Result<Self, BotIdError> {
        if (1..=MAX_TOKEN_TTL_DAYS).contains(&days) {
            Ok(Self(days))
        } else {
            Err(BotIdError::InvalidTokenTtl)
        }
    }

    fn duration(self) -> Duration {
        Duration::days(i64::from(self.0))
    }
}

impl Default for BotTokenTtl {
    fn default() -> Self {
        Self(DEFAULT_TOKEN_TTL_DAYS)
    }
}

#[derive(Clone)]
pub struct BotTokenStore {
    actor: ActorRef<DbActor>,
    permissions: ActorRef<PermissionActor>,
    server_id: String,
    hasher: TokenHasher,
}

impl BotTokenStore {
    pub(crate) fn new(
        actor: ActorRef<DbActor>,
        permissions: ActorRef<PermissionActor>,
        server_id: String,
        hasher: TokenHasher,
    ) -> Self {
        Self {
            actor,
            permissions,
            server_id,
            hasher,
        }
    }

    /// Mint a token for `bot`. Fails with `TokenLimitReached` once the bot
    /// holds [`MAX_ACTIVE_TOKENS_PER_BOT`] unrevoked, unexpired tokens.
    pub async fn mint(
        &self,
        bot: &BotJid,
        label: BotTokenLabel,
        ttl: BotTokenTtl,
    ) -> Result<MintedBotToken, BotTokenError> {
        use rand::RngExt;
        let bytes: [u8; BOT_TOKEN_SECRET_BYTES] = rand::rng().random();
        let secret = BotTokenSecret(format!(
            "{BOT_TOKEN_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(bytes)
        ));
        let id = BotTokenId::new();
        let created_at = Utc::now();
        let expires_at = created_at + ttl.duration();

        let outcome = self
            .actor
            .ask(MintBotToken {
                bot: bot.clone(),
                id,
                token_hash: self.hasher.bot_token(secret.expose()),
                label: label.clone(),
                created_at,
                expires_at,
                max_active: MAX_ACTIVE_TOKENS_PER_BOT,
            })
            .await
            .map_err(storage)?;
        match outcome {
            MintOutcome::Minted => Ok(MintedBotToken {
                summary: BotTokenSummary {
                    id,
                    label,
                    created_at: reparse(created_at)?,
                    last_used_at: None,
                    expires_at: reparse(expires_at)?,
                    revoked_at: None,
                },
                secret,
            }),
            MintOutcome::LimitReached => Err(BotTokenError::TokenLimitReached),
            MintOutcome::BotDisabled => Err(BotTokenError::BotDisabled),
            MintOutcome::BotMissing => Err(BotTokenError::NotFound),
        }
    }

    /// Resolve a presented token to its bot. Checks, in order: syntax, the
    /// stored row (unknown, revoked, expired, bot disabled), then the live
    /// owner gate: the owner must still hold `create_bot` on the server.
    /// Never cached; every call reads storage and the permission backend.
    #[instrument(skip_all, fields(token_id, bot_jid))]
    pub async fn validate(&self, presented: &str) -> Result<BotPrincipal, BotTokenError> {
        if !is_well_formed(presented) {
            return Err(BotTokenError::Malformed);
        }

        let row = self
            .actor
            .ask(DbQueryOne {
                sql: "SELECT t.id, t.bot_jid, u.owner_jid, t.expires_at, t.revoked_at, \
                      t.last_used_at, u.disabled_at \
                      FROM bot_tokens t JOIN users u ON u.jid = t.bot_jid AND u.kind = 'bot' \
                      WHERE t.token_hash = ?"
                    .to_string(),
                params: vec![Value::from(self.hasher.bot_token(presented))],
            })
            .await
            .map_err(storage)?
            .ok_or(BotTokenError::Unknown)?;
        let row = TokenRow::parse(&row)?;
        let span = tracing::Span::current();
        span.record("token_id", tracing::field::display(&row.id));
        span.record("bot_jid", tracing::field::display(&row.bot_jid));

        let now = Utc::now();
        if row.revoked {
            return Err(BotTokenError::Revoked);
        }
        if row.expires_at <= now {
            return Err(BotTokenError::Expired);
        }
        if row.disabled {
            return Err(BotTokenError::BotDisabled);
        }

        let gate = self
            .permissions
            .ask(CheckPermission {
                subject: Subject::user(row.owner_jid.to_string()),
                permission: Permission::CreateBot,
                object: Object::new(ObjectType::Server, self.server_id.as_str()),
            })
            .await
            .map(|response| response.allowed)
            .map_err(|error| error.to_string());
        owner_gate(gate)?;

        let threshold = now - Duration::seconds(LAST_USED_TOUCH_INTERVAL_SECS);
        if row.last_used_at.is_none_or(|at| at < threshold) {
            let touched = self
                .actor
                .ask(DbExecute {
                    sql: "UPDATE bot_tokens SET last_used_at = ? \
                          WHERE id = ? AND (last_used_at IS NULL OR last_used_at < ?)"
                        .to_string(),
                    params: vec![
                        Value::from(storage_timestamp(now)),
                        Value::from(row.id.to_string()),
                        Value::from(storage_timestamp(threshold)),
                    ],
                })
                .await;
            if let Err(error) = touched {
                warn!(%error, "failed to record bot token use");
            }
        }

        Ok(BotPrincipal {
            bot_jid: row.bot_jid,
            owner_jid: row.owner_jid,
            token_id: row.id,
        })
    }

    /// Every token of `bot`, revoked ones included, oldest first.
    pub async fn list_for_bot(&self, bot: &BotJid) -> Result<Vec<BotTokenSummary>, BotTokenError> {
        let rows = self
            .actor
            .ask(DbQuery {
                sql: "SELECT id, label, created_at, last_used_at, expires_at, revoked_at \
                      FROM bot_tokens WHERE bot_jid = ? ORDER BY created_at, id"
                    .to_string(),
                params: vec![Value::from(bot.as_bare().as_str())],
            })
            .await
            .map_err(storage)?;
        rows.iter().map(|row| summary(row)).collect()
    }

    /// Revoke token `id` of `bot`. A token of another bot is `NotFound`.
    pub async fn revoke(
        &self,
        bot: &BotJid,
        id: BotTokenId,
    ) -> Result<RevokeOutcome, BotTokenError> {
        let revoked = self
            .actor
            .ask(DbExecute {
                sql: "UPDATE bot_tokens SET revoked_at = ? \
                      WHERE id = ? AND bot_jid = ? AND revoked_at IS NULL"
                    .to_string(),
                params: vec![
                    Value::from(storage_timestamp(Utc::now())),
                    Value::from(id.to_string()),
                    Value::from(bot.as_bare().as_str()),
                ],
            })
            .await
            .map_err(storage)?;
        if revoked > 0 {
            return Ok(RevokeOutcome::Revoked);
        }
        let existing = self
            .actor
            .ask(DbQueryOne {
                sql: "SELECT 1 FROM bot_tokens WHERE id = ? AND bot_jid = ?".to_string(),
                params: vec![
                    Value::from(id.to_string()),
                    Value::from(bot.as_bare().as_str()),
                ],
            })
            .await
            .map_err(storage)?;
        Ok(match existing {
            Some(_) => RevokeOutcome::AlreadyRevoked,
            None => RevokeOutcome::NotFound,
        })
    }

    /// Delete tokens that expired before `now`, and revoked tokens older
    /// than `revoked_retention`. Returns the number deleted.
    pub async fn sweep(
        &self,
        now: DateTime<Utc>,
        revoked_retention: Duration,
    ) -> Result<u64, BotTokenError> {
        self.actor
            .ask(DbExecute {
                sql: "DELETE FROM bot_tokens \
                      WHERE expires_at < ? OR (revoked_at IS NOT NULL AND revoked_at < ?)"
                    .to_string(),
                params: vec![
                    Value::from(storage_timestamp(now)),
                    Value::from(storage_timestamp(now - revoked_retention)),
                ],
            })
            .await
            .map_err(storage)
    }
}

/// D24 live owner gate: `Ok(true)` passes, a denial is `OwnerNotMember` and
/// any backend failure is `Unavailable`, so the gate fails closed.
fn owner_gate(check: Result<bool, String>) -> Result<(), BotTokenError> {
    match check {
        Ok(true) => Ok(()),
        Ok(false) => Err(BotTokenError::OwnerNotMember),
        Err(error) => {
            warn!(%error, "bot owner permission check failed");
            Err(BotTokenError::Unavailable)
        }
    }
}

fn is_well_formed(presented: &str) -> bool {
    presented.len() == BOT_TOKEN_LEN
        && presented
            .strip_prefix(BOT_TOKEN_PREFIX)
            .is_some_and(|body| {
                body.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            })
}

/// What the stored timestamp will read back as, so a fresh summary matches
/// a listed one.
fn reparse(at: DateTime<Utc>) -> Result<DateTime<Utc>, BotTokenError> {
    parse_storage_timestamp(&storage_timestamp(at))
        .ok_or_else(|| BotTokenError::Storage("unrepresentable timestamp".to_string()))
}

fn storage(error: impl std::fmt::Display) -> BotTokenError {
    BotTokenError::Storage(error.to_string())
}

fn column<T: DbDecode>(row: &[Value], index: usize) -> Result<T, BotTokenError> {
    let value = row_value(row, index).map_err(storage)?;
    T::decode(value.clone()).map_err(storage)
}

fn timestamp(row: &[Value], index: usize) -> Result<DateTime<Utc>, BotTokenError> {
    let value: String = column(row, index)?;
    parse_storage_timestamp(&value)
        .ok_or_else(|| BotTokenError::Storage(format!("invalid stored timestamp {value:?}")))
}

fn optional_timestamp(row: &[Value], index: usize) -> Result<Option<DateTime<Utc>>, BotTokenError> {
    let value: Option<String> = column(row, index)?;
    value
        .map(|value| {
            parse_storage_timestamp(&value).ok_or_else(|| {
                BotTokenError::Storage(format!("invalid stored timestamp {value:?}"))
            })
        })
        .transpose()
}

fn token_id(row: &[Value], index: usize) -> Result<BotTokenId, BotTokenError> {
    let value: String = column(row, index)?;
    BotTokenId::parse(&value).map_err(storage)
}

fn bare_jid(row: &[Value], index: usize) -> Result<BareJid, BotTokenError> {
    let value: Option<String> = column(row, index)?;
    value
        .ok_or_else(|| BotTokenError::Storage(format!("missing JID in column {index}")))?
        .parse()
        .map_err(storage)
}

fn summary(row: &[Value]) -> Result<BotTokenSummary, BotTokenError> {
    let label: String = column(row, 1)?;
    Ok(BotTokenSummary {
        id: token_id(row, 0)?,
        label: BotTokenLabel::try_from(label.as_str()).map_err(storage)?,
        created_at: timestamp(row, 2)?,
        last_used_at: optional_timestamp(row, 3)?,
        expires_at: timestamp(row, 4)?,
        revoked_at: optional_timestamp(row, 5)?,
    })
}

/// The validation lookup row, parsed once.
struct TokenRow {
    id: BotTokenId,
    bot_jid: BareJid,
    owner_jid: BareJid,
    expires_at: DateTime<Utc>,
    revoked: bool,
    last_used_at: Option<DateTime<Utc>>,
    disabled: bool,
}

impl TokenRow {
    fn parse(row: &[Value]) -> Result<Self, BotTokenError> {
        let revoked: Option<String> = column(row, 4)?;
        let disabled: Option<String> = column(row, 6)?;
        Ok(Self {
            id: token_id(row, 0)?,
            bot_jid: bare_jid(row, 1)?,
            owner_jid: bare_jid(row, 2)?,
            expires_at: timestamp(row, 3)?,
            revoked: revoked.is_some(),
            last_used_at: optional_timestamp(row, 5)?,
            disabled: disabled.is_some(),
        })
    }
}
