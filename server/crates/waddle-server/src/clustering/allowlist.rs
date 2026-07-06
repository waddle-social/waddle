//! Peer allowlist for the clustering swarm (ADR-0017 element 3).
//!
//! The cluster maintains an allowlist of **enrolled peer IDs** in Postgres.
//! Connections from peers not on the list are rejected at the swarm behaviour
//! layer (`libp2p::allow_block_list` composed into `WaddleBehaviour`) —
//! completing the Noise handshake is necessary but never sufficient. A
//! periodic refresh diffs the enrolled set and **actively closes live
//! connections** whose peer ID is no longer enrolled, bounding a revoked
//! peer's swarm access to one refresh interval.
//!
//! Enrollment authority is separate from the runtime by design: rows are
//! provisioned only by the deployment pipeline (an admin-role migration/Helm
//! hook job — Phase 4 ships the grants making the runtime role `SELECT`-only
//! on this table). The runtime never inserts or deletes allowlist rows; it
//! only reads them. Tests seed rows directly, playing the enrollment
//! authority's part.
//!
//! Two operational rules fall out of enforcement being symmetric (each side
//! checks the **remote** peer against its own copy of the same table):
//! - **Every node's PeerId must be enrolled** — including the dialer. A ↔ B
//!   connects only if both A and B are on the list. In practice enrollment
//!   means enrolling the PeerIds derived from the pre-enrolled keypair pool;
//!   ephemeral identities (empty pool) mint a fresh PeerId per process that
//!   can never be pre-enrolled, so ephemeral-identity nodes **cannot form a
//!   cluster** — that fallback only serves single-node spikes and tests.
//! - **The containment bound is approximate**: worst case is ~2× the refresh
//!   interval plus query latency (single-flight refresh + skipped ticks), and
//!   a Postgres outage freezes revocation at the last-known set for the
//!   outage's duration — the deliberate fail-closed trade-off (never fail
//!   open, never mass-revoke on a transient error).

use crate::db::{Database, DatabaseError};
use async_trait::async_trait;
use libp2p::PeerId;
use std::collections::HashSet;
use std::str::FromStr;

/// Allowlist read failures.
#[derive(Debug, thiserror::Error)]
pub enum AllowlistError {
    /// The backing database returned an error.
    #[error("clustering peer-allowlist database error: {0}")]
    Database(#[from] DatabaseError),
    /// The table has rows but not one parsed as a valid PeerId. Distinguished
    /// from a genuinely empty table (deny-all) so a systematic
    /// encoding/driver regression fails the refresh — keeping the last-known
    /// set — instead of silently mass-revoking the whole cluster.
    #[error(
        "clustering peer-allowlist has {rows} rows but none parsed as a PeerId; \
         refusing to apply an empty set from a populated table"
    )]
    AllRowsInvalid { rows: usize },
}

/// Read-only view of the enrolled peer set. A trait so a dedicated
/// control-plane pool implementation (Phase 3) or a test double can
/// substitute without touching callers.
#[async_trait]
pub trait AllowlistStore: Send + Sync {
    /// Create the backing table if it does not exist (idempotent; inserts
    /// nothing — enrollment stays with the pipeline authority).
    ///
    /// Phase-4 note: `CREATE TABLE IF NOT EXISTS` still requires `CREATE` on
    /// the schema even when the table already exists (Postgres checks the ACL
    /// before the existence short-circuit), so when the SELECT-only runtime
    /// grants land, the pipeline must provision this table and this call must
    /// be dropped or gated — otherwise startup fails on the hardened role.
    async fn ensure_schema(&self) -> Result<(), AllowlistError>;

    /// The currently enrolled peer IDs. Rows that fail to parse as a libp2p
    /// `PeerId` are skipped with a warning (a malformed enrollment must not
    /// take down the reader).
    async fn enrolled_peers(&self) -> Result<HashSet<PeerId>, AllowlistError>;
}

/// Postgres implementation.
pub struct PostgresAllowlistStore {
    db: Database,
}

impl PostgresAllowlistStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AllowlistStore for PostgresAllowlistStore {
    async fn ensure_schema(&self) -> Result<(), AllowlistError> {
        let conn = self.db.guard().await?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_peer_allowlist (
                peer_id     TEXT PRIMARY KEY,
                enrolled_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
            (),
        )
        .await?;
        Ok(())
    }

    async fn enrolled_peers(&self) -> Result<HashSet<PeerId>, AllowlistError> {
        let conn = self.db.guard().await?;
        let mut rows = conn
            .query("SELECT peer_id FROM clustering_peer_allowlist", ())
            .await?;
        let mut peers = HashSet::new();
        let mut total_rows = 0usize;
        while let Some(row) = rows.next().await? {
            total_rows += 1;
            let raw: String = row.get(0)?;
            match PeerId::from_str(&raw) {
                Ok(peer) => {
                    peers.insert(peer);
                }
                Err(error) => {
                    tracing::warn!(
                        peer_id = %raw,
                        %error,
                        "clustering allowlist row is not a valid PeerId; skipping"
                    );
                }
            }
        }
        // A populated table where *nothing* parsed is a systematic failure,
        // not an enrollment decision: fail the read (the refresh keeps the
        // last-known set) rather than mass-revoke every peer.
        if peers.is_empty() && total_rows > 0 {
            return Err(AllowlistError::AllRowsInvalid { rows: total_rows });
        }
        Ok(peers)
    }
}

