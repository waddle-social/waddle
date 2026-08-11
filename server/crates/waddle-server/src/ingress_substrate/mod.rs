//! Dark PostgreSQL storage for ingress identity (#1653).
//!
//! This substrate is consumed by tests now and by the #1654 repositories
//! later.  It deliberately has no production caller in this foundation slice.

use chrono::{DateTime, Duration, Utc};
use jid::BareJid;
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::ingress::{
    resolve_alias, AliasResolution, DeliveryKey, IngressOrdinal, IngressStreamId, MessageKey,
    NormalizedTarget, SemanticDigest, StoredAlias,
};
use waddle_xmpp_core::xep0359::OriginId;

use crate::db::{Database, DatabaseDriver, DatabaseError, Row, Transaction};

/// The time an origin-id alias remains available after its message becomes
/// terminal.  The garbage collector receives `now` and binds the derived
/// cutoff, keeping wall-clock decisions at its caller boundary.
pub const ALIAS_RETENTION: Duration = Duration::days(8);

/// Ingress tables protected by the epoch-proof triggers from migration V1009.
///
/// Keep this list in lock-step with the migration manifest: tests query the
/// live catalog to ensure a newly-added ingress table cannot accidentally be
/// left outside the activation boundary.
pub const EPOCH_GUARDED_TABLES: [&str; 4] = [
    "ingress_messages",
    "ingress_origin_aliases",
    "ingress_sm_refs",
    "ingress_deliveries",
];

/// Fail-closed errors for the dark ingress substrate.
///
/// The database adapter error is intentionally not retained as a source:
/// database diagnostics can include SQL values, while an origin-id is an
/// opaque client value that must never appear in Debug or Display output.
#[derive(Debug, Error)]
pub enum IngressSubstrateError {
    #[error("ingress substrate requires PostgreSQL")]
    PostgresRequired,
    #[error("ingress substrate database operation failed")]
    Database,
    #[error("ingress substrate returned a malformed stored message key")]
    InvalidStoredMessageKey,
    #[error("ingress substrate returned a malformed semantic digest")]
    InvalidStoredDigest,
    #[error("ingress alias disappeared during concurrent resolution")]
    AliasMissingAfterConflict,
}

/// Outcome of adding a child identity that requires a live message row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageWriteOutcome {
    Recorded,
    MessageVanished,
}

/// Outcome of recording a terminal proof for a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalizeOutcome {
    Terminalized,
    AlreadyTerminal,
    MessageVanished,
}

/// Work completed by one alias garbage-collection pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AliasGcOutcome {
    pub deleted_messages: usize,
}

/// Typed handle for the PostgreSQL-only ingress substrate.
///
/// The per-operation functions below accept a caller-owned transaction so a
/// future repository can compose ingress writes with its own atomic work.
#[derive(Clone)]
pub struct PostgresIngressSubstrate {
    db: Database,
}

impl PostgresIngressSubstrate {
    /// Open the dark substrate against the global PostgreSQL database.
    pub fn open(db: Database) -> Result<Self, IngressSubstrateError> {
        if db.driver() != DatabaseDriver::Postgres {
            return Err(IngressSubstrateError::PostgresRequired);
        }
        Ok(Self { db })
    }

    /// Start a transaction on the substrate's PostgreSQL pool.
    pub async fn begin(&self) -> Result<Transaction<'_>, IngressSubstrateError> {
        self.db.begin().await.map_err(discard_database_error)
    }

    pub async fn record_message(
        &self,
        tx: &mut Transaction<'_>,
        message_key: MessageKey,
        digest: &SemanticDigest,
    ) -> Result<(), IngressSubstrateError> {
        record_message(tx, message_key, digest).await
    }

    pub async fn resolve_and_record_alias(
        &self,
        tx: &mut Transaction<'_>,
        sender: &BareJid,
        target: &NormalizedTarget,
        origin_id: &OriginId,
        digest: &SemanticDigest,
        mint: impl FnOnce() -> MessageKey,
    ) -> Result<AliasResolution, IngressSubstrateError> {
        resolve_and_record_alias(tx, sender, target, origin_id, digest, mint).await
    }

    pub async fn insert_sm_ref(
        &self,
        tx: &mut Transaction<'_>,
        stream_id: IngressStreamId,
        ordinal: IngressOrdinal,
        message_key: MessageKey,
    ) -> Result<MessageWriteOutcome, IngressSubstrateError> {
        insert_sm_ref(tx, stream_id, ordinal, message_key).await
    }

    pub async fn record_delivery(
        &self,
        tx: &mut Transaction<'_>,
        delivery_key: DeliveryKey,
        message_key: MessageKey,
    ) -> Result<MessageWriteOutcome, IngressSubstrateError> {
        record_delivery(tx, delivery_key, message_key).await
    }

    pub async fn terminalize_message(
        &self,
        tx: &mut Transaction<'_>,
        message_key: MessageKey,
        proven_terminal_at: DateTime<Utc>,
    ) -> Result<TerminalizeOutcome, IngressSubstrateError> {
        terminalize_message(tx, message_key, proven_terminal_at).await
    }

    pub async fn gc_expired_aliases(
        &self,
        now: DateTime<Utc>,
    ) -> Result<AliasGcOutcome, IngressSubstrateError> {
        gc_expired_aliases(&self.db, now).await
    }
}

