//! Stores of the Spaces accounts at other servers use over S2S (ADR-0020
//! D10; XEP-0503 over XEP-0060; migrations 2012 and 2013):
//!
//! - which Spaces are open to other servers ([`FederatedSpaces`]): a row
//!   for each Space whose `federated` setting is on, bound to the PubSub
//!   node it was turned on for;
//! - the subscriptions remote accounts made to those Spaces over inbound
//!   streams ([`SpaceSubscriptions`]), one for an account and a Space as
//!   the PubSub service keeps them, each with what its stream proved,
//!   beside the service's own subscription record. An event notification
//!   leaves for an account at another server only while such a row exists.
//!
//! Both bind a row to the creation time of its node, so a row left by a
//! Space that was deleted counts for nothing once a Space of the same name
//! is created again.
//!
//! A subscription is counted against its bounds and recorded in one
//! transaction: an immediate transaction on SQLite, and on PostgreSQL under
//! two transaction-scoped advisory locks, one keyed on the subscriber's
//! domain and then one keyed on the Space, so that the subscriptions of one
//! remote domain, and those to one Space, are counted one at a time on
//! every replica. Turning a Space's `federated` setting off and removing
//! one subscription take the Space's lock too, and a subscription re-reads
//! the setting under it, so no subscription is recorded for a Space after
//! its setting went off. A ban is decided by the caller once the
//! subscription is committed (see [`SpaceSubscriptions::record`]): no
//! transaction of this store waits for another connection while it holds
//! one.
//!
//! A subscription lapses [`SUBSCRIPTION_LIFETIME_MS`] after it was last
//! made: a lapsed row counts against no bound and names no subscriber, and
//! subscribing again renews it. Recording a subscription removes lapsed
//! rows, and rows of domains no longer admitted, and tells its caller which
//! ([`Recording::removed`]), so that the PubSub service's own records of
//! them go too.
//!
//! A Space's deletion runs under a lock of its own
//! ([`lock_space_deletion`]), so two deletions of one Space never
//! interleave. The decisions are recorded in
//! `server/docs/planning/s2s-routing-2047.md`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use jid::BareJid;
use thiserror::Error;
use uuid::Uuid;

use super::{Database, DatabaseDriver, Row, Transaction};
use crate::permissions::host_grant::{
    guard_queue_key, AuthorityIncarnation, GrantGeneration, ScopeLifecycleId, StreamEvidence,
    HOST_GRANT_POLICY_VERSION,
};
use crate::s2s::conn::AuthenticationStrength;
use crate::s2s::verify::TrustProfileKind;

/// Subscriptions accounts at other servers may hold to one local Space.
pub const MAX_REMOTE_SUBSCRIBERS_PER_SPACE: usize = 256;

/// Subscriptions the accounts of one remote domain may hold to one local
/// Space, so that one domain cannot take a Space's every place.
pub const MAX_REMOTE_SUBSCRIBERS_PER_DOMAIN_AND_SPACE: usize = 32;

/// Subscriptions one remote account may hold to local Spaces.
pub const MAX_SUBSCRIPTIONS_PER_REMOTE_ACCOUNT: usize = 64;

/// Subscriptions the accounts of one remote domain may hold to local
/// Spaces.
pub const MAX_SUBSCRIPTIONS_PER_REMOTE_DOMAIN: usize = 4_096;

/// Subscriptions accounts at other servers may hold to the Spaces of the
/// deployment.
pub const MAX_REMOTE_SUBSCRIPTIONS: usize = 65_536;

/// How long a subscription holds, in milliseconds, after the remote
/// account last made it: thirty days. A subscription not made again within
/// that time lapses, as one whose server stopped answering would otherwise
/// hold its place for ever.
pub const SUBSCRIPTION_LIFETIME_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// The most remote domains whose rows one sweep of the whole store reads,
/// when the bound on the deployment's remote subscriptions is reached.
/// Each sweep starts after the last domain the one before it read
/// ([`SWEEP_CURSOR`]), so successive sweeps read every domain in turn.
const MAX_SWEPT_DOMAINS: usize = 4096;

/// The most rows one sweep of the whole store removes, lapsed rows and rows
/// of domains no longer admitted together: what one subscription request
/// removes, and whose PubSub records its caller then removes, stays
/// bounded.
const MAX_SWEPT_ROWS: usize = 1024;

/// The remote domain after which the next sweep of the whole store starts
/// reading domains, wrapping round to the first once it reaches the last.
/// Each process keeps its own; empty before its first sweep.
static SWEEP_CURSOR: Mutex<String> = Mutex::new(String::new());

fn sweep_cursor() -> String {
    SWEEP_CURSOR
        .lock()
        .map(|cursor| cursor.clone())
        .unwrap_or_default()
}

fn set_sweep_cursor(domain: &str) {
    if let Ok(mut cursor) = SWEEP_CURSOR.lock() {
        domain.clone_into(&mut cursor);
    }
}

/// The longest node name either store records, in bytes.
pub const MAX_NODE_BYTES: usize = 1023;

/// The longest subscribed address a subscription records, in bytes.
const MAX_ADDRESS_BYTES: usize = 1023;

/// The longest service address a subscription records, in bytes.
const MAX_SERVICE_BYTES: usize = 255;

/// Why a store could not be used.
#[derive(Debug, Error)]
pub enum SpaceStoreError {
    /// The store failed; the outcome of a write is unknown.
    #[error("the federated spaces store failed: {0}")]
    Storage(String),
}

fn storage(error: impl std::fmt::Display) -> SpaceStoreError {
    SpaceStoreError::Storage(error.to_string())
}

