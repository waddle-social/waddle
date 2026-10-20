//! User bot accounts: storage, API tokens and authorization.
//!
//! A bot is a `users` row with `kind = 'bot'` and an `owner_jid`, addressed
//! as `<owner node>+<name>@bots.<xmpp domain>`. It authenticates with
//! `wbt_` bearer tokens ([`BotTokenStore`]) and acts only where both it and
//! its owner are allowed ([`authorize_bot`]).

pub mod authz;
pub mod store;
pub mod tokens;
mod types;

#[cfg(test)]
mod tests;

use chrono::{DateTime, SecondsFormat, Utc};

pub use authz::authorize_bot;
pub use store::{BotStore, BotStoreError};
pub use tokens::{
    BotPrincipal, BotTokenError, BotTokenSecret, BotTokenStore, BotTokenSummary, BotTokenTtl,
    MintedBotToken, RevokeOutcome, BOT_TOKEN_PREFIX, MAX_ACTIVE_TOKENS_PER_BOT,
};
pub use types::{
    BotIdError, BotJid, BotName, BotRecord, BotTokenId, BotTokenLabel, DisabledReason, UserKind,
};

/// Fixed-width RFC 3339 form for bot timestamps, so SQL text comparisons
/// (`expires_at > ?`, `last_used_at < ?`) order them correctly.
pub(crate) fn storage_timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub(crate) fn parse_storage_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}
