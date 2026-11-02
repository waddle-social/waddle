//! Closed, unwired experiment. `FixtureConnection` is synthetic and cannot
//! authorize a real connection or enqueue. No runtime writer calls this lab.

use crate::ingress_uow::{start_fresh_postgres, start_fresh_sqlite, SendLease};
use chrono::{DateTime, Utc};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Postgres, Row, Transaction};
use thiserror::Error;
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::ownership::NodeIdentity;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::{SmIngressAppendKey, SmIngressReceiptKind};

use super::super::{
    prepare_postgres, quote_identifier, read_sqlite_epoch, Backend, DarkIngressAuthority,
    DarkIngressAuthorityError, Database, DatabaseDriver, PhysicalAttestationConfig,
    TransactionBounds,
};
use super::{encode_payload, identical, validate, AdmissionPin};

// Unregistered and disposable. This is deliberately not a migration or an
// epoch-guard activation; the existing runtime has not adopted the contract.
const POSTGRES_DDL: &str = r#"
CREATE TABLE __SCHEMA__.direct_admission_lab_quota (id INTEGER PRIMARY KEY CHECK (id = 1));
INSERT INTO __SCHEMA__.direct_admission_lab_quota VALUES (1);
CREATE TABLE __SCHEMA__.direct_admission_lab (
    admission_id UUID PRIMARY KEY,
    message_key UUID NOT NULL REFERENCES __SCHEMA__.ingress_messages (message_key),
    receipt_kind INTEGER NOT NULL,
    semantic_identity_hash BYTEA NOT NULL CHECK (octet_length(semantic_identity_hash) = 32),
    resource TEXT NOT NULL,
    attempt_token UUID NOT NULL UNIQUE,
    node_id TEXT NOT NULL,
    node_incarnation TEXT NOT NULL,
    connection_generation UUID NOT NULL,
    sm_session_id TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (octet_length(payload) BETWEEN 1 AND 262144),
    received_at TEXT NOT NULL,
    UNIQUE (message_key, receipt_kind, semantic_identity_hash, resource)
);
CREATE FUNCTION __SCHEMA__.direct_admission_lab_immutable() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN RAISE EXCEPTION 'direct admission laboratory records are immutable'; END $$;
CREATE TRIGGER direct_admission_lab_immutable_row BEFORE UPDATE OR DELETE ON __SCHEMA__.direct_admission_lab
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.direct_admission_lab_immutable();
CREATE TRIGGER direct_admission_lab_immutable_truncate BEFORE TRUNCATE ON __SCHEMA__.direct_admission_lab
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.direct_admission_lab_immutable();
"#;

/// Render the unregistered fixture DDL for the configured schema.
pub fn direct_lab_postgres_ddl(config: &PhysicalAttestationConfig) -> String {
    POSTGRES_DDL.replace(
        "__SCHEMA__",
        &quote_identifier(&config.postgres().schema.name.0),
    )
}

/// Unregistered fixture DDL for the runtime's actual shared SQLite database.
pub const DIRECT_LAB_SQLITE_DDL: &str = r#"
CREATE TABLE main.direct_admission_lab_quota (id INTEGER PRIMARY KEY CHECK (id = 1));
INSERT INTO main.direct_admission_lab_quota VALUES (1);
CREATE TABLE main.direct_admission_lab (
    admission_id TEXT PRIMARY KEY,
    message_key TEXT NOT NULL REFERENCES ingress_messages (message_key),
    receipt_kind INTEGER NOT NULL,
    semantic_identity_hash BLOB NOT NULL CHECK (length(semantic_identity_hash) = 32),
    resource TEXT NOT NULL,
    attempt_token TEXT NOT NULL UNIQUE,
    node_id TEXT NOT NULL,
    node_incarnation TEXT NOT NULL,
    connection_generation TEXT NOT NULL,
    sm_session_id TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (length(CAST(payload AS BLOB)) BETWEEN 1 AND 262144),
    received_at TEXT NOT NULL,
    UNIQUE (message_key, receipt_kind, semantic_identity_hash, resource)
) WITHOUT ROWID;
CREATE TRIGGER main.direct_admission_lab_no_replace BEFORE INSERT ON direct_admission_lab
WHEN EXISTS (SELECT 1 FROM main.direct_admission_lab WHERE admission_id = NEW.admission_id
 OR attempt_token = NEW.attempt_token OR (message_key = NEW.message_key AND receipt_kind = NEW.receipt_kind
 AND semantic_identity_hash = NEW.semantic_identity_hash AND resource = NEW.resource))