fn limit(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn strength_code(strength: AuthenticationStrength) -> &'static str {
    match strength {
        AuthenticationStrength::Pkix => "pkix",
        AuthenticationStrength::Dialback => "dialback",
    }
}

fn trust_profile_code(profile: TrustProfileKind) -> &'static str {
    match profile {
        TrustProfileKind::PublicWebPki => "public_webpki",
        TrustProfileKind::PrivateCa => "private_ca",
    }
}

/// The Spaces open to accounts at other servers (`s2s_federated_spaces`).
#[derive(Clone)]
pub struct FederatedSpaces {
    db: Database,
}

impl FederatedSpaces {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// The creation time, in milliseconds, of the node the `federated`
    /// setting of `node` at `service` was turned on for; `None` when the
    /// setting is off. The setting counts only for a node created at that
    /// time.
    pub async fn enabled_for(
        &self,
        service: &BareJid,
        node: &str,
    ) -> Result<Option<i64>, SpaceStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT node_created_at_ms FROM s2s_federated_spaces \
                 WHERE service_jid = ? AND node_name = ?",
                crate::db_params![service.as_str(), node],
            )
            .await
            .map_err(storage)?;
        match rows.next().await.map_err(storage)? {
            Some(row) => Ok(Some(row.get(0).map_err(storage)?)),
            None => Ok(None),
        }
    }

    /// Whether the `federated` setting is on for the node `node` at
    /// `service` that was created at `node_created_at_ms`.
    pub async fn is_federated(
        &self,
        service: &BareJid,
        node: &str,
        node_created_at_ms: i64,
    ) -> Result<bool, SpaceStoreError> {
        Ok(self.enabled_for(service, node).await? == Some(node_created_at_ms))
    }

    /// Turn the `federated` setting on for the node `node` at `service`
    /// that was created at `node_created_at_ms`, replacing what an earlier
    /// node of that name left. `false` when the names do not fit the store,
    /// and nothing was written.
    pub async fn enable(
        &self,
        service: &BareJid,
        node: &str,
        node_created_at_ms: i64,
        now_ms: i64,
    ) -> Result<bool, SpaceStoreError> {
        if node.is_empty()
            || node.len() > MAX_NODE_BYTES
            || service.as_str().len() > MAX_ADDRESS_BYTES
        {
            return Ok(false);
        }
        let guard = self.db.guard().await.map_err(storage)?;
        guard
            .execute(
                "INSERT INTO s2s_federated_spaces \
                 (service_jid, node_name, node_created_at_ms, enabled_at_ms) \
                 VALUES (?, ?, ?, ?) \
                 ON CONFLICT (service_jid, node_name) DO UPDATE SET \
                 node_created_at_ms = excluded.node_created_at_ms, \
                 enabled_at_ms = excluded.enabled_at_ms",
                crate::db_params![service.as_str(), node, node_created_at_ms, now_ms],
            )
            .await
            .map_err(storage)?;
        Ok(true)
    }

    /// Turn the `federated` setting off for `node` at `service`, whichever
    /// node of that name it was on for, and forget the subscriptions remote
    /// accounts made to it, in one transaction under the Space's lock: a
    /// subscription being recorded for the Space at the same time is
    /// either forgotten here or refused for the setting it re-reads.
    pub async fn close(&self, service: &BareJid, node: &str) -> Result<(), SpaceStoreError> {
        let mut tx = self.db.begin_immediate().await.map_err(storage)?;
        lock_space(&self.db, &mut tx, service, node).await?;
        tx.execute(
            "DELETE FROM s2s_federated_spaces WHERE service_jid = ? AND node_name = ?",
            crate::db_params![service.as_str(), node],
        )
        .await
        .map_err(storage)?;
        tx.execute(
            "DELETE FROM s2s_space_subscriptions WHERE service_jid = ? AND node_name = ?",
            crate::db_params![service.as_str(), node],
        )
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    /// Up to `most` nodes of `service` whose setting is on, in name order,
    /// each with the creation time of the node it is on for.
    pub async fn list(
        &self,
        service: &BareJid,
        most: usize,
    ) -> Result<Vec<(String, i64)>, SpaceStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT node_name, node_created_at_ms FROM s2s_federated_spaces \
                 WHERE service_jid = ? ORDER BY node_name ASC LIMIT ?",
                crate::db_params![service.as_str(), limit(most)],
            )
            .await
            .map_err(storage)?;
        let mut nodes = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            nodes.push((row.get(0).map_err(storage)?, row.get(1).map_err(storage)?));
        }
        Ok(nodes)
    }
}

/// A subscription a remote account made to a local Space over an inbound
/// stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceSubscription {
    /// The account that subscribed. The PubSub service keeps one
    /// subscription for an account, under its bare JID, and notifications
    /// are addressed to it.
    pub account: BareJid,
    /// What the stream that carried the subscription request proved.
    pub evidence: StreamEvidence,
    /// The S2S snapshot that stream was admitted under.
    pub snapshot: Uuid,
    /// The canonical affiliation under which this protected preference was created.
    pub grant: Option<SpaceSubscriptionGrant>,
}

/// The scope and effective affiliation generation behind a protected preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceSubscriptionGrant {
    pub lifecycle: ScopeLifecycleId,
    pub incarnation: AuthorityIncarnation,
    pub generation: GrantGeneration,
}

/// A subscription request as the store records it.
#[derive(Debug, Clone, Copy)]
pub struct SubscriptionRequest<'a> {
    /// The remote account the request is from, which is what subscribes.
    pub account: &'a BareJid,
    /// What the stream that carried the request proved.
    pub evidence: StreamEvidence,
    /// The S2S snapshot that stream was admitted under.
    pub snapshot: Uuid,
    pub now_ms: i64,
}

