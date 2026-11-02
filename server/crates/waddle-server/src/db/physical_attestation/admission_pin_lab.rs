//! Test-only admission pins for a narrow prepared direct-route experiment.
//!
//! No runtime writer uses these pins. They have no allocation or settlement
//! transition and are retained indefinitely; they do not close the production
//! late-append race. Only exact prepared direct messages are admitted.

use chrono::{DateTime, Utc};
use sqlx::postgres::PgConnectOptions;
use sqlx::Row;
use thiserror::Error;
use waddle_xmpp::ingress::IngressEffectIntent;
use waddle_xmpp::stream_management::SmIngressAppendKey;
use xmpp_parsers::message::Message;

use super::{
    prepare_postgres, quote_identifier, read_sqlite_epoch, Backend, DarkIngressAuthority,
    DarkIngressAuthorityError, Database, DatabaseDriver, PhysicalAttestationConfig,
    TransactionBounds,
};
use crate::ingress_substrate::{EnvelopeVersion, MessageEnvelope};

const PIN_LAB_POSTGRES_DDL: &str = r#"
CREATE TABLE __PIN_SCHEMA__.ingress_admission_pins (
    message_key UUID NOT NULL,
    receipt_kind INTEGER NOT NULL,
    semantic_identity_hash BYTEA NOT NULL CHECK (octet_length(semantic_identity_hash) = 32),
    resource TEXT NOT NULL,
    admission_id UUID NOT NULL UNIQUE,
    payload TEXT NOT NULL,
    received_at TEXT NOT NULL,
    PRIMARY KEY (message_key, receipt_kind, semantic_identity_hash, resource)
);
CREATE FUNCTION __PIN_SCHEMA__.ingress_admission_pins_immutable() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    RAISE EXCEPTION 'laboratory admission pins are immutable';
END $$;
CREATE TRIGGER ingress_admission_pins_immutable_row
BEFORE UPDATE OR DELETE ON __PIN_SCHEMA__.ingress_admission_pins FOR EACH ROW
EXECUTE FUNCTION __PIN_SCHEMA__.ingress_admission_pins_immutable();
CREATE TRIGGER ingress_admission_pins_immutable_truncate
BEFORE TRUNCATE ON __PIN_SCHEMA__.ingress_admission_pins FOR EACH STATEMENT
EXECUTE FUNCTION __PIN_SCHEMA__.ingress_admission_pins_immutable();
"#;

/// Render unregistered laboratory DDL for the configured attested schema.
/// The schema owner installs this only in a disposable test database.
pub fn pin_lab_postgres_ddl(expected: &PhysicalAttestationConfig) -> String {
    PIN_LAB_POSTGRES_DDL.replace(
        "__PIN_SCHEMA__",
        &quote_identifier(&expected.postgres().schema.name.0),
    )
}

/// Unregistered laboratory schema for the shared SQLite runtime database.
pub const PIN_LAB_SQLITE_DDL: &str = r#"
-- A hidden rowid would provide a third REPLACE collision target.
CREATE TABLE main.ingress_admission_pins (
    message_key TEXT NOT NULL,
    receipt_kind INTEGER NOT NULL,
    semantic_identity_hash BLOB NOT NULL CHECK (length(semantic_identity_hash) = 32),
    resource TEXT NOT NULL,
    admission_id TEXT NOT NULL UNIQUE,
    payload TEXT NOT NULL,
    received_at TEXT NOT NULL,
    PRIMARY KEY (message_key, receipt_kind, semantic_identity_hash, resource)
) WITHOUT ROWID;
-- REPLACE can delete conflicting rows without firing DELETE triggers when
-- recursive_triggers is disabled. Reject both possible unique collisions first.
CREATE TRIGGER main.ingress_admission_pins_immutable_insert
BEFORE INSERT ON ingress_admission_pins
WHEN EXISTS (
    SELECT 1 FROM main.ingress_admission_pins
    WHERE admission_id = NEW.admission_id
       OR (message_key = NEW.message_key AND receipt_kind = NEW.receipt_kind
           AND semantic_identity_hash = NEW.semantic_identity_hash AND resource = NEW.resource)
)
BEGIN SELECT RAISE(ABORT, 'laboratory admission pins are immutable'); END;
CREATE TRIGGER main.ingress_admission_pins_immutable_update
BEFORE UPDATE ON ingress_admission_pins BEGIN SELECT RAISE(ABORT, 'laboratory admission pins are immutable'); END;
CREATE TRIGGER main.ingress_admission_pins_immutable_delete
BEFORE DELETE ON ingress_admission_pins BEGIN SELECT RAISE(ABORT, 'laboratory admission pins are immutable'); END;
"#;