BEGIN SELECT RAISE(ABORT, 'direct admission laboratory records are immutable'); END;
CREATE TRIGGER main.direct_admission_lab_no_update BEFORE UPDATE ON direct_admission_lab
BEGIN SELECT RAISE(ABORT, 'direct admission laboratory records are immutable'); END;
CREATE TRIGGER main.direct_admission_lab_no_delete BEFORE DELETE ON direct_admission_lab
BEGIN SELECT RAISE(ABORT, 'direct admission laboratory records are immutable'); END;
"#;

const MAX_ROWS: u32 = 1024;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 256 * 1024;

/// A synthetic fixed witness for tests only. Its UUID is not a captured runtime
/// connection identity and grants no authority to invoke an enqueue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureConnection {
    pub resource: jid::FullJid,
    pub generation: uuid::Uuid,
    pub sm_session: SmSessionId,
}

/// Full typed laboratory request, known before its commit is attempted.
#[derive(Debug, Clone)]
pub struct DirectAdmission {
    pub pin: AdmissionPin,
    pub owner: NodeIdentity,
    pub attempt_token: uuid::Uuid,
    pub connection: FixtureConnection,
}

impl DirectAdmission {
    /// Construct a fixture record from a real claim capability. Neither this
    /// value nor the synthetic connection witness grants enqueue authority.
    pub fn for_lease(
        pin: AdmissionPin,
        lease: &SendLease,
        connection: FixtureConnection,
    ) -> Result<Self, DirectAdmissionLabError> {
        let obligation = lease.obligation();
        if pin.key.message_key != obligation.message
            || pin.key.kind.to_storage() != obligation.receipt.kind.to_storage()
            || pin.key.semantic_identity_hash != obligation.receipt.semantic_identity_hash
            || pin.key.resource != obligation.recipient
        {
            return Err(DirectAdmissionLabError::Rejected);
        }
        Ok(Self {
            pin,
            owner: lease.owner().clone(),
            attempt_token: lease.token(),
            connection,
        })
    }
}

/// Bounds can tighten, but never exceed, the laboratory's hard global limits.
#[derive(Debug, Clone, Copy)]
pub struct DirectAdmissionLimits {
    max_rows: u32,
    max_bytes: u64,
}

impl DirectAdmissionLimits {
    pub fn new(max_rows: u32, max_bytes: u64) -> Result<Self, DirectAdmissionLabError> {
        if max_rows == 0 || max_rows > MAX_ROWS || max_bytes == 0 || max_bytes > MAX_BYTES {
            return Err(DirectAdmissionLabError::InvalidLimits);
        }
        Ok(Self {
            max_rows,
            max_bytes,
        })
    }
}

/// Neither outcome authorizes socket I/O. A recovered record cannot establish
/// whether an earlier owner enqueued after an uncertain commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectAdmissionOutcome {
    Recorded,
    Recovered,
}

#[derive(Debug, Error)]
pub enum DirectAdmissionLabError {
    #[error("invalid laboratory admission limits")]
    InvalidLimits,
    #[error("the exact recorded obligation, lease, or fixture witness was rejected")]
    Rejected,
    #[error("the admission or attempt identity conflicts with an immutable record")]
    Conflict,
    #[error("the laboratory admission capacity is exhausted")]
    Capacity,
    #[error("the canonical parent is missing; admission reconciliation remains unresolved")]
    ReconciliationUnresolved,
    #[error(transparent)]
    Authority(#[from] DarkIngressAuthorityError),
}

/// Closed test fixture: no connection handle, transaction, callback, or socket
/// operation is exposed. All admitted rows are pending indefinitely in this slice.
pub struct DirectAdmissionLab {
    authority: DarkIngressAuthority,
    connection: FixtureConnection,
    limits: DirectAdmissionLimits,
}

impl DirectAdmissionLab {
    pub fn postgres(
        options: PgConnectOptions,
        expected: PhysicalAttestationConfig,
        pool_size: u32,
        bounds: TransactionBounds,
        connection: FixtureConnection,
        limits: DirectAdmissionLimits,
    ) -> Result<Self, DarkIngressAuthorityError> {
        Ok(Self {
            authority: DarkIngressAuthority::postgres(options, expected, pool_size, bounds)?,
            connection,
            limits,
        })
    }

