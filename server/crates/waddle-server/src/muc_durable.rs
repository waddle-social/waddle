//! Postgres-backed durable MUC room ownership state (ADR-0017 Phase 3
//! Slice 7, element 7).
//!
//! Implements `waddle_xmpp::muc::MucDurableStore` for [`PostgresMucRoomStore`]
//! — the concrete, clustering-only backing store a freshly spawned/claimed
//! `RoomActor` restores configuration, affiliations, and subject from
//! before accepting any join, and persists the same three pieces to on
//! every change. `None` (no `MucDurableStore` configured at all) in
//! single-node/non-clustering deployments, matching today's purely
//! in-memory room behavior exactly — this module is only ever constructed
//! when clustering is enabled against a Postgres backend, mirroring
//! `sm_persistence_fenced::PostgresFencedSmPersistence`'s identical gating.
//!
//! **Schema** (PROPOSED — no ADR-locked DDL exists for MUC room durability,
//! only the column shapes element 7's text names): `clustering_` prefixed,
//! following the `clustering_nodes`/`clustering_claims`/
//! `clustering_steal_intents` convention (ensure-schema-on-the-store, not a
//! versioned app migration, since these tables exist purely to back the
//! clustering-durability subsystem):
//! - `clustering_muc_rooms` — one row per durably-written room: `waddle_id`/
//!   `channel_id` plus the JSON-serialized `RoomConfig`/`SubjectState`
//!   (both already `Serialize`/`Deserialize` for exactly this purpose).
//! - `clustering_muc_room_affiliations` — one row per `(room_jid, member_jid)`
//!   affiliation grant. `affiliation` is stored via a small
//!   `affiliation_to_db_str`/`affiliation_from_db_str` pair, mirroring
//!   `EntityType::as_db_str`/`from_db_str`'s exact convention, rather than a
//!   JSON blob for a five-variant closed enum.
//!
//! **Fencing**: every `save_*` write runs [`PostgresMucRoomStore::assert_fenced`]
//! — the exact `SELECT ... FOR SHARE` shape `sm_persistence_fenced::
//! assert_fenced` already established — as the first statement inside the
//! same [`crate::db::Database::begin`] transaction as the write it guards,
//! on the **main pool**, never the control-plane pool (the Slice 0/4/7
//! pool-assignment rule). The epoch bound into that check comes from a
//! per-room cache ([`PostgresMucRoomStore::claim_epochs`]) populated by the
//! room registry calling [`Self::record_claim_epoch`] immediately after a
//! successful claim acquire/steal — never re-derived here, mirroring
//! `PostgresFencedSmPersistence`'s own "epoch side channel" design note.
//!
//! **Fan-out and archive fencing**: [`Self::check_fenced_fanout`] provides
//! the standalone pre-effect proof used even when archiving is disabled.
//! When a groupchat message is archived, `MamStorage::store_message_fenced`
//! repeats the exact claim plus serving-eligible node-incarnation locks inside
//! the same Postgres transaction as the MAM insert. Origin-id dedup hits are
//! resolved inside that transaction too; a retry can never bypass the
//! ownership proof merely because its archive row already exists.

use dashmap::DashMap;
use jid::BareJid;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::muc::affiliation::AffiliationEntry;
use waddle_xmpp::muc::{
    DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext, RoomConfig,
    SubjectState,
};
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity, SharedNodeIdentity};
use waddle_xmpp::{Affiliation, XmppError};

use crate::clustering::relay::RelayHandle;
use crate::clustering::NodeId;
use crate::db::{Database, DatabaseError, Transaction};

fn db_err(error: DatabaseError) -> XmppError {
    XmppError::internal(format!("MUC durable store backend error: {error}"))
}

/// Injective `(entity_type, id) -> TEXT` encoding for the
/// `clustering_claims.entity` primary key, mirroring
/// `clustering::claims::entity_key`/`sm_persistence_fenced::
/// sm_session_entity_key` exactly. Duplicated here rather than imported
/// for the same accepted-tradeoff reason those two already duplicate it
/// from each other: this impl owns its own inline fencing SQL rather than
/// delegating to `ClaimStore`.
fn room_entity_key(room_jid: &BareJid) -> String {
    format!("{}:{}", EntityType::RoomActor.as_db_str(), room_jid)
}

/// `EntityType::as_db_str`/`from_db_str`'s exact convention, applied to
/// `Affiliation` (a closed, five-variant enum) for the
/// `clustering_muc_room_affiliations.affiliation` column — a small typed
/// mapping rather than a JSON blob for a value this simple.
fn affiliation_to_db_str(affiliation: Affiliation) -> &'static str {
    match affiliation {
        Affiliation::Outcast => "outcast",
        Affiliation::None => "none",
        Affiliation::Member => "member",
        Affiliation::Admin => "admin",
        Affiliation::Owner => "owner",
    }
}

fn affiliation_from_db_str(value: &str) -> Option<Affiliation> {
    match value {
        "outcast" => Some(Affiliation::Outcast),
        "none" => Some(Affiliation::None),
        "member" => Some(Affiliation::Member),
        "admin" => Some(Affiliation::Admin),
        "owner" => Some(Affiliation::Owner),
        _ => None,
    }
}

/// Postgres-backed [`MucDurableStore`] (ADR-0017 Phase 3 Slice 7). See the
/// module doc for the schema, fencing design, and the MAM-backstop
/// code-research correction.
pub struct PostgresMucRoomStore {
    db: Database,
    node_identity: SharedNodeIdentity,
    /// This node's clustering-scope cancellation token, threaded into the
    /// `RelayHandle` [`Self::notify_previous_owner_demoted`] constructs
    /// per-call (mirroring `resume_asker::SwarmRemoteResumeAsker`'s
    /// identical "fresh `RelayHandle` per ask" pattern).
    stop_token: CancellationToken,
    /// Per-room claim epoch cache — see the module doc's fencing section.
    claim_epochs: DashMap<BareJid, ClaimEpoch>,
}

impl PostgresMucRoomStore {
    /// Open against an already-opened Postgres [`Database`] handle — the
    /// SAME global handle `clustering::start_if_enabled` gives the claims
    /// store, never a second, independently-resolved database (the fencing
    /// `SELECT ... FOR SHARE` this impl issues targets `clustering_claims`,
    /// which lives there).
    pub async fn open(
        db: Database,
        node_identity: SharedNodeIdentity,
        stop_token: CancellationToken,
    ) -> Result<Self, XmppError> {
        let store = Self {
            db,
            node_identity,
            stop_token,
            claim_epochs: DashMap::new(),
        };
        store.ensure_schema().await.map_err(db_err)?;
        Ok(store)
    }