/// Immutable pending laboratory pin. The receipt time must equal the
/// canonical ingress row's recorded `created_at`; the same identity cannot be
/// reused with a different ID, payload or receipt time.
#[derive(Debug, Clone)]
pub struct AdmissionPin {
    pub admission_id: uuid::Uuid,
    pub key: SmIngressAppendKey,
    pub payload: Message,
    pub received_at: DateTime<Utc>,
}

/// Whether a new pin committed or the same immutable pin already existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    Created,
    Existing,
}

/// A rejection is distinct from database or indeterminate commit failures.
#[derive(Debug, Error)]
pub enum AdmissionPinLabError {
    #[error("the recorded direct-route obligation does not authorize this pin")]
    Rejected,
    #[error("the pin identity conflicts with an existing admission")]
    Conflict,
    #[error(transparent)]
    Authority(#[from] DarkIngressAuthorityError),
}

/// Dark test-only pin creation and recovery. No raw connection or transaction
/// escapes. This is not an SM writer or a production admission authority.
pub struct AdmissionPinLab {
    authority: DarkIngressAuthority,
}

impl AdmissionPinLab {
    /// Use a private pool that attests each physical PostgreSQL checkout.
    pub fn postgres(
        options: PgConnectOptions,
        expected: PhysicalAttestationConfig,
        pool_size: u32,
        bounds: TransactionBounds,
    ) -> Result<Self, DarkIngressAuthorityError> {
        Ok(Self {
            authority: DarkIngressAuthority::postgres(options, expected, pool_size, bounds)?,
        })
    }

    /// Use the exact pool of the supplied SQLite runtime handle.
    pub fn sqlite(
        runtime: &Database,
        bounds: TransactionBounds,
    ) -> Result<Self, DarkIngressAuthorityError> {
        if runtime.driver() != DatabaseDriver::Sqlite {
            return Err(DarkIngressAuthorityError::Database);
        }
        Ok(Self {
            authority: DarkIngressAuthority {
                backend: Backend::Sqlite(runtime.clone()),
                bounds,
            },
        })
    }

    /// Validate and persist an immutable pin under the canonical row lock.
    /// Only a confirmed commit returns an outcome.
    pub async fn admit(&self, pin: &AdmissionPin) -> Result<PinOutcome, AdmissionPinLabError> {
        match &self.authority.backend {
            Backend::Postgres {
                pool,
                schema,
                schema_oid,
            } => {
                let mut conn = self.authority.acquire_postgres(pool).await?;
                let mut prepared = tokio::time::timeout(
                    self.authority.bounds.acquisition,
                    prepare_postgres(&mut conn, schema, *schema_oid, self.authority.bounds),
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Timeout)??;
                let outcome = admit_postgres(&mut prepared.transaction, schema, pin).await?;
                prepared.commit().await?;
                Ok(outcome)
            }
            Backend::Sqlite(db) => {
                let mut tx = self.authority.begin_sqlite(db).await?;
                read_sqlite_epoch(&mut tx).await?;
                let outcome = admit_sqlite(&mut tx, pin).await?;
                tx.commit()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)?;
                Ok(outcome)
            }
        }
    }

    /// Recover an exact pending pin without acquiring a nested pool checkout.
    pub async fn lookup(
        &self,
        key: &SmIngressAppendKey,
    ) -> Result<Option<AdmissionPin>, AdmissionPinLabError> {
        match &self.authority.backend {
            Backend::Postgres {
                pool,
                schema,
                schema_oid,
            } => {
                let mut conn = self.authority.acquire_postgres(pool).await?;
                let mut prepared = tokio::time::timeout(
                    self.authority.bounds.acquisition,
                    prepare_postgres(&mut conn, schema, *schema_oid, self.authority.bounds),
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Timeout)??;
                let pin = lookup_postgres(&mut prepared.transaction, schema, key).await?;
                prepared.commit().await?;
                Ok(pin)
            }
            Backend::Sqlite(db) => {
                let mut tx = self.authority.begin_sqlite(db).await?;
                read_sqlite_epoch(&mut tx).await?;
                let pin = lookup_sqlite(&mut tx, key).await?;
                tx.commit()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)?;
                Ok(pin)
            }
        }
    }
}

fn decode_pin(
    key: &SmIngressAppendKey,
    admission_id: uuid::Uuid,
    payload: &str,
    received_at: &str,
) -> Result<AdmissionPin, AdmissionPinLabError> {
    let payload = waddle_xmpp::parser::message_from_string(payload)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let received_at = DateTime::parse_from_rfc3339(received_at)
        .map_err(|_| DarkIngressAuthorityError::Database)?
        .with_timezone(&Utc);
    Ok(AdmissionPin {
        admission_id,
        key: key.clone(),
        payload,
        received_at,
    })
}

