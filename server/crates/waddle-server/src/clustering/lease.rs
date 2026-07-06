//! Keypair-slot Postgres CAS lease (ADR-0017 element 3/4).
//!
//! A pod leases exactly one slot of the pre-enrolled keypair pool via a
//! server-time (`now()`) compare-and-set, so at most one live node ever holds
//! a given slot — and therefore no two swarm members share a libp2p `PeerId`.
//! The lease is heartbeat-renewed; a renewal that finds its slot stolen or
//! expired is **fencing loss** and the node must stop using that identity.
//!
//! Postgres-only: the control plane has no SQLite equivalent (`now()`-based
//! row CAS), and clustering requires Postgres. The store goes through the
//! portable `Database` (which rewrites `?`→`$n` for Postgres and leaves
//! `now()`/interval SQL intact); it is only ever invoked on a Postgres
//! deployment.
//!
//! The dedicated control-plane connection pool the ADR prescribes (element 4/12)
//! lands with the Phase 3 ownership control plane; Phase 2's only control-plane
//! traffic is this slot heartbeat.

use super::NodeId;
use crate::db::{Database, DatabaseError};
use async_trait::async_trait;
use std::time::Duration;

/// Per-process leaseholder identity. `node_id`/`node_epoch` are freshly
/// generated on every process start and never reused, so a restarted pod never
/// looks like its former self holding a slot. Both are typed values
/// (typed-payloads rule); serialization to TEXT happens only at the SQL
/// parameter boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseIdentity {
    pub node_id: NodeId,
    pub node_epoch: uuid::Uuid,
}

/// A successfully leased keypair-pool slot (index into the enrolled pool).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeasedSlot {
    pub slot_index: u32,
}

/// Keypair-slot lease failures.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// The backing database returned an error.
    #[error("clustering keypair-slot lease database error: {0}")]
    Database(#[from] DatabaseError),
    /// Every pool slot is currently held by a live node.
    #[error(
        "no free clustering keypair-slot could be leased: all {pool_size} enrolled pool slots \
         are held by live nodes (scale-up beyond the enrolled pool needs a pipeline enrollment run)"
    )]
    NoFreeSlot { pool_size: usize },
    /// A heartbeat renewal found the slot no longer held by this node
    /// (stolen after lease expiry, or epoch superseded).
    #[error("clustering keypair-slot {slot_index} lease lost (fencing): slot no longer held by this node")]
    FencingLoss { slot_index: u32 },
    /// `acquire` was called with an empty pool — a configuration error the
    /// caller should have screened out.
    #[error("clustering keypair-slot lease requested with an empty keypair pool")]
    EmptyPool,
}

/// The keypair-slot lease store. A trait so a dedicated control-plane pool
/// implementation (Phase 3) or a test double can substitute without touching
/// callers.
#[async_trait]
pub trait KeypairSlotLease: Send + Sync {
    /// Create the backing table if it does not exist.
    async fn ensure_schema(&self) -> Result<(), LeaseError>;

    /// Lease any free or expired slot in `[0, pool_size)`. Returns the leased
    /// slot, or [`LeaseError::NoFreeSlot`] if all are held by live nodes.
    async fn acquire(
        &self,
        identity: &LeaseIdentity,
        pool_size: usize,
        lease_ttl: Duration,
    ) -> Result<LeasedSlot, LeaseError>;

    /// Renew the lease on a held slot. Returns [`LeaseError::FencingLoss`] when
    /// the freshness-gated CAS affects zero rows (lease lapsed or stolen).
    async fn heartbeat(
        &self,
        identity: &LeaseIdentity,
        slot: LeasedSlot,
        lease_ttl: Duration,
    ) -> Result<(), LeaseError>;

    /// Release a held slot on graceful drain (epoch-gated, best-effort).
    async fn release(&self, identity: &LeaseIdentity, slot: LeasedSlot) -> Result<(), LeaseError>;
}

/// Postgres implementation using `now()`-based row CAS.
pub struct PostgresKeypairSlotLease {
    db: Database,
}

