//! Transactional writes for user bot accounts and their tokens.
//!
//! Both messages take a row lock first (`UPDATE ... SET updated_at`) so the
//! limit checks that follow serialize across connections and replicas on
//! SQLite and Postgres alike.

use chrono::{DateTime, Utc};
use jid::BareJid;
use kameo::message::Context;

use super::{mark_actor_result, DbActor};
use crate::auth::bots::{storage_timestamp, BotJid, BotTokenId, BotTokenLabel, UserKind};
use crate::db::{DatabaseError, Transaction, Value};

/// Insert a `kind = 'bot'` users row owned by `owner`, enforcing the
/// per-owner `limit` and every account-name collision.
pub struct CreateBotAccount {
    pub bot: BotJid,
    pub owner: BareJid,
    pub display_name: Option<String>,
    pub limit: u32,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateBotOutcome {
    Created,
    /// The owner has no human `users` row.
    OwnerMissing,
    /// The bot's JID or localpart is taken. `same_owner` is true only when
    /// the taken row is this very bot, already owned by `owner`.
    AlreadyExists {
        same_owner: bool,
    },
    LimitReached,
}

impl kameo::message::Message<CreateBotAccount> for DbActor {
    type Reply = Result<CreateBotOutcome, DatabaseError>;

    async fn handle(
        &mut self,
        msg: CreateBotAccount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.touch();
        let result = async {
            let mut tx = self.db.begin_immediate().await?;
            let outcome = create_bot_account(&mut tx, &msg).await;
            match outcome {
                Ok(CreateBotOutcome::Created) => {
                    tx.commit().await?;
                    Ok(CreateBotOutcome::Created)
                }
                Ok(other) => Ok(other),
                // A concurrent create on another replica won the insert.
                Err(error) if is_unique_violation(&error) => {
                    Ok(CreateBotOutcome::AlreadyExists { same_owner: false })
                }
                Err(error) => Err(error),
            }
        }
        .await;
        mark_actor_result(result)
    }
}

async fn create_bot_account(
    tx: &mut Transaction<'_>,
    msg: &CreateBotAccount,
) -> Result<CreateBotOutcome, DatabaseError> {
    let now = storage_timestamp(msg.now);
    let bot = msg.bot.as_bare().as_str();
    let owner = msg.owner.as_str();
    let localpart = msg.bot.localpart();

    let locked = tx
        .execute(
            "UPDATE users SET updated_at = ? WHERE jid = ? AND kind = ?",
            [now.as_str(), owner, UserKind::Human.as_str()],
        )
        .await?;
    if locked == 0 {
        return Ok(CreateBotOutcome::OwnerMissing);
    }

    let mut colliding = tx
        .query(
            "SELECT jid, kind, owner_jid FROM users \
             WHERE jid = ? OR username = ? OR xmpp_localpart = ? OR localpart_key = ?",
            [bot, localpart, localpart, localpart],
        )
        .await?;
    let mut collision = false;
    let mut same_owner = false;
    while let Some(row) = colliding.next().await? {
        collision = true;
        let jid: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let row_owner: Option<String> = row.get(2)?;
        same_owner |= jid == bot
            && UserKind::parse(&kind) == Some(UserKind::Bot)
            && row_owner.as_deref() == Some(owner);
    }
    if collision {
        return Ok(CreateBotOutcome::AlreadyExists { same_owner });
    }

    // A native account registered under the same localpart on the primary
    // domain (where every owner lives) would share the bot's account name.
    let native_jid = BareJid::from_parts(msg.bot.as_bare().node(), msg.owner.domain());
    let mut native = tx
        .query(
            "SELECT 1 FROM native_users WHERE jid_key = ? LIMIT 1",
            [native_jid.as_str()],
        )
        .await?;
    if native.next().await?.is_some() {
        return Ok(CreateBotOutcome::AlreadyExists { same_owner: false });
    }

    let mut count = tx
        .query(
            "SELECT COUNT(*) FROM users WHERE kind = ? AND owner_jid = ?",
            [UserKind::Bot.as_str(), owner],
        )
        .await?;
    let owned: i64 = match count.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };
    if owned >= i64::from(msg.limit) {
        return Ok(CreateBotOutcome::LimitReached);
    }

