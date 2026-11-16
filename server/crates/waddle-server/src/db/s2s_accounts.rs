//! Stores of account traffic between remote senders and local accounts
//! (ADR-0020 D7; gate G18; migration 2010):
//!
//! - the subscription requests remote senders sent that an account has
//!   neither approved nor denied (RFC 6121 §3.1.3), delivered again on the
//!   first available presence of each new session, one per sender and
//!   account, bounded per account and per remote domain;
//! - the XEP-0060 nodes of remote services an account subscribed to, which
//!   gate the event notifications those services send it.
//!
//! The decisions are recorded in
//! `server/docs/planning/s2s-account-traffic-2047.md`.

use jid::BareJid;
use minidom::Element;
use thiserror::Error;

use super::Database;

/// The most subscription requests from remote senders one account holds.
pub const MAX_PENDING_PER_ACCOUNT: usize = 256;

/// The most subscription requests one account holds from one remote
/// domain.
pub const MAX_PENDING_PER_DOMAIN: usize = 16;

/// The longest recorded subscription request, serialized, in bytes.
pub const MAX_PENDING_STANZA_BYTES: usize = 16 * 1024;

/// The most remote PubSub subscriptions one account holds.
pub const MAX_REMOTE_SUBSCRIPTIONS: usize = 1024;

/// The longest node name a remote PubSub subscription records, in bytes.
pub const MAX_NODE_BYTES: usize = 256;

/// The longest address either store records, in bytes.
const MAX_ADDRESS_BYTES: usize = 1023;

/// Why a store could not be used.
#[derive(Debug, Error)]
pub enum S2sAccountStoreError {
    /// The store failed; the outcome of a write is unknown.
    #[error("the account traffic store failed: {0}")]
    Storage(String),
}

fn storage(error: impl std::fmt::Display) -> S2sAccountStoreError {
    S2sAccountStoreError::Storage(error.to_string())
}

/// What became of a subscription request offered to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOutcome {
    /// Recorded, replacing an earlier request from the same sender.
    Recorded,
    /// Not recorded: the account's bound, its bound for the sender's
    /// domain, or the size of one record would be exceeded.
    Bounds,
}

/// Count, up to `limit`, the rows `sql` selects.
async fn count_up_to(
    db: &Database,
    sql: &str,
    params: Vec<super::Value>,
) -> Result<usize, S2sAccountStoreError> {
    let guard = db.guard().await.map_err(storage)?;
    let mut rows = guard.query(sql, params).await.map_err(storage)?;
    let mut count = 0;
    while rows.next().await.map_err(storage)?.is_some() {
        count += 1;
    }
    Ok(count)
}

fn limit(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// The pending subscription requests of remote senders
/// (`s2s_pending_subscriptions`).
#[derive(Clone)]
pub struct PendingSubscriptions {
    db: Database,
}

impl PendingSubscriptions {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Record `request`, the subscription request `contact` sent `account`,
    /// replacing an earlier one from the same contact, within the bounds.
    pub async fn record(
        &self,
        account: &BareJid,
        contact: &BareJid,
        request: &Element,
        now_ms: i64,
    ) -> Result<PendingOutcome, S2sAccountStoreError> {
        let stanza = String::from(request);
        if stanza.is_empty()
            || stanza.len() > MAX_PENDING_STANZA_BYTES
            || account.as_str().len() > MAX_ADDRESS_BYTES
            || contact.as_str().len() > MAX_ADDRESS_BYTES
        {
            return Ok(PendingOutcome::Bounds);
        }
        if !self.holds(account, contact).await? {
            let of_account = count_up_to(
                &self.db,
                "SELECT 1 FROM s2s_pending_subscriptions WHERE account_jid = ? LIMIT ?",
                crate::db_params![account.as_str(), limit(MAX_PENDING_PER_ACCOUNT)],
            )
            .await?;
            let of_domain = count_up_to(
                &self.db,
                "SELECT 1 FROM s2s_pending_subscriptions \
                 WHERE account_jid = ? AND contact_domain = ? LIMIT ?",
                crate::db_params![
                    account.as_str(),
                    contact.domain().as_str(),
                    limit(MAX_PENDING_PER_DOMAIN)
                ],
            )
            .await?;
            if of_account >= MAX_PENDING_PER_ACCOUNT || of_domain >= MAX_PENDING_PER_DOMAIN {
                return Ok(PendingOutcome::Bounds);
            }
        }
        let guard = self.db.guard().await.map_err(storage)?;
        guard
            .execute(
                "INSERT INTO s2s_pending_subscriptions \
                 (account_jid, contact_jid, contact_domain, stanza, received_at_ms) \
                 VALUES (?, ?, ?, ?, ?) \
                 ON CONFLICT (account_jid, contact_jid) DO UPDATE \
                 SET stanza = excluded.stanza, received_at_ms = excluded.received_at_ms",
                crate::db_params![
                    account.as_str(),
                    contact.as_str(),
                    contact.domain().as_str(),
                    stanza,
                    now_ms
                ],
            )
            .await
            .map_err(storage)?;
        Ok(PendingOutcome::Recorded)
    }

    /// Whether `account` holds a request from `contact`.
    pub async fn holds(
        &self,
        account: &BareJid,
        contact: &BareJid,
    ) -> Result<bool, S2sAccountStoreError> {
        let count = count_up_to(
            &self.db,
            "SELECT 1 FROM s2s_pending_subscriptions \
             WHERE account_jid = ? AND contact_jid = ? LIMIT 1",
            crate::db_params![account.as_str(), contact.as_str()],
        )
        .await?;
        Ok(count > 0)
    }

    /// Forget the request `contact` sent `account`, once the account
    /// answered it or the contact withdrew it. `true` when one was held.
    pub async fn remove(
        &self,
        account: &BareJid,
        contact: &BareJid,
    ) -> Result<bool, S2sAccountStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let removed = guard
            .execute(
                "DELETE FROM s2s_pending_subscriptions WHERE account_jid = ? AND contact_jid = ?",
                crate::db_params![account.as_str(), contact.as_str()],
            )
            .await
            .map_err(storage)?;
        Ok(removed > 0)
    }

    /// Every request `account` holds, oldest first, as the contact that
    /// sent it and the stanza. A stored stanza that no longer parses is
    /// skipped.
    pub async fn list(
        &self,
        account: &BareJid,
    ) -> Result<Vec<(BareJid, Element)>, S2sAccountStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT contact_jid, stanza FROM s2s_pending_subscriptions \
                 WHERE account_jid = ? ORDER BY received_at_ms, contact_jid LIMIT ?",
                crate::db_params![account.as_str(), limit(MAX_PENDING_PER_ACCOUNT)],
            )
            .await
            .map_err(storage)?;
        let mut requests = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            let contact: String = row.get(0).map_err(storage)?;
            let stanza: String = row.get(1).map_err(storage)?;
            let (Ok(contact), Ok(stanza)) = (contact.parse::<BareJid>(), stanza.parse::<Element>())
            else {
                continue;
            };
            requests.push((contact, stanza));
        }
        Ok(requests)
    }
}

