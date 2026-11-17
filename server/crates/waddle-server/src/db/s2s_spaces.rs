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
//! every replica. Turning a Space's `federated` setting off takes the
//! Space's lock too, and a subscription re-reads the setting under it, so
//! no subscription is recorded for a Space after its setting went off.
//!
//! A subscription lapses [`SUBSCRIPTION_LIFETIME_MS`] after it was last
//! made: a lapsed row counts against no bound and names no subscriber, and
//! subscribing again renews it. The decisions are recorded in
//! `server/docs/planning/s2s-routing-2047.md`.

use jid::BareJid;
use thiserror::Error;
use uuid::Uuid;

use super::{Database, DatabaseDriver, Row, Transaction};
use crate::permissions::host_grant::StreamEvidence;
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
const MAX_SWEPT_DOMAINS: usize = 4096;

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

/// The subscriptions of remote accounts to local Spaces
/// (`s2s_space_subscriptions`).
#[derive(Clone)]
pub struct SpaceSubscriptions {
    db: Database,
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
) -> Result<Transaction<'a>, SpaceStoreError> {
    let mut tx = db.begin_immediate().await.map_err(storage)?;
    let key = ["waddle.s2s.space_subscriptions.domain:", domain].concat();
    advisory_lock(db, &mut tx, key).await?;
    lock_space(db, &mut tx, service, node).await?;
    Ok(tx)
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

/// Remove, inside `tx`, the rows matching `filter` whose remote domain
/// `admitted` no longer admits, reading at most `most` domains. A domain
/// that is not admitted receives nothing (the notification path decides
/// admission again for each subscriber), so its rows only hold places.
async fn sweep_unadmitted(
    tx: &mut Transaction<'_>,
    filter: &str,
    params: Vec<super::Value>,
    most: usize,
    admitted: &(dyn Fn(&str) -> bool + Send + Sync),
) -> Result<(), SpaceStoreError> {
    let domains = domains_in(tx, filter, params.clone(), most).await?;
    for domain in domains.iter().filter(|domain| !admitted(domain)) {
        let joiner = if filter.is_empty() { "WHERE " } else { " AND " };
        let sql = [
            "DELETE FROM s2s_space_subscriptions ",
            filter,
            joiner,
            "subscriber_domain = ?",
        ]
        .concat();
        let mut params = params.clone();
        params.push(domain.as_str().into());
        tx.execute(&sql, params).await.map_err(storage)?;
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
    Ok(SpaceSubscription {
        account: account.parse().map_err(storage)?,
        evidence: StreamEvidence::recorded(strength, trust_profile),
        snapshot: snapshot.parse().map_err(storage)?,
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
    /// Only the rows of domains `admitted` admits now hold a place against
    /// the bound on the Space's remote subscribers and against the
    /// deployment's: when either is reached, the rows of the other domains
    /// are removed (for the deployment's, of at most [`MAX_SWEPT_DOMAINS`]
    /// domains, and lapsed rows with them) and the bound is counted again.
    pub async fn record(
        &self,
        service: &BareJid,
        node: &str,
        node_created_at_ms: i64,
        request: SubscriptionRequest<'_>,
        admitted: &(dyn Fn(&str) -> bool + Send + Sync),
    ) -> Result<SubscriptionRecord, SpaceStoreError> {
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
            return Ok(SubscriptionRecord::Bounds);
        }
        let domain = account.domain().as_str();
        let lapsed_before = now_ms.saturating_sub(SUBSCRIPTION_LIFETIME_MS);
        let mut tx = begin_for_subscription(&self.db, domain, service, node).await?;
        // The setting, read again under the Space's lock: turning it off
        // takes that lock, so a subscription is recorded only for a Space
        // whose setting is still on for this very node.
        let federated = {
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
            return Ok(SubscriptionRecord::NotFederated);
        }
        tx.execute(
            "DELETE FROM s2s_space_subscriptions \
             WHERE service_jid = ? AND node_name = ? \
             AND (node_created_at_ms <> ? OR subscribed_at_ms < ?)",
            crate::db_params![service.as_str(), node, node_created_at_ms, lapsed_before],
        )
        .await
        .map_err(storage)?;
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
                .within_bounds(&mut tx, service, node, account, now_ms, admitted)
                .await?
        {
            tx.rollback().await.map_err(storage)?;
            return Ok(SubscriptionRecord::Bounds);
        }
        tx.execute(
            "INSERT INTO s2s_space_subscriptions \
             (service_jid, node_name, subscriber_jid, account_jid, subscriber_domain, \
              node_created_at_ms, strength, trust_profile, snapshot_generation, subscribed_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (service_jid, node_name, subscriber_jid) DO UPDATE SET \
             strength = excluded.strength, trust_profile = excluded.trust_profile, \
             snapshot_generation = excluded.snapshot_generation, \
             subscribed_at_ms = excluded.subscribed_at_ms",
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
                now_ms
            ],
        )
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(SubscriptionRecord::Recorded)
    }

    /// Whether one more subscription of `account` to the Space `node` of
    /// `service` fits every bound, counted inside `tx`, which holds the
    /// locks of [`begin_for_subscription`]. Lapsed rows count against none.
    async fn within_bounds(
        &self,
        tx: &mut Transaction<'_>,
        service: &BareJid,
        node: &str,
        account: &BareJid,
        now_ms: i64,
        admitted: &(dyn Fn(&str) -> bool + Send + Sync),
    ) -> Result<bool, SpaceStoreError> {
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
            tx.execute(
                "DELETE FROM s2s_space_subscriptions WHERE subscribed_at_ms < ?",
                crate::db_params![lapsed_before],
            )
            .await
            .map_err(storage)?;
            sweep_unadmitted(tx, "", Vec::new(), MAX_SWEPT_DOMAINS, admitted).await?;
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

    /// Forget the subscription of `account` to `node` of `service`.
    pub async fn remove(
        &self,
        service: &BareJid,
        node: &str,
        account: &BareJid,
    ) -> Result<(), SpaceStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        guard
            .execute(
                "DELETE FROM s2s_space_subscriptions \
                 WHERE service_jid = ? AND node_name = ? AND subscriber_jid = ?",
                crate::db_params![service.as_str(), node, account.as_str()],
            )
            .await
            .map_err(storage)?;
        Ok(())
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
                "SELECT subscriber_jid, strength, trust_profile, snapshot_generation \
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