    pub fn sqlite(
        runtime: &Database,
        bounds: TransactionBounds,
        connection: FixtureConnection,
        limits: DirectAdmissionLimits,
    ) -> Result<Self, DarkIngressAuthorityError> {
        if runtime.driver() != DatabaseDriver::Sqlite {
            return Err(DarkIngressAuthorityError::Database);
        }
        Ok(Self {
            authority: DarkIngressAuthority {
                backend: Backend::Sqlite(runtime.clone()),
                bounds,
            },
            connection,
            limits,
        })
    }

    /// Validate the stored direct obligation and exact unexpired attempt, then
    /// atomically start that attempt and persist this immutable payload.
    pub async fn start(
        &self,
        admission: &DirectAdmission,
        lease: &SendLease,
    ) -> Result<DirectAdmissionOutcome, DirectAdmissionLabError> {
        if admission.connection != self.connection
            || admission.pin.key.resource != self.connection.resource
            || admission.pin.admission_id.is_nil()
            || admission.attempt_token.is_nil()
            || admission.connection.generation.is_nil()
            || admission.connection.sm_session.as_str().is_empty()
            || SmSessionId::try_from_wire(admission.connection.sm_session.as_str()).is_err()
            || !admission.owner.is_active()
            || !lease.never_started()
        {
            return Err(DirectAdmissionLabError::Rejected);
        }
        let claimed =
            DirectAdmission::for_lease(admission.pin.clone(), lease, admission.connection.clone())?;
        if !identical_admission(&claimed, admission) {
            return Err(DirectAdmissionLabError::Rejected);
        }
        let payload = encode_payload(&admission.pin.payload)
            .map_err(|_| DirectAdmissionLabError::Rejected)?;
        if payload.len() > MAX_FRAME_BYTES {
            return Err(DirectAdmissionLabError::Capacity);
        }
        match &self.authority.backend {
            Backend::Postgres {
                pool,
                schema,
                schema_oid,
            } => {
                let mut conn = self.authority.acquire_postgres(pool).await?;
                let mut prepared = tokio::time::timeout(
                    self.authority.bounds.acquisition,
                    prepare_v1009(&mut conn, schema, *schema_oid, self.authority.bounds),
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Timeout)??;
                let result = start_postgres(
                    &mut prepared.transaction,
                    schema,
                    admission,
                    lease,
                    &payload,
                    self.limits,
                )
                .await?;
                prepared.commit().await?;
                Ok(result)
            }
            Backend::Sqlite(db) => {
                let mut tx = self.authority.begin_sqlite(db).await?;
                read_sqlite_epoch(&mut tx).await?;
                let result = start_sqlite(&mut tx, admission, lease, &payload, self.limits).await?;
                tx.commit()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)?;
                Ok(result)
            }
        }
    }