/// What became of a subscription offered to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionRecord {
    /// Recorded, or recorded again with the evidence of this request.
    Recorded,
    /// Not recorded: a bound on the Space's remote subscribers, on those of
    /// the account's domain to the Space, on the account's or its domain's
    /// subscriptions or on the deployment's would be exceeded, or an
    /// address does not fit the store.
    Bounds,
    /// Not recorded: the Space's `federated` setting is not on for the node
    /// created at the time the request named, read under the Space's lock.
    NotFederated,
}

/// A subscription the store removed while it recorded another: it had
/// lapsed, or its domain is no longer admitted. The PubSub service's own
/// record of it is the caller's to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedSubscription {
    pub service: BareJid,
    pub node: String,
    pub account: BareJid,
}

/// What became of a subscription offered to the store, and which others it
/// removed meanwhile. Only a subscription that was recorded removes any:
/// otherwise nothing the request did is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    pub outcome: SubscriptionRecord,
    /// The subscriptions removed, the account's own renewed one aside.
    pub removed: Vec<RemovedSubscription>,
}

impl Recording {
    fn only(outcome: SubscriptionRecord) -> Self {
        Self {
            outcome,
            removed: Vec::new(),
        }
    }
}

/// The subscriptions of remote accounts to local Spaces
/// (`s2s_space_subscriptions`).
#[derive(Clone)]
pub struct SpaceSubscriptions {
    db: Database,
}

/// One subscription being counted against the bounds: of `account` to the
/// Space `node` of `service`, made at `now_ms`.
#[derive(Clone, Copy)]
struct Offer<'a> {
    service: &'a BareJid,
    node: &'a str,
    account: &'a BareJid,
    now_ms: i64,
}

/// Take the transaction-scoped advisory lock keyed on `key` on
/// PostgreSQL. SQLite's immediate transaction already holds the database's
/// write lock.
async fn advisory_lock(
    db: &Database,
    tx: &mut Transaction<'_>,
    key: String,
) -> Result<(), SpaceStoreError> {
    if db.driver() == DatabaseDriver::Postgres {
        tx.query(
            "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended(?, 0))",
            crate::db_params![key],
        )
        .await
        .map_err(storage)?;
    }
    Ok(())
}

/// Take the lock under which the subscriptions to the Space `node` of
/// `service` are counted and its `federated` setting is turned off. A
/// service is a bare domain, so no `/` of a node name makes two Spaces
/// share a key.
async fn lock_space(
    db: &Database,
    tx: &mut Transaction<'_>,
    service: &BareJid,
    node: &str,
) -> Result<(), SpaceStoreError> {
    let key = [
        "waddle.s2s.space_subscriptions.space:",
        service.as_str(),
        "/",
        node,
    ]
    .concat();
    advisory_lock(db, tx, key).await
}

/// Begin a transaction that holds the locks under which a subscription of
/// an account of `domain` to the Space `node` of `service` is counted and
/// written: SQLite's write lock, taken at once, or on PostgreSQL the
/// domain's advisory lock and then the Space's. Every transaction that
/// takes both takes them in that order, and one that takes the Space's
/// alone never waits for a domain's, so they cannot wait on each other.
async fn begin_for_subscription<'a>(
    db: &'a Database,
    domain: &str,
    service: &BareJid,
    node: &str,
    grant: Option<SpaceSubscriptionGrant>,
) -> Result<Option<Transaction<'a>>, SpaceStoreError> {
    let mut tx = db.begin_immediate().await.map_err(storage)?;
    if let Some(grant) = grant {
        if db.driver() != DatabaseDriver::Postgres {
            return Ok(None);
        }
        tx.execute(
            "SELECT pg_catalog.pg_advisory_xact_lock_shared(?)",
            crate::db_params![guard_queue_key(grant.lifecycle)],
        )
        .await
        .map_err(storage)?;
        let mut rows = tx
            .query(
                "SELECT lifecycle_id FROM host_grant_scopes WHERE lifecycle_id = ? FOR SHARE",
                crate::db_params![grant.lifecycle.to_storage()],
            )
            .await
            .map_err(storage)?;
        let exists = rows.next().await.map_err(storage)?.is_some();
        drop(rows);
        if !exists {
            tx.rollback().await.map_err(storage)?;
            return Ok(None);
        }
    }
    let key = ["waddle.s2s.space_subscriptions.domain:", domain].concat();
    advisory_lock(db, &mut tx, key).await?;
    lock_space(db, &mut tx, service, node).await?;
    Ok(Some(tx))
}

