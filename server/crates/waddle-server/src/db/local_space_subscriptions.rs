//! Local account preferences for enrolled Spaces. They carry no peer evidence.

use std::time::Duration;

use thiserror::Error;
use waddle_xmpp::pubsub::SubId;

use super::s2s_spaces::SpaceSubscriptionGrant;
use super::{Database, DatabaseDriver, DatabaseError, Transaction};
use crate::ingress_substrate::set_local_transaction_timeouts;
use crate::permissions::host_grant::{
    guard_queue_key, AuthorityIncarnation, GrantGeneration, GrantSubject, ScopeKey,
    ScopeLifecycleId, ServedAccountDomains, SpaceBookmarkVersion, SubjectHome,
    HOST_GRANT_POLICY_VERSION, HOST_GRANT_SCOPE_MAX_GRANT_ROWS, SPACE_BOOKMARK_RESTORE_LOCK,
};

const LOCK_TIMEOUT: Duration = Duration::from_millis(500);
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum LocalSpaceSubscriptionError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("the local Space preference store could not prove its transaction bounds")]
    BoundsUnproven,
    #[error("the local Space preference store contains an invalid row")]
    InvalidRow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSpaceSubscription {
    pub account: GrantSubject,
    pub grant: SpaceSubscriptionGrant,
    pub subid: SubId,
}

/// The authenticated account's unsubscribe outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalSpaceUnsubscribe {
    Removed,
    Absent,
    Invalid,
    Required,
}

#[derive(Clone)]
pub struct LocalSpaceSubscriptions {
    db: Database,
}

impl std::fmt::Debug for LocalSpaceSubscriptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalSpaceSubscriptions")
            .finish_non_exhaustive()
    }
}

// Always address a scope by its immutable binding and an authorized subject.
// The current affiliation era and the already effective role are separate.
const CURRENT_GRANT: &str = "FROM host_grant_space_bindings b
    JOIN host_grant_scope_lifecycles l ON l.lifecycle_id = b.lifecycle_id
    JOIN host_grant_scopes s ON s.lifecycle_id = b.lifecycle_id
    JOIN host_grant_authority a ON a.singleton AND a.incarnation = s.authority_incarnation
    JOIN pubsub_nodes p ON p.owner_jid = b.service_jid AND p.node_name = b.space_node
    JOIN host_grant_space_affiliations g ON g.lifecycle_id = b.lifecycle_id";

const ACTIVE_SPACE: &str = "b.lifecycle_id = ? AND b.service_jid = ? AND b.space_node = ?
    AND l.retired_at_ms IS NULL AND b.authority_incarnation = a.incarnation
    AND s.admission_state = 'active' AND s.policy_version = ?
    AND p.access_model = b.access_model AND (p.node_type IS NULL OR p.node_type = ?)
    AND g.affiliation <> 'outcast'
    AND (g.effective_affiliation = 'member'
        OR (g.effective_affiliation IN ('owner', 'publisher') AND g.issuance_basis = 'local'))";

const LIVE_PREFERENCE: &str =
    "JOIN local_space_subscriptions sub ON sub.lifecycle_id = g.lifecycle_id
    AND sub.subject_jid = g.subject_jid AND sub.authority_incarnation = b.authority_incarnation
    AND sub.grant_generation = g.changed_generation";

impl LocalSpaceSubscriptions {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Record the authenticated local account's preference for this exact era.
    /// Renewing an existing preference in the same era keeps its subscription ID.
    pub async fn record(
        &self,
        key: &ScopeKey,
        account: &GrantSubject,
        served: &ServedAccountDomains,
        grant: SpaceSubscriptionGrant,
    ) -> Result<Option<LocalSpaceSubscription>, LocalSpaceSubscriptionError> {
        if served.home_of(account.jid()) != SubjectHome::Served {
            return Ok(None);
        }
        let Some(mut tx) = self.begin().await? else {
            return Ok(None);
        };
        if !hold_scope(&mut tx, grant.lifecycle).await? {
            return Ok(None);
        }
        let lifecycle = grant.lifecycle.to_storage();
        let incarnation = grant.incarnation.to_storage();
        let mut rows = tx
            .query(
                &format!(
                    "SELECT 1 {CURRENT_GRANT} WHERE {ACTIVE_SPACE}
                     AND b.authority_incarnation = ? AND g.subject_jid = ?
                     AND g.changed_generation = ? FOR SHARE OF p"
                ),
                crate::db_params![
                    lifecycle.as_str(),
                    key.service().as_str(),
                    key.node(),
                    HOST_GRANT_POLICY_VERSION,
                    waddle_xmpp::xep::xep0503::NS_SPACES,
                    incarnation.as_str(),
                    account.as_str(),
                    grant.generation.value(),
                ],
            )
            .await?;
        let allowed = rows.next().await?.is_some();
        drop(rows);
        if !allowed {
            return Ok(None);
        }
        let mut saved = tx
            .query(
                "INSERT INTO local_space_subscriptions
                   (lifecycle_id, subject_jid, authority_incarnation, grant_generation, subid)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT (lifecycle_id, subject_jid) DO UPDATE SET
                    authority_incarnation = EXCLUDED.authority_incarnation,
                    grant_generation = EXCLUDED.grant_generation,
                    subid = CASE WHEN local_space_subscriptions.authority_incarnation = EXCLUDED.authority_incarnation
                               AND local_space_subscriptions.grant_generation = EXCLUDED.grant_generation
                            THEN local_space_subscriptions.subid ELSE EXCLUDED.subid END
                 RETURNING subid",
                crate::db_params![
                    lifecycle, account.as_str(), incarnation, grant.generation.value(),
                    SubId::generate().as_str(),
                ],
            )
            .await?;
        let subid = saved
            .next()
            .await?
            .ok_or(LocalSpaceSubscriptionError::InvalidRow)?
            .get::<String>(0)?;
        drop(saved);
        tx.commit().await?;
        Ok(Some(LocalSpaceSubscription {
            account: account.clone(),
            grant,
            subid: SubId::from_raw(subid),
        }))
    }

