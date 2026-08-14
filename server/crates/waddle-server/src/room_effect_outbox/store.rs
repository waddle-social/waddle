use jid::BareJid;
use waddle_xmpp::muc::{RoomEffectReservation, RoomEffectStagingClass, RoomMutationEffects};

use super::schema;
use super::types::{decode_effect, encode_effect};
use super::{
    ClaimedRoomEffect, RoomEffectKey, RoomEffectLastError, RoomEffectLeaseToken,
    RoomEffectOriginInstanceId, RoomEffectOutboxError, RoomEffectProducingNode,
    RoomEffectReleaseOutcome, RoomEffectRow,
};
use crate::db::{Database, Row, Transaction};

pub const MAX_ATTEMPTS: i64 = 20;
pub const BASE_RETRY_DELAY_MS: i64 = 5_000;
pub const MAX_RETRY_DELAY_MS: i64 = 600_000;
pub const CLAIM_TIMEOUT_MS: i64 = 300_000;
pub const HANDLER_GRACE_MS: i64 = 30_000;
const INERT: i64 = i64::MAX;

pub fn retry_delay_ms(attempt: i64) -> i64 {
    let shift = if attempt <= 1 {
        0
    } else {
        (attempt - 1).min(20) as u32
    };
    BASE_RETRY_DELAY_MS
        .saturating_mul(1_i64 << shift)
        .min(MAX_RETRY_DELAY_MS)
}

/// All inputs of one in-transaction enqueue; the mutation's committed
/// coordinates plus the producer identity that owns the staged rows.
pub struct RoomEffectEnqueue<'a> {
    pub lifecycle: waddle_xmpp::muc::RoomLifecycleId,
    pub revision: waddle_xmpp::muc::RoomRevision,
    pub effects: &'a RoomMutationEffects,
    pub origin: &'a RoomEffectOriginInstanceId,
    pub producing_node: &'a RoomEffectProducingNode,
    pub now_ms: i64,
}