async fn protected_subscription_still_allowed(
    tx: &mut Transaction<'_>,
    service: &BareJid,
    node: &str,
    node_created_at_ms: i64,
    request: &SubscriptionRequest<'_>,
    grant: SpaceSubscriptionGrant,
) -> Result<bool, SpaceStoreError> {
    let mut rows = tx.query(
        "SELECT 1 FROM host_grant_space_bindings b \
         JOIN host_grant_scopes s ON s.lifecycle_id = b.lifecycle_id \
         JOIN host_grant_authority a ON a.singleton AND a.incarnation = s.authority_incarnation \
         JOIN pubsub_nodes p ON p.owner_jid = b.service_jid AND p.node_name = b.space_node \
         JOIN host_grant_space_affiliations g ON g.lifecycle_id = b.lifecycle_id \
         WHERE b.lifecycle_id = ? AND b.service_jid = ? AND b.space_node = ? AND b.federated \
         AND b.authority_incarnation = a.incarnation AND a.incarnation = ? \
         AND s.admission_state = 'active' AND s.policy_version = ? \
         AND p.created_at_ms = ? AND p.access_model = b.access_model \
         AND (p.node_type IS NULL OR p.node_type = ?) \
         AND g.subject_jid = ? AND g.changed_generation = ? AND g.affiliation <> 'outcast' \
         AND (g.effective_affiliation = 'member' OR (g.effective_affiliation IN ('owner', 'publisher') \
              AND g.issuance_basis = 'trusted_home' AND (? <> 0)))",
        crate::db_params![
            grant.lifecycle.to_storage(), service.as_str(), node,
            grant.incarnation.to_storage(), HOST_GRANT_POLICY_VERSION, node_created_at_ms,
            waddle_xmpp::xep::xep0503::NS_SPACES, request.account.as_str(), grant.generation.value(),
            request.evidence.strength() == AuthenticationStrength::Pkix
        ],
    ).await.map_err(storage)?;
    Ok(rows.next().await.map_err(storage)?.is_some())
}

/// The remote domains of the rows of `s2s_space_subscriptions` that match
/// `filter`, at most `most` of them, inside `tx`.
async fn domains_in(
    tx: &mut Transaction<'_>,
    filter: &str,
    mut params: Vec<super::Value>,
    most: usize,
) -> Result<Vec<String>, SpaceStoreError> {
    let sql = [
        "SELECT DISTINCT subscriber_domain FROM s2s_space_subscriptions ",
        filter,
        " ORDER BY subscriber_domain LIMIT ?",
    ]
    .concat();
    params.push(limit(most).into());
    let mut rows = tx.query(&sql, params).await.map_err(storage)?;
    let mut domains = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage)? {
        domains.push(row.get(0).map_err(storage)?);
    }
    Ok(domains)
}

/// The subscriptions named by the rows a `RETURNING service_jid,
/// node_name, subscriber_jid` statement gave back. A row whose addresses do
/// not parse names no subscription the PubSub service could hold.
async fn removed_from(
    mut rows: super::Rows,
    removed: &mut Vec<RemovedSubscription>,
) -> Result<usize, SpaceStoreError> {
    let mut count = 0;
    while let Some(row) = rows.next().await.map_err(storage)? {
        count += 1;
        let service: String = row.get(0).map_err(storage)?;
        let node: String = row.get(1).map_err(storage)?;
        let account: String = row.get(2).map_err(storage)?;
        if let (Ok(service), Ok(account)) = (service.parse(), account.parse()) {
            removed.push(RemovedSubscription {
                service,
                node,
                account,
            });
        }
    }
    Ok(count)
}

/// Delete, inside `tx`, the rows of `s2s_space_subscriptions` matching
/// `filter`, adding the subscriptions they recorded to `removed`. Every row
/// it reaches belongs to the Space whose lock `tx` holds, which no other
/// transaction but a sweep of the whole store ([`sweep_rows`]) changes, so
/// waiting for a row another transaction holds closes no cycle of waits.
async fn delete_rows(
    tx: &mut Transaction<'_>,
    filter: &str,
    params: Vec<super::Value>,
    removed: &mut Vec<RemovedSubscription>,
) -> Result<(), SpaceStoreError> {
    let sql = [
        "DELETE FROM s2s_space_subscriptions ",
        filter,
        " RETURNING service_jid, node_name, subscriber_jid",
    ]
    .concat();
    let rows = tx.query(&sql, params).await.map_err(storage)?;
    removed_from(rows, removed).await?;
    Ok(())
}

/// Delete, inside `tx`, at most `most` rows of `s2s_space_subscriptions`
/// matching `filter`, taken in `order`, adding the subscriptions they
/// recorded to `removed`; the number of rows deleted.
///
/// These rows may belong to any Space, whose lock `tx` does not hold. On
/// PostgreSQL a row another transaction holds is passed over, never waited
/// for (`FOR UPDATE SKIP LOCKED`), so a sweep never waits for a row and can
/// close no cycle of waits with the transactions that hold rows it reaches;
/// the row is left to that transaction, or to a later sweep. SQLite's
/// immediate transactions run one at a time.
async fn sweep_rows(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    filter: &str,
    mut params: Vec<super::Value>,
    order: &str,
    most: usize,
    removed: &mut Vec<RemovedSubscription>,
) -> Result<usize, SpaceStoreError> {
    let skip_locked = if driver == DatabaseDriver::Postgres {
        " FOR UPDATE SKIP LOCKED"
    } else {
        ""
    };
    let sql = [
        "DELETE FROM s2s_space_subscriptions \
         WHERE (service_jid, node_name, subscriber_jid) IN \
         (SELECT service_jid, node_name, subscriber_jid FROM s2s_space_subscriptions ",
        filter,
        order,
        " LIMIT ?",
        skip_locked,
        ") RETURNING service_jid, node_name, subscriber_jid",
    ]
    .concat();
    params.push(limit(most).into());
    let rows = tx.query(&sql, params).await.map_err(storage)?;
    removed_from(rows, removed).await
}

/// Remove, inside `tx`, the rows of the Space that `filter` names whose
/// remote domain `admitted` no longer admits, reading at most `most`
/// domains. A domain that is not admitted receives nothing (the
/// notification path decides admission again for each subscriber), so its
/// rows only hold places.
async fn sweep_unadmitted(
    tx: &mut Transaction<'_>,
    filter: &str,
    params: Vec<super::Value>,
    most: usize,
    admitted: &(dyn Fn(&str) -> bool + Send + Sync),
    removed: &mut Vec<RemovedSubscription>,
) -> Result<(), SpaceStoreError> {
    let domains = domains_in(tx, filter, params.clone(), most).await?;
    for domain in domains.iter().filter(|domain| !admitted(domain)) {
        let filter = [filter, " AND subscriber_domain = ?"].concat();
        let mut params = params.clone();
        params.push(domain.as_str().into());
        delete_rows(tx, &filter, params, removed).await?;
    }
    Ok(())
}