impl PostgresKeypairSlotLease {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Try to claim one specific slot. Returns `true` on success (row inserted
    /// or a stale/free row stolen), `false` if the slot is held fresh.
    async fn try_acquire_slot(
        &self,
        identity: &LeaseIdentity,
        slot_index: u32,
        lease_ttl: Duration,
    ) -> Result<bool, LeaseError> {
        let conn = self.db.guard().await?;
        // Upsert CAS: insert a fresh hold, or steal an existing row only when
        // it is unheld, already expired, or its heartbeat has aged past the
        // TTL. A held-and-fresh slot fails the ON CONFLICT WHERE and affects
        // zero rows. Postgres row-locks the conflicting row, so two racing
        // acquirers of the same slot serialize and exactly one wins.
        let affected = conn
            .execute(
                r#"
                INSERT INTO clustering_keypair_slots
                    (slot_index, holder_node, holder_epoch, heartbeat, expired)
                VALUES (?, ?, ?, now(), false)
                ON CONFLICT (slot_index) DO UPDATE SET
                    holder_node = EXCLUDED.holder_node,
                    holder_epoch = EXCLUDED.holder_epoch,
                    heartbeat = now(),
                    expired = false
                WHERE clustering_keypair_slots.holder_node IS NULL
                   OR clustering_keypair_slots.expired
                   OR clustering_keypair_slots.heartbeat < now() - (? || ' milliseconds')::interval
                "#,
                // Storage boundary: typed identity serializes to TEXT here.
                crate::db_params![
                    i64::from(slot_index),
                    identity.node_id.as_str().to_string(),
                    identity.node_epoch.to_string(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await?;
        Ok(affected == 1)
    }
}

#[async_trait]
impl KeypairSlotLease for PostgresKeypairSlotLease {
    async fn ensure_schema(&self) -> Result<(), LeaseError> {
        let conn = self.db.guard().await?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_keypair_slots (
                slot_index   INTEGER PRIMARY KEY,
                holder_node  TEXT,
                holder_epoch TEXT,
                heartbeat    TIMESTAMPTZ NOT NULL DEFAULT now(),
                expired      BOOLEAN NOT NULL DEFAULT TRUE
            )
            "#,
            (),
        )
        .await?;
        Ok(())
    }

    async fn acquire(
        &self,
        identity: &LeaseIdentity,
        pool_size: usize,
        lease_ttl: Duration,
    ) -> Result<LeasedSlot, LeaseError> {
        if pool_size == 0 {
            return Err(LeaseError::EmptyPool);
        }
        // Scan slots, starting at an offset derived from this node's id so
        // concurrently-starting pods don't all contend on slot 0 first. The
        // CAS guarantees correctness regardless of scan order.
        let start = (stable_offset(identity.node_id.as_str()) % pool_size) as u32;
        let pool = pool_size as u32;
        for step in 0..pool {
            let slot_index = (start + step) % pool;
            if self
                .try_acquire_slot(identity, slot_index, lease_ttl)
                .await?
            {
                return Ok(LeasedSlot { slot_index });
            }
        }
        Err(LeaseError::NoFreeSlot { pool_size })
    }

    async fn heartbeat(
        &self,
        identity: &LeaseIdentity,
        slot: LeasedSlot,
        lease_ttl: Duration,
    ) -> Result<(), LeaseError> {
        let conn = self.db.guard().await?;
        // Freshness-gated renewal CAS (ADR element 4): renew only while we
        // still hold the slot under our epoch and the lease has not lapsed.
        let affected = conn
            .execute(
                r#"
                UPDATE clustering_keypair_slots
                SET heartbeat = now()
                WHERE slot_index = ?
                  AND holder_node = ?
                  AND holder_epoch = ?
                  AND NOT expired
                  AND heartbeat >= now() - (? || ' milliseconds')::interval
                "#,
                crate::db_params![
                    i64::from(slot.slot_index),
                    identity.node_id.as_str().to_string(),
                    identity.node_epoch.to_string(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await?;
        if affected == 0 {
            return Err(LeaseError::FencingLoss {
                slot_index: slot.slot_index,
            });
        }
        Ok(())
    }

    async fn release(&self, identity: &LeaseIdentity, slot: LeasedSlot) -> Result<(), LeaseError> {
        let conn = self.db.guard().await?;
        // Epoch-gated release: mark the slot free+expired so a replacement can
        // acquire it immediately, but only if we still own it.
        conn.execute(
            r#"
            UPDATE clustering_keypair_slots
            SET holder_node = NULL, holder_epoch = NULL, expired = true
            WHERE slot_index = ? AND holder_node = ? AND holder_epoch = ?
            "#,
            crate::db_params![
                i64::from(slot.slot_index),
                identity.node_id.as_str().to_string(),
                identity.node_epoch.to_string(),
            ],
        )
        .await?;
        Ok(())
    }
}

/// A cheap, stable, non-cryptographic offset from a node id, so pods spread
/// their initial slot scan instead of all starting at 0.
fn stable_offset(node_id: &str) -> usize {
    // FNV-1a over the bytes — deterministic and dependency-free.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in node_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabaseDriver};
    use std::sync::OnceLock;
    use std::time::Duration;

    // These tests share one `clustering_keypair_slots` table, so serialize
    // them and wipe the table at each start. They are skipped unless
    // `WADDLE_TEST_POSTGRES_URL` points at a Postgres (the control-plane CAS
    // has no SQLite equivalent). CI runs them under the clustering feature +
    // the Nix-spawned Postgres.
    fn serial_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    fn ident() -> LeaseIdentity {
        LeaseIdentity {
            node_id: NodeId::generate(),
            node_epoch: uuid::Uuid::new_v4(),
        }
    }

    async fn clean_store() -> Option<PostgresKeypairSlotLease> {
        let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
        let db = Database::from_config(
            "clustering-lease-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url),
        )
        .await
        .expect("open test postgres");
        let store = PostgresKeypairSlotLease::new(db);
        store.ensure_schema().await.expect("ensure schema");
        store
            .db
            .guard()
            .await
            .expect("guard")
            .execute("DELETE FROM clustering_keypair_slots", ())
            .await
            .expect("clean slots");
        Some(store)
    }

    const TTL: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn two_nodes_lease_distinct_slots() {
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        let slot_a = store.acquire(&a, 2, TTL).await.expect("a acquires");
        let slot_b = store.acquire(&b, 2, TTL).await.expect("b acquires");
        assert_ne!(slot_a.slot_index, slot_b.slot_index);
    }

    #[tokio::test]
    async fn single_slot_pool_admits_exactly_one() {
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        store.acquire(&a, 1, TTL).await.expect("a acquires slot 0");
        let err = store.acquire(&b, 1, TTL).await.expect_err("b is refused");
        assert!(matches!(err, LeaseError::NoFreeSlot { pool_size: 1 }));
    }

    #[tokio::test]
    async fn heartbeat_renews_and_release_frees() {
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        let slot = store.acquire(&a, 1, TTL).await.expect("a acquires");
        store.heartbeat(&a, slot, TTL).await.expect("a renews");
        // While A holds it, B cannot take it.
        assert!(store.acquire(&b, 1, TTL).await.is_err());
        // After A releases, B acquires immediately.
        store.release(&a, slot).await.expect("a releases");
        let slot_b = store.acquire(&b, 1, TTL).await.expect("b acquires freed");
        assert_eq!(slot_b.slot_index, slot.slot_index);
    }

    #[tokio::test]
    async fn expired_slot_is_stolen_and_old_holder_fences() {
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        let short_ttl = Duration::from_secs(1);
        let slot = store.acquire(&a, 1, short_ttl).await.expect("a acquires");
        // Let the lease lapse, then B steals the expired slot.
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        let slot_b = store
            .acquire(&b, 1, short_ttl)
            .await
            .expect("b steals expired slot");
        assert_eq!(slot_b.slot_index, slot.slot_index);
        // A's renewal now fails — fencing loss.
        let err = store
            .heartbeat(&a, slot, short_ttl)
            .await
            .expect_err("a fences");
        assert!(matches!(err, LeaseError::FencingLoss { .. }));
    }

    #[tokio::test]
    async fn sub_second_ttl_uses_millisecond_precision() {
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        // A 300ms TTL must NOT floor to '0 seconds' (which would make every
        // slot instantly stealable and every renewal fence).
        let ttl = Duration::from_millis(300);
        let slot = store.acquire(&a, 1, ttl).await.expect("a acquires");
        // Immediately renewable within the sub-second window, and the fresh
        // slot is not stealable by B.
        store
            .heartbeat(&a, slot, ttl)
            .await
            .expect("a renews in-window");
        assert!(store.acquire(&b, 1, ttl).await.is_err());
        // After the window lapses, B steals it.
        tokio::time::sleep(Duration::from_millis(450)).await;
        let slot_b = store.acquire(&b, 1, ttl).await.expect("b steals expired");
        assert_eq!(slot_b.slot_index, slot.slot_index);
    }

    #[test]
    fn stable_offset_is_deterministic() {
        assert_eq!(stable_offset("node-a"), stable_offset("node-a"));
    }
}
