//! Bot account rows in `users`.

use chrono::{DateTime, Utc};
use jid::BareJid;
use kameo::actor::ActorRef;

use super::parse_storage_timestamp;
use super::types::{BotJid, BotRecord, DisabledReason, UserKind};
use crate::auth::AuthError;
use crate::db::actor::{
    CreateBotAccount, CreateBotOutcome, DbActor, DbExecute, DbQuery, DbQueryOne, DisableBot,
    DisableBotReport, EnableBot,
};
use crate::db::{row_value, DbDecode, Value};

#[derive(Debug, thiserror::Error)]
pub enum BotStoreError {
    #[error("bot storage failed: {0}")]
    Storage(String),
}

const BOT_COLUMNS: &str = "jid, owner_jid, display_name, created_at, disabled_at, disabled_reason";

/// One bot and what the lifecycle janitor needs to know about its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BotOwnerHealthRow {
    pub bot: BotJid,
    pub owner: BareJid,
    pub disabled_reason: Option<DisabledReason>,
    /// The owner still has a human `users` row.
    pub owner_row_exists: bool,
    /// The owner can still sign in: an OIDC identity or a native account.
    pub owner_has_credential: bool,
}

/// `native_users.jid_key` is the canonical bare JID, the same form
/// `users.jid` and `owner_jid` hold for primary-domain accounts.
const LIFECYCLE_COLUMNS: &str = "SELECT u.jid, u.owner_jid, u.disabled_reason, \
     o.jid IS NOT NULL, \
     (EXISTS(SELECT 1 FROM auth_identities a WHERE a.user_jid = u.owner_jid) \
      OR EXISTS(SELECT 1 FROM native_users n WHERE n.jid_key = u.owner_jid)) \
     FROM users u LEFT JOIN users o ON o.jid = u.owner_jid AND o.kind = 'human' \
     WHERE u.kind = 'bot'";

#[derive(Clone)]
pub struct BotStore {
    actor: ActorRef<DbActor>,
}

impl BotStore {
    pub fn new(actor: ActorRef<DbActor>) -> Self {
        Self { actor }
    }

    /// Insert the bot row. Callers write the `bot#owner` tuple afterwards.
    pub async fn create(
        &self,
        bot: &BotJid,
        owner: &BareJid,
        display_name: Option<String>,
        limit: u32,
    ) -> Result<CreateBotOutcome, BotStoreError> {
        self.actor
            .ask(CreateBotAccount {
                bot: bot.clone(),
                owner: owner.clone(),
                display_name,
                limit,
                now: Utc::now(),
            })
            .await
            .map_err(storage)
    }

    pub async fn get(&self, bot: &BotJid) -> Result<Option<BotRecord>, BotStoreError> {
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: format!("SELECT {BOT_COLUMNS} FROM users WHERE jid = ? AND kind = ?"),
                params: vec![
                    Value::from(bot.as_bare().as_str()),
                    Value::from(UserKind::Bot.as_str()),
                ],
            })
            .await
            .map_err(storage)?;
        row.map(|row| bot_record(&row)).transpose()
    }

    /// The owner's bots, oldest first.
    pub async fn list_for_owner(&self, owner: &BareJid) -> Result<Vec<BotRecord>, BotStoreError> {
        let rows = self
            .actor
            .ask(DbQuery {
                sql: format!(
                    "SELECT {BOT_COLUMNS} FROM users WHERE kind = ? AND owner_jid = ? \
                     ORDER BY created_at, jid"
                ),
                params: vec![
                    Value::from(UserKind::Bot.as_str()),
                    Value::from(owner.as_str()),
                ],
            })
            .await
            .map_err(storage)?;
        rows.iter().map(|row| bot_record(row)).collect()
    }

    /// Delete the bot row; its tokens go with it (`ON DELETE CASCADE`).
    /// Returns whether a row was deleted.
    pub async fn delete(&self, bot: &BotJid) -> Result<bool, BotStoreError> {
        let deleted = self
            .actor
            .ask(DbExecute {
                sql: "DELETE FROM users WHERE jid = ? AND kind = ?".to_string(),
                params: vec![
                    Value::from(bot.as_bare().as_str()),
                    Value::from(UserKind::Bot.as_str()),
                ],
            })
            .await
            .map_err(storage)?;
        Ok(deleted > 0)
    }

    /// Up to `limit` bots ordered by JID, starting after `after`, with the
    /// health of each owner.
    pub(crate) async fn lifecycle_page(
        &self,
        after: Option<&BareJid>,
        limit: u32,
    ) -> Result<Vec<BotOwnerHealthRow>, BotStoreError> {
        let (sql, mut params) = match after {
            Some(after) => (
                format!("{LIFECYCLE_COLUMNS} AND u.jid > ? ORDER BY u.jid LIMIT ?"),
                vec![Value::from(after.as_str())],
            ),
            None => (
                format!("{LIFECYCLE_COLUMNS} ORDER BY u.jid LIMIT ?"),
                Vec::new(),
            ),
        };
        params.push(Value::from(i64::from(limit)));
        let rows = self
            .actor
            .ask(DbQuery { sql, params })
            .await
            .map_err(storage)?;
        rows.iter().map(|row| owner_health_row(row)).collect()
    }

    /// D3: disable `bot` and revoke its tokens in one transaction.
    pub(crate) async fn disable(
        &self,
        bot: &BotJid,
        reason: DisabledReason,
        now: DateTime<Utc>,
    ) -> Result<DisableBotReport, BotStoreError> {
        self.actor
            .ask(DisableBot {
                bot: bot.clone(),
                reason,
                now,
            })
            .await
            .map_err(storage)
    }

    /// Clear `bot`'s disabled state. Returns whether it was disabled.
    pub(crate) async fn enable(
        &self,
        bot: &BotJid,
        now: DateTime<Utc>,
    ) -> Result<bool, BotStoreError> {
        self.actor
            .ask(EnableBot {
                bot: bot.clone(),
                now,
            })
            .await
            .map_err(storage)
    }

    /// Whether `bot` is an existing bot account that is not disabled.
    pub async fn exists_enabled(&self, bot: &BareJid) -> Result<bool, BotStoreError> {
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: "SELECT 1 FROM users WHERE jid = ? AND kind = ? AND disabled_at IS NULL"
                    .to_string(),
                params: vec![
                    Value::from(bot.as_str()),
                    Value::from(UserKind::Bot.as_str()),
                ],
            })
            .await
            .map_err(storage)?;
        Ok(row.is_some())
    }
}