/// What became of a remote PubSub subscription offered to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionOutcome {
    Recorded,
    /// The account already holds as many as it may, or the record is too
    /// long.
    Bounds,
}

/// The remote PubSub nodes local accounts subscribed to
/// (`s2s_remote_pubsub_subscriptions`).
#[derive(Clone)]
pub struct RemotePubSubSubscriptions {
    db: Database,
}

impl RemotePubSubSubscriptions {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Whether `account` may record one more subscription.
    pub async fn has_room(&self, account: &BareJid) -> Result<bool, S2sAccountStoreError> {
        let held = count_up_to(
            &self.db,
            "SELECT 1 FROM s2s_remote_pubsub_subscriptions WHERE account_jid = ? LIMIT ?",
            crate::db_params![account.as_str(), limit(MAX_REMOTE_SUBSCRIPTIONS)],
        )
        .await?;
        Ok(held < MAX_REMOTE_SUBSCRIPTIONS)
    }

    /// Record that `account` subscribed to `node` of `service`.
    pub async fn record(
        &self,
        account: &BareJid,
        service: &BareJid,
        node: &str,
        now_ms: i64,
    ) -> Result<SubscriptionOutcome, S2sAccountStoreError> {
        if node.is_empty()
            || node.len() > MAX_NODE_BYTES
            || account.as_str().len() > MAX_ADDRESS_BYTES
            || service.as_str().len() > MAX_ADDRESS_BYTES
        {
            return Ok(SubscriptionOutcome::Bounds);
        }
        if !self.holds(account, service, node).await? && !self.has_room(account).await? {
            return Ok(SubscriptionOutcome::Bounds);
        }
        let guard = self.db.guard().await.map_err(storage)?;
        guard
            .execute(
                "INSERT INTO s2s_remote_pubsub_subscriptions \
                 (account_jid, service_jid, node, subscribed_at_ms) VALUES (?, ?, ?, ?) \
                 ON CONFLICT (account_jid, service_jid, node) DO NOTHING",
                crate::db_params![account.as_str(), service.as_str(), node, now_ms],
            )
            .await
            .map_err(storage)?;
        Ok(SubscriptionOutcome::Recorded)
    }

    /// Forget that `account` subscribed to `node` of `service`.
    pub async fn remove(
        &self,
        account: &BareJid,
        service: &BareJid,
        node: &str,
    ) -> Result<(), S2sAccountStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        guard
            .execute(
                "DELETE FROM s2s_remote_pubsub_subscriptions \
                 WHERE account_jid = ? AND service_jid = ? AND node = ?",
                crate::db_params![account.as_str(), service.as_str(), node],
            )
            .await
            .map_err(storage)?;
        Ok(())
    }

    /// Whether `account` subscribed to `node` of `service`.
    pub async fn holds(
        &self,
        account: &BareJid,
        service: &BareJid,
        node: &str,
    ) -> Result<bool, S2sAccountStoreError> {
        let count = count_up_to(
            &self.db,
            "SELECT 1 FROM s2s_remote_pubsub_subscriptions \
             WHERE account_jid = ? AND service_jid = ? AND node = ? LIMIT 1",
            crate::db_params![account.as_str(), service.as_str(), node],
        )
        .await?;
        Ok(count > 0)
    }
}
