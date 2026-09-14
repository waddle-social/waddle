//! Durable plugin grants, synchronized only by explicit configuration lifecycle work.

use std::collections::HashSet;

use chrono::Utc;
use jid::BareJid;
use uuid::Uuid;
use waddle_extensions::PluginId;
use waddle_xmpp::auth::{ExtensionGrantId, ExtensionGrantRef, ExtensionGrantScope};

use super::{IngressUowError, IngressUowTransaction};
use crate::db::{DatabaseDriver, Row};

/// Complete send capability and provider-room configuration for one plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredPluginGrants {
    pub plugin: PluginId,
    pub can_send: bool,
    pub provider_rooms: Vec<BareJid>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GrantSync {
    pub inserted: u64,
    pub revoked: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantAssertion {
    Asserted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrantAssertionFailure {
    #[error("extension grant does not exist")]
    Missing,
    #[error("extension grant was revoked")]
    Revoked,
    #[error("extension grant reference does not match durable authority")]
    Mismatch,
    #[error("extension requester no longer exists")]
    RequesterGone,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ExtensionGrantRepository;

impl ExtensionGrantRepository {
    /// Reconcile the complete configured plugin set at startup.
    pub async fn sync_configured(
        tx: &mut IngressUowTransaction<'_>,
        configured: &[ConfiguredPluginGrants],
    ) -> Result<GrantSync, IngressUowError> {
        let mut desired = HashSet::new();
        for config in configured.iter().filter(|config| config.can_send) {
            desired.insert((config.plugin.clone(), ExtensionGrantScope::Send));
            desired.extend(config.provider_rooms.iter().map(|room| {
                (
                    config.plugin.clone(),
                    ExtensionGrantScope::ProviderRoom(room.clone()),
                )
            }));
        }
        // Serialize complete-set reconciliation with other configuration and
        // revocation writers. Grant assertions remain compatible share readers;
        // individual revoked rows still wait for their admission share locks.
        if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            tx.transaction_mut()
                .execute(
                    "LOCK TABLE extension_grants IN SHARE ROW EXCLUSIVE MODE",
                    (),
                )
                .await?;
        }
        let mut rows = tx.transaction_mut().query(
            "SELECT grant_id, plugin_id, scope, room_jid FROM extension_grants WHERE revoked_at IS NULL ORDER BY grant_id",
            (),
        ).await?;
        let mut active = Vec::new();
        while let Some(row) = rows.next().await? {
            active.push(decode_grant(&row)?);
        }
        drop(rows);
        let mut result = GrantSync::default();
        for grant in active {
            if !desired.remove(&(grant.plugin.clone(), grant.scope.clone())) {
                result.revoked += Self::revoke_grant(tx, grant.grant_id).await?;
            }
        }
        for (plugin, scope) in desired {
            Self::insert_grant(tx, &plugin, &scope).await?;
            result.inserted += 1;
        }
        Ok(result)
    }

    pub async fn active_send_grant(
        tx: &mut IngressUowTransaction<'_>,
        plugin: &PluginId,
    ) -> Result<Option<ExtensionGrantRef>, IngressUowError> {
        let mut rows = tx.transaction_mut().query(
            "SELECT grant_id, plugin_id, scope, room_jid FROM extension_grants WHERE plugin_id = ? AND scope = 0 AND revoked_at IS NULL",
            crate::db_params![plugin.as_str()],
        ).await?;
        rows.next().await?.as_ref().map(decode_grant).transpose()
    }

    pub async fn active_room_grant(
        tx: &mut IngressUowTransaction<'_>,
        plugin: &PluginId,
        room: &BareJid,
    ) -> Result<Option<ExtensionGrantRef>, IngressUowError> {
        let mut rows = tx.transaction_mut().query(
            "SELECT grant_id, plugin_id, scope, room_jid FROM extension_grants WHERE plugin_id = ? AND scope = 1 AND room_jid = ? AND revoked_at IS NULL",
            crate::db_params![plugin.as_str(), room.to_string()],
        ).await?;
        rows.next().await?.as_ref().map(decode_grant).transpose()
    }

    /// Hold the exact durable grant against revocation until this UoW commits.
    pub async fn assert_grant(
        tx: &mut IngressUowTransaction<'_>,
        grant: &ExtensionGrantRef,
    ) -> Result<GrantAssertion, IngressUowError> {
        let sql = dialect_sql(tx,
            "SELECT grant_id, plugin_id, scope, room_jid, CASE WHEN revoked_at IS NULL THEN 0 ELSE 1 END FROM extension_grants WHERE grant_id = ? FOR SHARE",
            "SELECT grant_id, plugin_id, scope, room_jid, CASE WHEN revoked_at IS NULL THEN 0 ELSE 1 END FROM extension_grants WHERE grant_id = ?");
        let mut rows = tx
            .transaction_mut()
            .query(sql, crate::db_params![grant.grant_id.as_uuid().to_string()])
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(IngressUowError::ExtensionGrantAssertionFailed(
                GrantAssertionFailure::Missing,
            ))?;
        if row.get::<i64>(4)? != 0 {
            return Err(IngressUowError::ExtensionGrantAssertionFailed(
                GrantAssertionFailure::Revoked,
            ));
        }
        if decode_grant(&row)? != *grant {
            return Err(IngressUowError::ExtensionGrantAssertionFailed(
                GrantAssertionFailure::Mismatch,
            ));
        }
        Ok(GrantAssertion::Asserted)
    }

    /// Hold the requester's account against deletion for this admission.
    pub async fn assert_requester(
        tx: &mut IngressUowTransaction<'_>,
        requester: &BareJid,
    ) -> Result<GrantAssertion, IngressUowError> {
        let sql = dialect_sql(
            tx,
            "SELECT jid FROM users WHERE jid = ? FOR SHARE",
            "SELECT jid FROM users WHERE jid = ?",
        );
        let mut rows = tx
            .transaction_mut()
            .query(sql, crate::db_params![requester.to_string()])
            .await?;
        if rows.next().await?.is_some() {
            return Ok(GrantAssertion::Asserted);
        }
        drop(rows);
        let username = requester
            .node()
            .ok_or(IngressUowError::ExtensionGrantAssertionFailed(
                GrantAssertionFailure::RequesterGone,
            ))?;
        let sql = dialect_sql(
            tx,
            "SELECT username FROM native_users WHERE username = ? AND domain = ? FOR SHARE",
            "SELECT username FROM native_users WHERE username = ? AND domain = ?",
        );
        let mut rows = tx
            .transaction_mut()
            .query(
                sql,
                crate::db_params![username.as_str(), requester.domain().as_str()],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Err(IngressUowError::ExtensionGrantAssertionFailed(
                GrantAssertionFailure::RequesterGone,
            ));
        }
        Ok(GrantAssertion::Asserted)
    }

    async fn revoke_grant(
        tx: &mut IngressUowTransaction<'_>,
        id: ExtensionGrantId,
    ) -> Result<u64, IngressUowError> {
        let sql = dialect_sql(tx,
            "UPDATE extension_grants SET revoked_at = ?::timestamptz WHERE grant_id = ? AND revoked_at IS NULL",
            "UPDATE extension_grants SET revoked_at = ? WHERE grant_id = ? AND revoked_at IS NULL");
        Ok(tx
            .transaction_mut()
            .execute(
                sql,
                crate::db_params![Utc::now().to_rfc3339(), id.as_uuid().to_string()],
            )
            .await?)
    }

    async fn insert_grant(
        tx: &mut IngressUowTransaction<'_>,
        plugin: &PluginId,
        scope: &ExtensionGrantScope,
    ) -> Result<(), IngressUowError> {
        let (scope, room) = match scope {
            ExtensionGrantScope::Send => (0_i64, None),
            ExtensionGrantScope::ProviderRoom(room) => (1_i64, Some(room.to_string())),
        };
        let sql = dialect_sql(tx,
            "INSERT INTO extension_grants (grant_id, plugin_id, scope, room_jid, granted_at) VALUES (?, ?, ?, ?, ?::timestamptz)",
            "INSERT INTO extension_grants (grant_id, plugin_id, scope, room_jid, granted_at) VALUES (?, ?, ?, ?, ?)");
        tx.transaction_mut()
            .execute(
                sql,
                crate::db_params![
                    Uuid::new_v4().to_string(),
                    plugin.as_str(),
                    scope,
                    room,
                    Utc::now().to_rfc3339()
                ],
            )
            .await?;
        Ok(())
    }
}

fn dialect_sql(
    tx: &mut IngressUowTransaction<'_>,
    postgres: &'static str,
    sqlite: &'static str,
) -> &'static str {
    match tx.transaction_mut().driver() {
        DatabaseDriver::Postgres => postgres,
        DatabaseDriver::Sqlite => sqlite,
    }
}

/// Decode storage exactly once before durable authority enters typed code.
fn decode_grant(row: &Row) -> Result<ExtensionGrantRef, IngressUowError> {
    let grant_id = ExtensionGrantId::new(
        row.get::<String>(0)?
            .parse()
            .map_err(|_| IngressUowError::InvalidStoredExtensionGrant)?,
    );
    let plugin = PluginId::new(row.get::<String>(1)?)
        .map_err(|_| IngressUowError::InvalidStoredExtensionGrant)?;
    let scope = match (row.get::<i64>(2)?, row.get::<Option<String>>(3)?) {
        (0, None) => ExtensionGrantScope::Send,
        (1, Some(room)) => ExtensionGrantScope::ProviderRoom(
            room.parse()
                .map_err(|_| IngressUowError::InvalidStoredExtensionGrant)?,
        ),
        _ => return Err(IngressUowError::InvalidStoredExtensionGrant),
    };
    Ok(ExtensionGrantRef {
        grant_id,
        plugin,
        scope,
    })
}