fn identical(existing: &AdmissionPin, proposed: &AdmissionPin) -> bool {
    existing.admission_id == proposed.admission_id
        && existing.key == proposed.key
        && existing.payload == proposed.payload
        && existing.received_at == proposed.received_at
}

fn encode_payload(payload: &Message) -> Result<String, AdmissionPinLabError> {
    let encoded = waddle_xmpp::parser::message_to_string(payload)
        .map_err(|_| AdmissionPinLabError::Rejected)?;
    let decoded = waddle_xmpp::parser::message_from_string(&encoded)
        .map_err(|_| AdmissionPinLabError::Rejected)?;
    if &decoded != payload {
        return Err(AdmissionPinLabError::Rejected);
    }
    Ok(encoded)
}

// Deliberately narrower than all runtime routes: this first slice requires a
// frozen direct payload identical to the canonical envelope, an exact effect
// identity, and the recorded full-JID fanout. Other shapes fail closed.
fn validate(
    pin: &AdmissionPin,
    envelope_version: i64,
    envelope_bytes: Vec<u8>,
    intent_kind: i32,
    intent_version: i64,
    intent_bytes: &[u8],
    settled: bool,
) -> Result<(), AdmissionPinLabError> {
    if intent_version != 1 || settled {
        return Err(AdmissionPinLabError::Rejected);
    }
    let envelope = MessageEnvelope::from_pin_lab_storage(envelope_version, envelope_bytes)
        .map_err(|_| AdmissionPinLabError::Rejected)?;
    if envelope.version() != EnvelopeVersion::V1 {
        return Err(AdmissionPinLabError::Rejected);
    }
    let intent = IngressEffectIntent::decode_v1(intent_kind, intent_bytes)
        .map_err(|_| AdmissionPinLabError::Rejected)?;
    let receipt =
        crate::ingress::receipt_key(&intent).map_err(|_| AdmissionPinLabError::Rejected)?;
    if receipt.kind.to_storage() != pin.key.kind.to_storage()
        || receipt.semantic_identity_hash != pin.key.semantic_identity_hash
    {
        return Err(AdmissionPinLabError::Rejected);
    }
    let IngressEffectIntent::RouteDirect {
        recipient,
        fanout,
        prepared: Some(prepared),
        ..
    } = intent
    else {
        return Err(AdmissionPinLabError::Rejected);
    };
    if prepared.message() != &pin.payload
        || envelope.message() != &pin.payload
        || pin.payload.from.is_none()
        || !pin
            .payload
            .to
            .as_ref()
            .is_some_and(|to| to.to_bare() == recipient)
        || pin.key.resource.to_bare() != recipient
        || !fanout.contains(&pin.key.resource)
        || fanout.iter().any(|target| target.to_bare() != recipient)
        || !matches!(
            pin.payload.type_,
            xmpp_parsers::message::MessageType::Chat
                | xmpp_parsers::message::MessageType::Normal
                | xmpp_parsers::message::MessageType::Headline
        )
    {
        return Err(AdmissionPinLabError::Rejected);
    }
    Ok(())
}