    /// Reconcile a lost response while holding the same canonical lock as the
    /// old transaction. Absence is only an observation at this lock boundary;
    /// callers must also stop any subsequent start using the same capability.
    /// A missing canonical parent is not reported as proof of absence.
    pub async fn lookup(
        &self,
        admission: &DirectAdmission,
    ) -> Result<Option<DirectAdmission>, DirectAdmissionLabError> {
        match &self.authority.backend {
            Backend::Postgres {
                pool,
                schema,
                schema_oid,
            } => {
                let mut conn = self.authority.acquire_postgres(pool).await?;
                let mut prepared = tokio::time::timeout(
                    self.authority.bounds.acquisition,
                    prepare_v1009(&mut conn, schema, *schema_oid, self.authority.bounds),
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Timeout)??;
                let lock = format!(
                    "SELECT 1 FROM {}.ingress_messages WHERE message_key = $1 FOR UPDATE",
                    quote_identifier(schema)
                );
                sqlx::query(&lock)
                    .bind(admission.pin.key.message_key.to_storage())
                    .fetch_optional(&mut *prepared.transaction)
                    .await
                    .map_err(super::super::postgres_error)?
                    .ok_or(DirectAdmissionLabError::ReconciliationUnresolved)?;
                let record = lookup_postgres(
                    &mut prepared.transaction,
                    schema,
                    admission.pin.admission_id,
                )
                .await?;
                let record = check_record(record, admission)?;
                prepared.commit().await?;
                Ok(record)
            }
            Backend::Sqlite(db) => {
                let mut tx = self.authority.begin_sqlite(db).await?;
                read_sqlite_epoch(&mut tx).await?;
                let mut rows = tx
                    .query(
                        "SELECT 1 FROM main.ingress_messages WHERE message_key = ?",
                        crate::db_params![admission.pin.key.message_key.to_storage().to_string()],
                    )
                    .await
                    .map_err(|_| DarkIngressAuthorityError::Database)?;
                rows.next()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::Database)?
                    .ok_or(DirectAdmissionLabError::ReconciliationUnresolved)?;
                drop(rows);
                let record = check_record(
                    lookup_sqlite(&mut tx, admission.pin.admission_id).await?,
                    admission,
                )?;
                tx.commit()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)?;
                Ok(record)
            }
        }
    }
}

fn check_record(
    record: Option<DirectAdmission>,
    expected: &DirectAdmission,
) -> Result<Option<DirectAdmission>, DirectAdmissionLabError> {
    if record
        .as_ref()
        .is_some_and(|r| !identical_admission(r, expected))
    {
        return Err(DirectAdmissionLabError::Conflict);
    }
    Ok(record)
}

// V1009's installed guard reads the epoch row under a row lock. Acquire that
// lock before any canonical row and recheck the earlier attested epoch. The
// candidate relation-only laboratory keeps its existing separate entry.
async fn prepare_v1009<'a>(
    connection: &'a mut sqlx::PgConnection,
    schema: &str,
    schema_oid: u32,
    bounds: TransactionBounds,
) -> Result<super::super::PreparedPostgres<'a>, DarkIngressAuthorityError> {
    let mut prepared = prepare_postgres(connection, schema, schema_oid, bounds).await?;
    let locked: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT epoch FROM {}.ingress_protocol_epoch WHERE id = 1 FOR SHARE",
        quote_identifier(schema)
    ))
    .fetch_optional(&mut *prepared.transaction)
    .await
    .map_err(super::super::postgres_error)?;
    let locked =
        super::super::supported_epoch(locked.ok_or(DarkIngressAuthorityError::MissingEpoch)?)?;
    if locked != prepared.epoch {
        return Err(DarkIngressAuthorityError::Database);
    }
    Ok(prepared)
}

fn identical_admission(a: &DirectAdmission, b: &DirectAdmission) -> bool {
    identical(&a.pin, &b.pin)
        && a.owner == b.owner
        && a.attempt_token == b.attempt_token
        && a.connection == b.connection
}

fn validate_stored(
    admission: &DirectAdmission,
    version: i64,
    envelope: Vec<u8>,
    kind: i32,
    intent_version: i64,
    intent: &[u8],
    received_at: DateTime<Utc>,
) -> Result<(), DirectAdmissionLabError> {
    if received_at != admission.pin.received_at {
        return Err(DirectAdmissionLabError::Rejected);
    }
    validate(
        &admission.pin,
        version,
        envelope,
        kind,
        intent_version,
        intent,
        false,
    )
    .map_err(|_| DirectAdmissionLabError::Rejected)
}

fn capacity(
    rows: i64,
    bytes: i64,
    payload: usize,
    limits: DirectAdmissionLimits,
) -> Result<(), DirectAdmissionLabError> {
    let rows = u64::try_from(rows).map_err(|_| DarkIngressAuthorityError::Database)?;
    let bytes = u64::try_from(bytes).map_err(|_| DarkIngressAuthorityError::Database)?;
    if rows >= u64::from(limits.max_rows)
        || bytes
            .checked_add(payload as u64)
            .is_none_or(|n| n > limits.max_bytes)
    {
        return Err(DirectAdmissionLabError::Capacity);
    }
    Ok(())
}

// All remaining adapters operate on the already checked-out transaction.