/// The difference between two enrolled sets: peers to newly allow and peers
/// to revoke (disallow + close live connections).
#[derive(Debug, PartialEq, Eq)]
pub struct AllowlistDiff {
    pub added: Vec<PeerId>,
    pub removed: Vec<PeerId>,
}

/// Compute the changes needed to move the swarm's allowed set from `current`
/// to `next`.
pub fn diff_allowlist(current: &HashSet<PeerId>, next: &HashSet<PeerId>) -> AllowlistDiff {
    let mut added: Vec<PeerId> = next.difference(current).copied().collect();
    let mut removed: Vec<PeerId> = current.difference(next).copied().collect();
    // `HashSet` iteration order is non-deterministic; sort so the diff (and
    // anything logging or asserting on it) is stable across runs.
    added.sort_unstable();
    removed.sort_unstable();
    AllowlistDiff { added, removed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabaseDriver};

    fn peer() -> PeerId {
        PeerId::random()
    }

    #[test]
    fn diff_reports_added_and_removed() {
        let (a, b, c) = (peer(), peer(), peer());
        let current: HashSet<PeerId> = [a, b].into_iter().collect();
        let next: HashSet<PeerId> = [b, c].into_iter().collect();
        let diff = diff_allowlist(&current, &next);
        assert_eq!(diff.added, vec![c]);
        assert_eq!(diff.removed, vec![a]);
    }

    #[test]
    fn diff_output_is_deterministically_ordered() {
        let peers: Vec<PeerId> = (0..8).map(|_| peer()).collect();
        let next: HashSet<PeerId> = peers.iter().copied().collect();
        let diff = diff_allowlist(&HashSet::new(), &next);
        let mut expected = peers;
        expected.sort_unstable();
        assert_eq!(diff.added, expected);
    }

    #[test]
    fn diff_of_identical_sets_is_empty() {
        let set: HashSet<PeerId> = [peer(), peer()].into_iter().collect();
        let diff = diff_allowlist(&set, &set.clone());
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
    }

    // Postgres-gated: seeds rows directly (playing the enrollment pipeline's
    // part — the runtime itself never writes this table).
    #[tokio::test]
    async fn enrolled_peers_reads_rows_and_skips_invalid() {
        let _guard = crate::clustering::allowlist_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            return;
        };
        let db = Database::from_config(
            "clustering-allowlist-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url),
        )
        .await
        .expect("open test postgres");
        let store = PostgresAllowlistStore::new(db.clone());
        store.ensure_schema().await.expect("ensure schema");

        let conn = db.guard().await.expect("guard");
        conn.execute("DELETE FROM clustering_peer_allowlist", ())
            .await
            .expect("clean table");
        let (a, b) = (peer(), peer());
        for enrolled in [a.to_string(), b.to_string(), "not-a-peer-id".to_string()] {
            conn.execute(
                "INSERT INTO clustering_peer_allowlist (peer_id) VALUES (?)",
                crate::db_params![enrolled],
            )
            .await
            .expect("enroll row");
        }

        let peers = store.enrolled_peers().await.expect("read enrolled");
        assert_eq!(peers.len(), 2);
        assert!(peers.contains(&a) && peers.contains(&b));
    }

    // A populated table where nothing parses must fail the read (the refresh
    // keeps the last-known set) rather than yield an empty set that would
    // mass-revoke the whole cluster.
    #[tokio::test]
    async fn all_rows_invalid_is_an_error_not_an_empty_set() {
        let _guard = crate::clustering::allowlist_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            return;
        };
        let db = Database::from_config(
            "clustering-allowlist-test-invalid",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url),
        )
        .await
        .expect("open test postgres");
        let store = PostgresAllowlistStore::new(db.clone());
        store.ensure_schema().await.expect("ensure schema");

        let conn = db.guard().await.expect("guard");
        conn.execute("DELETE FROM clustering_peer_allowlist", ())
            .await
            .expect("clean table");
        conn.execute(
            "INSERT INTO clustering_peer_allowlist (peer_id) VALUES (?)",
            crate::db_params!["definitely-not-a-peer-id".to_string()],
        )
        .await
        .expect("seed invalid row");

        let err = store.enrolled_peers().await.expect_err("read must fail");
        assert!(matches!(err, AllowlistError::AllRowsInvalid { rows: 1 }));

        // A genuinely empty table is still Ok(empty) — deny-all, not an error.
        conn.execute("DELETE FROM clustering_peer_allowlist", ())
            .await
            .expect("empty table");
        let peers = store.enrolled_peers().await.expect("empty read succeeds");
        assert!(peers.is_empty());
    }
}