async fn lookup_postgres(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    schema: &str,
    key: &SmIngressAppendKey,
) -> Result<Option<AdmissionPin>, AdmissionPinLabError> {
    let sql = format!("SELECT admission_id, payload, received_at FROM {}.ingress_admission_pins WHERE message_key = $1 AND receipt_kind = $2 AND semantic_identity_hash = $3 AND resource = $4", quote_identifier(schema));
    let row = sqlx::query(&sql)
        .bind(key.message_key.to_storage())
        .bind(key.kind.to_storage())
        .bind(key.semantic_identity_hash.as_slice())
        .bind(key.resource.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(super::postgres_error)?;
    row.map(|row| {
        let id: uuid::Uuid = row
            .try_get(0)
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        let payload: String = row
            .try_get(1)
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        let time: String = row
            .try_get(2)
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        decode_pin(key, id, &payload, &time)
    })
    .transpose()
}

async fn lookup_sqlite(
    tx: &mut crate::db::Transaction<'_>,
    key: &SmIngressAppendKey,
) -> Result<Option<AdmissionPin>, AdmissionPinLabError> {
    let mut rows = tx.query("SELECT admission_id, payload, received_at FROM main.ingress_admission_pins WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ?", crate::db_params![key.message_key.to_storage().to_string(), key.kind.to_storage(), key.semantic_identity_hash.to_vec(), key.resource.to_string()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
    else {
        return Ok(None);
    };
    let id: String = row
        .get(0)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let payload: String = row
        .get(1)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let time: String = row
        .get(2)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    decode_pin(
        key,
        id.parse()
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        &payload,
        &time,
    )
    .map(Some)
}

async fn admit_postgres(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    schema: &str,
    pin: &AdmissionPin,
) -> Result<PinOutcome, AdmissionPinLabError> {
    let q = quote_identifier(schema);
    let canonical = sqlx::query(&format!("SELECT envelope_version::bigint, envelope, created_at FROM {q}.ingress_messages WHERE message_key = $1 FOR UPDATE"))
        .bind(pin.key.message_key.to_storage()).fetch_optional(&mut **tx).await.map_err(super::postgres_error)?.ok_or(AdmissionPinLabError::Rejected)?;
    let existing = lookup_postgres(tx, schema, &pin.key).await?;
    if existing
        .as_ref()
        .is_some_and(|existing| !identical(existing, pin))
    {
        return Err(AdmissionPinLabError::Conflict);
    }
    let version: Option<i64> = canonical
        .try_get(0)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let envelope: Option<Vec<u8>> = canonical
        .try_get(1)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let recorded_at: DateTime<Utc> = canonical
        .try_get(2)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    if pin.received_at != recorded_at {
        return Err(AdmissionPinLabError::Rejected);
    }
    let row = sqlx::query(&format!(
        "SELECT i.kind, i.payload_version::bigint, i.payload, \
         (EXISTS (SELECT 1 FROM {q}.ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash) \
          OR EXISTS (SELECT 1 FROM {q}.ingress_delivery_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash AND r.resource = $4) \
          OR EXISTS (SELECT 1 FROM {q}.ingress_carbon_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash AND r.recipient = $4) \
          OR EXISTS (SELECT 1 FROM {q}.ingress_send_attempts a WHERE a.message_key = i.message_key AND a.kind = i.kind AND a.semantic_identity_hash = i.semantic_identity_hash AND a.recipient = $4) \
          OR EXISTS (SELECT 1 FROM {q}.sm_ingress_appends a WHERE a.message_key = i.message_key::text AND a.receipt_kind = i.kind AND a.semantic_identity_hash = i.semantic_identity_hash AND a.resource = $4)) \
         FROM {q}.ingress_effect_intents i WHERE i.message_key = $1 AND i.kind = $2 AND i.semantic_identity_hash = $3"
    )).bind(pin.key.message_key.to_storage()).bind(pin.key.kind.to_storage()).bind(pin.key.semantic_identity_hash.as_slice()).bind(pin.key.resource.to_string())
        .fetch_optional(&mut **tx).await.map_err(super::postgres_error)?.ok_or(AdmissionPinLabError::Rejected)?;
    validate(
        pin,
        version.ok_or(AdmissionPinLabError::Rejected)?,
        envelope.ok_or(AdmissionPinLabError::Rejected)?,
        row.try_get(0)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        row.try_get(1)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        &row.try_get::<Vec<u8>, _>(2)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        row.try_get::<bool, _>(3)
            .map_err(|_| DarkIngressAuthorityError::Database)?
            && existing.is_none(),
    )?;
    if let Some(existing) = existing {
        return if identical(&existing, pin) {
            Ok(PinOutcome::Existing)
        } else {
            Err(AdmissionPinLabError::Conflict)
        };
    }
    let payload = encode_payload(&pin.payload)?;
    let inserted = sqlx::query(&format!("INSERT INTO {q}.ingress_admission_pins (message_key, receipt_kind, semantic_identity_hash, resource, admission_id, payload, received_at) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING"))
        .bind(pin.key.message_key.to_storage()).bind(pin.key.kind.to_storage()).bind(pin.key.semantic_identity_hash.as_slice()).bind(pin.key.resource.to_string()).bind(pin.admission_id).bind(payload).bind(pin.received_at.to_rfc3339()).execute(&mut **tx).await.map_err(super::postgres_error)?.rows_affected();
    if inserted != 1 {
        return Err(AdmissionPinLabError::Conflict);
    }
    sqlx::query(&format!(
        "UPDATE {q}.ingress_messages SET retention_eligible_at = NULL WHERE message_key = $1"
    ))
    .bind(pin.key.message_key.to_storage())
    .execute(&mut **tx)
    .await
    .map_err(super::postgres_error)?;
    Ok(PinOutcome::Created)
}

async fn admit_sqlite(
    tx: &mut crate::db::Transaction<'_>,
    pin: &AdmissionPin,
) -> Result<PinOutcome, AdmissionPinLabError> {
    let mut rows = tx
        .query(
            "SELECT envelope_version, envelope, created_at FROM main.ingress_messages WHERE message_key = ?",
            crate::db_params![pin.key.message_key.to_storage().to_string()],
        )
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let canonical = rows
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
        .ok_or(AdmissionPinLabError::Rejected)?;
    drop(rows);
    let existing = lookup_sqlite(tx, &pin.key).await?;
    if existing
        .as_ref()
        .is_some_and(|existing| !identical(existing, pin))
    {
        return Err(AdmissionPinLabError::Conflict);
    }
    let version: Option<i64> = canonical
        .get(0)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let envelope: Option<Vec<u8>> = canonical
        .get(1)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let recorded_at: String = canonical
        .get(2)
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let recorded_at = DateTime::parse_from_rfc3339(&recorded_at)
        .map_err(|_| DarkIngressAuthorityError::Database)?
        .with_timezone(&Utc);
    if pin.received_at != recorded_at {
        return Err(AdmissionPinLabError::Rejected);
    }
    let mut rows = tx.query(
        "SELECT i.kind, i.payload_version, i.payload, \
         (EXISTS (SELECT 1 FROM main.ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash) \
          OR EXISTS (SELECT 1 FROM main.ingress_delivery_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash AND r.resource = ?1) \
          OR EXISTS (SELECT 1 FROM main.ingress_carbon_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash AND r.recipient = ?1) \
          OR EXISTS (SELECT 1 FROM main.ingress_send_attempts a WHERE a.message_key = i.message_key AND a.kind = i.kind AND a.semantic_identity_hash = i.semantic_identity_hash AND a.recipient = ?1) \
          OR EXISTS (SELECT 1 FROM main.sm_ingress_appends a WHERE a.message_key = i.message_key AND a.receipt_kind = i.kind AND a.semantic_identity_hash = i.semantic_identity_hash AND a.resource = ?1)) \
         FROM main.ingress_effect_intents i WHERE i.message_key = ?2 AND i.kind = ?3 AND i.semantic_identity_hash = ?4",
        crate::db_params![pin.key.resource.to_string(), pin.key.message_key.to_storage().to_string(), pin.key.kind.to_storage(), pin.key.semantic_identity_hash.to_vec()],
    ).await.map_err(|_| DarkIngressAuthorityError::Database)?;
    let row = rows
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
        .ok_or(AdmissionPinLabError::Rejected)?;
    drop(rows);
    let kind = i32::try_from(
        row.get::<i64>(0)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
    )
    .map_err(|_| DarkIngressAuthorityError::Database)?;
    validate(
        pin,
        version.ok_or(AdmissionPinLabError::Rejected)?,
        envelope.ok_or(AdmissionPinLabError::Rejected)?,
        kind,
        row.get(1)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        &row.get::<Vec<u8>>(2)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        row.get::<bool>(3)
            .map_err(|_| DarkIngressAuthorityError::Database)?
            && existing.is_none(),
    )?;
    if let Some(existing) = existing {
        return if identical(&existing, pin) {
            Ok(PinOutcome::Existing)
        } else {
            Err(AdmissionPinLabError::Conflict)
        };
    }
    // BEGIN IMMEDIATE excludes concurrent writers. Avoid invoking the
    // immutability trigger for a known ID collision so it remains a typed
    // conflict rather than an opaque database error.
    let mut conflicts = tx
        .query(
            "SELECT 1 FROM main.ingress_admission_pins WHERE admission_id = ?",
            crate::db_params![pin.admission_id.to_string()],
        )
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    if conflicts
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
        .is_some()
    {
        return Err(AdmissionPinLabError::Conflict);
    }
    drop(conflicts);
    let payload = encode_payload(&pin.payload)?;
    let inserted = tx.execute("INSERT INTO main.ingress_admission_pins (message_key, receipt_kind, semantic_identity_hash, resource, admission_id, payload, received_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING", crate::db_params![pin.key.message_key.to_storage().to_string(), pin.key.kind.to_storage(), pin.key.semantic_identity_hash.to_vec(), pin.key.resource.to_string(), pin.admission_id.to_string(), payload, pin.received_at.to_rfc3339()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
    if inserted != 1 {
        return Err(AdmissionPinLabError::Conflict);
    }
    tx.execute(
        "UPDATE main.ingress_messages SET retention_eligible_at = NULL WHERE message_key = ?",
        crate::db_params![pin.key.message_key.to_storage().to_string()],
    )
    .await
    .map_err(|_| DarkIngressAuthorityError::Database)?;
    Ok(PinOutcome::Created)
}