struct Stored {
    id: String,
    message: String,
    kind: i64,
    hash: Vec<u8>,
    resource: String,
    token: String,
    node: String,
    incarnation: String,
    generation: String,
    session: String,
    payload: String,
    received_at: String,
}

fn decode(stored: Stored) -> Result<DirectAdmission, DirectAdmissionLabError> {
    let bad = || DarkIngressAuthorityError::Database;
    let resource: jid::FullJid = stored.resource.parse().map_err(|_| bad())?;
    let payload = waddle_xmpp::parser::message_from_string(&stored.payload).map_err(|_| bad())?;
    Ok(DirectAdmission {
        pin: AdmissionPin {
            admission_id: stored.id.parse().map_err(|_| bad())?,
            key: SmIngressAppendKey {
                message_key: MessageKey::from_storage(stored.message.parse().map_err(|_| bad())?),
                kind: SmIngressReceiptKind::from_storage(
                    i32::try_from(stored.kind).map_err(|_| bad())?,
                ),
                semantic_identity_hash: stored.hash.try_into().map_err(|_| bad())?,
                resource: resource.clone(),
            },
            payload,
            received_at: DateTime::parse_from_rfc3339(&stored.received_at)
                .map_err(|_| bad())?
                .with_timezone(&Utc),
        },
        owner: NodeIdentity::new(stored.node, stored.incarnation),
        attempt_token: stored.token.parse().map_err(|_| bad())?,
        connection: FixtureConnection {
            resource,
            generation: stored.generation.parse().map_err(|_| bad())?,
            sm_session: SmSessionId::try_from_wire(stored.session).map_err(|_| bad())?,
        },
    })
}

async fn lookup_postgres(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    id: uuid::Uuid,
) -> Result<Option<DirectAdmission>, DirectAdmissionLabError> {
    let row = sqlx::query(&format!("SELECT admission_id::text, message_key::text, receipt_kind::bigint, semantic_identity_hash, resource, attempt_token::text, node_id, node_incarnation, connection_generation::text, sm_session_id, payload, received_at FROM {}.direct_admission_lab WHERE admission_id = $1", quote_identifier(schema)))
        .bind(id).fetch_optional(&mut **tx).await.map_err(super::super::postgres_error)?;
    row.map(|r| {
        let bad = |_| DarkIngressAuthorityError::Database;
        decode(Stored {
            id: r.try_get(0).map_err(bad)?,
            message: r.try_get(1).map_err(bad)?,
            kind: r.try_get(2).map_err(bad)?,
            hash: r.try_get(3).map_err(bad)?,
            resource: r.try_get(4).map_err(bad)?,
            token: r.try_get(5).map_err(bad)?,
            node: r.try_get(6).map_err(bad)?,
            incarnation: r.try_get(7).map_err(bad)?,
            generation: r.try_get(8).map_err(bad)?,
            session: r.try_get(9).map_err(bad)?,
            payload: r.try_get(10).map_err(bad)?,
            received_at: r.try_get(11).map_err(bad)?,
        })
    })
    .transpose()
}