/// Sweep the whole store inside `tx`, once the bound on the deployment's
/// remote subscriptions is reached: lapsed rows, oldest first, and then the
/// rows of domains `admitted` no longer admits, read from the domain after
/// [`SWEEP_CURSOR`] on and wrapping round, at most [`MAX_SWEPT_DOMAINS`]
/// domains and [`MAX_SWEPT_ROWS`] rows in all. The cursor is left at the
/// last domain whose rows were all read.
async fn sweep_store(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    lapsed_before: i64,
    admitted: &(dyn Fn(&str) -> bool + Send + Sync),
    removed: &mut Vec<RemovedSubscription>,
) -> Result<(), SpaceStoreError> {
    let mut budget = MAX_SWEPT_ROWS;
    budget -= sweep_rows(
        tx,
        driver,
        "WHERE subscribed_at_ms < ?",
        crate::db_params![lapsed_before],
        " ORDER BY subscribed_at_ms",
        budget,
        removed,
    )
    .await?
    .min(budget);
    if budget == 0 {
        return Ok(());
    }
    let start = sweep_cursor();
    let mut domains = domains_in(
        tx,
        "WHERE subscriber_domain > ?",
        crate::db_params![start.as_str()],
        MAX_SWEPT_DOMAINS,
    )
    .await?;
    if domains.len() < MAX_SWEPT_DOMAINS && !start.is_empty() {
        let wrapped = domains_in(
            tx,
            "WHERE subscriber_domain <= ?",
            crate::db_params![start.as_str()],
            MAX_SWEPT_DOMAINS - domains.len(),
        )
        .await?;
        domains.extend(wrapped);
    }
    let mut read_through = None;
    for domain in &domains {
        if !admitted(domain) {
            let swept = sweep_rows(
                tx,
                driver,
                "WHERE subscriber_domain = ?",
                crate::db_params![domain.as_str()],
                "",
                budget,
                removed,
            )
            .await?;
            if swept >= budget {
                // The domain may hold more rows: the next sweep starts
                // with it.
                break;
            }
            budget -= swept;
        }
        read_through = Some(domain);
    }
    if let Some(domain) = read_through {
        set_sweep_cursor(domain);
    }
    Ok(())
}