    tx.execute(
        "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, display_name, kind, \
         owner_jid, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        vec![
            Value::from(bot),
            Value::from(localpart),
            Value::from(localpart),
            Value::from(localpart),
            Value::from(msg.display_name.clone()),
            Value::from(UserKind::Bot.as_str()),
            Value::from(owner),
            Value::from(now.as_str()),
            Value::from(now.as_str()),
        ],
    )
    .await?;
    Ok(CreateBotOutcome::Created)
}

/// Insert a `bot_tokens` row, enforcing that the bot exists, is enabled and
/// holds fewer than `max_active` unrevoked, unexpired tokens.
pub struct MintBotToken {
    pub bot: BotJid,
    pub id: BotTokenId,
    /// [`crate::auth::token_hash::TokenHasher::bot_token`] of the secret.
    pub token_hash: String,
    pub label: BotTokenLabel,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub max_active: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintOutcome {
    Minted,
    LimitReached,
    BotDisabled,
    BotMissing,
}

impl kameo::message::Message<MintBotToken> for DbActor {
    type Reply = Result<MintOutcome, DatabaseError>;

    async fn handle(
        &mut self,
        msg: MintBotToken,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.touch();
        let result = async {
            let mut tx = self.db.begin_immediate().await?;
            let outcome = mint_bot_token(&mut tx, &msg).await?;
            if outcome == MintOutcome::Minted {
                tx.commit().await?;
            }
            Ok(outcome)
        }
        .await;
        mark_actor_result(result)
    }
}

async fn mint_bot_token(
    tx: &mut Transaction<'_>,
    msg: &MintBotToken,
) -> Result<MintOutcome, DatabaseError> {
    let created_at = storage_timestamp(msg.created_at);
    let bot = msg.bot.as_bare().as_str();

    let locked = tx
        .execute(
            "UPDATE users SET updated_at = ? WHERE jid = ? AND kind = ?",
            [created_at.as_str(), bot, UserKind::Bot.as_str()],
        )
        .await?;
    if locked == 0 {
        return Ok(MintOutcome::BotMissing);
    }

    let mut state = tx
        .query("SELECT disabled_at FROM users WHERE jid = ?", [bot])
        .await?;
    let disabled_at: Option<String> = match state.next().await? {
        Some(row) => row.get(0)?,
        None => return Ok(MintOutcome::BotMissing),
    };
    if disabled_at.is_some() {
        return Ok(MintOutcome::BotDisabled);
    }

    let mut count = tx
        .query(
            "SELECT COUNT(*) FROM bot_tokens \
             WHERE bot_jid = ? AND revoked_at IS NULL AND expires_at > ?",
            [bot, created_at.as_str()],
        )
        .await?;
    let active: i64 = match count.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };
    if active >= i64::from(msg.max_active) {
        return Ok(MintOutcome::LimitReached);
    }

    tx.execute(
        "INSERT INTO bot_tokens (id, bot_jid, token_hash, label, created_at, last_used_at, \
         expires_at, revoked_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        vec![
            Value::from(msg.id.to_string()),
            Value::from(bot),
            Value::from(msg.token_hash.as_str()),
            Value::from(msg.label.as_str()),
            Value::from(created_at.as_str()),
            Value::NullText,
            Value::from(storage_timestamp(msg.expires_at)),
            Value::NullText,
        ],
    )
    .await?;
    Ok(MintOutcome::Minted)
}

fn is_unique_violation(error: &DatabaseError) -> bool {
    matches!(
        error,
        DatabaseError::Internal(sqlx::Error::Database(database)) if database.is_unique_violation()
    )
}