async fn start_postgres(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    admission: &DirectAdmission,
    lease: &SendLease,
    payload: &str,
    limits: DirectAdmissionLimits,
) -> Result<DirectAdmissionOutcome, DirectAdmissionLabError> {
    let q = quote_identifier(schema);
    let pin = &admission.pin;
    let canonical = sqlx::query(&format!("SELECT envelope_version::bigint, envelope, created_at FROM {q}.ingress_messages WHERE message_key = $1 FOR UPDATE"))
        .bind(pin.key.message_key.to_storage()).fetch_optional(&mut **tx).await.map_err(super::super::postgres_error)?.ok_or(DirectAdmissionLabError::Rejected)?;
    let intent = sqlx::query(&format!("SELECT kind, payload_version::bigint, payload FROM {q}.ingress_effect_intents WHERE message_key = $1 AND kind = $2 AND semantic_identity_hash = $3"))
        .bind(pin.key.message_key.to_storage()).bind(pin.key.kind.to_storage()).bind(pin.key.semantic_identity_hash.as_slice()).fetch_optional(&mut **tx).await.map_err(super::super::postgres_error)?.ok_or(DirectAdmissionLabError::Rejected)?;
    let bad = |_| DarkIngressAuthorityError::Database;
    let version: Option<i64> = canonical.try_get(0).map_err(bad)?;
    let envelope: Option<Vec<u8>> = canonical.try_get(1).map_err(bad)?;
    let received_at = canonical.try_get(2).map_err(bad)?;
    let intent_bytes: Vec<u8> = intent.try_get(2).map_err(bad)?;
    validate_stored(
        admission,
        version.ok_or(DirectAdmissionLabError::Rejected)?,
        envelope.ok_or(DirectAdmissionLabError::Rejected)?,
        intent.try_get(0).map_err(bad)?,
        intent.try_get(1).map_err(bad)?,
        &intent_bytes,
        received_at,
    )?;
    let attempt = sqlx::query(&format!("SELECT 1 FROM {q}.ingress_send_attempts WHERE message_key = $1 AND kind = $2 AND semantic_identity_hash = $3 AND recipient = $4 FOR UPDATE"))
        .bind(pin.key.message_key.to_storage()).bind(pin.key.kind.to_storage()).bind(pin.key.semantic_identity_hash.as_slice()).bind(pin.key.resource.to_string()).fetch_optional(&mut **tx).await.map_err(super::super::postgres_error)?;
    attempt.ok_or(DirectAdmissionLabError::Rejected)?;
    sqlx::query(&format!(
        "SELECT id FROM {q}.direct_admission_lab_quota WHERE id = 1 FOR UPDATE"
    ))
    .fetch_optional(&mut **tx)
    .await
    .map_err(super::super::postgres_error)?
    .ok_or(DarkIngressAuthorityError::Database)?;
    if let Some(existing) = lookup_postgres(tx, schema, pin.admission_id).await? {
        return if identical_admission(&existing, admission) {
            Ok(DirectAdmissionOutcome::Recovered)
        } else {
            Err(DirectAdmissionLabError::Conflict)
        };
    }
    let conflict: bool = sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {q}.direct_admission_lab WHERE attempt_token = $1 OR (message_key = $2 AND receipt_kind = $3 AND semantic_identity_hash = $4 AND resource = $5))"))
        .bind(admission.attempt_token).bind(pin.key.message_key.to_storage()).bind(pin.key.kind.to_storage()).bind(pin.key.semantic_identity_hash.as_slice()).bind(pin.key.resource.to_string()).fetch_one(&mut **tx).await.map_err(super::super::postgres_error)?;
    if conflict {
        return Err(DirectAdmissionLabError::Conflict);
    }
    let blocked: bool = sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {q}.ingress_effect_receipts WHERE message_key = $1 AND kind = $2 AND semantic_identity_hash = $3) OR EXISTS (SELECT 1 FROM {q}.ingress_delivery_receipts WHERE message_key = $1 AND kind = $2 AND semantic_identity_hash = $3 AND resource = $4) OR EXISTS (SELECT 1 FROM {q}.ingress_carbon_receipts WHERE message_key = $1 AND kind = $2 AND semantic_identity_hash = $3 AND recipient = $4) OR EXISTS (SELECT 1 FROM {q}.sm_ingress_appends WHERE message_key = $1::text AND receipt_kind = $2 AND semantic_identity_hash = $3 AND resource = $4)"))
        .bind(pin.key.message_key.to_storage()).bind(pin.key.kind.to_storage()).bind(pin.key.semantic_identity_hash.as_slice()).bind(pin.key.resource.to_string()).fetch_one(&mut **tx).await.map_err(super::super::postgres_error)?;
    if blocked {
        return Err(DirectAdmissionLabError::Rejected);
    }
    let (rows, bytes): (i64, i64) = sqlx::query_as(&format!("SELECT COUNT(*), COALESCE(SUM(octet_length(payload)), 0)::bigint FROM {q}.direct_admission_lab"))
        .fetch_one(&mut **tx).await.map_err(super::super::postgres_error)?;
    capacity(rows, bytes, payload.len(), limits)?;
    if !start_fresh_postgres(tx, &format!("{q}.ingress_send_attempts"), lease)
        .await
        .map_err(super::super::postgres_error)?
    {
        return Err(DirectAdmissionLabError::Rejected);
    }
    sqlx::query(&format!("INSERT INTO {q}.direct_admission_lab (admission_id, message_key, receipt_kind, semantic_identity_hash, resource, attempt_token, node_id, node_incarnation, connection_generation, sm_session_id, payload, received_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"))
        .bind(pin.admission_id).bind(pin.key.message_key.to_storage()).bind(pin.key.kind.to_storage()).bind(pin.key.semantic_identity_hash.as_slice()).bind(pin.key.resource.to_string()).bind(admission.attempt_token).bind(&admission.owner.node_id).bind(&admission.owner.node_epoch).bind(admission.connection.generation).bind(admission.connection.sm_session.as_str()).bind(payload).bind(pin.received_at.to_rfc3339()).execute(&mut **tx).await.map_err(super::super::postgres_error)?;
    Ok(DirectAdmissionOutcome::Recorded)
}