/// Insert a message's immutable semantic digest.
pub async fn record_message(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    digest: &SemanticDigest,
) -> Result<(), IngressSubstrateError> {
    let (digest_version, digest_bytes) = digest.to_storage();
    tx.execute(
        r#"
        INSERT INTO ingress_messages (message_key, digest_version, digest)
        VALUES (?::uuid, ?, ?)
        "#,
        crate::db_params![
            message_key.to_storage().to_string(),
            i32::from(digest_version),
            digest_bytes.to_vec(),
        ],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(())
}

/// Resolve and atomically persist one sender/target/origin-id alias.
///
/// An existing alias is read with its message digest in one joined locked
/// query.  On a concurrent first-insert loss, the unreferenced candidate is
/// removed before repeating that same query, so neither outcome observes an
/// impossible alias/digest split.
pub async fn resolve_and_record_alias(
    tx: &mut Transaction<'_>,
    sender: &BareJid,
    target: &NormalizedTarget,
    origin_id: &OriginId,
    digest: &SemanticDigest,
    mint: impl FnOnce() -> MessageKey,
) -> Result<AliasResolution, IngressSubstrateError> {
    let alias_key = AliasStorageKey::new(sender, target, origin_id);
    if let Some(stored) = locked_alias(tx, &alias_key).await? {
        return Ok(resolve_alias(true, digest, Some(&stored), mint));
    }

    let candidate = resolve_alias(true, digest, None, mint);
    let candidate_key = match candidate {
        AliasResolution::Aliased(waddle_xmpp::ingress::AliasOutcome::Inserted(key)) => key,
        _ => return Err(IngressSubstrateError::AliasMissingAfterConflict),
    };
    record_message(tx, candidate_key, digest).await?;
    let inserted = tx
        .execute(
            r#"
            INSERT INTO ingress_origin_aliases
                (sender_bare_jid, target_kind, target_jid, origin_id, message_key)
            VALUES (?, ?, ?, ?, ?::uuid)
            ON CONFLICT (sender_bare_jid, target_kind, target_jid, origin_id) DO NOTHING
            "#,
            crate::db_params![
                &alias_key.sender,
                alias_key.target_kind,
                &alias_key.target,
                &alias_key.origin_id,
                candidate_key.to_storage().to_string(),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    if inserted == 1 {
        return Ok(candidate);
    }

    tx.execute(
        "DELETE FROM ingress_messages WHERE message_key = ?::uuid",
        crate::db_params![candidate_key.to_storage().to_string()],
    )
    .await
    .map_err(discard_database_error)?;

    let stored = locked_alias(tx, &alias_key)
        .await?
        .ok_or(IngressSubstrateError::AliasMissingAfterConflict)?;
    Ok(resolve_alias(true, digest, Some(&stored), || candidate_key))
}

/// Attach a stream-management ordinal to a live message.
pub async fn insert_sm_ref(
    tx: &mut Transaction<'_>,
    stream_id: IngressStreamId,
    ordinal: IngressOrdinal,
    message_key: MessageKey,
) -> Result<MessageWriteOutcome, IngressSubstrateError> {
    if !lock_message_for_child(tx, message_key).await? {
        return Ok(MessageWriteOutcome::MessageVanished);
    }
    tx.execute(
        r#"
        INSERT INTO ingress_sm_refs (ingress_stream_id, ingress_ordinal, message_key)
        VALUES (?::uuid, ?::numeric, ?::uuid)
        "#,
        crate::db_params![
            stream_id.to_storage().to_string(),
            ordinal.to_storage().to_string(),
            message_key.to_storage().to_string(),
        ],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(MessageWriteOutcome::Recorded)
}

/// Record a delivery/effect identity for a live message.
pub async fn record_delivery(
    tx: &mut Transaction<'_>,
    delivery_key: DeliveryKey,
    message_key: MessageKey,
) -> Result<MessageWriteOutcome, IngressSubstrateError> {
    if !lock_message_for_child(tx, message_key).await? {
        return Ok(MessageWriteOutcome::MessageVanished);
    }
    tx.execute(
        r#"
        INSERT INTO ingress_deliveries (delivery_key, message_key)
        VALUES (?::uuid, ?::uuid)
        "#,
        crate::db_params![
            delivery_key.to_storage().to_string(),
            message_key.to_storage().to_string(),
        ],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(MessageWriteOutcome::Recorded)
}

/// Record a terminal proof once; later calls preserve the first proof time.
pub async fn terminalize_message(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    proven_terminal_at: DateTime<Utc>,
) -> Result<TerminalizeOutcome, IngressSubstrateError> {
    let changed = tx
        .execute(
            r#"
            UPDATE ingress_messages
            SET terminal_at = ?::timestamptz
            WHERE message_key = ?::uuid AND terminal_at IS NULL
            "#,
            crate::db_params![
                proven_terminal_at.to_rfc3339(),
                message_key.to_storage().to_string(),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    if changed == 1 {
        return Ok(TerminalizeOutcome::Terminalized);
    }
    if message_exists(tx, message_key).await? {
        Ok(TerminalizeOutcome::AlreadyTerminal)
    } else {
        Ok(TerminalizeOutcome::MessageVanished)
    }
}

/// Garbage collect terminal messages whose alias retention has elapsed.
///
/// Each candidate is locked before checking children.  That lock interlocks
/// with child writes and alias resolution, whose first statement on a live
/// message is `FOR SHARE`.
pub async fn gc_expired_aliases(
    db: &Database,
    now: DateTime<Utc>,
) -> Result<AliasGcOutcome, IngressSubstrateError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(IngressSubstrateError::PostgresRequired);
    }
    let cutoff = (now - ALIAS_RETENTION).to_rfc3339();
    let candidates = expired_candidates(db, &cutoff).await?;
    let mut deleted_messages = 0usize;

    for message_key in candidates {
        let mut tx = db.begin().await.map_err(discard_database_error)?;
        // First statement: Postgres's default READ COMMITTED mode is required
        // so every candidate lock/recheck sees the latest committed children.
        tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await
            .map_err(discard_database_error)?;
        if !lock_eligible_terminal_message(&mut tx, message_key, &cutoff).await? {
            tx.commit().await.map_err(discard_database_error)?;
            continue;
        }
        if has_live_non_alias_children(&mut tx, message_key).await? {
            tx.commit().await.map_err(discard_database_error)?;
            continue;
        }
        tx.execute(
            "DELETE FROM ingress_origin_aliases WHERE message_key = ?::uuid",
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
        let deleted = tx
            .execute(
                r#"
                DELETE FROM ingress_messages m
                WHERE m.message_key = ?::uuid
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_origin_aliases a WHERE a.message_key = m.message_key
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_sm_refs r WHERE r.message_key = m.message_key
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM ingress_deliveries d WHERE d.message_key = m.message_key
                  )
                "#,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await
            .map_err(discard_database_error)?;
        tx.commit().await.map_err(discard_database_error)?;
        deleted_messages +=
            usize::try_from(deleted).map_err(|_| IngressSubstrateError::Database)?;
    }

    Ok(AliasGcOutcome { deleted_messages })
}

struct AliasStorageKey {
    sender: String,
    target_kind: i32,
    target: String,
    origin_id: String,
}

impl AliasStorageKey {
    fn new(sender: &BareJid, target: &NormalizedTarget, origin_id: &OriginId) -> Self {
        let (target_kind, target) = target.to_storage();
        Self {
            sender: sender.to_string(),
            target_kind,
            target,
            origin_id: origin_id.as_str().to_owned(),
        }
    }
}

async fn locked_alias(
    tx: &mut Transaction<'_>,
    key: &AliasStorageKey,
) -> Result<Option<StoredAlias>, IngressSubstrateError> {
    let mut rows = tx
        .query(
            r#"
            SELECT m.message_key::text, m.digest_version, m.digest
            FROM ingress_origin_aliases a
            JOIN ingress_messages m USING (message_key)
            WHERE a.sender_bare_jid = ?
              AND a.target_kind = ?
              AND a.target_jid = ?
              AND a.origin_id = ?
            FOR SHARE OF m
            "#,
            crate::db_params![&key.sender, key.target_kind, &key.target, &key.origin_id],
        )
        .await
        .map_err(discard_database_error)?;
    match rows.next().await.map_err(discard_database_error)? {
        Some(row) => decode_stored_alias(&row).map(Some),
        None => Ok(None),
    }
}

fn decode_stored_alias(row: &Row) -> Result<StoredAlias, IngressSubstrateError> {
    let key_text: String = row.get(0).map_err(discard_database_error)?;
    let digest_version: i64 = row.get(1).map_err(discard_database_error)?;
    let digest: Vec<u8> = row.get(2).map_err(discard_database_error)?;
    let key = key_text
        .parse::<Uuid>()
        .map(MessageKey::from_storage)
        .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)?;
    let digest_version =
        u8::try_from(digest_version).map_err(|_| IngressSubstrateError::InvalidStoredDigest)?;
    let digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| IngressSubstrateError::InvalidStoredDigest)?;
    let digest = SemanticDigest::from_storage(digest_version, digest)
        .map_err(|_| IngressSubstrateError::InvalidStoredDigest)?;
    Ok(StoredAlias { key, digest })
}

async fn lock_message_for_child(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM ingress_messages WHERE message_key = ?::uuid FOR SHARE",
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(rows.next().await.map_err(discard_database_error)?.is_some())
}

async fn message_exists(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM ingress_messages WHERE message_key = ?::uuid",
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(rows.next().await.map_err(discard_database_error)?.is_some())
}

async fn expired_candidates(
    db: &Database,
    cutoff: &str,
) -> Result<Vec<MessageKey>, IngressSubstrateError> {
    let conn = db.guard().await.map_err(discard_database_error)?;
    let mut rows = conn
        .query(
            r#"
            SELECT message_key::text
            FROM ingress_messages
            WHERE terminal_at IS NOT NULL AND terminal_at <= ?::timestamptz
            ORDER BY terminal_at, message_key
            "#,
            crate::db_params![cutoff],
        )
        .await
        .map_err(discard_database_error)?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await.map_err(discard_database_error)? {
        let message_key: String = row.get(0).map_err(discard_database_error)?;
        let message_key = message_key
            .parse::<Uuid>()
            .map(MessageKey::from_storage)
            .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)?;
        candidates.push(message_key);
    }
    Ok(candidates)
}

async fn lock_eligible_terminal_message(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    cutoff: &str,
) -> Result<bool, IngressSubstrateError> {
    let mut rows = tx
        .query(
            r#"
            SELECT terminal_at <= ?::timestamptz
            FROM ingress_messages
            WHERE message_key = ?::uuid AND terminal_at IS NOT NULL
            FOR UPDATE
            "#,
            crate::db_params![cutoff, message_key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    let Some(row) = rows.next().await.map_err(discard_database_error)? else {
        return Ok(false);
    };
    row.get(0).map_err(discard_database_error)
}

async fn has_live_non_alias_children(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    let mut rows = tx
        .query(
            r#"
            SELECT EXISTS (SELECT 1 FROM ingress_sm_refs WHERE message_key = ?::uuid)
                OR EXISTS (SELECT 1 FROM ingress_deliveries WHERE message_key = ?::uuid)
            "#,
            crate::db_params![
                message_key.to_storage().to_string(),
                message_key.to_storage().to_string(),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    let row = rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::Database)?;
    row.get(0).map_err(discard_database_error)
}

fn discard_database_error(_: DatabaseError) -> IngressSubstrateError {
    IngressSubstrateError::Database
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use sqlx::Connection;
    use tokio::sync::Barrier;
    use waddle_xmpp::ingress::{AliasOutcome, AliasResolution};

    use super::*;
    use crate::db::{DatabaseConfig, MigrationRunner};

    #[test]
    fn storage_codecs_round_trip_full_range_values_and_uuid_keys() {
        let ordinal = IngressOrdinal::from_storage(u64::MAX)
            .expect("u64::MAX is a valid persisted ingress ordinal");
        assert_eq!(ordinal.to_storage(), u64::MAX);
        assert!(IngressOrdinal::from_storage(0).is_err());

        let uuid =
            Uuid::parse_str("018e68e7-6a5f-7d4d-a0bc-64dc70a9ce10").expect("fixture UUID is valid");
        assert_eq!(MessageKey::from_storage(uuid).to_storage(), uuid);
        assert_eq!(DeliveryKey::from_storage(uuid).to_storage(), uuid);
        assert_eq!(IngressStreamId::from_storage(uuid).to_storage(), uuid);

        for target in [
            NormalizedTarget::Absent,
            NormalizedTarget::Bare(
                "romeo@example.com"
                    .parse()
                    .expect("fixture is a valid bare JID"),
            ),
            NormalizedTarget::Full(
                "romeo@example.com/phone"
                    .parse()
                    .expect("fixture is a valid full JID"),
            ),
        ] {
            let (kind, value) = target.to_storage();
            assert_eq!(
                NormalizedTarget::from_storage(kind, &value),
                Ok(target),
                "target codec must round trip every variant"
            );
        }
    }

    #[tokio::test]
    async fn alias_key_is_unique_and_resolves_existing_or_conflict_from_joined_digest() {
        let Some(fixture) = Fixture::open("alias_key").await else {
            return;
        };
        let sender = sender();
        let target = target();
        let origin = OriginId::new("opaque-client-origin");
        let first_digest = digest(7);
        let first_key = MessageKey::new();

        let mut tx = fixture.store.begin().await.expect("begin alias insert");
        let first = fixture
            .store
            .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &first_digest, || {
                first_key
            })
            .await
            .expect("insert alias");
        tx.commit().await.expect("commit alias insert");
        assert_eq!(first, inserted(first_key));

        let mut tx = fixture.store.begin().await.expect("begin existing read");
        let resolved = fixture
            .store
            .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &first_digest, || {
                MessageKey::new()
            })
            .await
            .expect("resolve existing alias");
        tx.commit().await.expect("commit existing read");
        assert_eq!(resolved, existing(first_key));

        let mut tx = fixture.store.begin().await.expect("begin conflict read");
        let conflict = fixture
            .store
            .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &digest(8), || {
                MessageKey::new()
            })
            .await
            .expect("resolve conflicting alias");
        tx.commit().await.expect("commit conflict read");
        assert!(matches!(
            conflict,
            AliasResolution::Aliased(AliasOutcome::Conflict(ref value))
                if value.existing == first_key && value.stored == first_digest && value.offered == digest(8)
        ));
        assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
        assert_eq!(fixture.count("ingress_messages").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn concurrent_first_alias_insert_same_digest_leaves_one_message_and_existing_result() {
        let Some(fixture) = Fixture::open("alias_race_same").await else {
            return;
        };
        let sender = sender();
        let target = target();
        let origin = OriginId::new("race-same-digest");
        let start = Arc::new(Barrier::new(2));
        let first = race_alias(
            fixture.store.clone(),
            sender.clone(),
            target.clone(),
            origin.clone(),
            digest(9),
            Arc::clone(&start),
        );
        let second = race_alias(
            fixture.store.clone(),
            sender,
            target,
            origin,
            digest(9),
            start,
        );
        let (first, second) = tokio::join!(first, second);
        let outcomes = [
            first.expect("first race result"),
            second.expect("second race result"),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    AliasResolution::Aliased(AliasOutcome::Inserted(_))
                ))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    AliasResolution::Aliased(AliasOutcome::Existing(_))
                ))
                .count(),
            1
        );
        assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
        assert_eq!(fixture.count("ingress_messages").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn concurrent_first_alias_insert_different_digest_leaves_one_message_and_conflict_result()
    {
        let Some(fixture) = Fixture::open("alias_race_conflict").await else {
            return;
        };
        let sender = sender();
        let target = target();
        let origin = OriginId::new("race-different-digest");
        let start = Arc::new(Barrier::new(2));
        let first = race_alias(
            fixture.store.clone(),
            sender.clone(),
            target.clone(),
            origin.clone(),
            digest(10),
            Arc::clone(&start),
        );
        let second = race_alias(
            fixture.store.clone(),
            sender,
            target,
            origin,
            digest(11),
            start,
        );
        let (first, second) = tokio::join!(first, second);
        let outcomes = [
            first.expect("first race result"),
            second.expect("second race result"),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    AliasResolution::Aliased(AliasOutcome::Inserted(_))
                ))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    AliasResolution::Aliased(AliasOutcome::Conflict(_))
                ))
                .count(),
            1
        );
        assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
        assert_eq!(fixture.count("ingress_messages").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn terminalize_keeps_the_first_proven_terminal_time() {
        let Some(fixture) = Fixture::open("terminalize").await else {
            return;
        };
        let key = fixture.record_message().await;
        let first_time = timestamp(1);
        let second_time = timestamp(2);
        let mut tx = fixture
            .store
            .begin()
            .await
            .expect("begin first terminalize");
        assert_eq!(
            fixture
                .store
                .terminalize_message(&mut tx, key, first_time)
                .await
                .expect("first terminalize"),
            TerminalizeOutcome::Terminalized
        );
        tx.commit().await.expect("commit first terminalize");
        let mut tx = fixture
            .store
            .begin()
            .await
            .expect("begin repeated terminalize");
        assert_eq!(
            fixture
                .store
                .terminalize_message(&mut tx, key, second_time)
                .await
                .expect("repeat terminalize"),
            TerminalizeOutcome::AlreadyTerminal
        );
        tx.commit().await.expect("commit repeated terminalize");
        assert!(fixture.terminal_is(key, first_time).await);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_deletes_aliasless_terminal_messages_and_makes_child_writes_vanish() {
        let Some(fixture) = Fixture::open("gc_aliasless").await else {
            return;
        };
        let key = fixture.record_message().await;
        let terminal_at = timestamp(3);
        fixture.terminalize(key, terminal_at).await;
        let result = fixture
            .store
            .gc_expired_aliases(terminal_at + ALIAS_RETENTION)
            .await
            .expect("garbage collect terminal message");
        assert_eq!(result.deleted_messages, 1);
        let mut tx = fixture
            .store
            .begin()
            .await
            .expect("begin missing child insert");
        assert_eq!(
            fixture
                .store
                .insert_sm_ref(&mut tx, IngressStreamId::new(), IngressOrdinal::FIRST, key)
                .await
                .expect("record vanished child"),
            MessageWriteOutcome::MessageVanished
        );
        tx.commit().await.expect("commit vanished child result");
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_respects_the_exact_terminal_retention_cutoff() {
        let Some(fixture) = Fixture::open("gc_cutoff").await else {
            return;
        };
        let key = fixture.record_message().await;
        let terminal_at = timestamp(4);
        fixture.terminalize(key, terminal_at).await;
        assert_eq!(
            fixture
                .store
                .gc_expired_aliases(terminal_at + ALIAS_RETENTION - Duration::microseconds(1))
                .await
                .expect("collect before exact cutoff")
                .deleted_messages,
            0
        );
        assert_eq!(
            fixture
                .store
                .gc_expired_aliases(terminal_at + ALIAS_RETENTION)
                .await
                .expect("collect at exact cutoff")
                .deleted_messages,
            1
        );
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_preserves_messages_with_live_sm_refs_or_deliveries() {
        let Some(fixture) = Fixture::open("gc_live_children").await else {
            return;
        };
        let key = fixture.record_message().await;
        let terminal_at = timestamp(5);
        let mut tx = fixture.store.begin().await.expect("begin child writes");
        assert_eq!(
            fixture
                .store
                .insert_sm_ref(&mut tx, IngressStreamId::new(), IngressOrdinal::FIRST, key)
                .await
                .expect("insert sm ref"),
            MessageWriteOutcome::Recorded
        );
        assert_eq!(
            fixture
                .store
                .record_delivery(&mut tx, DeliveryKey::new(), key)
                .await
                .expect("record delivery"),
            MessageWriteOutcome::Recorded
        );
        tx.commit().await.expect("commit child writes");
        fixture.terminalize(key, terminal_at).await;
        assert_eq!(
            fixture
                .store
                .gc_expired_aliases(terminal_at + ALIAS_RETENTION)
                .await
                .expect("collect terminal message with children")
                .deleted_messages,
            0
        );
        assert_eq!(fixture.count("ingress_messages").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn epoch_one_rejects_unproven_writes_and_accepts_transaction_bound_proof() {
        let Some(fixture) = Fixture::open("epoch_proof").await else {
            return;
        };
        let key = fixture.record_message().await;
        fixture
            .db
            .execute(
                "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            )
            .await
            .expect("activate epoch one");

        for statement in [
            format!(
                "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
                Uuid::new_v4(),
                key.to_storage()
            ),
            format!(
                "UPDATE ingress_messages SET terminal_at = now() WHERE message_key = '{}'",
                key.to_storage()
            ),
            format!(
                "DELETE FROM ingress_messages WHERE message_key = '{}'",
                key.to_storage()
            ),
            "TRUNCATE ingress_deliveries".to_string(),
        ] {
            assert!(
                fixture.db.execute(&statement).await.is_err(),
                "epoch-one protected operation must require proof: {statement}"
            );
        }

        let mut tx = fixture
            .store
            .begin()
            .await
            .expect("begin proof transaction");
        tx.execute("SET LOCAL waddle.protocol_epoch = '1'", ())
            .await
            .expect("set epoch proof");
        tx.execute(
            "SELECT set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)",
            (),
        )
        .await
        .expect("set xid proof");
        fixture
            .store
            .record_delivery(&mut tx, DeliveryKey::new(), key)
            .await
            .expect("proof authorizes protected write");
        tx.commit().await.expect("commit proof transaction");

        // A proof with a wrong epoch or no xid is intentionally incomplete.
        let mut wrong_epoch = fixture
            .store
            .begin()
            .await
            .expect("begin wrong epoch proof");
        wrong_epoch
            .execute("SET LOCAL waddle.protocol_epoch = '0'", ())
            .await
            .expect("set wrong epoch proof");
        assert!(fixture
            .store
            .record_delivery(&mut wrong_epoch, DeliveryKey::new(), key)
            .await
            .is_err());
        drop(wrong_epoch); // dropping an uncommitted transaction rolls it back
        fixture.close().await;
    }

    #[tokio::test]
    async fn epoch_and_manifest_tables_enforce_their_singleton_and_append_only_rules() {
        let Some(fixture) = Fixture::open("epoch_invariants").await else {
            return;
        };
        for statement in [
            "UPDATE ingress_protocol_epoch SET epoch = 2, activated_at = now(), lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            "UPDATE ingress_protocol_epoch SET epoch = 1 WHERE id = 1",
            "DELETE FROM ingress_protocol_epoch WHERE id = 1",
            "TRUNCATE ingress_protocol_epoch",
            "INSERT INTO ingress_protocol_epoch (id, epoch) VALUES (2, 0)",
            "UPDATE ingress_epoch_guard_manifest SET table_name = 'bad' WHERE table_name = 'ingress_messages'",
            "DELETE FROM ingress_epoch_guard_manifest WHERE table_name = 'ingress_messages'",
            "TRUNCATE ingress_epoch_guard_manifest",
        ] {
            assert!(fixture.db.execute(statement).await.is_err(), "must reject: {statement}");
        }
        let mut tx = fixture.store.begin().await.expect("begin manifest probe");
        tx.execute(
            "INSERT INTO ingress_epoch_guard_manifest (table_name) VALUES ('ingress_future_probe')",
            (),
        )
        .await
        .expect("manifest permits future enrollment");
        drop(tx); // probe must leave the append-only manifest unchanged
        fixture.close().await;
    }

    #[tokio::test]
    async fn epoch_guard_manifest_matches_rust_and_live_trigger_catalog() {
        let Some(fixture) = Fixture::open("guard_manifest").await else {
            return;
        };
        let conn = fixture.db.guard().await.expect("guard manifest database");
        let mut rows = conn
            .query(
                "SELECT table_name FROM ingress_epoch_guard_manifest ORDER BY table_name",
                (),
            )
            .await
            .expect("read guard manifest");
        let mut manifest = Vec::new();
        while let Some(row) = rows.next().await.expect("read manifest row") {
            manifest.push(row.get::<String>(0).expect("decode manifest table"));
        }
        let mut expected = EPOCH_GUARDED_TABLES.map(str::to_owned).to_vec();
        expected.sort();
        assert_eq!(manifest, expected, "migration manifest and Rust list agree");

        for table in EPOCH_GUARDED_TABLES {
            let mut trigger_rows = conn
                .query(
                    "SELECT tg.tgname, tg.tgenabled::text \
                     FROM pg_trigger tg \
                     JOIN pg_class c ON c.oid = tg.tgrelid \
                     JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = current_schema() AND c.relname = ? \
                       AND NOT tg.tgisinternal ORDER BY tg.tgname",
                    crate::db_params![table],
                )
                .await
                .expect("read guard triggers");
            let mut triggers = Vec::new();
            while let Some(row) = trigger_rows.next().await.expect("read trigger row") {
                triggers.push((
                    row.get::<String>(0).expect("decode trigger name"),
                    row.get::<String>(1).expect("decode trigger mode"),
                ));
            }
            assert!(triggers.contains(&(format!("{table}_epoch_guard_dml"), "A".to_string())));
            assert!(triggers.contains(&(format!("{table}_epoch_guard_truncate"), "A".to_string())));
        }
        fixture.close().await;
    }

    #[tokio::test]
    async fn guard_uses_its_table_schema_not_the_callers_search_path() {
        let Some(fixture) = Fixture::open("hostile_search_path").await else {
            return;
        };
        let key = fixture.record_message().await;
        // Independent short name: suffixing the fixture schema would exceed
        // PostgreSQL's 63-byte identifier cap and silently truncate into a
        // collision with the fixture schema itself.
        let hostile = format!("waddle_test_hostile_{}", Uuid::new_v4().simple());
        let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open one hostile-search-path connection");
        sqlx::query(&format!("CREATE SCHEMA {hostile}"))
            .execute(&mut conn)
            .await
            .expect("create hostile schema");
        sqlx::query(&format!(
            "CREATE TABLE {hostile}.ingress_protocol_epoch (id INTEGER PRIMARY KEY, epoch BIGINT NOT NULL)"
        ))
        .execute(&mut conn)
        .await
        .expect("create hostile epoch shadow");
        sqlx::query(&format!(
            "INSERT INTO {hostile}.ingress_protocol_epoch (id, epoch) VALUES (1, 1)"
        ))
        .execute(&mut conn)
        .await
        .expect("seed hostile epoch shadow");
        sqlx::query(&format!("SET search_path = {hostile}, {}", fixture.schema))
            .execute(&mut conn)
            .await
            .expect("place hostile schema first");
        sqlx::query(&format!(
            "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
            Uuid::new_v4(),
            key.to_storage()
        ))
        .execute(&mut conn)
        .await
        .expect("epoch-zero guard must ignore hostile epoch-one shadow");
        drop(conn);
        fixture.close().await;
    }

    /// Poll `pg_stat_activity` until a backend is blocked on a heavyweight
    /// lock while running a query containing `fragment`.  The fragment is a
    /// bound parameter, so this poll's own query text never matches itself.
    async fn wait_for_lock_waiter(admin: &sqlx::PgPool, fragment: &str) {
        for _ in 0..400 {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pg_stat_activity \
                 WHERE wait_event_type = 'Lock' AND query LIKE $1",
            )
            .bind(format!("%{fragment}%"))
            .fetch_one(admin)
            .await
            .expect("poll pg_stat_activity for a lock waiter");
            if waiting > 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("no blocked backend appeared for query fragment {fragment:?}");
    }

    #[tokio::test]
    async fn session_level_guc_and_stale_xid_proofs_do_not_authorize_writes() {
        let Some(fixture) = Fixture::open("epoch_proof_matrix").await else {
            return;
        };
        let key = fixture.record_message().await;
        fixture
            .db
            .execute(
                "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            )
            .await
            .expect("activate epoch one");

        let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open one dedicated connection");
        let insert = format!(
            "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
            Uuid::new_v4(),
            key.to_storage()
        );

        // Session-level SET of BOTH GUCs: the xid captured now belongs to
        // this transaction, so it cannot prove any later transaction.
        sqlx::query("SET waddle.protocol_epoch = '1'")
            .execute(&mut conn)
            .await
            .expect("session-level epoch setting");
        let stale_xid: String = sqlx::query_scalar(
            "SELECT set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, false)",
        )
        .fetch_one(&mut conn)
        .await
        .expect("session-level xid setting");
        assert!(
            sqlx::query(&insert).execute(&mut conn).await.is_err(),
            "a session-retained proof must not authorize a later transaction"
        );

        // Missing xid half of the proof.
        let mut tx = conn.begin().await.expect("begin missing-xid transaction");
        sqlx::query("SET LOCAL waddle.protocol_epoch = '1'")
            .execute(&mut *tx)
            .await
            .expect("set epoch half only");
        assert!(
            sqlx::query(&insert).execute(&mut *tx).await.is_err(),
            "the epoch GUC alone must not authorize a write"
        );
        drop(tx);

        // Stale xid replayed as a literal from an earlier transaction.
        let mut tx = conn.begin().await.expect("begin stale-xid transaction");
        sqlx::query("SET LOCAL waddle.protocol_epoch = '1'")
            .execute(&mut *tx)
            .await
            .expect("set epoch for stale proof");
        sqlx::query("SELECT set_config('waddle.protocol_epoch_xid', $1, true)")
            .bind(&stale_xid)
            .execute(&mut *tx)
            .await
            .expect("replay stale xid literal");
        assert!(
            sqlx::query(&insert).execute(&mut *tx).await.is_err(),
            "a stale xid proof must not authorize a write"
        );
        drop(tx);

        // A correct transaction-local proof works — and does not survive
        // into the next transaction on the SAME pooled connection.
        let mut tx = conn.begin().await.expect("begin proven transaction");
        sqlx::query("SET LOCAL waddle.protocol_epoch = '1'")
            .execute(&mut *tx)
            .await
            .expect("set local epoch");
        sqlx::query(
            "SELECT set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)",
        )
        .execute(&mut *tx)
        .await
        .expect("set local xid");
        sqlx::query(&insert)
            .execute(&mut *tx)
            .await
            .expect("transaction-bound proof authorizes the write");
        tx.commit().await.expect("commit proven transaction");
        assert!(
            sqlx::query(&format!(
                "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
                Uuid::new_v4(),
                key.to_storage()
            ))
            .execute(&mut conn)
            .await
            .is_err(),
            "SET LOCAL must not be retained past commit on the same connection"
        );
        drop(conn);
        fixture.close().await;
    }

    #[tokio::test]
    async fn epoch_activation_waits_behind_in_flight_epoch_zero_writes() {
        let Some(fixture) = Fixture::open("activation_race").await else {
            return;
        };
        let key = fixture.record_message().await;

        // Transaction A: an epoch-0 protected write whose statement trigger
        // took FOR SHARE on the epoch row, held until commit.
        let mut writer = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open epoch-zero writer connection");
        let mut tx = writer.begin().await.expect("begin epoch-zero write");
        sqlx::query(&format!(
            "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
            Uuid::new_v4(),
            key.to_storage()
        ))
        .execute(&mut *tx)
        .await
        .expect("epoch-zero write starts before activation");

        // Concurrent activation must block behind the in-flight write.
        let flip_url = fixture.schema_url.clone();
        let flip = tokio::spawn(async move {
            let mut conn = sqlx::PgConnection::connect(&flip_url)
                .await
                .expect("open activation connection");
            sqlx::query(
                "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
                 lineage_uuid = '8a1d35a6-5e5a-41f1-8e2e-b864e60a4a92' WHERE id = 1",
            )
            .execute(&mut conn)
            .await
            .expect("activation update completes after the writer commits");
        });
        wait_for_lock_waiter(&fixture.admin, "UPDATE ingress_protocol_epoch").await;
        assert!(!flip.is_finished(), "activation must still be blocked");
        tx.commit().await.expect("commit the epoch-zero write");
        flip.await.expect("join activation task");

        // First post-activation write without a proof is rejected.
        assert!(
            sqlx::query(&format!(
                "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('{}', '{}')",
                Uuid::new_v4(),
                key.to_storage()
            ))
            .execute(&mut writer)
            .await
            .is_err(),
            "the first post-activation write requires the transaction proof"
        );
        drop(writer);
        fixture.close().await;
    }

    #[tokio::test]
    async fn gc_blocks_behind_an_in_flight_child_insert_and_skips_the_message() {
        let Some(fixture) = Fixture::open("gc_ref_race").await else {
            return;
        };
        let key = fixture.record_message().await;
        let terminal_at = timestamp(6);
        fixture.terminalize(key, terminal_at).await;

        // Child insert holds FOR SHARE on the message row, uncommitted.
        let mut tx = fixture.store.begin().await.expect("begin child insert");
        assert_eq!(
            fixture
                .store
                .insert_sm_ref(&mut tx, IngressStreamId::new(), IngressOrdinal::FIRST, key)
                .await
                .expect("insert sm ref before GC"),
            MessageWriteOutcome::Recorded
        );

        // Concurrent GC must block on that row's FOR UPDATE, then re-check
        // children under the lock and skip the message.
        let gc_store = fixture.store.clone();
        let gc_now = terminal_at + ALIAS_RETENTION;
        let gc = tokio::spawn(async move { gc_store.gc_expired_aliases(gc_now).await });
        wait_for_lock_waiter(&fixture.admin, "FOR UPDATE").await;
        assert!(
            !gc.is_finished(),
            "GC must still be blocked on the row lock"
        );
        tx.commit().await.expect("commit the child insert");
        let outcome = gc
            .await
            .expect("join GC task")
            .expect("GC completes after the writer commits");
        assert_eq!(
            outcome.deleted_messages, 0,
            "GC re-checks children under the lock and skips the message"
        );
        assert_eq!(fixture.count("ingress_messages").await, 1);
        assert_eq!(fixture.count("ingress_sm_refs").await, 1);
        fixture.close().await;
    }

    #[tokio::test]
    async fn child_insert_blocked_behind_gc_deletion_observes_message_vanished() {
        let Some(fixture) = Fixture::open("ref_gc_race").await else {
            return;
        };
        let key = fixture.record_message().await;

        // Simulate GC's deletion transaction: FOR UPDATE on the message row,
        // held while a child insert arrives.
        let mut gc_conn = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open GC connection");
        let mut gc_tx = gc_conn.begin().await.expect("begin GC transaction");
        sqlx::query(&format!(
            "SELECT 1 FROM ingress_messages WHERE message_key = '{}' FOR UPDATE",
            key.to_storage()
        ))
        .execute(&mut *gc_tx)
        .await
        .expect("GC locks the message row");

        let insert_store = fixture.store.clone();
        let insert = tokio::spawn(async move {
            let mut tx = insert_store.begin().await?;
            let outcome = insert_store
                .insert_sm_ref(&mut tx, IngressStreamId::new(), IngressOrdinal::FIRST, key)
                .await?;
            tx.commit()
                .await
                .map_err(|_| IngressSubstrateError::Database)?;
            Ok::<_, IngressSubstrateError>(outcome)
        });
        wait_for_lock_waiter(&fixture.admin, "FOR SHARE").await;
        assert!(!insert.is_finished(), "child insert must wait behind GC");
        sqlx::query(&format!(
            "DELETE FROM ingress_messages WHERE message_key = '{}'",
            key.to_storage()
        ))
        .execute(&mut *gc_tx)
        .await
        .expect("GC deletes the childless message");
        gc_tx.commit().await.expect("commit GC deletion");
        assert_eq!(
            insert
                .await
                .expect("join child insert task")
                .expect("child insert completes after GC commits"),
            MessageWriteOutcome::MessageVanished,
            "a child insert serialized behind GC observes the deletion"
        );
        drop(gc_conn);
        fixture.close().await;
    }

    #[tokio::test]
    async fn restricted_role_cannot_disable_or_replace_the_epoch_guard() {
        let Some(fixture) = Fixture::open("restricted_role").await else {
            return;
        };
        let role = format!("waddle_test_dml_{}", Uuid::new_v4().simple());
        let mut conn = sqlx::PgConnection::connect(&fixture.schema_url)
            .await
            .expect("open role-test connection");
        sqlx::query(&format!("CREATE ROLE {role} NOLOGIN"))
            .execute(&mut conn)
            .await
            .expect("create restricted DML role");
        sqlx::query(&format!(
            "GRANT USAGE ON SCHEMA {} TO {role}",
            fixture.schema
        ))
        .execute(&mut conn)
        .await
        .expect("grant schema usage");
        for table in EPOCH_GUARDED_TABLES {
            sqlx::query(&format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.{table} TO {role}",
                fixture.schema
            ))
            .execute(&mut conn)
            .await
            .expect("grant DML on protected table");
        }
        sqlx::query(&format!(
            "GRANT SELECT ON {}.ingress_protocol_epoch TO {role}",
            fixture.schema
        ))
        .execute(&mut conn)
        .await
        .expect("grant epoch read");

        sqlx::query(&format!("SET ROLE {role}"))
            .execute(&mut conn)
            .await
            .expect("assume restricted role");
        for statement in [
            "ALTER TABLE ingress_messages DISABLE TRIGGER ingress_messages_epoch_guard_dml"
                .to_string(),
            "CREATE OR REPLACE FUNCTION waddle_ingress_epoch_guard() RETURNS trigger \
             LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$"
                .to_string(),
            "UPDATE ingress_protocol_epoch SET epoch = 1, activated_at = now(), \
             lineage_uuid = 'x' WHERE id = 1"
                .to_string(),
            "INSERT INTO ingress_epoch_guard_manifest (table_name) VALUES ('rogue')".to_string(),
            "DROP TRIGGER ingress_messages_epoch_guard_dml ON ingress_messages".to_string(),
        ] {
            assert!(
                sqlx::query(&statement).execute(&mut conn).await.is_err(),
                "the restricted role must not bypass the guard: {statement}"
            );
        }
        sqlx::query("RESET ROLE")
            .execute(&mut conn)
            .await
            .expect("reset role");
        sqlx::query(&format!("DROP OWNED BY {role}"))
            .execute(&mut conn)
            .await
            .expect("drop role grants");
        sqlx::query(&format!("DROP ROLE {role}"))
            .execute(&mut conn)
            .await
            .expect("drop restricted role");
        drop(conn);
        fixture.close().await;
    }

    async fn race_alias(
        store: PostgresIngressSubstrate,
        sender: BareJid,
        target: NormalizedTarget,
        origin: OriginId,
        digest: SemanticDigest,
        start: Arc<Barrier>,
    ) -> Result<AliasResolution, IngressSubstrateError> {
        let mut tx = store.begin().await?;
        start.wait().await;
        let result = store
            .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &digest, MessageKey::new)
            .await?;
        tx.commit().await.map_err(discard_database_error)?;
        Ok(result)
    }

    fn inserted(key: MessageKey) -> AliasResolution {
        AliasResolution::Aliased(AliasOutcome::Inserted(key))
    }

    fn existing(key: MessageKey) -> AliasResolution {
        AliasResolution::Aliased(AliasOutcome::Existing(key))
    }

    fn digest(byte: u8) -> SemanticDigest {
        SemanticDigest::from_storage(1, [byte; 32]).expect("valid semantic digest fixture")
    }

    fn sender() -> BareJid {
        "romeo@example.com"
            .parse()
            .expect("fixture is a valid bare JID")
    }

    fn target() -> NormalizedTarget {
        NormalizedTarget::Full(
            "juliet@example.com/laptop"
                .parse()
                .expect("fixture is a valid full JID"),
        )
    }

    fn timestamp(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, second)
            .single()
            .expect("fixture timestamp is valid")
    }

    struct Fixture {
        store: PostgresIngressSubstrate,
        db: Database,
        admin: sqlx::PgPool,
        schema: String,
        schema_url: String,
    }

    impl Fixture {
        async fn open(test_name: &str) -> Option<Self> {
            let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
                eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (ingress substrate)");
                return None;
            };
            let schema = format!(
                "waddle_test_ingress_{test_name}_{}",
                Uuid::new_v4().simple()
            );
            let admin = sqlx::PgPool::connect(&database_url)
                .await
                .expect("connect postgres admin pool");
            sqlx::query(&format!("CREATE SCHEMA {schema}"))
                .execute(&admin)
                .await
                .expect("create isolated postgres schema");
            let schema_url = postgres_url_with_search_path(&database_url, &schema);
            let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, schema_url.clone());
            config.pool_size = 10;
            let db = Database::from_config("ingress-substrate-test", &config)
                .await
                .expect("open isolated postgres database");
            MigrationRunner::single()
                .run(&db)
                .await
                .expect("apply migrations to isolated schema");
            let store =
                PostgresIngressSubstrate::open(db.clone()).expect("open Postgres substrate");
            Some(Self {
                store,
                db,
                admin,
                schema,
                schema_url,
            })
        }

        async fn close(self) {
            drop(self.store);
            drop(self.db);
            sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
                .execute(&self.admin)
                .await
                .expect("drop isolated postgres schema");
        }

        async fn record_message(&self) -> MessageKey {
            let key = MessageKey::new();
            let mut tx = self.store.begin().await.expect("begin message insert");
            self.store
                .record_message(&mut tx, key, &digest(1))
                .await
                .expect("record message");
            tx.commit().await.expect("commit message insert");
            key
        }

        async fn terminalize(&self, key: MessageKey, terminal_at: DateTime<Utc>) {
            let mut tx = self.store.begin().await.expect("begin terminalize");
            assert_eq!(
                self.store
                    .terminalize_message(&mut tx, key, terminal_at)
                    .await
                    .expect("terminalize message"),
                TerminalizeOutcome::Terminalized
            );
            tx.commit().await.expect("commit terminalize");
        }

        async fn count(&self, table: &str) -> i64 {
            let conn = self.db.guard().await.expect("database guard");
            let mut rows = conn
                .query(&format!("SELECT COUNT(*) FROM {table}"), ())
                .await
                .expect("count ingress rows");
            let row = rows
                .next()
                .await
                .expect("read count row")
                .expect("count row exists");
            row.get(0).expect("decode count")
        }

        async fn terminal_is(&self, key: MessageKey, expected: DateTime<Utc>) -> bool {
            let conn = self.db.guard().await.expect("database guard");
            let mut rows = conn
                .query(
                    "SELECT terminal_at = ?::timestamptz FROM ingress_messages WHERE message_key = ?::uuid",
                    crate::db_params![expected.to_rfc3339(), key.to_storage().to_string()],
                )
                .await
                .expect("read terminal time");
            let row = rows
                .next()
                .await
                .expect("read terminal row")
                .expect("message row exists");
            row.get(0).expect("decode terminal comparison")
        }
    }

    fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
        let mut url = url::Url::parse(database_url).expect("parse postgres URL");
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
            .append_pair("options", &format!("-c search_path={schema}"));
        url.to_string()
    }
}
