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
            let mut config = DatabaseConfig::new(
                DatabaseDriver::Postgres,
                postgres_url_with_search_path(&database_url, &schema),
            );
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