/// Whether `jid` is an enabled bot account. Routing uses this to decide if
/// a stanza addressed to the bots domain has a recipient: a missing or
/// disabled bot is treated like a nonexistent account.
pub async fn bot_account_exists(
    actor: &ActorRef<DbActor>,
    jid: &BareJid,
) -> Result<bool, AuthError> {
    BotStore::new(actor.clone())
        .exists_enabled(jid)
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))
}

fn storage(error: impl std::fmt::Display) -> BotStoreError {
    BotStoreError::Storage(error.to_string())
}

fn column<T: DbDecode>(row: &[Value], index: usize) -> Result<T, BotStoreError> {
    let value = row_value(row, index).map_err(storage)?;
    T::decode(value.clone()).map_err(storage)
}

fn stored_bot_jid(row: &[Value], index: usize) -> Result<BotJid, BotStoreError> {
    let jid: String = column(row, index)?;
    jid.parse::<BareJid>()
        .map_err(storage)
        .and_then(|jid| BotJid::from_storage(jid).map_err(storage))
}

fn owner_health_row(row: &[Value]) -> Result<BotOwnerHealthRow, BotStoreError> {
    let bot = stored_bot_jid(row, 0)?;
    let owner: Option<String> = column(row, 1)?;
    let owner = owner
        .ok_or_else(|| BotStoreError::Storage(format!("bot {bot} has no owner")))?
        .parse::<BareJid>()
        .map_err(storage)?;
    let disabled_reason: Option<String> = column(row, 2)?;
    let disabled_reason = disabled_reason
        .map(|reason| {
            DisabledReason::parse(&reason).ok_or_else(|| {
                BotStoreError::Storage(format!("bot {bot} has an invalid disabled_reason"))
            })
        })
        .transpose()?;
    Ok(BotOwnerHealthRow {
        bot,
        owner,
        disabled_reason,
        owner_row_exists: column(row, 3)?,
        owner_has_credential: column(row, 4)?,
    })
}

fn bot_record(row: &[Value]) -> Result<BotRecord, BotStoreError> {
    let jid = stored_bot_jid(row, 0)?;
    let owner: Option<String> = column(row, 1)?;
    let owner = owner
        .ok_or_else(|| BotStoreError::Storage(format!("bot {jid} has no owner")))?
        .parse::<BareJid>()
        .map_err(storage)?;
    let display_name: Option<String> = column(row, 2)?;
    let created_at: String = column(row, 3)?;
    let created_at = parse_storage_timestamp(&created_at)
        .ok_or_else(|| BotStoreError::Storage(format!("bot {jid} has an invalid created_at")))?;
    let disabled_at: Option<String> = column(row, 4)?;
    let disabled_reason: Option<String> = column(row, 5)?;
    let disabled = match (disabled_at, disabled_reason) {
        (None, _) => None,
        (Some(at), reason) => {
            let at = parse_storage_timestamp(&at).ok_or_else(|| {
                BotStoreError::Storage(format!("bot {jid} has an invalid disabled_at"))
            })?;
            let reason = reason
                .as_deref()
                .and_then(DisabledReason::parse)
                .ok_or_else(|| {
                    BotStoreError::Storage(format!("bot {jid} has an invalid disabled_reason"))
                })?;
            Some((at, reason))
        }
    };
    Ok(BotRecord {
        name: jid.name(),
        jid,
        owner,
        display_name,
        created_at,
        disabled,
    })
}
