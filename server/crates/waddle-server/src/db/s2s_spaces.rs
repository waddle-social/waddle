//! Stores of the Spaces accounts at other servers use over S2S (ADR-0020
//! D10; XEP-0503 over XEP-0060; migrations 2012 and 2013):
//!
//! - which Spaces are open to other servers ([`FederatedSpaces`]): a row
//!   for each Space whose `federated` setting is on, bound to the PubSub
//!   node it was turned on for;
//! - the subscriptions remote accounts made to those Spaces over inbound
//!   streams ([`SpaceSubscriptions`]), each with what its stream proved,
//!   beside the PubSub service's own subscription record. An event
//!   notification leaves for a JID at another server only while such a row
//!   exists.
//!
//! Both bind a row to the creation time of its node, so a row left by a
//! Space that was deleted counts for nothing once a Space of the same name
//! is created again.
//!
//! A subscription is counted against its bounds and recorded in one
//! transaction: an immediate transaction on SQLite, and on PostgreSQL under
//! a transaction-scoped advisory lock keyed on the subscriber's domain, so
//! that the subscriptions of one remote domain are counted one at a time on
//! every replica. The decisions are recorded in
//! `server/docs/planning/s2s-routing-2047.md`.

use jid::{BareJid, Jid};
use thiserror::Error;
use uuid::Uuid;

use super::{Database, DatabaseDriver, Row, Transaction};
use crate::permissions::host_grant::StreamEvidence;
use crate::s2s::conn::AuthenticationStrength;
use crate::s2s::verify::TrustProfileKind;

/// Subscriptions accounts at other servers may hold to one local Space.
pub const MAX_REMOTE_SUBSCRIBERS_PER_SPACE: usize = 256;

/// Subscriptions one remote account may hold to local Spaces, under every
/// address it subscribed.
pub const MAX_SUBSCRIPTIONS_PER_REMOTE_ACCOUNT: usize = 64;

/// Subscriptions the accounts of one remote domain may hold to local
/// Spaces.
pub const MAX_SUBSCRIPTIONS_PER_REMOTE_DOMAIN: usize = 4_096;

/// Subscriptions accounts at other servers may hold to the Spaces of the
/// deployment.
pub const MAX_REMOTE_SUBSCRIPTIONS: usize = 65_536;

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
    /// node of that name it was on for.
    pub async fn disable(&self, service: &BareJid, node: &str) -> Result<(), SpaceStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        guard
            .execute(
                "DELETE FROM s2s_federated_spaces WHERE service_jid = ? AND node_name = ?",
                crate::db_params![service.as_str(), node],
            )
            .await
            .map_err(storage)?;
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
    /// The address that subscribed: the account's bare JID or one of its
    /// full JIDs. Notifications are addressed to it.
    pub subscriber: Jid,
    /// What the stream that carried the subscription request proved.
    pub evidence: StreamEvidence,
    /// The S2S snapshot that stream was admitted under.
    pub snapshot: Uuid,
}

/// A subscription request as the store records it.
#[derive(Debug, Clone, Copy)]
pub struct SubscriptionRequest<'a> {
    /// The address that subscribes: an address of `account`.
    pub subscriber: &'a Jid,
    /// The remote account the request is from.
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
    /// Not recorded: a bound on the Space's remote subscribers, on the
    /// account's or its domain's subscriptions or on the deployment's would
    /// be exceeded, or an address does not fit the store.
    Bounds,
}

/// The subscriptions of remote accounts to local Spaces
/// (`s2s_space_subscriptions`).
#[derive(Clone)]
pub struct SpaceSubscriptions {
    db: Database,
}

/// Begin a transaction that holds the lock under which the subscriptions
/// of `domain` are counted and written: SQLite's write lock, taken at once,
/// or on PostgreSQL a transaction-scoped advisory lock keyed on the domain.
async fn begin_for_domain<'a>(
    db: &'a Database,
    domain: &str,
) -> Result<Transaction<'a>, SpaceStoreError> {
    let mut tx = db.begin_immediate().await.map_err(storage)?;
    if db.driver() == DatabaseDriver::Postgres {
        let key = ["waddle.s2s.space_subscriptions:", domain].concat();
        tx.query(
            "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended(?, 0))",
            crate::db_params![key],
        )
        .await
        .map_err(storage)?;
    }
    Ok(tx)
}

/// Count, up to the `LIMIT` it carries, the rows `sql` selects inside `tx`.
async fn count_in(
    tx: &mut Transaction<'_>,
    sql: &str,
    params: Vec<super::Value>,
) -> Result<usize, SpaceStoreError> {
    let mut rows = tx.query(sql, params).await.map_err(storage)?;
    let mut count = 0;
    while rows.next().await.map_err(storage)?.is_some() {
        count += 1;
    }
    Ok(count)
}

/// A row read back as the subscription it records. A row whose evidence
/// cannot be read is an error, never evidence the row does not hold.
fn decode(row: &Row) -> Result<SpaceSubscription, SpaceStoreError> {
    let subscriber: String = row.get(0).map_err(storage)?;
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
        subscriber: subscriber.parse().map_err(storage)?,
        evidence: StreamEvidence::recorded(strength, trust_profile),
        snapshot: snapshot.parse().map_err(storage)?,
    })
}