#[derive(Clone)]
pub struct RoomEffectOutboxStore {
    db: Database,
}
impl RoomEffectOutboxStore {
    pub async fn new(db: Database) -> Result<Self, RoomEffectOutboxError> {
        schema::initialize(&db).await?;
        Ok(Self { db })
    }
    pub fn database(&self) -> &Database {
        &self.db
    }
    pub async fn enqueue_in_tx(
        &self,
        tx: &mut Transaction<'_>,
        enqueue: RoomEffectEnqueue<'_>,
    ) -> Result<RoomEffectReservation, RoomEffectOutboxError> {
        let RoomEffectEnqueue {
            lifecycle,
            revision,
            effects,
            origin,
            producing_node,
            now_ms,
        } = enqueue;
        let available_at_ms = match effects.staging() {
            RoomEffectStagingClass::HandlerWindow => now_ms.saturating_add(HANDLER_GRACE_MS),
            RoomEffectStagingClass::StagedConfig | RoomEffectStagingClass::Terminal => INERT,
        };
        let producing_node = producing_node.as_db_value()?;
        let room_jid = effects.room_jid();
        let mut ordinal = waddle_xmpp::muc::RoomEffectOrdinal::first();
        let mut ordinals = Vec::with_capacity(effects.effects().len());
        for effect in effects.effects() {
            tx.execute("INSERT INTO clustering_muc_room_effects (lifecycle_id, revision, ordinal, room_jid, kind, terminal, payload_json, available_at_ms, superseded, origin_instance_id, producing_node, lease_token, leased_at_ms, attempt_count, last_error, created_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, FALSE, ?, ?, NULL, NULL, 0, NULL, ?)", crate::db_params![lifecycle.to_string(), revision.as_i64(), ordinal.as_i64(), room_jid.ok_or(RoomEffectOutboxError::InvalidPayload)?.to_string(), effect.kind().as_db_str(), effect.is_terminal(), encode_effect(effect)?, available_at_ms, origin.as_str(), producing_node.clone(), now_ms]).await?;
            ordinals.push(ordinal);
            ordinal = ordinal
                .next()
                .ok_or(RoomEffectOutboxError::InvalidCoordinate)?;
        }
        Ok(RoomEffectReservation {
            lifecycle,
            revision,
            ordinals,
        })
    }
    pub async fn find(
        &self,
        key: &RoomEffectKey,
    ) -> Result<Option<RoomEffectRow>, RoomEffectOutboxError> {
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                &format!(
                    "{} WHERE lifecycle_id = ? AND revision = ? AND ordinal = ?",
                    select_columns()
                ),
                crate::db_params![
                    key.lifecycle.to_string(),
                    key.revision.as_i64(),
                    key.ordinal.as_i64()
                ],
            )
            .await?;
        rows.next().await?.map(|row| decode_row(&row)).transpose()
    }
    pub async fn list_for_lifecycle(
        &self,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
    ) -> Result<Vec<RoomEffectRow>, RoomEffectOutboxError> {
        let c = self.db.guard().await?;
        let mut rows = c
            .query(
                &format!(
                    "{} WHERE lifecycle_id = ? ORDER BY revision, ordinal",
                    select_columns()
                ),
                crate::db_params![lifecycle.to_string()],
            )
            .await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            result.push(decode_row(&row)?);
        }
        Ok(result)
    }
    pub async fn arm(
        &self,
        key: &RoomEffectKey,
        now_ms: i64,
    ) -> Result<bool, RoomEffectOutboxError> {
        let c = self.db.guard().await?;
        Ok(c.execute("UPDATE clustering_muc_room_effects SET available_at_ms = ? WHERE lifecycle_id = ? AND revision = ? AND ordinal = ? AND available_at_ms = ? AND NOT superseded", crate::db_params![now_ms, key.lifecycle.to_string(), key.revision.as_i64(), key.ordinal.as_i64(), INERT]).await? == 1)
    }
    pub async fn arm_reservation(
        &self,
        reservation: &RoomEffectReservation,
        now_ms: i64,
    ) -> Result<u64, RoomEffectOutboxError> {
        let mut changed = 0;
        for ordinal in &reservation.ordinals {
            changed += u64::from(
                self.arm(
                    &RoomEffectKey {
                        lifecycle: reservation.lifecycle,
                        revision: reservation.revision,
                        ordinal: *ordinal,
                    },
                    now_ms,
                )
                .await?,
            );
        }
        Ok(changed)
    }
    pub async fn supersede_non_terminal_in_tx(
        &self,
        tx: &mut Transaction<'_>,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
        now_ms: i64,
    ) -> Result<(), RoomEffectOutboxError> {
        let stale = now_ms.saturating_sub(CLAIM_TIMEOUT_MS);
        tx.execute("DELETE FROM clustering_muc_room_effects WHERE lifecycle_id = ? AND NOT terminal AND (lease_token IS NULL OR leased_at_ms <= ?)", crate::db_params![lifecycle.to_string(), stale]).await?;
        tx.execute("UPDATE clustering_muc_room_effects SET superseded = TRUE WHERE lifecycle_id = ? AND NOT terminal AND lease_token IS NOT NULL AND leased_at_ms > ?", crate::db_params![lifecycle.to_string(), stale]).await?;
        Ok(())
    }
    pub async fn supersede_reservation_in_tx(
        &self,
        tx: &mut Transaction<'_>,
        reservation: &RoomEffectReservation,
    ) -> Result<Vec<RoomEffectRow>, RoomEffectOutboxError> {
        let mut deleted = Vec::new();
        for ordinal in &reservation.ordinals {
            let mut rows = tx
                .query(
                    &format!(
                        "{} WHERE lifecycle_id = ? AND revision = ? AND ordinal = ?",
                        select_columns()
                    ),
                    crate::db_params![
                        reservation.lifecycle.to_string(),
                        reservation.revision.as_i64(),
                        ordinal.as_i64()
                    ],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                let decoded = decode_row(&row)?;
                drop(rows);
                if tx.execute("DELETE FROM clustering_muc_room_effects WHERE lifecycle_id = ? AND revision = ? AND ordinal = ?", crate::db_params![reservation.lifecycle.to_string(), reservation.revision.as_i64(), ordinal.as_i64()]).await? == 1 { deleted.push(decoded); }
            }
        }
        Ok(deleted)
    }
    pub async fn supersede_idempotent_config_in_tx(
        &self,
        tx: &mut Transaction<'_>,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
    ) -> Result<u64, RoomEffectOutboxError> {
        let mut rows = tx.query(&format!("{} WHERE lifecycle_id = ? AND kind = 'config_changed' AND available_at_ms = ? AND NOT superseded", select_columns()), crate::db_params![lifecycle.to_string(), INERT]).await?;
        let mut keys = Vec::new();
        while let Some(row) = rows.next().await? {
            let decoded = decode_row(&row)?;
            if matches!(&decoded.effect, waddle_xmpp::muc::RoomEffect::ConfigChanged { status_codes, .. } if status_codes.iter().all(|c| *c == waddle_xmpp::muc::MucConfigStatusCode::NonPrivacyConfigurationChange))
            {
                keys.push(decoded.key);
            }
        }
        drop(rows);
        let mut changed = 0;
        for key in keys {
            changed += tx.execute("DELETE FROM clustering_muc_room_effects WHERE lifecycle_id = ? AND revision = ? AND ordinal = ? AND available_at_ms = ?", crate::db_params![key.lifecycle.to_string(), key.revision.as_i64(), key.ordinal.as_i64(), INERT]).await?;
        }
        Ok(changed)
    }
    pub async fn claim_due_head(
        &self,
        now_ms: i64,
        batch: usize,
    ) -> Result<Vec<ClaimedRoomEffect>, RoomEffectOutboxError> {
        let stale = now_ms.saturating_sub(CLAIM_TIMEOUT_MS);
        let c = self.db.guard().await?;
        let mut rows = c.query(&format!("{} WHERE available_at_ms <= ? AND NOT superseded AND (lease_token IS NULL OR leased_at_ms <= ?) ORDER BY available_at_ms, lifecycle_id LIMIT ?", select_columns()), crate::db_params![now_ms, stale, batch.clamp(1,1000) as i64]).await?;
        let mut candidates = Vec::new();
        while let Some(row) = rows.next().await? {
            candidates.push(decode_row(&row)?);
        }
        drop(rows);
        drop(c);
        let mut claimed = Vec::new();
        for row in candidates {
            if let Some(job) = self.claim_inner(&row.key, now_ms, false).await? {
                claimed.push(job);
            }
        }
        Ok(claimed)
    }
    pub async fn claim_exact(
        &self,
        key: &RoomEffectKey,
        now_ms: i64,
    ) -> Result<Option<ClaimedRoomEffect>, RoomEffectOutboxError> {
        self.claim_inner(key, now_ms, true).await
    }
    async fn claim_inner(
        &self,
        key: &RoomEffectKey,
        now_ms: i64,
        exact: bool,
    ) -> Result<Option<ClaimedRoomEffect>, RoomEffectOutboxError> {
        let stale = now_ms.saturating_sub(CLAIM_TIMEOUT_MS);
        let token = RoomEffectLeaseToken::new();
        let c = self.db.guard().await?;
        let eligibility = if exact {
            ""
        } else {
            "AND available_at_ms <= ?"
        };
        let sql=format!("UPDATE clustering_muc_room_effects SET lease_token = ?, leased_at_ms = ? WHERE lifecycle_id = ? AND revision = ? AND ordinal = ? {eligibility} AND NOT superseded AND (lease_token IS NULL OR leased_at_ms <= ?) AND NOT EXISTS (SELECT 1 FROM clustering_muc_room_effects earlier WHERE earlier.lifecycle_id = clustering_muc_room_effects.lifecycle_id AND (earlier.revision < clustering_muc_room_effects.revision OR (earlier.revision = clustering_muc_room_effects.revision AND earlier.ordinal < clustering_muc_room_effects.ordinal))) AND (NOT terminal OR NOT EXISTS (SELECT 1 FROM clustering_muc_room_effects active WHERE active.lifecycle_id = clustering_muc_room_effects.lifecycle_id AND (active.revision <> clustering_muc_room_effects.revision OR active.ordinal <> clustering_muc_room_effects.ordinal) AND active.lease_token IS NOT NULL AND active.leased_at_ms > ?))");
        let mut params = crate::db_params![
            token.as_str(),
            now_ms,
            key.lifecycle.to_string(),
            key.revision.as_i64(),
            key.ordinal.as_i64()
        ];
        if !exact {
            params.push(now_ms.into());
        }
        params.push(stale.into());
        params.push(stale.into());
        if c.execute(&sql, params).await? != 1 {
            return Ok(None);
        }
        drop(c);
        let mut row = self
            .find(key)
            .await?
            .ok_or(RoomEffectOutboxError::InvalidCoordinate)?;
        row.lease_token = Some(token.clone());
        Ok(Some(ClaimedRoomEffect {
            row,
            lease_token: token,
        }))
    }
    pub async fn revalidate(
        &self,
        key: &RoomEffectKey,
        token: &RoomEffectLeaseToken,
    ) -> Result<bool, RoomEffectOutboxError> {
        let c = self.db.guard().await?;
        let mut r=c.query("SELECT 1 FROM clustering_muc_room_effects WHERE lifecycle_id=? AND revision=? AND ordinal=? AND lease_token=? AND NOT superseded", crate::db_params![key.lifecycle.to_string(),key.revision.as_i64(),key.ordinal.as_i64(),token.as_str()]).await?;
        Ok(r.next().await?.is_some())
    }
    pub async fn complete(
        &self,
        key: &RoomEffectKey,
        token: &RoomEffectLeaseToken,
    ) -> Result<bool, RoomEffectOutboxError> {
        let c = self.db.guard().await?;
        Ok(c.execute("DELETE FROM clustering_muc_room_effects WHERE lifecycle_id=? AND revision=? AND ordinal=? AND lease_token=?",crate::db_params![key.lifecycle.to_string(),key.revision.as_i64(),key.ordinal.as_i64(),token.as_str()]).await?==1)
    }
    pub async fn renew_lease(
        &self,
        key: &RoomEffectKey,
        token: &RoomEffectLeaseToken,
        now_ms: i64,
    ) -> Result<bool, RoomEffectOutboxError> {
        let c = self.db.guard().await?;
        Ok(c.execute("UPDATE clustering_muc_room_effects SET leased_at_ms=? WHERE lifecycle_id=? AND revision=? AND ordinal=? AND lease_token=?",crate::db_params![now_ms,key.lifecycle.to_string(),key.revision.as_i64(),key.ordinal.as_i64(),token.as_str()]).await?==1)
    }
    pub async fn release(
        &self,
        key: &RoomEffectKey,
        token: &RoomEffectLeaseToken,
        now_ms: i64,
        error: RoomEffectLastError,
    ) -> Result<RoomEffectReleaseOutcome, RoomEffectOutboxError> {
        let Some(row) = self.find(key).await? else {
            return Ok(RoomEffectReleaseOutcome::LostLease);
        };
        if row.lease_token.as_ref() != Some(token) {
            return Ok(RoomEffectReleaseOutcome::LostLease);
        };
        let next = row.attempt_count + 1;
        let terminal = row.effect.is_terminal();
        let c = self.db.guard().await?;
        if next >= MAX_ATTEMPTS
            && !terminal
            && error != RoomEffectLastError::InfrastructureTransient
        {
            let n=c.execute("DELETE FROM clustering_muc_room_effects WHERE lifecycle_id=? AND revision=? AND ordinal=? AND lease_token=?",crate::db_params![key.lifecycle.to_string(),key.revision.as_i64(),key.ordinal.as_i64(),token.as_str()]).await?;
            return Ok(if n == 1 {
                RoomEffectReleaseOutcome::DeadLettered {
                    attempt_count: next,
                }
            } else {
                RoomEffectReleaseOutcome::LostLease
            });
        }
        let n=c.execute("UPDATE clustering_muc_room_effects SET attempt_count=?, last_error=?, available_at_ms=?, lease_token=NULL, leased_at_ms=NULL WHERE lifecycle_id=? AND revision=? AND ordinal=? AND lease_token=?",crate::db_params![next,error.as_db_str(),now_ms.saturating_add(retry_delay_ms(next)),key.lifecycle.to_string(),key.revision.as_i64(),key.ordinal.as_i64(),token.as_str()]).await?;
        Ok(if n == 1 {
            RoomEffectReleaseOutcome::Released {
                attempt_count: next,
            }
        } else {
            RoomEffectReleaseOutcome::LostLease
        })
    }
    pub async fn reap_superseded(&self, now_ms: i64) -> Result<u64, RoomEffectOutboxError> {
        let c = self.db.guard().await?;
        Ok(c.execute("DELETE FROM clustering_muc_room_effects WHERE superseded AND (lease_token IS NULL OR leased_at_ms <= ?)",crate::db_params![now_ms.saturating_sub(CLAIM_TIMEOUT_MS)]).await?)
    }
    /// Returns staged rows whose producing process incarnation is no longer in
    /// the supplied live-node set. Callers arm these rows; they never discard
    /// them, because the committed mutation remains truthful after a crash.
    pub async fn list_foreign_inert(
        &self,
        current_nodes: &[RoomEffectProducingNode],
    ) -> Result<Vec<RoomEffectRow>, RoomEffectOutboxError> {
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                &format!(
                    "{} WHERE available_at_ms = ? AND NOT superseded",
                    select_columns()
                ),
                crate::db_params![INERT],
            )
            .await?;
        let mut stale = Vec::new();
        while let Some(row) = rows.next().await? {
            let row = decode_row(&row)?;
            if !current_nodes.iter().any(|node| {
                node.node_identity()
                    .same_incarnation(row.producing_node.node_identity())
            }) {
                stale.push(row);
            }
        }
        Ok(stale)
    }
    pub async fn arm_foreign_inert(
        &self,
        current_nodes: &[RoomEffectProducingNode],
        now_ms: i64,
    ) -> Result<u64, RoomEffectOutboxError> {
        let rows = self.list_foreign_inert(current_nodes).await?;
        let mut armed = 0;
        for row in rows {
            armed += u64::from(self.arm(&row.key, now_ms).await?);
        }
        Ok(armed)
    }
    pub async fn pending_rows_for_lifecycle(
        &self,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
    ) -> Result<i64, RoomEffectOutboxError> {
        let c = self.db.guard().await?;
        let mut r = c
            .query(
                "SELECT COUNT(*) FROM clustering_muc_room_effects WHERE lifecycle_id=?",
                crate::db_params![lifecycle.to_string()],
            )
            .await?;
        Ok(r.next()
            .await?
            .ok_or(RoomEffectOutboxError::InvalidCoordinate)?
            .get(0)?)
    }
    pub async fn has_pending_terminal_for_room_in_tx(
        &self,
        tx: &mut Transaction<'_>,
        room: &BareJid,
    ) -> Result<bool, RoomEffectOutboxError> {
        let mut r = tx
            .query(
                "SELECT 1 FROM clustering_muc_room_effects WHERE room_jid=? AND terminal LIMIT 1",
                crate::db_params![room.to_string()],
            )
            .await?;
        Ok(r.next().await?.is_some())
    }
}
fn select_columns() -> &'static str {
    "SELECT lifecycle_id, revision, ordinal, room_jid, kind, terminal, payload_json, available_at_ms, superseded, origin_instance_id, producing_node, lease_token, leased_at_ms, attempt_count, last_error, created_at_ms FROM clustering_muc_room_effects"
}
fn decode_row(row: &Row) -> Result<RoomEffectRow, RoomEffectOutboxError> {
    let lifecycle_string: String = row.get(0)?;
    let lifecycle = uuid::Uuid::parse_str(&lifecycle_string)
        .map(waddle_xmpp::muc::RoomLifecycleId::from_uuid)
        .map_err(|_| RoomEffectOutboxError::InvalidCoordinate)?;
    let revision = waddle_xmpp::muc::RoomRevision::from_stored(row.get(1)?)
        .ok_or(RoomEffectOutboxError::InvalidCoordinate)?;
    let ordinal = waddle_xmpp::muc::RoomEffectOrdinal::from_stored(row.get(2)?)
        .ok_or(RoomEffectOutboxError::InvalidCoordinate)?;
    let room_text: String = row.get(3)?;
    let room_jid = room_text
        .parse()
        .map_err(|_| RoomEffectOutboxError::InvalidRoomJid(room_text))?;
    let kind: String = row.get(4)?;
    Ok(RoomEffectRow {
        key: RoomEffectKey {
            lifecycle,
            revision,
            ordinal,
        },
        room_jid,
        effect: decode_effect(&kind, &row.get::<String>(6)?)?,
        available_at_ms: row.get(7)?,
        superseded: row.get(8)?,
        origin_instance_id: RoomEffectOriginInstanceId::new(row.get(9)?)
            .ok_or(RoomEffectOutboxError::InvalidPayload)?,
        producing_node: RoomEffectProducingNode::from_db_value(row.get(10)?)?,
        lease_token: row
            .get::<Option<String>>(11)?
            .map(RoomEffectLeaseToken::from_stored),
        leased_at_ms: row.get(12)?,
        attempt_count: row.get(13)?,
        last_error: row
            .get::<Option<String>>(14)?
            .and_then(|value| RoomEffectLastError::from_db_str(&value)),
        created_at_ms: row.get(15)?,
    })
}
