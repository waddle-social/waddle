//! Stores of account traffic between remote senders and local accounts
//! (ADR-0020 D7; gate G18; migrations 2010 and 2011):
//!
//! - the subscription requests remote senders sent that an account has
//!   neither approved nor denied (RFC 6121 §3.1.3), delivered again on the
//!   first available presence of each new session, one per sender and
//!   account, bounded per account and per sender domain, and forgotten
//!   after [`PENDING_TTL_MS`];
//! - the XEP-0060 nodes of remote services an account subscribed to, which
//!   gate the event notifications those services send it;
//! - the messages from remote senders that are not the account's contacts
//!   admitted to the account's offline store in the last
//!   [`OFFLINE_WINDOW_MS`], which bound how many more each sender and each
//!   sender domain may have stored.
//!
//! The bounded writes count and insert in one transaction under a lock per
//! account: an immediate transaction on SQLite, and on PostgreSQL a
//! transaction-scoped advisory lock keyed by the account, so that
//! concurrent writes for one account, on any replica, never exceed a
//! bound. The decisions are recorded in
//! `server/docs/planning/s2s-account-traffic-2047.md`.

use jid::BareJid;
use minidom::Element;
use thiserror::Error;

use super::{Database, DatabaseDriver, Transaction};

/// The most subscription requests from remote senders one account holds.
pub const MAX_PENDING_PER_ACCOUNT: usize = 256;

/// The most subscription requests one account holds from senders at one
/// domain ([`share_domain`]).
pub const MAX_PENDING_PER_DOMAIN: usize = 16;

/// The longest recorded subscription request, serialized, in bytes.
pub const MAX_PENDING_STANZA_BYTES: usize = 16 * 1024;

/// How long a recorded subscription request is kept, in milliseconds: 30
/// days. An older one is neither delivered nor counted, and is deleted by
/// the account's next recorded request.
pub const PENDING_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// The window over which offline admissions of non-contacts' messages are
/// counted, in milliseconds: one day.
pub const OFFLINE_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// The most messages from one remote sender that is not the account's
/// contact admitted to one account's offline store per window.
pub const MAX_OFFLINE_PER_SENDER: usize = 32;

/// The most messages from remote senders at one domain ([`share_domain`])
/// that are not the account's contacts admitted to one account's offline
/// store per window.
pub const MAX_OFFLINE_PER_DOMAIN: usize = 128;

/// The most messages from remote senders that are not the account's
/// contacts admitted to one account's offline store per window, whatever
/// their domain.
pub const MAX_OFFLINE_PER_ACCOUNT: usize = 512;

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

