use std::collections::HashSet;

#[cfg(test)]
use std::collections::HashMap;

use jid::{BareJid, FullJid};
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

#[cfg(test)]
struct StagedReservationLookupFailures {
    exact: HashMap<(String, i64), usize>,
    up_to: HashMap<(String, i64), usize>,
}

#[cfg(test)]
fn staged_reservation_lookup_failures() -> &'static std::sync::Mutex<StagedReservationLookupFailures>
{
    static FAILURES: std::sync::OnceLock<std::sync::Mutex<StagedReservationLookupFailures>> =
        std::sync::OnceLock::new();
    FAILURES.get_or_init(|| {
        std::sync::Mutex::new(StagedReservationLookupFailures {
            exact: HashMap::new(),
            up_to: HashMap::new(),
        })
    })
}

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
    /// Make the next exact staged-reservation lookup fail. This test seam
    /// exercises recovery after the database read itself fails.
    #[cfg(test)]
    pub fn fail_next_staged_reservation_lookup_for_test(
        &self,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
        revision: waddle_xmpp::muc::RoomRevision,
    ) {
        self.fail_staged_reservation_lookup_times_for_test(lifecycle, revision, 1);
    }
    /// Fail an exact staged-reservation lookup a bounded number of times.
    #[cfg(test)]
    pub fn fail_staged_reservation_lookup_times_for_test(
        &self,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
        revision: waddle_xmpp::muc::RoomRevision,
        failures: usize,
    ) {
        staged_reservation_lookup_failures()
            .lock()
            .expect("staged reservation lookup-failure lock")
            .exact
            .insert((lifecycle.to_string(), revision.as_i64()), failures);
    }
    /// Fail an up-to staged-reservation lookup a bounded number of times.
    #[cfg(test)]
    pub fn fail_staged_reservations_up_to_lookup_times_for_test(
        &self,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
        max_revision: waddle_xmpp::muc::RoomRevision,
        failures: usize,
    ) {
        staged_reservation_lookup_failures()
            .lock()
            .expect("staged reservation lookup-failure lock")
            .up_to
            .insert((lifecycle.to_string(), max_revision.as_i64()), failures);
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
            tx.execute("INSERT INTO clustering_muc_room_effects (lifecycle_id, revision, ordinal, room_jid, kind, terminal, payload_json, available_at_ms, superseded, origin_instance_id, producing_node, lease_token, leased_at_ms, attempt_count, last_error, created_at_ms) VALUES (?, ?, ?, ?, ?, (? <> 0), ?, ?, FALSE, ?, ?, NULL, NULL, 0, NULL, ?)", crate::db_params![lifecycle.to_string(), revision.as_i64(), ordinal.as_i64(), room_jid.ok_or(RoomEffectOutboxError::InvalidPayload)?.to_string(), effect.kind().as_db_str(), effect.is_terminal(), encode_effect(effect)?, available_at_ms, origin.as_str(), producing_node.clone(), now_ms]).await?;
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
    pub async fn staged_reservation_for(
        &self,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
        revision: waddle_xmpp::muc::RoomRevision,
    ) -> Result<Option<RoomEffectReservation>, RoomEffectOutboxError> {
        #[cfg(test)]
        let fail = {
            let mut failures = staged_reservation_lookup_failures()
                .lock()
                .expect("staged reservation lookup-failure lock");
            let key = (lifecycle.to_string(), revision.as_i64());
            let fail = failures.exact.get_mut(&key).is_some_and(|remaining| {
                if *remaining == 0 {
                    false
                } else {
                    *remaining -= 1;
                    true
                }
            });
            failures.exact.retain(|_, remaining| *remaining > 0);
            fail
        };
        #[cfg(test)]
        if fail {
            return Err(RoomEffectOutboxError::InvalidPayload);
        }
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                "SELECT ordinal FROM clustering_muc_room_effects \
                 WHERE lifecycle_id = ? AND revision = ? AND available_at_ms = ? \
                 AND NOT superseded ORDER BY ordinal",
                crate::db_params![lifecycle.to_string(), revision.as_i64(), INERT],
            )
            .await?;
        let mut ordinals = Vec::new();
        while let Some(row) = rows.next().await? {
            let ordinal = waddle_xmpp::muc::RoomEffectOrdinal::from_stored(row.get(0)?)
                .ok_or(RoomEffectOutboxError::InvalidCoordinate)?;
            ordinals.push(ordinal);
        }
        Ok((!ordinals.is_empty()).then_some(RoomEffectReservation {
            lifecycle,
            revision,
            ordinals,
        }))
    }
    /// Every still-inert, non-terminal reservation of `lifecycle` at or below
    /// `max_revision`, oldest first. Config-class rows are inert until armed
    /// and describe durably committed configs (arm-by-default invariant), so a
    /// recovery that can no longer identify its exact row arms all of them:
    /// an unarmed row would otherwise head-of-line-block the lifecycle FIFO.
    pub async fn staged_reservations_up_to(
        &self,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
        max_revision: waddle_xmpp::muc::RoomRevision,
    ) -> Result<Vec<RoomEffectReservation>, RoomEffectOutboxError> {
        #[cfg(test)]
        {
            let fail = {
                let mut failures = staged_reservation_lookup_failures()
                    .lock()
                    .expect("staged reservation lookup-failure lock");
                let key = (lifecycle.to_string(), max_revision.as_i64());
                let fail = failures.up_to.get_mut(&key).is_some_and(|remaining| {
                    if *remaining == 0 {
                        false
                    } else {
                        *remaining -= 1;
                        true
                    }
                });
                failures.up_to.retain(|_, remaining| *remaining > 0);
                fail
            };
            if fail {
                return Err(RoomEffectOutboxError::InvalidPayload);
            }
        }
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                "SELECT revision, ordinal FROM clustering_muc_room_effects \
                 WHERE lifecycle_id = ? AND revision <= ? AND available_at_ms = ? \
                 AND NOT superseded AND NOT terminal ORDER BY revision, ordinal",
                crate::db_params![lifecycle.to_string(), max_revision.as_i64(), INERT],
            )
            .await?;
        let mut reservations: Vec<RoomEffectReservation> = Vec::new();
        while let Some(row) = rows.next().await? {
            let revision = waddle_xmpp::muc::RoomRevision::from_stored(row.get(0)?)
                .ok_or(RoomEffectOutboxError::InvalidCoordinate)?;
            let ordinal = waddle_xmpp::muc::RoomEffectOrdinal::from_stored(row.get(1)?)
                .ok_or(RoomEffectOutboxError::InvalidCoordinate)?;
            match reservations.last_mut() {
                Some(current) if current.revision == revision => current.ordinals.push(ordinal),
                _ => reservations.push(RoomEffectReservation {
                    lifecycle,
                    revision,
                    ordinals: vec![ordinal],
                }),
            }
        }
        Ok(reservations)
    }
    pub async fn reservation_for_revision(
        &self,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
        revision: waddle_xmpp::muc::RoomRevision,
    ) -> Result<Option<RoomEffectReservation>, RoomEffectOutboxError> {
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                "SELECT ordinal FROM clustering_muc_room_effects \
                 WHERE lifecycle_id = ? AND revision = ? AND NOT superseded \
                 ORDER BY ordinal",
                crate::db_params![lifecycle.to_string(), revision.as_i64()],
            )
            .await?;
        let mut ordinals = Vec::new();
        while let Some(row) = rows.next().await? {
            let ordinal = waddle_xmpp::muc::RoomEffectOrdinal::from_stored(row.get(0)?)
                .ok_or(RoomEffectOutboxError::InvalidCoordinate)?;
            ordinals.push(ordinal);
        }
        Ok((!ordinals.is_empty()).then_some(RoomEffectReservation {
            lifecycle,
            revision,
            ordinals,
        }))
    }
    /// Locate the exact terminal destroy reservation for a tombstoned
    /// lifecycle.  The destroy-completion executor uses this after its
    /// app-level wipe succeeds, so recovery cannot depend on an actor-local
    /// reservation that may have been lost in a crash.
    pub async fn terminal_reservation_for_lifecycle(
        &self,
        lifecycle: waddle_xmpp::muc::RoomLifecycleId,
    ) -> Result<Option<RoomEffectReservation>, RoomEffectOutboxError> {
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                "SELECT revision, ordinal FROM clustering_muc_room_effects \
                 WHERE lifecycle_id = ? AND terminal AND NOT superseded \
                 ORDER BY revision, ordinal",
                crate::db_params![lifecycle.to_string()],
            )
            .await?;
        let mut reservation: Option<RoomEffectReservation> = None;
        while let Some(row) = rows.next().await? {
            let revision = waddle_xmpp::muc::RoomRevision::from_stored(row.get(0)?)
                .ok_or(RoomEffectOutboxError::InvalidCoordinate)?;
            let ordinal = waddle_xmpp::muc::RoomEffectOrdinal::from_stored(row.get(1)?)
                .ok_or(RoomEffectOutboxError::InvalidCoordinate)?;
            match &mut reservation {
                Some(existing) if existing.revision == revision => existing.ordinals.push(ordinal),
                Some(_) => return Err(RoomEffectOutboxError::InvalidCoordinate),
                slot @ None => {
                    *slot = Some(RoomEffectReservation {
                        lifecycle,
                        revision,
                        ordinals: vec![ordinal],
                    });
                }
            }
        }
        Ok(reservation)
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
        successor_codes: &[waddle_xmpp::muc::MucConfigStatusCode],
    ) -> Result<u64, RoomEffectOutboxError> {
        if !successor_codes
            .contains(&waddle_xmpp::muc::MucConfigStatusCode::NonPrivacyConfigurationChange)
        {
            return Ok(0);
        }
        let mut rows = tx.query(&format!("{} WHERE lifecycle_id = ? AND kind = 'config_changed' AND available_at_ms = ? AND NOT superseded", select_columns()), crate::db_params![lifecycle.to_string(), INERT]).await?;
        let mut keys = Vec::new();
        while let Some(row) = rows.next().await? {
            let decoded = decode_row(&row)?;
            if matches!(
                &decoded.effect,
                waddle_xmpp::muc::RoomEffect::ConfigChanged {
                    status_codes,
                    voice_changes,
                    ..
                } if voice_changes.is_empty()
                    && status_codes.iter().all(|c| *c
                        == waddle_xmpp::muc::MucConfigStatusCode::NonPrivacyConfigurationChange)
            ) {
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
        // Preselect only per-lifecycle FIFO heads: a requeued head carries a
        // LATER available_at_ms than its successor ordinal, so without this
        // filter a small batch would keep selecting the unclaimable successor
        // and the lifecycle would never drain.
        let mut rows = c.query(&format!("{} WHERE available_at_ms <= ? AND NOT superseded AND (lease_token IS NULL OR leased_at_ms <= ?) AND NOT EXISTS (SELECT 1 FROM clustering_muc_room_effects earlier WHERE earlier.lifecycle_id = clustering_muc_room_effects.lifecycle_id AND (earlier.revision < clustering_muc_room_effects.revision OR (earlier.revision = clustering_muc_room_effects.revision AND earlier.ordinal < clustering_muc_room_effects.ordinal))) ORDER BY available_at_ms, lifecycle_id LIMIT ?", select_columns()), crate::db_params![now_ms, stale, batch.clamp(1,1000) as i64]).await?;
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
    pub async fn claim_exact_with_owned_leases(
        &self,
        key: &RoomEffectKey,
        now_ms: i64,
        owned_leases: &HashSet<RoomEffectLeaseToken>,
    ) -> Result<Option<ClaimedRoomEffect>, RoomEffectOutboxError> {
        if let Some(claimed) = self.claim_exact(key, now_ms).await? {
            return Ok(Some(claimed));
        }
        if owned_leases.is_empty()
            || !self
                .earlier_retained_rows_are_owned_and_recipient_disjoint(key, now_ms, owned_leases)
                .await?
        {
            return Ok(None);
        }
        self.claim_exact_without_earlier_exists(key, now_ms).await
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
        let sql = format!(
            "UPDATE clustering_muc_room_effects SET lease_token = ?, leased_at_ms = ? WHERE lifecycle_id = ? AND revision = ? AND ordinal = ? {eligibility} AND NOT superseded AND (lease_token IS NULL OR leased_at_ms <= ?) AND NOT EXISTS (SELECT 1 FROM clustering_muc_room_effects earlier WHERE earlier.lifecycle_id = clustering_muc_room_effects.lifecycle_id AND (earlier.revision < clustering_muc_room_effects.revision OR (earlier.revision = clustering_muc_room_effects.revision AND earlier.ordinal < clustering_muc_room_effects.ordinal))) AND (NOT terminal OR NOT EXISTS (SELECT 1 FROM clustering_muc_room_effects active WHERE active.lifecycle_id = clustering_muc_room_effects.lifecycle_id AND (active.revision <> clustering_muc_room_effects.revision OR active.ordinal <> clustering_muc_room_effects.ordinal) AND active.lease_token IS NOT NULL AND active.leased_at_ms > ?))"
        );
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
    async fn earlier_retained_rows_are_owned_and_recipient_disjoint(
        &self,
        key: &RoomEffectKey,
        now_ms: i64,
        owned_leases: &HashSet<RoomEffectLeaseToken>,
    ) -> Result<bool, RoomEffectOutboxError> {
        let Some(candidate) = self.find(key).await? else {
            return Ok(false);
        };
        let candidate_recipients = effect_recipients(&candidate.effect);
        let stale = now_ms.saturating_sub(CLAIM_TIMEOUT_MS);
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                &format!(
                    "{} WHERE lifecycle_id = ? AND (revision < ? OR (revision = ? AND ordinal < ?))",
                    select_columns()
                ),
                crate::db_params![
                    key.lifecycle.to_string(),
                    key.revision.as_i64(),
                    key.revision.as_i64(),
                    key.ordinal.as_i64(),
                ],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let row = decode_row(&row)?;
            let Some(token) = row.lease_token.clone() else {
                return Ok(false);
            };
            let Some(leased_at_ms) = row.leased_at_ms else {
                return Ok(false);
            };
            if leased_at_ms <= stale
                || !owned_leases.contains(&token)
                || !effect_recipients(&row.effect).is_disjoint(&candidate_recipients)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    async fn claim_exact_without_earlier_exists(
        &self,
        key: &RoomEffectKey,
        now_ms: i64,
    ) -> Result<Option<ClaimedRoomEffect>, RoomEffectOutboxError> {
        let stale = now_ms.saturating_sub(CLAIM_TIMEOUT_MS);
        let token = RoomEffectLeaseToken::new();
        let connection = self.db.guard().await?;
        if connection
            .execute(
                "UPDATE clustering_muc_room_effects SET lease_token = ?, leased_at_ms = ? WHERE lifecycle_id = ? AND revision = ? AND ordinal = ? AND NOT superseded AND (lease_token IS NULL OR leased_at_ms <= ?) AND (NOT terminal OR NOT EXISTS (SELECT 1 FROM clustering_muc_room_effects active WHERE active.lifecycle_id = clustering_muc_room_effects.lifecycle_id AND (active.revision <> clustering_muc_room_effects.revision OR active.ordinal <> clustering_muc_room_effects.ordinal) AND active.lease_token IS NOT NULL AND active.leased_at_ms > ?))",
                crate::db_params![
                    token.as_str(),
                    now_ms,
                    key.lifecycle.to_string(),
                    key.revision.as_i64(),
                    key.ordinal.as_i64(),
                    stale,
                    stale,
                ],
            )
            .await?
            != 1
        {
            return Ok(None);
        }
        drop(connection);
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
        Ok(c.execute("UPDATE clustering_muc_room_effects SET leased_at_ms=? WHERE lifecycle_id=? AND revision=? AND ordinal=? AND lease_token=? AND NOT superseded",crate::db_params![now_ms,key.lifecycle.to_string(),key.revision.as_i64(),key.ordinal.as_i64(),token.as_str()]).await?==1)
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
        let n=c.execute("UPDATE clustering_muc_room_effects SET attempt_count=?, last_error=?, available_at_ms=?, lease_token=NULL, leased_at_ms=NULL, unowned_since_ms=NULL WHERE lifecycle_id=? AND revision=? AND ordinal=? AND lease_token=?",crate::db_params![next,error.as_db_str(),now_ms.saturating_add(retry_delay_ms(next)),key.lifecycle.to_string(),key.revision.as_i64(),key.ordinal.as_i64(),token.as_str()]).await?;
        Ok(if n == 1 {
            RoomEffectReleaseOutcome::Released {
                attempt_count: next,
            }
        } else {
            RoomEffectReleaseOutcome::LostLease
        })
    }
    /// Return a lease to the queue without turning an ownership miss into a
    /// delivery attempt.  A room actor can move between nodes while its
    /// durable FIFO backlog remains shared; the new owner must be able to
    /// claim promptly without consuming retry budget.
    pub async fn release_unattempted(
        &self,
        key: &RoomEffectKey,
        token: &RoomEffectLeaseToken,
        now_ms: i64,
        delay_ms: i64,
    ) -> Result<bool, RoomEffectOutboxError> {
        let connection = self.db.guard().await?;
        Ok(connection
            .execute(
                "UPDATE clustering_muc_room_effects SET available_at_ms = ?, lease_token = NULL, leased_at_ms = NULL WHERE lifecycle_id = ? AND revision = ? AND ordinal = ? AND lease_token = ?",
                crate::db_params![
                    now_ms.saturating_add(delay_ms),
                    key.lifecycle.to_string(),
                    key.revision.as_i64(),
                    key.ordinal.as_i64(),
                    token.as_str(),
                ],
            )
            .await?
            == 1)
    }
    pub async fn note_unowned_since_if_absent(
        &self,
        key: &RoomEffectKey,
        token: &RoomEffectLeaseToken,
        now_ms: i64,
    ) -> Result<Option<i64>, RoomEffectOutboxError> {
        let Some(row) = self.find(key).await? else {
            return Ok(None);
        };
        if row.lease_token.as_ref() != Some(token) {
            return Ok(None);
        }
        if let Some(unowned_since_ms) = row.unowned_since_ms {
            return Ok(Some(unowned_since_ms));
        }
        let connection = self.db.guard().await?;
        if connection
            .execute(
                "UPDATE clustering_muc_room_effects SET unowned_since_ms = ? \
                 WHERE lifecycle_id = ? AND revision = ? AND ordinal = ? AND lease_token = ? \
                   AND unowned_since_ms IS NULL",
                crate::db_params![
                    now_ms,
                    key.lifecycle.to_string(),
                    key.revision.as_i64(),
                    key.ordinal.as_i64(),
                    token.as_str(),
                ],
            )
            .await?
            == 1
        {
            return Ok(Some(now_ms));
        }
        Ok(self
            .find(key)
            .await?
            .and_then(|row| {
                (row.lease_token.as_ref() == Some(token)).then_some(row.unowned_since_ms)
            })
            .flatten())
    }
    pub async fn clear_unowned_since(
        &self,
        key: &RoomEffectKey,
    ) -> Result<(), RoomEffectOutboxError> {
        let connection = self.db.guard().await?;
        connection
            .execute(
                "UPDATE clustering_muc_room_effects SET unowned_since_ms = NULL \
                 WHERE lifecycle_id = ? AND revision = ? AND ordinal = ? AND unowned_since_ms IS NOT NULL",
                crate::db_params![
                    key.lifecycle.to_string(),
                    key.revision.as_i64(),
                    key.ordinal.as_i64(),
                ],
            )
            .await?;
        Ok(())
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
                    "{} WHERE available_at_ms = ? AND NOT terminal AND NOT superseded",
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
    /// In standalone topology there is exactly one live process, so any inert
    /// committed row from a different process incarnation belongs to a dead
    /// predecessor and can be armed without consulting cluster claims.
    pub async fn list_predecessor_inert(
        &self,
        current_origin: &RoomEffectOriginInstanceId,
    ) -> Result<Vec<RoomEffectRow>, RoomEffectOutboxError> {
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                &format!(
                    "{} WHERE available_at_ms = ? AND NOT terminal AND NOT superseded AND origin_instance_id <> ?",
                    select_columns()
                ),
                crate::db_params![INERT, current_origin.as_str()],
            )
            .await?;
        let mut stale = Vec::new();
        while let Some(row) = rows.next().await? {
            stale.push(decode_row(&row)?);
        }
        Ok(stale)
    }
    pub async fn arm_predecessor_inert(
        &self,
        current_origin: &RoomEffectOriginInstanceId,
        now_ms: i64,
    ) -> Result<u64, RoomEffectOutboxError> {
        let rows = self.list_predecessor_inert(current_origin).await?;
        let mut armed = 0;
        for row in rows {
            armed += u64::from(self.arm(&row.key, now_ms).await?);
        }
        Ok(armed)
    }
    /// Snapshot currently-live cluster node incarnations.  Epoch is part of
    /// the identity, so a restarted node with the same node id still arms the
    /// predecessor's inert committed rows.
    pub async fn current_producing_nodes(
        &self,
    ) -> Result<Vec<RoomEffectProducingNode>, RoomEffectOutboxError> {
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                "SELECT node_id, node_epoch FROM clustering_nodes WHERE NOT expired",
                (),
            )
            .await?;
        let mut nodes = Vec::new();
        while let Some(row) = rows.next().await? {
            let node_id: String = row.get(0)?;
            let node_epoch: String = row.get(1)?;
            nodes.push(RoomEffectProducingNode::from_node_identity(
                waddle_xmpp::ownership::NodeIdentity::new(node_id, node_epoch),
            ));
        }
        Ok(nodes)
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

    /// Fence a drained effect to its exact room incarnation.  A matching
    /// tombstone remains valid because terminal destroy effects deliberately
    /// run after the durable room row is gone; any other row must still be the
    /// room's currently live lifecycle.  This keeps stale rows from a reused
    /// room JID from firing into its successor.
    pub async fn lifecycle_is_executable(
        &self,
        row: &RoomEffectRow,
    ) -> Result<bool, RoomEffectOutboxError> {
        let connection = self.db.guard().await?;
        let mut exact = connection
            .query(
                "SELECT state FROM clustering_muc_room_lifecycles WHERE room_jid = ? AND lifecycle_id = ?",
                crate::db_params![row.room_jid.to_string(), row.key.lifecycle.to_string()],
            )
            .await?;
        let Some(exact) = exact.next().await? else {
            return Ok(false);
        };
        let state: String = exact.get(0)?;
        if state == waddle_xmpp::muc::RoomLifecycleState::Tombstoned.as_db_str() {
            return Ok(row.effect.is_terminal());
        }
        let mut current = connection
            .query(
                "SELECT lifecycle_id FROM clustering_muc_room_lifecycles WHERE room_jid = ? AND state <> 'tombstoned' LIMIT 1",
                crate::db_params![row.room_jid.to_string()],
            )
            .await?;
        Ok(current.next().await?.is_some_and(|current| {
            current
                .get::<String>(0)
                .is_ok_and(|id| id == row.key.lifecycle.to_string())
        }))
    }

    pub async fn queue_depth(&self) -> Result<i64, RoomEffectOutboxError> {
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query("SELECT COUNT(*) FROM clustering_muc_room_effects", ())
            .await?;
        rows.next()
            .await?
            .ok_or(RoomEffectOutboxError::InvalidCoordinate)
            .and_then(|row| row.get(0).map_err(RoomEffectOutboxError::from))
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
    "SELECT lifecycle_id, revision, ordinal, room_jid, kind, terminal, payload_json, available_at_ms, superseded, origin_instance_id, producing_node, lease_token, leased_at_ms, attempt_count, last_error, created_at_ms, unowned_since_ms FROM clustering_muc_room_effects"
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
        unowned_since_ms: row.get(16)?,
    })
}

fn effect_recipients(effect: &waddle_xmpp::muc::RoomEffect) -> HashSet<FullJid> {
    match effect {
        waddle_xmpp::muc::RoomEffect::ConfigChanged { recipients, .. } => {
            recipients.iter().cloned().collect()
        }
        waddle_xmpp::muc::RoomEffect::AdminSelfNotify { updates } => updates
            .iter()
            .map(|update| update.recipient.clone())
            .collect(),
        waddle_xmpp::muc::RoomEffect::AdminRemainingBroadcast {
            presence_updates, ..
        } => presence_updates
            .iter()
            .map(|update| update.recipient.clone())
            .collect(),
        waddle_xmpp::muc::RoomEffect::DestroyNotification { recipients, .. } => recipients
            .iter()
            .flat_map(|recipient| recipient.sessions.iter().cloned())
            .collect(),
    }
}