    /// Remove only the authenticated local account's preference.
    /// A revoked account may still remove its own stale preference.
    pub async fn remove_own(
        &self,
        key: &ScopeKey,
        account: &GrantSubject,
        served: &ServedAccountDomains,
        subid: Option<&SubId>,
    ) -> Result<LocalSpaceUnsubscribe, LocalSpaceSubscriptionError> {
        if served.home_of(account.jid()) != SubjectHome::Served {
            return Ok(LocalSpaceUnsubscribe::Absent);
        }
        let Some(mut tx) = self.begin().await? else {
            return Ok(LocalSpaceUnsubscribe::Absent);
        };
        let mut rows = tx
            .query(
                "WITH target AS (
                    SELECT sub.lifecycle_id, sub.subject_jid, sub.subid
                    FROM local_space_subscriptions sub
                    JOIN host_grant_space_bindings b ON sub.lifecycle_id = b.lifecycle_id
                    WHERE b.service_jid = ? AND b.space_node = ? AND sub.subject_jid = ?
                    FOR UPDATE OF sub
                 ), removed AS (
                    DELETE FROM local_space_subscriptions sub USING target
                    WHERE sub.lifecycle_id = target.lifecycle_id AND sub.subject_jid = target.subject_jid
                    AND target.subid = ? RETURNING 1
                 )
                 SELECT CASE WHEN EXISTS (SELECT 1 FROM removed) THEN 'removed'
                             WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'absent'
                             WHEN ?::text IS NULL THEN 'required'
                             ELSE 'invalid' END",
                crate::db_params![
                    key.service().as_str(), key.node(), account.as_str(),
                    subid.map(SubId::as_str), subid.map(SubId::as_str),
                ],
            )
            .await?;
        let outcome = rows
            .next()
            .await?
            .ok_or(LocalSpaceSubscriptionError::InvalidRow)?
            .get::<String>(0)?;
        drop(rows);
        let outcome = match outcome.as_str() {
            "removed" => LocalSpaceUnsubscribe::Removed,
            "absent" => LocalSpaceUnsubscribe::Absent,
            "required" => LocalSpaceUnsubscribe::Required,
            "invalid" => LocalSpaceUnsubscribe::Invalid,
            _ => return Err(LocalSpaceSubscriptionError::InvalidRow),
        };
        tx.commit().await?;
        Ok(outcome)
    }

    /// List live local preferences in a single scope, within its grant-row
    /// bound. The delivery path must still acquire the exact preference.
    pub async fn of_scope(
        &self,
        key: &ScopeKey,
        lifecycle: ScopeLifecycleId,
    ) -> Result<Vec<LocalSpaceSubscription>, LocalSpaceSubscriptionError> {
        let Some(mut tx) = self.begin().await? else {
            return Ok(Vec::new());
        };
        let mut rows = tx
            .query(
                &format!(
                    "SELECT g.subject_jid, b.authority_incarnation, g.changed_generation, sub.subid
                       {CURRENT_GRANT} {LIVE_PREFERENCE} WHERE {ACTIVE_SPACE}
                     ORDER BY g.subject_jid LIMIT ?"
                ),
                crate::db_params![
                    lifecycle.to_storage(),
                    key.service().as_str(),
                    key.node(),
                    HOST_GRANT_POLICY_VERSION,
                    waddle_xmpp::xep::xep0503::NS_SPACES,
                    i64::try_from(HOST_GRANT_SCOPE_MAX_GRANT_ROWS)
                        .expect("bounded local preferences"),
                ],
            )
            .await?;
        let mut subscriptions = Vec::new();
        while let Some(row) = rows.next().await? {
            subscriptions.push(LocalSpaceSubscription {
                account: GrantSubject::from_storage(&row.get::<String>(0)?)
                    .ok_or(LocalSpaceSubscriptionError::InvalidRow)?,
                grant: SpaceSubscriptionGrant {
                    lifecycle,
                    incarnation: AuthorityIncarnation::from_storage(&row.get::<String>(1)?)
                        .ok_or(LocalSpaceSubscriptionError::InvalidRow)?,
                    generation: GrantGeneration::new(row.get::<i64>(2)?)
                        .ok_or(LocalSpaceSubscriptionError::InvalidRow)?,
                },
                subid: SubId::from_raw(row.get::<String>(3)?),
            });
        }
        drop(rows);
        tx.rollback().await?;
        Ok(subscriptions)
    }

    /// Keep the preference current while staging its subscribe reply.
    pub async fn hold_preference(
        &self,
        key: &ScopeKey,
        subscription: &LocalSpaceSubscription,
    ) -> Result<Option<Transaction<'_>>, LocalSpaceSubscriptionError> {
        self.hold_exact(key, subscription, None).await
    }

    /// Keep both the preference and bookmark version current while staging
    /// the bounded delivery. Release the transaction before network I/O.
    pub async fn hold_delivery(
        &self,
        key: &ScopeKey,
        subscription: &LocalSpaceSubscription,
        version: &SpaceBookmarkVersion,
    ) -> Result<Option<Transaction<'_>>, LocalSpaceSubscriptionError> {
        if version.lifecycle != subscription.grant.lifecycle
            || version.incarnation != subscription.grant.incarnation
        {
            return Ok(None);
        }
        self.hold_exact(key, subscription, Some(version)).await
    }

    async fn hold_exact(
        &self,
        key: &ScopeKey,
        subscription: &LocalSpaceSubscription,
        version: Option<&SpaceBookmarkVersion>,
    ) -> Result<Option<Transaction<'_>>, LocalSpaceSubscriptionError> {
        let Some(mut tx) = self.begin().await? else {
            return Ok(None);
        };
        let grant = subscription.grant;
        if !hold_scope(&mut tx, grant.lifecycle).await? {
            return Ok(None);
        }
        if let Some(version) = version {
            if !super::s2s_spaces::hold_bookmark_delivery_version(
                &mut tx,
                key.service(),
                key.node(),
                version,
            )
            .await?
            {
                return Ok(None);
            }
        }
        let mut rows = tx
            .query(
                &format!(
                    "SELECT 1 {CURRENT_GRANT} {LIVE_PREFERENCE} WHERE {ACTIVE_SPACE}
                 AND b.authority_incarnation = ? AND g.subject_jid = ?
                 AND sub.grant_generation = ? AND sub.subid = ? FOR SHARE OF p, sub"
                ),
                crate::db_params![
                    grant.lifecycle.to_storage(),
                    key.service().as_str(),
                    key.node(),
                    HOST_GRANT_POLICY_VERSION,
                    waddle_xmpp::xep::xep0503::NS_SPACES,
                    grant.incarnation.to_storage(),
                    subscription.account.as_str(),
                    grant.generation.value(),
                    subscription.subid.as_str(),
                ],
            )
            .await?;
        let present = rows.next().await?.is_some();
        drop(rows);
        if !present {
            return Ok(None);
        }
        Ok(Some(tx))
    }

    async fn begin(&self) -> Result<Option<Transaction<'_>>, LocalSpaceSubscriptionError> {
        if self.db.driver() != DatabaseDriver::Postgres {
            return Ok(None);
        }
        let mut tx = self.db.begin().await?;
        if !set_local_transaction_timeouts(&mut tx, LOCK_TIMEOUT, STATEMENT_TIMEOUT).await? {
            return Err(LocalSpaceSubscriptionError::BoundsUnproven);
        }
        tx.execute(
            "SELECT pg_catalog.pg_advisory_xact_lock_shared(?)",
            crate::db_params![SPACE_BOOKMARK_RESTORE_LOCK],
        )
        .await?;
        Ok(Some(tx))
    }
}

async fn hold_scope(
    tx: &mut Transaction<'_>,
    lifecycle: ScopeLifecycleId,
) -> Result<bool, DatabaseError> {
    tx.execute(
        "SELECT pg_catalog.pg_advisory_xact_lock_shared(?)",
        crate::db_params![guard_queue_key(lifecycle)],
    )
    .await?;
    let mut rows = tx
        .query(
            "SELECT 1 FROM host_grant_scopes WHERE lifecycle_id = ? FOR SHARE",
            crate::db_params![lifecycle.to_storage()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}
