//! Bot account rows in `users`.

use chrono::Utc;
use jid::BareJid;
use kameo::actor::ActorRef;

use super::parse_storage_timestamp;
use super::types::{BotJid, BotRecord, DisabledReason, UserKind};
use crate::auth::AuthError;
use crate::db::actor::{
    CreateBotAccount, CreateBotOutcome, DbActor, DbExecute, DbQuery, DbQueryOne,
};
use crate::db::{row_value, DbDecode, Value};

#[derive(Debug, thiserror::Error)]
pub enum BotStoreError {
    #[error("bot storage failed: {0}")]
    Storage(String),
}

const BOT_COLUMNS: &str = "jid, owner_jid, display_name, created_at, disabled_at, disabled_reason";

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

fn bot_record(row: &[Value]) -> Result<BotRecord, BotStoreError> {
    let jid: String = column(row, 0)?;
    let jid = jid
        .parse::<BareJid>()
        .map_err(storage)
        .and_then(|jid| BotJid::from_storage(jid).map_err(storage))?;
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