/// The key under which a remote domain's senders share an account's
/// bounds: the sender's whole domain, as admitted, or an IP literal as
/// written. Senders at different subdomains of one registrable domain each
/// have a share; the account's own bounds ([`MAX_PENDING_PER_ACCOUNT`],
/// [`MAX_OFFLINE_PER_ACCOUNT`]) bound what many domains together can
/// record, and a domain is only reachable once admitted (ADR-0020 D1).
/// Keying by a parent domain instead would put unrelated registrants under
/// a public suffix of several labels (`example.co.uk`) into one share,
/// where one of them could exhaust it for the rest.
pub fn share_domain(domain: &str) -> String {
    domain.trim_end_matches('.').to_owned()
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

/// Count, up to `limit`, the rows `sql` selects inside `tx`.
async fn count_in(
    tx: &mut Transaction<'_>,
    sql: &str,
    params: Vec<super::Value>,
) -> Result<usize, S2sAccountStoreError> {
    let mut rows = tx.query(sql, params).await.map_err(storage)?;
    let mut count = 0;
    while rows.next().await.map_err(storage)?.is_some() {
        count += 1;
    }
    Ok(count)
}

/// Begin a transaction that holds `account`'s lock for the bounded writes
/// until it ends: SQLite's write lock, taken at once, or on PostgreSQL a
/// transaction-scoped advisory lock keyed by the account.
async fn begin_for_account<'a>(
    db: &'a Database,
    account: &BareJid,
) -> Result<Transaction<'a>, S2sAccountStoreError> {
    let mut tx = db.begin_immediate().await.map_err(storage)?;
    if db.driver() == DatabaseDriver::Postgres {
        let key = ["waddle.s2s.account_traffic:", account.as_str()].concat();
        tx.query(
            "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended(?, 0))",
            crate::db_params![key],
        )
        .await
        .map_err(storage)?;
    }
    Ok(tx)
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
    /// The account's expired requests are deleted first, and the count and
    /// the write are one transaction under the account's lock.
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
        let share = share_domain(contact.domain().as_str());
        let mut tx = begin_for_account(&self.db, account).await?;
        tx.execute(
            "DELETE FROM s2s_pending_subscriptions WHERE account_jid = ? AND received_at_ms < ?",
            crate::db_params![account.as_str(), now_ms.saturating_sub(PENDING_TTL_MS)],
        )
        .await
        .map_err(storage)?;
        let held = count_in(
            &mut tx,
            "SELECT 1 FROM s2s_pending_subscriptions \
             WHERE account_jid = ? AND contact_jid = ? LIMIT 1",
            crate::db_params![account.as_str(), contact.as_str()],
        )
        .await?;
        if held == 0 {
            let of_account = count_in(
                &mut tx,
                "SELECT 1 FROM s2s_pending_subscriptions WHERE account_jid = ? LIMIT ?",
                crate::db_params![account.as_str(), limit(MAX_PENDING_PER_ACCOUNT)],
            )
            .await?;
            let of_domain = count_in(
                &mut tx,
                "SELECT 1 FROM s2s_pending_subscriptions \
                 WHERE account_jid = ? AND contact_domain = ? LIMIT ?",
                crate::db_params![
                    account.as_str(),
                    share.as_str(),
                    limit(MAX_PENDING_PER_DOMAIN)
                ],
            )
            .await?;
            if of_account >= MAX_PENDING_PER_ACCOUNT || of_domain >= MAX_PENDING_PER_DOMAIN {
                tx.commit().await.map_err(storage)?;
                return Ok(PendingOutcome::Bounds);
            }
        }
        tx.execute(
            "INSERT INTO s2s_pending_subscriptions \
             (account_jid, contact_jid, contact_domain, stanza, received_at_ms) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (account_jid, contact_jid) DO UPDATE \
             SET contact_domain = excluded.contact_domain, stanza = excluded.stanza, \
             received_at_ms = excluded.received_at_ms",
            crate::db_params![
                account.as_str(),
                contact.as_str(),
                share.as_str(),
                stanza,
                now_ms
            ],
        )
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(PendingOutcome::Recorded)
    }

    /// Whether `account` holds a request from `contact` that has not
    /// expired at `now_ms`.
    pub async fn holds(
        &self,
        account: &BareJid,
        contact: &BareJid,
        now_ms: i64,
    ) -> Result<bool, S2sAccountStoreError> {
        let count = count_up_to(
            &self.db,
            "SELECT 1 FROM s2s_pending_subscriptions \
             WHERE account_jid = ? AND contact_jid = ? AND received_at_ms >= ? LIMIT 1",
            crate::db_params![
                account.as_str(),
                contact.as_str(),
                now_ms.saturating_sub(PENDING_TTL_MS)
            ],
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

    /// Every request `account` holds that has not expired at `now_ms`,
    /// oldest first, as the contact that sent it and the stanza. A stored
    /// stanza that no longer parses is skipped.
    pub async fn list(
        &self,
        account: &BareJid,
        now_ms: i64,
    ) -> Result<Vec<(BareJid, Element)>, S2sAccountStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT contact_jid, stanza FROM s2s_pending_subscriptions \
                 WHERE account_jid = ? AND received_at_ms >= ? \
                 ORDER BY received_at_ms, contact_jid LIMIT ?",
                crate::db_params![
                    account.as_str(),
                    now_ms.saturating_sub(PENDING_TTL_MS),
                    limit(MAX_PENDING_PER_ACCOUNT)
                ],
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

/// What became of a message from a remote sender offered for offline
/// admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfflineAdmission {
    /// Admitted and counted.
    Admitted,
    /// Not admitted: the sender's, its domain's or the account's bound for
    /// the window is reached.
    Bounds,
}

/// The offline admissions of messages from remote senders that are not
/// the account's contacts (`s2s_remote_offline_admissions`), which bound
/// how many such messages reach one account's offline store per window.
#[derive(Clone)]
pub struct RemoteOfflineAdmissions {
    db: Database,
}

impl RemoteOfflineAdmissions {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Admit one message from `sender` to `account`'s offline store at
    /// `now_ms`, within the bounds of the window. Admissions older than the
    /// window are deleted first, and the count and the write are one
    /// transaction under the account's lock.
    pub async fn admit(
        &self,
        account: &BareJid,
        sender: &BareJid,
        now_ms: i64,
    ) -> Result<OfflineAdmission, S2sAccountStoreError> {
        if account.as_str().len() > MAX_ADDRESS_BYTES || sender.as_str().len() > MAX_ADDRESS_BYTES {
            return Ok(OfflineAdmission::Bounds);
        }
        let share = share_domain(sender.domain().as_str());
        let mut tx = begin_for_account(&self.db, account).await?;
        tx.execute(
            "DELETE FROM s2s_remote_offline_admissions \
             WHERE account_jid = ? AND admitted_at_ms < ?",
            crate::db_params![account.as_str(), now_ms.saturating_sub(OFFLINE_WINDOW_MS)],
        )
        .await
        .map_err(storage)?;
        let of_sender = count_in(
            &mut tx,
            "SELECT 1 FROM s2s_remote_offline_admissions \
             WHERE account_jid = ? AND sender_jid = ? LIMIT ?",
            crate::db_params![
                account.as_str(),
                sender.as_str(),
                limit(MAX_OFFLINE_PER_SENDER)
            ],
        )
        .await?;
        let of_domain = count_in(
            &mut tx,
            "SELECT 1 FROM s2s_remote_offline_admissions \
             WHERE account_jid = ? AND sender_share = ? LIMIT ?",
            crate::db_params![
                account.as_str(),
                share.as_str(),
                limit(MAX_OFFLINE_PER_DOMAIN)
            ],
        )
        .await?;
        let of_account = count_in(
            &mut tx,
            "SELECT 1 FROM s2s_remote_offline_admissions WHERE account_jid = ? LIMIT ?",
            crate::db_params![account.as_str(), limit(MAX_OFFLINE_PER_ACCOUNT)],
        )
        .await?;
        if of_sender >= MAX_OFFLINE_PER_SENDER
            || of_domain >= MAX_OFFLINE_PER_DOMAIN
            || of_account >= MAX_OFFLINE_PER_ACCOUNT
        {
            tx.commit().await.map_err(storage)?;
            return Ok(OfflineAdmission::Bounds);
        }
        tx.execute(
            "INSERT INTO s2s_remote_offline_admissions \
             (id, account_jid, sender_jid, sender_share, admitted_at_ms) VALUES (?, ?, ?, ?, ?)",
            crate::db_params![
                uuid::Uuid::new_v4().to_string(),
                account.as_str(),
                sender.as_str(),
                share.as_str(),
                now_ms
            ],
        )
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(OfflineAdmission::Admitted)
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