impl SpaceSubscriptions {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Record that `request.subscriber` subscribed to the node `node` of
    /// `service` created at `node_created_at_ms`, within the bounds. A
    /// subscription the address already holds to that node is recorded
    /// again with this request's evidence and counts against no bound;
    /// rows left for an earlier node of the name are removed first.
    pub async fn record(
        &self,
        service: &BareJid,
        node: &str,
        node_created_at_ms: i64,
        request: SubscriptionRequest<'_>,
    ) -> Result<SubscriptionRecord, SpaceStoreError> {
        let SubscriptionRequest {
            subscriber,
            account,
            evidence,
            snapshot,
            now_ms,
        } = request;
        if node.is_empty()
            || node.len() > MAX_NODE_BYTES
            || service.as_str().len() > MAX_SERVICE_BYTES
            || subscriber.as_str().len() > MAX_ADDRESS_BYTES
        {
            return Ok(SubscriptionRecord::Bounds);
        }
        let domain = account.domain().as_str();
        let mut tx = begin_for_domain(&self.db, domain).await?;
        tx.execute(
            "DELETE FROM s2s_space_subscriptions \
             WHERE service_jid = ? AND node_name = ? AND node_created_at_ms <> ?",
            crate::db_params![service.as_str(), node, node_created_at_ms],
        )
        .await
        .map_err(storage)?;
        let held = count_in(
            &mut tx,
            "SELECT 1 FROM s2s_space_subscriptions \
             WHERE service_jid = ? AND node_name = ? AND subscriber_jid = ? LIMIT 1",
            crate::db_params![service.as_str(), node, subscriber.as_str()],
        )
        .await?
            > 0;
        if !held {
            let bounds: [(&str, Vec<super::Value>, usize); 4] = [
                (
                    "SELECT 1 FROM s2s_space_subscriptions \
                     WHERE service_jid = ? AND node_name = ? LIMIT ?",
                    crate::db_params![
                        service.as_str(),
                        node,
                        limit(MAX_REMOTE_SUBSCRIBERS_PER_SPACE)
                    ],
                    MAX_REMOTE_SUBSCRIBERS_PER_SPACE,
                ),
                (
                    "SELECT 1 FROM s2s_space_subscriptions WHERE account_jid = ? LIMIT ?",
                    crate::db_params![
                        account.as_str(),
                        limit(MAX_SUBSCRIPTIONS_PER_REMOTE_ACCOUNT)
                    ],
                    MAX_SUBSCRIPTIONS_PER_REMOTE_ACCOUNT,
                ),
                (
                    "SELECT 1 FROM s2s_space_subscriptions WHERE subscriber_domain = ? LIMIT ?",
                    crate::db_params![domain, limit(MAX_SUBSCRIPTIONS_PER_REMOTE_DOMAIN)],
                    MAX_SUBSCRIPTIONS_PER_REMOTE_DOMAIN,
                ),
                (
                    "SELECT 1 FROM s2s_space_subscriptions LIMIT ?",
                    crate::db_params![limit(MAX_REMOTE_SUBSCRIPTIONS)],
                    MAX_REMOTE_SUBSCRIPTIONS,
                ),
            ];
            for (sql, params, bound) in bounds {
                if count_in(&mut tx, sql, params).await? >= bound {
                    tx.rollback().await.map_err(storage)?;
                    return Ok(SubscriptionRecord::Bounds);
                }
            }
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
                subscriber.as_str(),
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

    /// Forget the subscription of `subscriber` to `node` of `service`.
    pub async fn remove(
        &self,
        service: &BareJid,
        node: &str,
        subscriber: &Jid,
    ) -> Result<(), SpaceStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        guard
            .execute(
                "DELETE FROM s2s_space_subscriptions \
                 WHERE service_jid = ? AND node_name = ? AND subscriber_jid = ?",
                crate::db_params![service.as_str(), node, subscriber.as_str()],
            )
            .await
            .map_err(storage)?;
        Ok(())
    }

    /// Forget every subscription to `node` of `service`: the Space was
    /// deleted or is no longer open to other servers.
    pub async fn forget_node(&self, service: &BareJid, node: &str) -> Result<(), SpaceStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        guard
            .execute(
                "DELETE FROM s2s_space_subscriptions WHERE service_jid = ? AND node_name = ?",
                crate::db_params![service.as_str(), node],
            )
            .await
            .map_err(storage)?;
        Ok(())
    }

    /// The subscriptions remote accounts made to the node `node` of
    /// `service` created at `node_created_at_ms`, at most
    /// [`MAX_REMOTE_SUBSCRIBERS_PER_SPACE`] of them, in the order of their
    /// addresses.
    pub async fn of_node(
        &self,
        service: &BareJid,
        node: &str,
        node_created_at_ms: i64,
    ) -> Result<Vec<SpaceSubscription>, SpaceStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT subscriber_jid, strength, trust_profile, snapshot_generation \
                 FROM s2s_space_subscriptions \
                 WHERE service_jid = ? AND node_name = ? AND node_created_at_ms = ? \
                 ORDER BY subscriber_jid ASC LIMIT ?",
                crate::db_params![
                    service.as_str(),
                    node,
                    node_created_at_ms,
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