/// How many rows of `s2s_space_subscriptions` match `filter`, counted by
/// the database up to `most`, inside `tx`: one row is read whatever the
/// bound, so a bound of thousands costs no more to check than one of tens.
async fn count_in(
    tx: &mut Transaction<'_>,
    filter: &str,
    mut params: Vec<super::Value>,
    most: usize,
) -> Result<usize, SpaceStoreError> {
    let sql = [
        "SELECT COUNT(*) FROM (SELECT 1 FROM s2s_space_subscriptions ",
        filter,
        " LIMIT ?) AS bounded",
    ]
    .concat();
    params.push(limit(most).into());
    let mut rows = tx.query(&sql, params).await.map_err(storage)?;
    let Some(row) = rows.next().await.map_err(storage)? else {
        return Err(storage("a count returned no row"));
    };
    let count: i64 = row.get(0).map_err(storage)?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

/// A row read back as the subscription it records. A row whose evidence
/// cannot be read is an error, never evidence the row does not hold.
fn decode(row: &Row) -> Result<SpaceSubscription, SpaceStoreError> {
    let account: String = row.get(0).map_err(storage)?;
    let strength: String = row.get(1).map_err(storage)?;
    let trust_profile: Option<String> = row.get(2).map_err(storage)?;
    let snapshot: String = row.get(3).map_err(storage)?;
    let strength = match strength.as_str() {
        "pkix" => AuthenticationStrength::Pkix,
        "dialback" => AuthenticationStrength::Dialback,
        _ => return Err(storage("an unknown authentication strength")),
    };
    let trust_profile = match trust_profile.as_deref() {
        None => None,
        Some("public_webpki") => Some(TrustProfileKind::PublicWebPki),
        Some("private_ca") => Some(TrustProfileKind::PrivateCa),
        Some(_) => return Err(storage("an unknown trust profile")),
    };
    let grant = match (
        row.get::<Option<String>>(4).map_err(storage)?,
        row.get::<Option<String>>(5).map_err(storage)?,
        row.get::<Option<i64>>(6).map_err(storage)?,
    ) {
        (None, None, None) => None,
        (Some(lifecycle), Some(incarnation), Some(generation)) => Some(SpaceSubscriptionGrant {
            lifecycle: ScopeLifecycleId::from_storage(&lifecycle)
                .ok_or_else(|| storage("invalid subscription scope"))?,
            incarnation: AuthorityIncarnation::from_storage(&incarnation)
                .ok_or_else(|| storage("invalid subscription authority incarnation"))?,
            generation: GrantGeneration::new(generation)
                .ok_or_else(|| storage("invalid subscription affiliation generation"))?,
        }),
        _ => return Err(storage("incomplete subscription grant identity")),
    };
    Ok(SpaceSubscription {
        account: account.parse().map_err(storage)?,
        evidence: StreamEvidence::recorded(strength, trust_profile),
        snapshot: snapshot.parse().map_err(storage)?,
        grant,
    })
}

impl SpaceSubscriptions {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Record that `request.account` subscribed to the node `node` of
    /// `service` created at `node_created_at_ms`, within the bounds, while
    /// the Space's `federated` setting is on for that node. A subscription
    /// the account already holds to that node is recorded again with this
    /// request's evidence, which renews it, and counts against no bound;
    /// rows left for an earlier node of the name, and lapsed rows of the
    /// Space, are removed first.
    ///
    /// The account's ban, which the PubSub service stores, is not read
    /// here: the transaction holds its connection and its locks, and reading
    /// from a store that may share the pool would wait for a second
    /// connection while holding them. The caller reads it once this
    /// returns: a ban is written before the banned account's row is removed
    /// ([`Self::remove`]), so either that read finds the ban, and the caller
    /// removes the row, or the ban's removal comes after this commit and
    /// finds the row.
    ///
    /// Only the rows of domains `admitted` admits now hold a place against
    /// the bound on the Space's remote subscribers and against the
    /// deployment's: when either is reached, the rows of the other domains
    /// are removed (for the deployment's, by a sweep of the whole store
    /// that removes lapsed rows too, within [`MAX_SWEPT_ROWS`]) and the
    /// bound is counted again. The rows removed are listed in the answer
    /// once the subscription is recorded.
    pub async fn record(
        &self,
        service: &BareJid,
        node: &str,
        node_created_at_ms: i64,
        request: SubscriptionRequest<'_>,
        admitted: &(dyn Fn(&str) -> bool + Send + Sync),
    ) -> Result<Recording, SpaceStoreError> {
        self.record_preference(service, node, node_created_at_ms, request, admitted, None)
            .await
    }

    /// Record a protected preference under the canonical scope guard.
    pub async fn record_protected(
        &self,
        service: &BareJid,
        node: &str,
        node_created_at_ms: i64,
        request: SubscriptionRequest<'_>,
        admitted: &(dyn Fn(&str) -> bool + Send + Sync),
        grant: SpaceSubscriptionGrant,
    ) -> Result<Recording, SpaceStoreError> {
        self.record_preference(
            service,
            node,
            node_created_at_ms,
            request,
            admitted,
            Some(grant),
        )
        .await
    }

    async fn record_preference(
        &self,
        service: &BareJid,
        node: &str,
        node_created_at_ms: i64,
        request: SubscriptionRequest<'_>,
        admitted: &(dyn Fn(&str) -> bool + Send + Sync),
        grant: Option<SpaceSubscriptionGrant>,
    ) -> Result<Recording, SpaceStoreError> {
        let SubscriptionRequest {
            account,
            evidence,
            snapshot,
            now_ms,
        } = request;
        if node.is_empty()
            || node.len() > MAX_NODE_BYTES
            || service.as_str().len() > MAX_SERVICE_BYTES
            || account.as_str().len() > MAX_ADDRESS_BYTES
        {
            return Ok(Recording::only(SubscriptionRecord::Bounds));
        }
        let domain = account.domain().as_str();
        let lapsed_before = now_ms.saturating_sub(SUBSCRIPTION_LIFETIME_MS);
        let Some(mut tx) = begin_for_subscription(&self.db, domain, service, node, grant).await?
        else {
            return Ok(Recording::only(SubscriptionRecord::NotFederated));
        };
        // The setting, read again under the Space's lock: turning it off
        // takes that lock, so a subscription is recorded only for a Space
        // whose setting is still on for this very node.
        let federated = if let Some(grant) = grant {
            protected_subscription_still_allowed(
                &mut tx,
                service,
                node,
                node_created_at_ms,
                &request,
                grant,
            )
            .await?
        } else {
            let mut rows = tx
                .query(
                    "SELECT node_created_at_ms FROM s2s_federated_spaces \
                     WHERE service_jid = ? AND node_name = ?",
                    crate::db_params![service.as_str(), node],
                )
                .await
                .map_err(storage)?;
            match rows.next().await.map_err(storage)? {
                Some(row) => row.get::<i64>(0).map_err(storage)? == node_created_at_ms,
                None => false,
            }
        };
        if !federated {
            tx.rollback().await.map_err(storage)?;
            return Ok(Recording::only(SubscriptionRecord::NotFederated));
        }
        // Rows of an earlier node of the name named a Space that was
        // deleted, whose PubSub records went with it.
        tx.execute(
            "DELETE FROM s2s_space_subscriptions \
             WHERE service_jid = ? AND node_name = ? AND node_created_at_ms <> ?",
            crate::db_params![service.as_str(), node, node_created_at_ms],
        )
        .await
        .map_err(storage)?;
        let mut removed = Vec::new();
        delete_rows(
            &mut tx,
            "WHERE service_jid = ? AND node_name = ? AND subscribed_at_ms < ?",
            crate::db_params![service.as_str(), node, lapsed_before],
            &mut removed,
        )
        .await?;
        let held = count_in(
            &mut tx,
            "WHERE service_jid = ? AND node_name = ? AND subscriber_jid = ?",
            crate::db_params![service.as_str(), node, account.as_str()],
            1,
        )
        .await?
            > 0;
        if !held
            && !self
                .within_bounds(
                    &mut tx,
                    Offer {
                        service,
                        node,
                        account,
                        now_ms,
                    },
                    admitted,
                    &mut removed,
                )
                .await?
        {
            tx.rollback().await.map_err(storage)?;
            return Ok(Recording::only(SubscriptionRecord::Bounds));
        }
        let recorded = tx
            .execute(
                "INSERT INTO s2s_space_subscriptions \
             (service_jid, node_name, subscriber_jid, account_jid, subscriber_domain, \
              node_created_at_ms, strength, trust_profile, snapshot_generation, subscribed_at_ms, \
              grant_lifecycle_id, grant_authority_incarnation, grant_generation) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (service_jid, node_name, subscriber_jid) DO UPDATE SET \
             strength = excluded.strength, trust_profile = excluded.trust_profile, \
             snapshot_generation = excluded.snapshot_generation, \
             subscribed_at_ms = excluded.subscribed_at_ms, \
             grant_lifecycle_id = excluded.grant_lifecycle_id, \
             grant_authority_incarnation = excluded.grant_authority_incarnation, \
             grant_generation = excluded.grant_generation \
             WHERE excluded.grant_lifecycle_id IS NOT NULL OR s2s_space_subscriptions.grant_lifecycle_id IS NULL",
                crate::db_params![
                    service.as_str(),
                    node,
                    account.as_str(),
                    account.as_str(),
                    domain,
                    node_created_at_ms,
                    strength_code(evidence.strength()),
                    evidence.trust_profile().map(trust_profile_code),
                    snapshot.to_string(),
                    now_ms,
                    grant.map(|grant| grant.lifecycle.to_storage()),
                    grant.map(|grant| grant.incarnation.to_storage()),
                    grant.map(|grant| grant.generation.value())
                ],
            )
            .await
            .map_err(storage)?;
        if recorded != 1 {
            tx.rollback().await.map_err(storage)?;
            return Ok(Recording::only(SubscriptionRecord::NotFederated));
        }
        if let Some(grant) = grant {
            if !protected_subscription_still_allowed(
                &mut tx,
                service,
                node,
                node_created_at_ms,
                &request,
                grant,
            )
            .await?
            {
                tx.rollback().await.map_err(storage)?;
                return Ok(Recording::only(SubscriptionRecord::NotFederated));
            }
        }
        tx.commit().await.map_err(storage)?;
        // The account's own lapsed row was renewed, not removed.
        removed.retain(|gone| {
            !(gone.account == *account && gone.service == *service && gone.node == node)
        });
        Ok(Recording {
            outcome: SubscriptionRecord::Recorded,
            removed,
        })
    }

    /// Whether one more subscription, `offer`, fits every bound, counted
    /// inside `tx`, which holds the locks of [`begin_for_subscription`].
    /// Lapsed rows count against none. The rows a sweep removes are added
    /// to `removed`.
    async fn within_bounds(
        &self,
        tx: &mut Transaction<'_>,
        offer: Offer<'_>,
        admitted: &(dyn Fn(&str) -> bool + Send + Sync),
        removed: &mut Vec<RemovedSubscription>,
    ) -> Result<bool, SpaceStoreError> {
        let Offer {
            service,
            node,
            account,
            now_ms,
        } = offer;
        let domain = account.domain().as_str();
        let lapsed_before = now_ms.saturating_sub(SUBSCRIPTION_LIFETIME_MS);
        // The Space's own rows hold no lapsed one any more (see
        // `record`); those of domains that are not admitted are removed
        // only when they would keep an admitted account out.
        let space = "WHERE service_jid = ? AND node_name = ?";
        let space_params = || crate::db_params![service.as_str(), node];
        if count_in(tx, space, space_params(), MAX_REMOTE_SUBSCRIBERS_PER_SPACE).await?
            >= MAX_REMOTE_SUBSCRIBERS_PER_SPACE
        {
            sweep_unadmitted(
                tx,
                space,
                space_params(),
                MAX_REMOTE_SUBSCRIBERS_PER_SPACE,
                admitted,
                removed,
            )
            .await?;
            if count_in(tx, space, space_params(), MAX_REMOTE_SUBSCRIBERS_PER_SPACE).await?
                >= MAX_REMOTE_SUBSCRIBERS_PER_SPACE
            {
                return Ok(false);
            }
        }
        let bounds: [(&str, Vec<super::Value>, usize); 3] = [
            (
                "WHERE service_jid = ? AND node_name = ? AND subscriber_domain = ?",
                crate::db_params![service.as_str(), node, domain],
                MAX_REMOTE_SUBSCRIBERS_PER_DOMAIN_AND_SPACE,
            ),
            (
                "WHERE account_jid = ? AND subscribed_at_ms >= ?",
                crate::db_params![account.as_str(), lapsed_before],
                MAX_SUBSCRIPTIONS_PER_REMOTE_ACCOUNT,
            ),
            (
                "WHERE subscriber_domain = ? AND subscribed_at_ms >= ?",
                crate::db_params![domain, lapsed_before],
                MAX_SUBSCRIPTIONS_PER_REMOTE_DOMAIN,
            ),
        ];
        for (filter, params, bound) in bounds {
            if count_in(tx, filter, params, bound).await? >= bound {
                return Ok(false);
            }
        }
        // The deployment's bound is counted under no lock of its own, so
        // concurrent subscriptions of different domains to different Spaces
        // may pass it together: it is approximate where the others are
        // exact.
        let live = "WHERE subscribed_at_ms >= ?";
        if count_in(
            tx,
            live,
            crate::db_params![lapsed_before],
            MAX_REMOTE_SUBSCRIPTIONS,
        )
        .await?
            >= MAX_REMOTE_SUBSCRIPTIONS
        {
            sweep_store(tx, self.db.driver(), lapsed_before, admitted, removed).await?;
            if count_in(
                tx,
                live,
                crate::db_params![lapsed_before],
                MAX_REMOTE_SUBSCRIPTIONS,
            )
            .await?
                >= MAX_REMOTE_SUBSCRIPTIONS
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Forget the subscription of `account` to `node` of `service`, under
    /// the Space's lock: a subscription being recorded for the account at
    /// the same time is committed first and removed here, or recorded
    /// after this ran, and then its caller reads what was written before
    /// (a ban among it, see [`Self::record`]).
    pub async fn remove(
        &self,
        service: &BareJid,
        node: &str,
        account: &BareJid,
    ) -> Result<(), SpaceStoreError> {
        let mut tx = self.db.begin_immediate().await.map_err(storage)?;
        lock_space(&self.db, &mut tx, service, node).await?;
        tx.execute(
            "DELETE FROM s2s_space_subscriptions \
             WHERE service_jid = ? AND node_name = ? AND subscriber_jid = ? \
             AND grant_lifecycle_id IS NULL",
            crate::db_params![service.as_str(), node, account.as_str()],
        )
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    /// Remove only the protected preference recorded under this grant.
    pub async fn remove_protected(
        &self,
        service: &BareJid,
        node: &str,
        account: &BareJid,
        grant: SpaceSubscriptionGrant,
    ) -> Result<bool, SpaceStoreError> {
        let Some(mut tx) = begin_for_subscription(
            &self.db,
            account.domain().as_str(),
            service,
            node,
            Some(grant),
        )
        .await?
        else {
            return Ok(false);
        };
        let removed = tx.execute(
            "DELETE FROM s2s_space_subscriptions \
             WHERE service_jid = ? AND node_name = ? AND subscriber_jid = ? \
             AND grant_lifecycle_id = ? AND grant_authority_incarnation = ? AND grant_generation = ?",
            crate::db_params![
                service.as_str(), node, account.as_str(), grant.lifecycle.to_storage(),
                grant.incarnation.to_storage(), grant.generation.value()
            ],
        ).await.map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(removed == 1)
    }

    /// The subscriptions remote accounts made to the node `node` of
    /// `service` created at `node_created_at_ms` that have not lapsed at
    /// `now_ms`, at most [`MAX_REMOTE_SUBSCRIBERS_PER_SPACE`] of them, in the
    /// order of their accounts. No Space holds more rows than that bound,
    /// those of domains no longer admitted included, so none is left out.
    pub async fn of_node(
        &self,
        service: &BareJid,
        node: &str,
        node_created_at_ms: i64,
        now_ms: i64,
    ) -> Result<Vec<SpaceSubscription>, SpaceStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT subscriber_jid, strength, trust_profile, snapshot_generation, \
                 grant_lifecycle_id, grant_authority_incarnation, grant_generation \
                 FROM s2s_space_subscriptions \
                 WHERE service_jid = ? AND node_name = ? AND node_created_at_ms = ? \
                 AND subscribed_at_ms >= ? \
                 ORDER BY subscriber_jid ASC LIMIT ?",
                crate::db_params![
                    service.as_str(),
                    node,
                    node_created_at_ms,
                    now_ms.saturating_sub(SUBSCRIPTION_LIFETIME_MS),
                    limit(MAX_REMOTE_SUBSCRIBERS_PER_SPACE)
                ],
            )
            .await
            .map_err(storage)?;
        let mut subscriptions = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            subscriptions.push(decode(&row)?);
        }
        Ok(subscriptions)
    }
}

/// Held while one Space is deleted ([`lock_space_deletion`]); released by
/// [`SpaceDeletionLock::release`].
pub struct SpaceDeletionLock<'a>(DeletionHold<'a>);