async fn lookup_sqlite(
    tx: &mut crate::db::Transaction<'_>,
    id: uuid::Uuid,
) -> Result<Option<DirectAdmission>, DirectAdmissionLabError> {
    let mut rows = tx.query("SELECT admission_id, message_key, receipt_kind, semantic_identity_hash, resource, attempt_token, node_id, node_incarnation, connection_generation, sm_session_id, payload, received_at FROM main.direct_admission_lab WHERE admission_id = ?", crate::db_params![id.to_string()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
    let Some(r) = rows
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
    else {
        return Ok(None);
    };
    let bad = |_| DarkIngressAuthorityError::Database;
    decode(Stored {
        id: r.get(0).map_err(bad)?,
        message: r.get(1).map_err(bad)?,
        kind: r.get(2).map_err(bad)?,
        hash: r.get(3).map_err(bad)?,
        resource: r.get(4).map_err(bad)?,
        token: r.get(5).map_err(bad)?,
        node: r.get(6).map_err(bad)?,
        incarnation: r.get(7).map_err(bad)?,
        generation: r.get(8).map_err(bad)?,
        session: r.get(9).map_err(bad)?,
        payload: r.get(10).map_err(bad)?,
        received_at: r.get(11).map_err(bad)?,
    })
    .map(Some)
}

async fn start_sqlite(
    tx: &mut crate::db::Transaction<'_>,
    admission: &DirectAdmission,
    lease: &SendLease,
    payload: &str,
    limits: DirectAdmissionLimits,
) -> Result<DirectAdmissionOutcome, DirectAdmissionLabError> {
    let pin = &admission.pin;
    let bad = |_| DarkIngressAuthorityError::Database;
    let mut rows = tx.query("SELECT envelope_version, envelope, created_at FROM main.ingress_messages WHERE message_key = ?", crate::db_params![pin.key.message_key.to_storage().to_string()]).await.map_err(bad)?;
    let canonical = rows
        .next()
        .await
        .map_err(bad)?
        .ok_or(DirectAdmissionLabError::Rejected)?;
    drop(rows);
    let version: Option<i64> = canonical.get(0).map_err(bad)?;
    let envelope: Option<Vec<u8>> = canonical.get(1).map_err(bad)?;
    let time: String = canonical.get(2).map_err(bad)?;
    let received_at = DateTime::parse_from_rfc3339(&time)
        .map_err(|_| DarkIngressAuthorityError::Database)?
        .with_timezone(&Utc);
    let mut rows = tx.query("SELECT kind, payload_version, payload FROM main.ingress_effect_intents WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ?", crate::db_params![pin.key.message_key.to_storage().to_string(), pin.key.kind.to_storage(), pin.key.semantic_identity_hash.to_vec()]).await.map_err(bad)?;
    let intent = rows
        .next()
        .await
        .map_err(bad)?
        .ok_or(DirectAdmissionLabError::Rejected)?;
    drop(rows);
    let kind: i64 = intent.get(0).map_err(bad)?;
    let intent_bytes: Vec<u8> = intent.get(2).map_err(bad)?;
    validate_stored(
        admission,
        version.ok_or(DirectAdmissionLabError::Rejected)?,
        envelope.ok_or(DirectAdmissionLabError::Rejected)?,
        i32::try_from(kind).map_err(|_| DarkIngressAuthorityError::Database)?,
        intent.get(1).map_err(bad)?,
        &intent_bytes,
        received_at,
    )?;
    let mut rows = tx.query("SELECT 1 FROM main.ingress_send_attempts WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? AND recipient = ?", crate::db_params![pin.key.message_key.to_storage().to_string(), pin.key.kind.to_storage(), pin.key.semantic_identity_hash.to_vec(), pin.key.resource.to_string()]).await.map_err(bad)?;
    rows.next()
        .await
        .map_err(bad)?
        .ok_or(DirectAdmissionLabError::Rejected)?;
    drop(rows);
    let mut rows = tx
        .query(
            "SELECT id FROM main.direct_admission_lab_quota WHERE id = 1",
            (),
        )
        .await
        .map_err(bad)?;
    rows.next()
        .await
        .map_err(bad)?
        .ok_or(DarkIngressAuthorityError::Database)?;
    drop(rows);
    if let Some(existing) = lookup_sqlite(tx, pin.admission_id).await? {
        return if identical_admission(&existing, admission) {
            Ok(DirectAdmissionOutcome::Recovered)
        } else {
            Err(DirectAdmissionLabError::Conflict)
        };
    }
    let mut rows = tx.query("SELECT 1 FROM main.direct_admission_lab WHERE attempt_token = ? OR (message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ?) LIMIT 1", crate::db_params![admission.attempt_token.to_string(), pin.key.message_key.to_storage().to_string(), pin.key.kind.to_storage(), pin.key.semantic_identity_hash.to_vec(), pin.key.resource.to_string()]).await.map_err(bad)?;
    if rows.next().await.map_err(bad)?.is_some() {
        return Err(DirectAdmissionLabError::Conflict);
    }
    drop(rows);
    let mut rows = tx.query("SELECT 1 WHERE EXISTS (SELECT 1 FROM main.ingress_effect_receipts WHERE message_key = ?1 AND kind = ?2 AND semantic_identity_hash = ?3) OR EXISTS (SELECT 1 FROM main.ingress_delivery_receipts WHERE message_key = ?1 AND kind = ?2 AND semantic_identity_hash = ?3 AND resource = ?4) OR EXISTS (SELECT 1 FROM main.ingress_carbon_receipts WHERE message_key = ?1 AND kind = ?2 AND semantic_identity_hash = ?3 AND recipient = ?4) OR EXISTS (SELECT 1 FROM main.sm_ingress_appends WHERE message_key = ?1 AND receipt_kind = ?2 AND semantic_identity_hash = ?3 AND resource = ?4)", crate::db_params![pin.key.message_key.to_storage().to_string(), pin.key.kind.to_storage(), pin.key.semantic_identity_hash.to_vec(), pin.key.resource.to_string()]).await.map_err(bad)?;
    if rows.next().await.map_err(bad)?.is_some() {
        return Err(DirectAdmissionLabError::Rejected);
    }
    drop(rows);
    let mut rows = tx.query("SELECT COUNT(*), COALESCE(SUM(length(CAST(payload AS BLOB))), 0) FROM main.direct_admission_lab", ()).await.map_err(bad)?;
    let row = rows
        .next()
        .await
        .map_err(bad)?
        .ok_or(DarkIngressAuthorityError::Database)?;
    capacity(
        row.get(0).map_err(bad)?,
        row.get(1).map_err(bad)?,
        payload.len(),
        limits,
    )?;
    drop(rows);
    if !start_fresh_sqlite(tx, lease).await.map_err(bad)? {
        return Err(DirectAdmissionLabError::Rejected);
    }
    tx.execute("INSERT INTO main.direct_admission_lab (admission_id, message_key, receipt_kind, semantic_identity_hash, resource, attempt_token, node_id, node_incarnation, connection_generation, sm_session_id, payload, received_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", crate::db_params![pin.admission_id.to_string(), pin.key.message_key.to_storage().to_string(), pin.key.kind.to_storage(), pin.key.semantic_identity_hash.to_vec(), pin.key.resource.to_string(), admission.attempt_token.to_string(), admission.owner.node_id.clone(), admission.owner.node_epoch.clone(), admission.connection.generation.to_string(), admission.connection.sm_session.as_str().to_string(), payload.to_owned(), pin.received_at.to_rfc3339()]).await.map_err(bad)?;
    Ok(DirectAdmissionOutcome::Recorded)
}