    async fn ensure_schema(&self) -> Result<(), DatabaseError> {
        let conn = self.db.guard().await?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_muc_rooms (
                room_jid     TEXT PRIMARY KEY,
                waddle_id    TEXT NOT NULL,
                channel_id   TEXT NOT NULL,
                config_json  TEXT NOT NULL,
                subject_json TEXT,
                updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
            (),
        )
        .await?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_muc_room_affiliations (
                room_jid    TEXT NOT NULL,
                member_jid  TEXT NOT NULL,
                affiliation TEXT NOT NULL,
                reason      TEXT,
                granted_at  TIMESTAMPTZ,
                PRIMARY KEY (room_jid, member_jid)
            )
            "#,
            (),
        )
        .await?;
        conn.execute(
            r#"
            CREATE INDEX IF NOT EXISTS clustering_muc_room_affiliations_room_jid_idx
                ON clustering_muc_room_affiliations (room_jid)
            "#,
            (),
        )
        .await?;
        Ok(())
    }

    fn epoch_for(&self, room_jid: &BareJid) -> Result<ClaimEpoch, XmppError> {
        self.claim_epochs
            .get(room_jid)
            .map(|entry| *entry)
            .ok_or_else(|| XmppError::RoomOwnershipLost(room_jid.clone()))
    }

    fn current_fence_for(&self, room_jid: &BareJid) -> Result<RoomClaimFenceContext, XmppError> {
        Ok(RoomClaimFenceContext {
            entity: Entity::new(EntityType::RoomActor, room_jid.to_string()),
            epoch: self.epoch_for(room_jid)?,
            owner: self.node_identity.current(),
        })
    }

    fn validate_exact_fence(
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), XmppError> {
        let expected = Entity::new(EntityType::RoomActor, room_jid.to_string());
        if fence.entity == expected {
            Ok(())
        } else {
            Err(XmppError::RoomOwnershipLost(room_jid.clone()))
        }
    }

    /// Take the fencing locks inside `tx` for both the exact claim and its
    /// exact, non-expired, heartbeat-fresh node incarnation. A matching stale
    /// claim row alone is not authority after the node lease has lapsed,
    /// expired, or rotated. Draining
    /// remains valid here: a draining owner serves its existing claims until
    /// terminal teardown; only new acquisition is barred while draining.
    async fn assert_fenced(
        &self,
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), XmppError> {
        Self::validate_exact_fence(room_jid, fence)?;
        let key = room_entity_key(room_jid);
        let mut rows = tx
            .query(
                r#"
                WITH locked AS MATERIALIZED (
                    SELECT n.heartbeat, n.expired, n.lease_ttl_ms
                    FROM clustering_claims AS c
                    JOIN clustering_nodes AS n
                      ON n.node_id = c.node_id
                     AND n.node_epoch = c.node_epoch
                    WHERE c.entity = ?
                      AND c.node_id = ?
                      AND c.node_epoch = ?
                      AND c.claim_epoch = ?
                    FOR SHARE OF c, n
                )
                SELECT 1 FROM locked
                WHERE NOT expired
                  AND heartbeat >= clock_timestamp()
                      - (lease_ttl_ms::text || ' milliseconds')::interval
                "#,
                crate::db_params![
                    key,
                    fence.owner.node_id.clone(),
                    fence.owner.node_epoch.clone(),
                    fence.epoch.0,
                ],
            )
            .await
            .map_err(db_err)?;
        let held = rows.next().await.map_err(db_err)?.is_some();
        if held {
            Ok(())
        } else {
            // Never let an old actor's failed E1 proof erase a newer E2
            // cache entry for the same room.
            self.claim_epochs
                .remove_if(room_jid, |_, cached| *cached == fence.epoch);
            Err(XmppError::RoomOwnershipLost(room_jid.clone()))
        }
    }

    async fn save_config_at(
        &self,
        room_jid: &BareJid,
        waddle_id: &str,
        channel_id: &str,
        config: &RoomConfig,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), XmppError> {
        let config_json = serde_json::to_string(config).map_err(|error| {
            XmppError::internal(format!("durable room config encode failed: {error}"))
        })?;
        let mut tx = self.db.begin_fenced().await.map_err(db_err)?;
        self.assert_fenced(&mut tx, room_jid, fence).await?;
        tx.execute(
            r#"
            INSERT INTO clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json, updated_at)
            VALUES (?, ?, ?, ?, now())
            ON CONFLICT (room_jid) DO UPDATE SET
                waddle_id = excluded.waddle_id,
                channel_id = excluded.channel_id,
                config_json = excluded.config_json,
                updated_at = excluded.updated_at
            "#,
            crate::db_params![
                room_jid.to_string(),
                waddle_id.to_string(),
                channel_id.to_string(),
                config_json,
            ],
        )
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn save_subject_at(
        &self,
        room_jid: &BareJid,
        subject: Option<&SubjectState>,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), XmppError> {
        let subject_json = subject
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| {
                XmppError::internal(format!("durable room subject encode failed: {error}"))
            })?;
        let mut tx = self.db.begin_fenced().await.map_err(db_err)?;
        self.assert_fenced(&mut tx, room_jid, fence).await?;
        let affected = tx
            .execute(
                "UPDATE clustering_muc_rooms SET subject_json = ?, updated_at = now() WHERE room_jid = ?",
                crate::db_params![subject_json, room_jid.to_string()],
            )
            .await
            .map_err(db_err)?;
        if affected == 0 {
            tracing::warn!(
                room = %room_jid,
                "durable subject persist skipped: no durable room row exists yet \
                 (config has not been durably written for this room)"
            );
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn save_affiliation_at(
        &self,
        room_jid: &BareJid,
        entry: &AffiliationEntry,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), XmppError> {
        let mut tx = self.db.begin_fenced().await.map_err(db_err)?;
        self.assert_fenced(&mut tx, room_jid, fence).await?;
        if entry.affiliation == Affiliation::None {
            tx.execute(
                "DELETE FROM clustering_muc_room_affiliations WHERE room_jid = ? AND member_jid = ?",
                crate::db_params![room_jid.to_string(), entry.jid.to_string()],
            )
            .await
            .map_err(db_err)?;
        } else {
            tx.execute(
                r#"
                INSERT INTO clustering_muc_room_affiliations
                    (room_jid, member_jid, affiliation, reason)
                VALUES (?, ?, ?, ?)
                ON CONFLICT (room_jid, member_jid) DO UPDATE SET
                    affiliation = excluded.affiliation,
                    reason = excluded.reason
                "#,
                crate::db_params![
                    room_jid.to_string(),
                    entry.jid.to_string(),
                    affiliation_to_db_str(entry.affiliation).to_string(),
                    entry.reason.clone(),
                ],
            )
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn check_fence_at(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<bool, XmppError> {
        Self::validate_exact_fence(room_jid, fence)?;
        let key = room_entity_key(room_jid);
        let mut tx = self.db.begin_fenced().await.map_err(db_err)?;
        let mut rows = tx
            .query(
                r#"
                WITH locked AS MATERIALIZED (
                    SELECT n.heartbeat, n.expired, n.lease_ttl_ms
                    FROM clustering_claims AS c
                    JOIN clustering_nodes AS n
                      ON n.node_id = c.node_id
                     AND n.node_epoch = c.node_epoch
                    WHERE c.entity = ?
                      AND c.node_id = ?
                      AND c.node_epoch = ?
                      AND c.claim_epoch = ?
                    FOR SHARE OF c, n
                )
                SELECT 1 FROM locked
                WHERE NOT expired
                  AND heartbeat >= clock_timestamp()
                      - (lease_ttl_ms::text || ' milliseconds')::interval
                "#,
                crate::db_params![
                    key,
                    fence.owner.node_id.clone(),
                    fence.owner.node_epoch.clone(),
                    fence.epoch.0,
                ],
            )
            .await
            .map_err(db_err)?;
        let held = rows.next().await.map_err(db_err)?.is_some();
        drop(rows);
        tx.commit().await.map_err(db_err)?;
        Ok(held)
    }
}

impl MucDurableStore for PostgresMucRoomStore {
    fn load_room_state<'a>(
        &'a self,
        room_jid: &'a BareJid,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        Box::pin(async move {
            let conn = self.db.guard().await.map_err(db_err)?;
            let mut room_rows = conn
                .query(
                    "SELECT waddle_id, channel_id, config_json, subject_json FROM \
                     clustering_muc_rooms WHERE room_jid = ?",
                    crate::db_params![room_jid.to_string()],
                )
                .await
                .map_err(db_err)?;
            let Some(row) = room_rows.next().await.map_err(db_err)? else {
                return Ok(None);
            };
            let waddle_id: String = row.get(0).map_err(db_err)?;
            let channel_id: String = row.get(1).map_err(db_err)?;
            let config_json: String = row.get(2).map_err(db_err)?;
            let subject_json: Option<String> = row.get(3).map_err(db_err)?;
            let config: RoomConfig = serde_json::from_str(&config_json).map_err(|error| {
                XmppError::internal(format!("durable room config decode failed: {error}"))
            })?;
            let subject: Option<SubjectState> = subject_json
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(|error| {
                    XmppError::internal(format!("durable room subject decode failed: {error}"))
                })?;

            let mut affiliation_rows = conn
                .query(
                    "SELECT member_jid, affiliation, reason FROM \
                     clustering_muc_room_affiliations WHERE room_jid = ?",
                    crate::db_params![room_jid.to_string()],
                )
                .await
                .map_err(db_err)?;
            let mut affiliations = Vec::new();
            while let Some(row) = affiliation_rows.next().await.map_err(db_err)? {
                let member_jid: String = row.get(0).map_err(db_err)?;
                let affiliation_str: String = row.get(1).map_err(db_err)?;
                let reason: Option<String> = row.get(2).map_err(db_err)?;
                let Ok(jid) = member_jid.parse::<BareJid>() else {
                    tracing::warn!(
                        room = %room_jid,
                        member_jid,
                        "durable affiliation row has an unparseable member JID; skipping"
                    );
                    continue;
                };
                let Some(affiliation) = affiliation_from_db_str(&affiliation_str) else {
                    tracing::warn!(
                        room = %room_jid,
                        affiliation = affiliation_str,
                        "durable affiliation row has an unrecognized affiliation tag; skipping"
                    );
                    continue;
                };
                affiliations.push(AffiliationEntry {
                    jid,
                    affiliation,
                    granted_at: None,
                    reason,
                });
            }

            Ok(Some(DurableRoomState {
                waddle_id,
                channel_id,
                config,
                subject,
                affiliations,
            }))
        })
    }

    fn save_config<'a>(
        &'a self,
        room_jid: &'a BareJid,
        waddle_id: &'a str,
        channel_id: &'a str,
        config: &'a RoomConfig,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            let fence = self.current_fence_for(room_jid)?;
            self.save_config_at(room_jid, waddle_id, channel_id, config, &fence)
                .await
        })
    }

    fn save_config_exact<'a>(
        &'a self,
        room_jid: &'a BareJid,
        waddle_id: &'a str,
        channel_id: &'a str,
        config: &'a RoomConfig,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            self.save_config_at(room_jid, waddle_id, channel_id, config, fence)
                .await
        })
    }

    fn save_subject<'a>(
        &'a self,
        room_jid: &'a BareJid,
        subject: Option<&'a SubjectState>,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            let fence = self.current_fence_for(room_jid)?;
            self.save_subject_at(room_jid, subject, &fence).await
        })
    }

    fn save_subject_exact<'a>(
        &'a self,
        room_jid: &'a BareJid,
        subject: Option<&'a SubjectState>,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move { self.save_subject_at(room_jid, subject, fence).await })
    }

    fn save_affiliation<'a>(
        &'a self,
        room_jid: &'a BareJid,
        entry: &'a AffiliationEntry,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            let fence = self.current_fence_for(room_jid)?;
            self.save_affiliation_at(room_jid, entry, &fence).await
        })
    }

    fn save_affiliation_exact<'a>(
        &'a self,
        room_jid: &'a BareJid,
        entry: &'a AffiliationEntry,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move { self.save_affiliation_at(room_jid, entry, fence).await })
    }

    fn record_claim_epoch(&self, room_jid: &BareJid, epoch: ClaimEpoch) {
        self.claim_epochs.insert(room_jid.clone(), epoch);
    }

    fn forget_claim_epoch(&self, room_jid: &BareJid) {
        self.claim_epochs.remove(room_jid);
    }

    /// The guaranteed demotion backstop (element 7): a fenced,
    /// bounded read-only transaction with `SELECT ... FOR SHARE` on the main pool, run
    /// before every local fan-out. It proves both the exact claim and the
    /// exact, non-expired, heartbeat-fresh node incarnation at the check boundary. The later
    /// MAM write repeats and holds the same locks inside its insert
    /// transaction when archiving is active.
    fn check_fenced_fanout<'a>(&'a self, room_jid: &'a BareJid) -> MucDurableFuture<'a, bool> {
        Box::pin(async move {
            let fence = self.current_fence_for(room_jid)?;
            self.check_fence_at(room_jid, &fence).await
        })
    }

    fn check_fenced_fanout_exact<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        Box::pin(async move { self.check_fence_at(room_jid, fence).await })
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 1: exposes the exact `(Entity,
    /// ClaimEpoch, NodeIdentity)` triple [`Self::check_fenced_fanout`]/
    /// [`Self::assert_fenced`] already resolve from `self.claim_epochs`, so
    /// `groupchat_archive.rs`'s MAM fenced write can bind the identical
    /// typed context rather than re-deriving it from a second mechanism.
    fn current_claim_fence(&self, room_jid: &BareJid) -> Option<RoomClaimFenceContext> {
        let epoch = self.claim_epochs.get(room_jid).map(|entry| *entry)?;
        let identity = self.node_identity.current();
        Some(RoomClaimFenceContext {
            entity: Entity::new(EntityType::RoomActor, room_jid.to_string()),
            epoch,
            owner: identity,
        })
    }

    fn notify_previous_owner_demoted<'a>(
        &'a self,
        room_jid: &'a BareJid,
        previous_owner_node_id: &'a str,
        previous_owner_node_epoch: &'a str,
        new_epoch: ClaimEpoch,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
            let mut relay_handle = RelayHandle::new(
                NodeId::new(previous_owner_node_id.to_string()),
                self.stop_token.clone(),
            );
            relay_handle
                .demote(
                    entity,
                    NodeIdentity::new(previous_owner_node_id, previous_owner_node_epoch),
                    new_epoch,
                )
                .await
                .map(|_reply| ())
                .map_err(|error| {
                    XmppError::internal(format!(
                        "best-effort Demote relay ask to the previous owner failed: {error}"
                    ))
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::claims::{
        clustering_control_plane_table_lock, NodeLeaseStore, PostgresClaimStore,
    };
    use crate::db::{DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE};
    use std::sync::Arc;
    use waddle_xmpp::muc::RoomSubjectTexts;
    use waddle_xmpp::ownership::{ClaimStore, NodeIdentity, StalePredicate};

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    async fn live_stealer(db: &crate::db::Database) -> NodeIdentity {
        let stealer = node_identity();
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "INSERT INTO clustering_nodes (node_id, node_epoch, expired) VALUES (?, ?, false)",
            crate::db_params![stealer.node_id.clone(), stealer.node_epoch.clone()],
        )
        .await
        .expect("seed live stealer node");
        stealer
    }

    async fn expire_node(db: &crate::db::Database, identity: &NodeIdentity) {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_nodes SET expired = true \
             WHERE node_id = ? AND node_epoch = ?",
            crate::db_params![identity.node_id.clone(), identity.node_epoch.clone()],
        )
        .await
        .expect("expire old owner incarnation");
    }

    async fn lapse_node_heartbeat(db: &crate::db::Database, identity: &NodeIdentity) {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_nodes SET heartbeat = now() - interval '1 hour', expired = false \
             WHERE node_id = ? AND node_epoch = ?",
            crate::db_params![identity.node_id.clone(), identity.node_epoch.clone()],
        )
        .await
        .expect("lapse node heartbeat without committing expiry");
    }

    fn room_jid(name: &str) -> BareJid {
        format!("{name}@muc.example.com")
            .parse()
            .expect("valid test room JID")
    }

    fn moderation_row(
        room: &BareJid,
        moderation_id: &str,
        target_id: &str,
    ) -> waddle_xmpp_core::mam::ArchivedMessage {
        use waddle_xmpp_core::mam::{
            ArchivedMessage, ArchivedModeration, ArchivedRichMessage, ArchivedRichPayload,
            RichMessageId,
        };
        let room_jid = jid::Jid::from(room.clone());
        let wire_id = format!("{moderation_id}-wire");
        let stamp = chrono::Utc::now();
        ArchivedMessage {
            id: moderation_id.to_string(),
            timestamp: stamp,
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                wire_id,
                room_jid.clone(),
            )),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            rich: Some(ArchivedRichMessage {
                payload: Some(ArchivedRichPayload::Moderation(ArchivedModeration {
                    target_id: RichMessageId::new(target_id).expect("target id"),
                    moderated_by: format!("{room}/moderator").parse().expect("jid"),
                    stamp: Some(stamp),
                    reason: None,
                })),
                reply: None,
                references: Vec::new(),
                mentions: Vec::new(),
                occupant_id: None,
                muc_sender: None,
            }),
            ..ArchivedMessage::for_test(room_jid.clone(), room_jid)
        }
    }

    fn retraction_tombstone(retraction_id: &str) -> waddle_xmpp_core::mam::ArchivedTombstone {
        waddle_xmpp_core::mam::ArchivedTombstone {
            retraction_id: waddle_xmpp_core::mam::RichMessageId::new(retraction_id),
            stamp: chrono::Utc::now(),
            moderation: None,
        }
    }

    fn retraction_row(
        room: &BareJid,
        retraction_id: &str,
        target_id: &str,
        nickname_generation: u64,
    ) -> waddle_xmpp_core::mam::ArchivedMessage {
        use waddle_xmpp_core::mam::{
            ArchivedMessage, ArchivedRetraction, ArchivedRichMessage, ArchivedRichPayload,
            RichMessageId,
        };
        let room_jid = jid::Jid::from(room.clone());
        let from: jid::Jid = format!("{room}/alice").parse().expect("occupant");
        ArchivedMessage {
            id: retraction_id.to_string(),
            stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
                retraction_id,
                room_jid.clone(),
            )),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            rich: Some(ArchivedRichMessage {
                payload: Some(ArchivedRichPayload::Retraction(ArchivedRetraction {
                    target_id: RichMessageId::new(target_id).expect("target id"),
                    stamp: None,
                    retraction_id: RichMessageId::new(retraction_id),
                })),
                reply: None,
                references: Vec::new(),
                mentions: Vec::new(),
                occupant_id: None,
                muc_sender: None,
            }),
            nickname_generation: Some(nickname_generation),
            ..ArchivedMessage::for_test(from, room_jid)
        }
    }

    fn moderation_tombstone(
        moderation: &waddle_xmpp_core::mam::ArchivedMessage,
    ) -> waddle_xmpp_core::mam::ArchivedTombstone {
        let payload = match moderation
            .rich
            .as_ref()
            .and_then(|rich| rich.payload.as_ref())
        {
            Some(waddle_xmpp_core::mam::ArchivedRichPayload::Moderation(payload)) => {
                payload.clone()
            }
            other => panic!("expected moderation payload, got {other:?}"),
        };
        let wire_id = moderation
            .stanza_id
            .as_ref()
            .expect("moderation wire id")
            .id
            .clone();
        waddle_xmpp_core::mam::ArchivedTombstone {
            retraction_id: waddle_xmpp_core::mam::RichMessageId::new(wire_id),
            stamp: moderation.timestamp,
            moderation: Some(payload),
        }
    }

    /// Open a clean `PostgresMucRoomStore` alongside a `PostgresClaimStore`
    /// sharing the same `Database`/tables, wiping every row this module's
    /// tests touch first. `None` (test skipped) when
    /// `WADDLE_TEST_POSTGRES_URL` is unset, mirroring every other
    /// Postgres-gated test in this workspace.
    async fn clean_store() -> Option<(PostgresMucRoomStore, Arc<PostgresClaimStore>, Database)> {
        let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
        let db = Database::from_config(
            "muc-durable-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let claim_store = Arc::new(PostgresClaimStore::new(db.clone()));
        claim_store
            .ensure_schema()
            .await
            .expect("ensure claim schema");
        let store = PostgresMucRoomStore::open(
            db.clone(),
            SharedNodeIdentity::new(node_identity()),
            CancellationToken::new(),
        )
        .await
        .expect("open muc durable store");
        let conn = db.guard().await.expect("guard");
        conn.execute("DELETE FROM clustering_claims", ())
            .await
            .expect("clean claims");
        conn.execute("DELETE FROM clustering_nodes", ())
            .await
            .expect("clean nodes");
        conn.execute("DELETE FROM clustering_muc_rooms", ())
            .await
            .expect("clean rooms");
        conn.execute("DELETE FROM clustering_muc_room_affiliations", ())
            .await
            .expect("clean affiliations");
        drop(conn);
        claim_store
            .register(&store.node_identity.current(), None)
            .await
            .expect("register live room owner");
        Some((store, claim_store, db))
    }

    #[tokio::test]
    async fn save_and_load_round_trips_config_subject_and_affiliations() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db)) = clean_store().await else {
            return;
        };
        let jid = room_jid("round-trip");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let me = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        store.record_claim_epoch(&jid, epoch);

        let config = RoomConfig {
            name: "test room".to_string(),
            members_only: true,
            ..RoomConfig::default()
        };
        store
            .save_config(&jid, "waddle-1", "chan-1", &config)
            .await
            .expect("save config");

        let subject = SubjectState {
            texts: RoomSubjectTexts::from_iter([(String::new(), "hello".to_string())]),
            setter: "alice@example.com".parse().expect("valid jid"),
            setter_nick: "alice".to_string(),
            set_at: chrono::Utc::now(),
        };
        store
            .save_subject(&jid, Some(&subject))
            .await
            .expect("save subject");

        let bob: BareJid = "bob@example.com".parse().expect("valid jid");
        let entry = AffiliationEntry::new(bob.clone(), Affiliation::Owner);
        store
            .save_affiliation(&jid, &entry)
            .await
            .expect("save affiliation");

        let loaded = store
            .load_room_state(&jid)
            .await
            .expect("load")
            .expect("row exists");
        assert_eq!(loaded.waddle_id, "waddle-1");
        assert_eq!(loaded.channel_id, "chan-1");
        assert_eq!(loaded.config.name, "test room");
        assert!(loaded.config.members_only);
        assert_eq!(
            loaded.subject.expect("subject persisted").texts.get(""),
            Some("hello")
        );
        assert_eq!(loaded.affiliations.len(), 1);
        assert_eq!(loaded.affiliations[0].jid, bob);
        assert_eq!(loaded.affiliations[0].affiliation, Affiliation::Owner);
    }

    #[tokio::test]
    async fn save_affiliation_none_deletes_the_row() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db)) = clean_store().await else {
            return;
        };
        let jid = room_jid("affiliation-removal");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let me = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        store.record_claim_epoch(&jid, epoch);
        store
            .save_config(&jid, "waddle-1", "chan-1", &RoomConfig::default())
            .await
            .expect("save config");

        let carol: BareJid = "carol@example.com".parse().expect("valid jid");
        store
            .save_affiliation(
                &jid,
                &AffiliationEntry::new(carol.clone(), Affiliation::Member),
            )
            .await
            .expect("save member");
        let loaded = store
            .load_room_state(&jid)
            .await
            .expect("load")
            .expect("row exists");
        assert_eq!(loaded.affiliations.len(), 1);

        store
            .save_affiliation(&jid, &AffiliationEntry::new(carol, Affiliation::None))
            .await
            .expect("save none removes the row");
        let loaded = store
            .load_room_state(&jid)
            .await
            .expect("load")
            .expect("row exists");
        assert!(loaded.affiliations.is_empty());
    }

    /// The plan's own Slice 7 Tests entry: "fenced pre-fan-out SELECT
    /// returns 0 rows immediately after a steal commits (the deposed
    /// owner's very next broadcast is blocked)."
    #[tokio::test]
    async fn check_fenced_fanout_returns_false_immediately_after_a_steal_commits() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let jid = room_jid("deposed");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let me = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        store.record_claim_epoch(&jid, epoch);

        assert!(
            store
                .check_fenced_fanout(&jid)
                .await
                .expect("check_fenced_fanout"),
            "the current owner's own fenced check must pass"
        );

        // Simulate another node stealing via steal_stale(OwnerStale): expire
        // the exact old owner incarnation while the stealer has a fresh live
        // row, matching Slice 1a's hardened steal CAS.
        expire_node(&db, &me).await;
        let stealer = live_stealer(&db).await;
        claim_store
            .steal_stale(&entity, epoch, StalePredicate::OwnerStale, &stealer)
            .await
            .expect("steal succeeds against a dead-owner claim");

        assert!(
            !store
                .check_fenced_fanout(&jid)
                .await
                .expect("check_fenced_fanout"),
            "the deposed owner's very next fenced check must observe 0 rows"
        );
    }

    #[tokio::test]
    async fn exact_claim_is_not_authority_after_its_node_incarnation_expires() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let jid = room_jid("expired-owner-row");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let me = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        store.record_claim_epoch(&jid, epoch);
        let fence = store
            .current_claim_fence(&jid)
            .expect("cached exact claim fence");

        // Leave clustering_claims untouched. Only expire the exact node
        // incarnation: a claim-only fence would incorrectly keep passing.
        expire_node(&db, &me).await;

        assert!(
            !store
                .check_fenced_fanout(&jid)
                .await
                .expect("fanout ownership check"),
            "an exact claim row cannot outlive its owner node incarnation"
        );
        assert!(store
            .save_config(&jid, "waddle", "channel", &RoomConfig::default())
            .await
            .is_err());

        let mam_storage = SqlxMamStorage::open(db.database_url())
            .await
            .expect("open colocated MAM")
            .with_cluster_fencing(true);
        let archive_id = uuid::Uuid::new_v4().to_string();
        let message = ArchivedMessage {
            id: archive_id.clone(),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{jid}/alice").parse().expect("valid occupant JID"),
                jid::Jid::from(jid.clone()),
            )
        };
        assert!(matches!(
            mam_storage
                .store_message_fenced(&jid, &message, &fence)
                .await,
            Err(MamStorageError::NotOwner { .. })
        ));
        assert!(mam_storage
            .get_message(&archive_id)
            .await
            .expect("read MAM row")
            .is_none());
    }

    #[tokio::test]
    async fn lapsed_nonexpired_room_owner_cannot_fanout_or_write_muc_or_mam_state() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let jid = room_jid("lapsed-owner-row");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let me = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        store.record_claim_epoch(&jid, epoch);
        let fence = store
            .current_claim_fence(&jid)
            .expect("cached exact claim fence");
        lapse_node_heartbeat(&db, &me).await;

        assert!(
            !store
                .check_fenced_fanout(&jid)
                .await
                .expect("fanout ownership check"),
            "raw deadline lapse must close fanout before a watchdog commits expiry"
        );
        assert!(store
            .save_config(&jid, "waddle", "channel", &RoomConfig::default())
            .await
            .is_err());

        let mam_storage = SqlxMamStorage::open(db.database_url())
            .await
            .expect("open colocated MAM")
            .with_cluster_fencing(true);
        let archive_id = uuid::Uuid::new_v4().to_string();
        let message = ArchivedMessage {
            id: archive_id.clone(),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{jid}/alice").parse().expect("valid occupant JID"),
                jid::Jid::from(jid.clone()),
            )
        };
        assert!(matches!(
            mam_storage
                .store_message_fenced(&jid, &message, &fence)
                .await,
            Err(MamStorageError::NotOwner { .. })
        ));
        assert!(mam_storage
            .get_message(&archive_id)
            .await
            .expect("read MAM row")
            .is_none());
    }

    #[tokio::test]
    async fn origin_id_dedup_retry_still_requires_a_live_exact_owner() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;
        use waddle_xmpp_core::xep0359::OriginId;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let jid = room_jid("dedup-expired-owner");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let me = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        store.record_claim_epoch(&jid, epoch);
        let fence = store
            .current_claim_fence(&jid)
            .expect("cached exact claim fence");
        let mam_storage = SqlxMamStorage::open(db.database_url())
            .await
            .expect("open colocated MAM")
            .with_cluster_fencing(true);
        let first_id = uuid::Uuid::new_v4().to_string();
        let retry_id = uuid::Uuid::new_v4().to_string();
        let origin_id = OriginId::new(uuid::Uuid::new_v4().to_string());
        let first = ArchivedMessage {
            id: first_id.clone(),
            body: Some("deduplicated message".to_string()),
            origin_id: Some(origin_id),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{jid}/alice").parse().expect("valid occupant JID"),
                jid::Jid::from(jid.clone()),
            )
        };
        mam_storage
            .store_message_fenced(&jid, &first, &fence)
            .await
            .expect("live owner stores original");

        expire_node(&db, &me).await;
        let retry = ArchivedMessage {
            id: retry_id.clone(),
            ..first
        };
        assert!(matches!(
            mam_storage.store_message_fenced(&jid, &retry, &fence).await,
            Err(MamStorageError::NotOwner { .. })
        ));
        assert!(mam_storage
            .get_message(&retry_id)
            .await
            .expect("read retry MAM row")
            .is_none());
        assert!(mam_storage
            .get_message(&first_id)
            .await
            .expect("read original MAM row")
            .is_some());
    }

    #[tokio::test]
    async fn draining_owner_can_finish_existing_room_and_mam_writes() {
        use waddle_xmpp::mam::{MamStorage, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let jid = room_jid("draining-owner");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let me = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        store.record_claim_epoch(&jid, epoch);
        let fence = store
            .current_claim_fence(&jid)
            .expect("cached exact claim fence");
        claim_store
            .mark_draining(&me)
            .await
            .expect("mark owner draining");

        assert!(
            store
                .check_fenced_fanout(&jid)
                .await
                .expect("fanout ownership check"),
            "draining bars acquisition, not service of existing claims"
        );
        store
            .save_config(&jid, "waddle", "channel", &RoomConfig::default())
            .await
            .expect("draining owner finishes durable room write");

        let mam_storage = SqlxMamStorage::open(db.database_url())
            .await
            .expect("open colocated MAM")
            .with_cluster_fencing(true);
        let archive_id = uuid::Uuid::new_v4().to_string();
        let message = ArchivedMessage {
            id: archive_id.clone(),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{jid}/alice").parse().expect("valid occupant JID"),
                jid::Jid::from(jid.clone()),
            )
        };
        assert_eq!(
            mam_storage
                .store_message_fenced(&jid, &message, &fence)
                .await
                .expect("draining owner finishes fenced MAM write"),
            archive_id
        );
    }

    #[tokio::test]
    async fn same_node_id_epoch_rotation_blocks_room_and_mam_writes() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let jid = room_jid("same-node-new-epoch");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let old = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &old)
            .await
            .expect("old incarnation claims room");
        store.record_claim_epoch(&jid, epoch);
        store
            .save_config(&jid, "waddle-old", "channel-old", &RoomConfig::default())
            .await
            .expect("old incarnation writes initial room state");
        let stale_mam_fence = store
            .current_claim_fence(&jid)
            .expect("room claim fence is available");

        let recovered = NodeIdentity::new(old.node_id.clone(), uuid::Uuid::new_v4().to_string());
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_nodes SET node_epoch = ? WHERE node_id = ?",
            crate::db_params![recovered.node_epoch.clone(), recovered.node_id.clone()],
        )
        .await
        .expect("rotate process node epoch");
        conn.execute(
            "UPDATE clustering_claims SET node_epoch = ? WHERE entity = ?",
            crate::db_params![recovered.node_epoch.clone(), room_entity_key(&jid),],
        )
        .await
        .expect("move claim without changing claim epoch");
        drop(conn);

        assert!(
            !store
                .check_fenced_fanout(&jid)
                .await
                .expect("fanout fence check"),
            "the old node epoch must not fan out under the recovered incarnation"
        );
        assert!(
            store
                .save_config(
                    &jid,
                    "waddle-stale",
                    "channel-stale",
                    &RoomConfig::default(),
                )
                .await
                .is_err(),
            "the old node epoch must not mutate durable room state"
        );

        let mam_storage = SqlxMamStorage::open(db.database_url())
            .await
            .expect("open colocated mam storage")
            .with_cluster_fencing(true);
        let archive_id = uuid::Uuid::new_v4().to_string();
        let message = ArchivedMessage {
            id: archive_id.clone(),
            body: Some("stale incarnation".to_string()),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{jid}/alice").parse().expect("valid full jid"),
                jid::Jid::from(jid.clone()),
            )
        };
        let result = mam_storage
            .store_message_fenced(&jid, &message, &stale_mam_fence)
            .await;
        assert!(
            matches!(result, Err(MamStorageError::NotOwner { .. })),
            "MAM must reject an old node epoch even when node_id and claim_epoch match: {result:?}"
        );
        assert!(
            mam_storage
                .get_message(&archive_id)
                .await
                .expect("get message")
                .is_none(),
            "the rejected stale-epoch archive write must not land"
        );
    }

    #[tokio::test]
    async fn retained_e1_cannot_use_a_room_cache_that_has_advanced_to_e2() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let room = room_jid("retained-e1-new-e2");
        let entity = Entity::new(EntityType::RoomActor, room.to_string());
        let owner = store.node_identity.current();
        let e1 = claim_store
            .ensure_claimed(&entity, &owner)
            .await
            .expect("acquire E1");
        store.record_claim_epoch(&room, e1);
        let fence_e1 = store.current_claim_fence(&room).expect("E1 fence");

        claim_store
            .release(&entity, &owner, e1)
            .await
            .expect("release E1");
        let e2 = claim_store
            .ensure_claimed(&entity, &owner)
            .await
            .expect("acquire E2");
        assert!(e2 > e1, "a replacement actor must receive a newer epoch");
        store.record_claim_epoch(&room, e2);
        let fence_e2 = store.current_claim_fence(&room).expect("E2 fence");

        assert!(!store
            .check_fenced_fanout_exact(&room, &fence_e1)
            .await
            .expect("stale E1 fanout check"));
        assert!(store
            .check_fenced_fanout_exact(&room, &fence_e2)
            .await
            .expect("live E2 fanout check"));

        let stale_config = RoomConfig {
            name: "must not persist from E1".to_string(),
            ..RoomConfig::default()
        };
        let stale_save = store
            .save_config_exact(&room, "waddle-e1", "channel-e1", &stale_config, &fence_e1)
            .await;
        assert!(matches!(
            stale_save,
            Err(XmppError::RoomOwnershipLost(ref lost_room)) if lost_room == &room
        ));
        assert_eq!(
            store.current_claim_fence(&room),
            Some(fence_e2.clone()),
            "a failed E1 proof must not erase the newer cached E2"
        );
        assert!(store
            .load_room_state(&room)
            .await
            .expect("load room")
            .is_none());

        let mam = SqlxMamStorage::open(db.database_url())
            .await
            .expect("MAM")
            .with_cluster_fencing(true);
        let archive_id = uuid::Uuid::new_v4().to_string();
        let stale_message = ArchivedMessage {
            id: archive_id.clone(),
            body: Some("must not archive from E1".to_string()),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{room}/alice").parse().expect("occupant"),
                jid::Jid::from(room.clone()),
            )
        };
        let stale_archive = mam
            .store_message_fenced(&room, &stale_message, &fence_e1)
            .await;
        assert!(matches!(
            stale_archive,
            Err(MamStorageError::NotOwner { .. })
        ));
        assert!(mam
            .get_message(&archive_id)
            .await
            .expect("read rejected archive")
            .is_none());
        assert!(store
            .check_fenced_fanout_exact(&room, &fence_e2)
            .await
            .expect("E2 remains authoritative"));
    }

    #[tokio::test]
    async fn muc_fence_uses_wall_clock_when_lease_lapses_inside_an_open_transaction() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let room = room_jid("muc-open-tx-lease-lapse");
        let entity = Entity::new(EntityType::RoomActor, room.to_string());
        let owner = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &owner)
            .await
            .expect("claim room");
        store.record_claim_epoch(&room, epoch);
        let fence = store.current_claim_fence(&room).expect("fence");
        db.guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_nodes SET heartbeat = clock_timestamp(), lease_ttl_ms = 50 WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![owner.node_id.clone(), owner.node_epoch.clone()],
            )
            .await
            .expect("shorten lease");

        let mut tx = db.begin().await.expect("begin before TTL lapse");
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let result = store.assert_fenced(&mut tx, &room, &fence).await;
        assert!(matches!(
            result,
            Err(XmppError::RoomOwnershipLost(ref lost_room)) if lost_room == &room
        ));
    }

    #[tokio::test]
    async fn blocked_muc_and_mam_fences_recheck_wall_clock_after_the_node_lock_releases() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let store = Arc::new(store);
        let room = room_jid("blocked-fence-lease-lapse");
        let entity = Entity::new(EntityType::RoomActor, room.to_string());
        let owner = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &owner)
            .await
            .expect("claim room");
        store.record_claim_epoch(&room, epoch);
        let fence = store.current_claim_fence(&room).expect("fence");
        let mam = SqlxMamStorage::open(db.database_url())
            .await
            .expect("MAM")
            .with_cluster_fencing(true);
        db.guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_nodes SET heartbeat = clock_timestamp(), lease_ttl_ms = 100 WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![owner.node_id.clone(), owner.node_epoch.clone()],
            )
            .await
            .expect("shorten lease");

        let mut blocker = db.begin().await.expect("begin node-lock blocker");
        let mut locked = blocker
            .query(
                "SELECT 1 FROM clustering_nodes WHERE node_id = ? AND node_epoch = ? FOR UPDATE",
                crate::db_params![owner.node_id.clone(), owner.node_epoch.clone()],
            )
            .await
            .expect("lock node row");
        assert!(locked.next().await.expect("read lock row").is_some());
        drop(locked);

        let archive_id = uuid::Uuid::new_v4().to_string();
        let message = ArchivedMessage {
            id: archive_id.clone(),
            body: Some("blocked until stale".to_string()),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{room}/alice").parse().expect("occupant"),
                jid::Jid::from(room.clone()),
            )
        };

        let muc_task = {
            let store = Arc::clone(&store);
            let room = room.clone();
            let fence = fence.clone();
            tokio::spawn(async move { store.check_fenced_fanout_exact(&room, &fence).await })
        };
        let mam_task = {
            let room = room.clone();
            let fence = fence.clone();
            tokio::spawn(async move { mam.store_message_fenced(&room, &message, &fence).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(175)).await;
        assert!(
            !muc_task.is_finished(),
            "MUC fence must wait on the node lock"
        );
        assert!(
            !mam_task.is_finished(),
            "MAM fence must wait on the node lock"
        );
        blocker.commit().await.expect("release node lock");

        assert!(!muc_task
            .await
            .expect("join MUC fence")
            .expect("MUC fence result"));
        assert!(matches!(
            mam_task.await.expect("join MAM fence"),
            Err(MamStorageError::NotOwner { .. })
        ));
        let mam = SqlxMamStorage::open(db.database_url())
            .await
            .expect("MAM readback");
        assert!(mam
            .get_message(&archive_id)
            .await
            .expect("read rejected message")
            .is_none());
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 1 (council-adjudicated): the MAM
    /// fenced-archive-write backstop. Exercises the full path this fix
    /// adds — `current_claim_fence` resolving the same typed context
    /// `check_fenced_fanout` uses, `SqlxMamStorage::store_message_fenced`
    /// running the fencing `SELECT ... FOR SHARE` inside the same
    /// transaction as the archive insert, and the deposed owner's very
    /// next fenced write observing `NotOwner` rather than silently
    /// archiving under a claim it no longer holds.
    #[tokio::test]
    async fn mam_store_message_fenced_blocks_the_deposed_owners_next_archive_write() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let jid = room_jid("mam-fenced");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let me = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        store.record_claim_epoch(&jid, epoch);

        let mam_storage = SqlxMamStorage::open(db.database_url())
            .await
            .expect("open mam storage against the same postgres database")
            .with_cluster_fencing(true);
        // This test's own room JID is unique per run (`room_jid` embeds no
        // randomness itself, but the shared Postgres instance persists
        // `mam_messages` rows across test runs within the same process —
        // unlike `clean_store()`'s clustering tables, nothing here wipes
        // MAM rows). Scope every assertion to a fresh archive id (UUID) so
        // repeated runs against the same long-lived database never
        // collide on a fixed literal id.
        let first_id = uuid::Uuid::new_v4().to_string();
        let second_id = uuid::Uuid::new_v4().to_string();

        let fence = store
            .current_claim_fence(&jid)
            .expect("current_claim_fence must resolve immediately after record_claim_epoch");
        assert_eq!(fence.entity, entity);
        assert_eq!(fence.epoch, epoch);
        assert_eq!(fence.owner, me);

        let message = ArchivedMessage {
            id: first_id.clone(),
            body: Some("hello, fenced world".to_string()),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{jid}/alice").parse().expect("valid full jid"),
                jid::Jid::from(jid.clone()),
            )
        };
        let archive_id = mam_storage
            .store_message_fenced(&jid, &message, &fence)
            .await
            .expect("the current owner's fenced write must succeed");
        assert_eq!(archive_id, first_id);
        let stored = mam_storage
            .get_message(&archive_id)
            .await
            .expect("get_message")
            .expect("row exists");
        assert_eq!(stored.body.as_deref(), Some("hello, fenced world"));

        // Steal the claim exactly like `check_fenced_fanout`'s own test: the
        // old owner incarnation is expired and the stealer is explicitly
        // live.
        expire_node(&db, &me).await;
        let stealer = live_stealer(&db).await;
        claim_store
            .steal_stale(&entity, epoch, StalePredicate::OwnerStale, &stealer)
            .await
            .expect("steal succeeds against a dead-owner claim");

        let second_message = ArchivedMessage {
            id: second_id.clone(),
            body: Some("should never be archived".to_string()),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{jid}/alice").parse().expect("valid full jid"),
                jid::Jid::from(jid.clone()),
            )
        };
        // The deposed owner's cached `fence` still carries the now-stolen
        // epoch — exactly the "believed was current" scenario the fencing
        // check exists to catch.
        let result = mam_storage
            .store_message_fenced(&jid, &second_message, &fence)
            .await;
        assert!(
            matches!(result, Err(MamStorageError::NotOwner { .. })),
            "the deposed owner's next fenced archive write must be rejected, got: {result:?}"
        );
        // And the message must genuinely not have landed in the archive.
        let should_be_absent = mam_storage
            .get_message(&second_id)
            .await
            .expect("get_message");
        assert!(
            should_be_absent.is_none(),
            "a rejected fenced write must not have committed any row"
        );
    }

    #[tokio::test]
    async fn xep0424_and_xep0425_fenced_writes_are_atomic_after_owner_expiry() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let room = room_jid("xep042x-expired-owner");
        let entity = Entity::new(EntityType::RoomActor, room.to_string());
        let owner = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &owner)
            .await
            .expect("claim room");
        store.record_claim_epoch(&room, epoch);
        let fence = store.current_claim_fence(&room).expect("fence");
        let mam = SqlxMamStorage::open(db.database_url())
            .await
            .expect("MAM")
            .with_cluster_fencing(true);

        let target_id = uuid::Uuid::new_v4().to_string();
        let target = ArchivedMessage {
            id: target_id.clone(),
            body: Some("secret".to_string()),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            nickname_generation: Some(1),
            ..ArchivedMessage::for_test(
                format!("{room}/alice").parse().expect("occupant"),
                jid::Jid::from(room.clone()),
            )
        };
        mam.store_message_fenced(&room, &target, &fence)
            .await
            .expect("seed target");
        let retraction_id = uuid::Uuid::new_v4().to_string();
        let retraction_from: jid::Jid = format!("{room}/alice").parse().expect("occupant");
        let retraction = retraction_row(&room, &retraction_id, &target_id, 1);
        mam.store_message_fenced(&room, &retraction, &fence)
            .await
            .expect("archive retraction event first");

        expire_node(&db, &owner).await;
        let xep0424 = mam
            .replace_with_tombstone_fenced(
                &room,
                &target_id,
                &retraction_id,
                &retraction_from,
                retraction_tombstone(&retraction_id),
                &fence,
            )
            .await;
        assert!(matches!(xep0424, Err(MamStorageError::NotOwner { .. })));

        let moderation_id = uuid::Uuid::new_v4().to_string();
        let moderation = moderation_row(&room, &moderation_id, &target_id);
        let xep0425 = mam
            .moderate_message_fenced(
                &room,
                &moderation,
                &target_id,
                moderation_tombstone(&moderation),
                &fence,
            )
            .await;
        assert!(matches!(xep0425, Err(MamStorageError::NotOwner { .. })));
        assert!(mam
            .get_message(&moderation_id)
            .await
            .expect("read moderation")
            .is_none());
        assert_eq!(
            mam.get_message(&target_id)
                .await
                .expect("read target")
                .expect("target exists")
                .body
                .as_deref(),
            Some("secret"),
            "neither failed transaction may tombstone the target"
        );
    }

    #[tokio::test]
    async fn xep0425_draining_exact_owner_commits_event_and_tombstone_together() {
        use waddle_xmpp::mam::{MamStorage, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let room = room_jid("xep0425-draining-owner");
        let entity = Entity::new(EntityType::RoomActor, room.to_string());
        let owner = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(&entity, &owner)
            .await
            .expect("claim room");
        store.record_claim_epoch(&room, epoch);
        let fence = store.current_claim_fence(&room).expect("fence");
        let mam = SqlxMamStorage::open(db.database_url())
            .await
            .expect("MAM")
            .with_cluster_fencing(true);
        let target_id = uuid::Uuid::new_v4().to_string();
        let target = ArchivedMessage {
            id: target_id.clone(),
            body: Some("moderate me".to_string()),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{room}/alice").parse().expect("occupant"),
                jid::Jid::from(room.clone()),
            )
        };
        mam.store_message_fenced(&room, &target, &fence)
            .await
            .expect("seed target");
        claim_store
            .mark_draining(&owner)
            .await
            .expect("mark draining");

        let moderation_id = uuid::Uuid::new_v4().to_string();
        let moderation = moderation_row(&room, &moderation_id, &target_id);
        assert!(mam
            .moderate_message_fenced(
                &room,
                &moderation,
                &target_id,
                moderation_tombstone(&moderation),
                &fence,
            )
            .await
            .expect("draining exact owner may finish"));
        assert!(mam
            .get_message(&moderation_id)
            .await
            .expect("read moderation")
            .is_some());
        assert!(mam
            .get_message(&target_id)
            .await
            .expect("read target")
            .expect("target")
            .body
            .is_none());
    }

    #[tokio::test]
    async fn xep0424_and_xep0425_atomic_boundaries_reject_mismatched_typed_proofs() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db)) = clean_store().await else {
            return;
        };
        let room = room_jid("xep042x-proof-binding");
        let owner = store.node_identity.current();
        let epoch = claim_store
            .ensure_claimed(
                &Entity::new(EntityType::RoomActor, room.to_string()),
                &owner,
            )
            .await
            .expect("claim room");
        store.record_claim_epoch(&room, epoch);
        let fence = store.current_claim_fence(&room).expect("fence");
        let mam = SqlxMamStorage::open(db.database_url())
            .await
            .expect("MAM")
            .with_cluster_fencing(true);
        let target_id = uuid::Uuid::new_v4().to_string();
        let target = ArchivedMessage {
            id: target_id.clone(),
            body: Some("must remain".to_string()),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            nickname_generation: Some(7),
            ..ArchivedMessage::for_test(
                format!("{room}/alice").parse().expect("occupant"),
                jid::Jid::from(room.clone()),
            )
        };
        mam.store_message_fenced(&room, &target, &fence)
            .await
            .expect("seed target");

        let mismatched_retraction_id = uuid::Uuid::new_v4().to_string();
        let mismatched_retraction = retraction_row(
            &room,
            &mismatched_retraction_id,
            &uuid::Uuid::new_v4().to_string(),
            7,
        );
        mam.store_message_fenced(&room, &mismatched_retraction, &fence)
            .await
            .expect("store mismatched retraction");
        let from: jid::Jid = format!("{room}/alice").parse().expect("occupant");
        assert!(!mam
            .replace_with_tombstone_fenced(
                &room,
                &target_id,
                &mismatched_retraction_id,
                &from,
                retraction_tombstone(&mismatched_retraction_id),
                &fence,
            )
            .await
            .expect("typed mismatch is a rejected proof"));

        let reused_nick_retraction_id = uuid::Uuid::new_v4().to_string();
        let reused_nick_retraction =
            retraction_row(&room, &reused_nick_retraction_id, &target_id, 8);
        mam.store_message_fenced(&room, &reused_nick_retraction, &fence)
            .await
            .expect("store later nickname generation");
        assert!(!mam
            .replace_with_tombstone_fenced(
                &room,
                &target_id,
                &reused_nick_retraction_id,
                &from,
                retraction_tombstone(&reused_nick_retraction_id),
                &fence,
            )
            .await
            .expect("generation mismatch is a rejected proof"));

        let moderation_id = uuid::Uuid::new_v4().to_string();
        let moderation = moderation_row(&room, &moderation_id, &target_id);
        let mut archive_id_as_wire_id = moderation_tombstone(&moderation);
        archive_id_as_wire_id.retraction_id =
            waddle_xmpp_core::mam::RichMessageId::new(moderation.id.clone());
        let wrong_wire_result = mam
            .moderate_message_fenced(
                &room,
                &moderation,
                &target_id,
                archive_id_as_wire_id,
                &fence,
            )
            .await;
        assert!(matches!(
            wrong_wire_result,
            Err(MamStorageError::Serialization(_))
        ));
        assert!(mam
            .get_message(&moderation_id)
            .await
            .expect("wrong-wire moderation lookup")
            .is_none());

        let mut mismatched_tombstone = moderation_tombstone(&moderation);
        mismatched_tombstone
            .moderation
            .as_mut()
            .expect("moderation")
            .target_id =
            waddle_xmpp_core::mam::RichMessageId::new(uuid::Uuid::new_v4().to_string())
                .expect("id");
        let moderation_result = mam
            .moderate_message_fenced(&room, &moderation, &target_id, mismatched_tombstone, &fence)
            .await;
        assert!(matches!(
            moderation_result,
            Err(MamStorageError::Serialization(_))
        ));
        assert!(mam
            .get_message(&moderation_id)
            .await
            .expect("moderation lookup")
            .is_none());
        assert_eq!(
            mam.get_message(&target_id)
                .await
                .expect("target lookup")
                .expect("target")
                .body
                .as_deref(),
            Some("must remain")
        );
    }
}