enum DeletionHold<'a> {
    /// On PostgreSQL: a transaction holding the deletion's advisory lock,
    /// whose connection is discarded rather than reused if it is dropped
    /// before it ends, so the lock never outlives its holder.
    Postgres(Transaction<'a>),
    /// On SQLite, which one process serves: the process's lock of the
    /// Space's deletion, released when dropped.
    Process {
        _held: tokio::sync::OwnedMutexGuard<()>,
    },
}

impl SpaceDeletionLock<'_> {
    /// Let the next deletion of the Space run.
    pub async fn release(self) {
        if let DeletionHold::Postgres(tx) = self.0 {
            if let Err(error) = tx.rollback().await {
                tracing::warn!(%error, "releasing a Space deletion lock failed");
            }
        }
    }
}

/// The process's lock of the deletion of the Space `key` names, shared by
/// every deletion of that Space running in this process while one is.
fn process_lock(key: String) -> Result<Arc<tokio::sync::Mutex<()>>, SpaceStoreError> {
    type Locks = Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>;
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Locks::default)
        .lock()
        .map_err(|_| storage("the Space deletion locks are unusable"))?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

/// Wait until no other deletion of the Space `node` of `service` runs, on
/// any replica, and hold that until the answer is released: a deletion
/// that fails and puts back what it removed never does so beside another
/// that completed. On PostgreSQL this is a transaction-scoped advisory lock
/// of its own, not the Space's lock under which subscriptions are counted,
/// which the deletion takes in turn when it closes the Space to other
/// servers; on SQLite, a lock in this process.
pub async fn lock_space_deletion<'a>(
    db: &'a Database,
    service: &BareJid,
    node: &str,
) -> Result<SpaceDeletionLock<'a>, SpaceStoreError> {
    let key = ["waddle.spaces.deletion:", service.as_str(), "/", node].concat();
    if db.driver() == DatabaseDriver::Postgres {
        let mut tx = db.begin_discard_postgres_on_drop().await.map_err(storage)?;
        advisory_lock(db, &mut tx, key).await?;
        return Ok(SpaceDeletionLock(DeletionHold::Postgres(tx)));
    }
    let lock = process_lock(key)?;
    Ok(SpaceDeletionLock(DeletionHold::Process {
        _held: lock.lock_owned().await,
    }))
}
