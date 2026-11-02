//! Immutable observations of existing SM ledger state. This never allocates,
//! sends, promotes, or repairs custody and is not used by runtime writers.

use sqlx::{Postgres, Row, Transaction};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::SmIngressAppendKey;

use super::super::{
    prepare_postgres, quote_identifier, read_sqlite_epoch, Backend, DarkIngressAuthorityError,
};
use super::{
    decode_pin, lookup_sqlite, AdmissionPin, AdmissionPinLab, AdmissionPinLabError, PinOutcome,
};

/// The exact stream and wrapping sequence already recorded for an admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinAllocationBinding {
    pub admission_id: uuid::Uuid,
    pub accepting_stream: SmSessionId,
    pub sequence: u32,
}

/// Positive terminal evidence recorded by the existing SM ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinTerminalDisposition {
    Acknowledged,
    Promoted,
    Tombstoned,
}

impl PinTerminalDisposition {
    fn storage(self) -> i64 {
        match self {
            Self::Acknowledged => 1,
            Self::Promoted => 2,
            Self::Tombstoned => 3,
        }
    }

    fn from_storage(value: i64) -> Result<Self, AdmissionPinLabError> {
        match value {
            1 => Ok(Self::Acknowledged),
            2 => Ok(Self::Promoted),
            3 => Ok(Self::Tombstoned),
            _ => Err(DarkIngressAuthorityError::Database.into()),
        }
    }
}

/// State recorded in the lab, not a claim about the current live SM session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinState {
    Admitted,
    BoundPending(PinAllocationBinding),
    Terminal {
        binding: PinAllocationBinding,
        disposition: PinTerminalDisposition,
    },
}

/// The original recoverable payload and the last immutable lab observation.
#[derive(Debug, Clone)]
pub struct PinRecovery {
    pub pin: AdmissionPin,
    pub state: PinState,
}

#[derive(Clone, Copy)]
enum Transition<'a> {
    Bind(&'a PinAllocationBinding),
    Settle(&'a PinAllocationBinding, PinTerminalDisposition),
}

impl<'a> Transition<'a> {
    fn binding(self) -> &'a PinAllocationBinding {
        match self {
            Self::Bind(binding) | Self::Settle(binding, _) => binding,
        }
    }
}

enum Decision {
    Existing,
    InsertBinding,
    InsertSettlement(PinTerminalDisposition),
}

struct LedgerProof {
    stream: SmSessionId,
    sequence: u32,
    payload: xmpp_parsers::message::Message,
    receipt_ms: i64,
    disposition: i64,
}

fn ledger_proof(
    stream: String,
    sequence: i64,
    payload: String,
    receipt_ms: i64,
    disposition: i64,
) -> Result<LedgerProof, AdmissionPinLabError> {
    if !(0..=3).contains(&disposition) {
        return Err(DarkIngressAuthorityError::Database.into());
    }
    Ok(LedgerProof {
        stream: SmSessionId::try_from_wire(stream)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        sequence: u32::try_from(sequence).map_err(|_| DarkIngressAuthorityError::Database)?,
        payload: waddle_xmpp::parser::message_from_string(&payload)
            .map_err(|_| AdmissionPinLabError::Rejected)?,
        receipt_ms,
        disposition,
    })
}

fn decide(
    pin: &AdmissionPin,
    existing: Option<&PinAllocationBinding>,
    ledger: Option<LedgerProof>,
    settled: Option<PinTerminalDisposition>,
    transition: Transition<'_>,
) -> Result<Decision, AdmissionPinLabError> {
    let binding = transition.binding();
    SmSessionId::try_from_wire(binding.accepting_stream.as_str())
        .map_err(|_| AdmissionPinLabError::Rejected)?;
    if pin.admission_id != binding.admission_id
        || existing.is_some_and(|recorded| recorded != binding)
    {
        return Err(AdmissionPinLabError::Conflict);
    }
    if matches!(transition, Transition::Settle(..)) && existing.is_none() {
        return Err(AdmissionPinLabError::Rejected);
    }
    let ledger = ledger.ok_or(AdmissionPinLabError::Rejected)?;
    // The current SM ledger stores milliseconds, not the pin's full precision.
    if ledger.stream != binding.accepting_stream
        || ledger.sequence != binding.sequence
        || ledger.payload != pin.payload
        || ledger.receipt_ms != pin.received_at.timestamp_millis()
    {
        return Err(AdmissionPinLabError::Rejected);
    }
    match transition {
        Transition::Bind(_) => {
            if existing.is_some() {
                Ok(Decision::Existing)
            } else if settled.is_some() {
                Err(DarkIngressAuthorityError::Database.into())
            } else {
                Ok(Decision::InsertBinding)
            }
        }
        Transition::Settle(_, disposition) => {
            if settled.is_some_and(|recorded| recorded != disposition) {
                return Err(AdmissionPinLabError::Conflict);
            }
            if ledger.disposition != disposition.storage() {
                return Err(AdmissionPinLabError::Rejected);
            }
            if settled.is_some() {
                Ok(Decision::Existing)
            } else {
                Ok(Decision::InsertSettlement(disposition))
            }
        }
    }
}

impl AdmissionPinLab {
    /// Bind a pin only to an existing exact SM ledger allocation. The ledger
    /// stores receipt time in milliseconds, so comparison uses the pin's
    /// millisecond projection while recovery retains its full timestamp.
    pub async fn bind_existing_allocation(
        &self,
        key: &SmIngressAppendKey,
        binding: &PinAllocationBinding,
    ) -> Result<PinOutcome, AdmissionPinLabError> {
        self.transition(key, Transition::Bind(binding)).await
    }

    /// Record only the exact positive terminal disposition in the ledger.
    pub async fn settle_existing_allocation(
        &self,
        key: &SmIngressAppendKey,
        binding: &PinAllocationBinding,
        disposition: PinTerminalDisposition,
    ) -> Result<PinOutcome, AdmissionPinLabError> {
        self.transition(key, Transition::Settle(binding, disposition))
            .await
    }

    async fn transition(
        &self,
        key: &SmIngressAppendKey,
        transition: Transition<'_>,
    ) -> Result<PinOutcome, AdmissionPinLabError> {
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
                let result =
                    transition_postgres(&mut prepared.transaction, schema, key, transition).await?;
                prepared.commit().await?;
                Ok(result)
            }
            Backend::Sqlite(db) => {
                let mut tx = self.authority.begin_sqlite(db).await?;
                read_sqlite_epoch(&mut tx).await?;
                let result = transition_sqlite(&mut tx, key, transition).await?;
                tx.commit()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)?;
                Ok(result)
            }
        }
    }

    /// Recover immutable records even if the canonical or ledger row has gone.
    /// BoundPending means no lab settlement exists; it does not assert that the
    /// current ledger still has pending custody.
    pub async fn recover_state(
        &self,
        key: &SmIngressAppendKey,
    ) -> Result<Option<PinRecovery>, AdmissionPinLabError> {
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
                lock_canonical_postgres(&mut prepared.transaction, schema, key).await?;
                let pin = lock_pin_postgres(&mut prepared.transaction, schema, key).await?;
                let result = if let Some(pin) = pin {
                    let binding =
                        binding_postgres(&mut prepared.transaction, schema, pin.admission_id)
                            .await?;
                    let settled =
                        settlement_postgres(&mut prepared.transaction, schema, pin.admission_id)
                            .await?;
                    Some(recovery(pin, binding, settled)?)
                } else {
                    None
                };
                prepared.commit().await?;
                Ok(result)
            }
            Backend::Sqlite(db) => {
                let mut tx = self.authority.begin_sqlite(db).await?;
                read_sqlite_epoch(&mut tx).await?;
                lock_canonical_sqlite(&mut tx, key).await?;
                let pin = lookup_sqlite(&mut tx, key).await?;
                let result = if let Some(pin) = pin {
                    let binding = binding_sqlite(&mut tx, pin.admission_id).await?;
                    let settled = settlement_sqlite(&mut tx, pin.admission_id).await?;
                    Some(recovery(pin, binding, settled)?)
                } else {
                    None
                };
                tx.commit()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)?;
                Ok(result)
            }
        }
    }
}

fn recovery(
    pin: AdmissionPin,
    binding: Option<PinAllocationBinding>,
    settled: Option<PinTerminalDisposition>,
) -> Result<PinRecovery, AdmissionPinLabError> {
    let state = match (binding, settled) {
        (None, None) => PinState::Admitted,
        (Some(binding), None) => PinState::BoundPending(binding),
        (Some(binding), Some(disposition)) => PinState::Terminal {
            binding,
            disposition,
        },
        (None, Some(_)) => return Err(DarkIngressAuthorityError::Database.into()),
    };
    Ok(PinRecovery { pin, state })
}

async fn lock_canonical_postgres(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    key: &SmIngressAppendKey,
) -> Result<bool, AdmissionPinLabError> {
    Ok(sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key = $1 FOR UPDATE",
        quote_identifier(schema)
    ))
    .bind(key.message_key.to_storage())
    .fetch_optional(&mut **tx)
    .await
    .map_err(super::super::postgres_error)?
    .is_some())
}

async fn lock_pin_postgres(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    key: &SmIngressAppendKey,
) -> Result<Option<AdmissionPin>, AdmissionPinLabError> {
    let row = sqlx::query(&format!("SELECT admission_id, payload, received_at FROM {}.ingress_admission_pins WHERE message_key = $1 AND receipt_kind = $2 AND semantic_identity_hash = $3 AND resource = $4 FOR UPDATE", quote_identifier(schema)))
        .bind(key.message_key.to_storage()).bind(key.kind.to_storage()).bind(key.semantic_identity_hash.as_slice()).bind(key.resource.to_string())
        .fetch_optional(&mut **tx).await.map_err(super::super::postgres_error)?;
    row.map(|row| {
        let id = row
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

fn decode_binding(
    id: uuid::Uuid,
    stream: String,
    sequence: i64,
) -> Result<PinAllocationBinding, AdmissionPinLabError> {
    Ok(PinAllocationBinding {
        admission_id: id,
        accepting_stream: SmSessionId::try_from_wire(stream)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        sequence: u32::try_from(sequence).map_err(|_| DarkIngressAuthorityError::Database)?,
    })
}

async fn binding_postgres(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    id: uuid::Uuid,
) -> Result<Option<PinAllocationBinding>, AdmissionPinLabError> {
    let row = sqlx::query(&format!("SELECT accepting_stream_id, sequence FROM {}.ingress_pin_allocations WHERE admission_id = $1 FOR UPDATE", quote_identifier(schema)))
        .bind(id).fetch_optional(&mut **tx).await.map_err(super::super::postgres_error)?;
    row.map(|row| {
        decode_binding(
            id,
            row.try_get(0)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
            row.try_get(1)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
        )
    })
    .transpose()
}

async fn settlement_postgres(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    id: uuid::Uuid,
) -> Result<Option<PinTerminalDisposition>, AdmissionPinLabError> {
    let value = sqlx::query_scalar::<_, i64>(&format!("SELECT disposition::bigint FROM {}.ingress_pin_settlements WHERE admission_id = $1 FOR UPDATE", quote_identifier(schema)))
        .bind(id).fetch_optional(&mut **tx).await.map_err(super::super::postgres_error)?;
    value.map(PinTerminalDisposition::from_storage).transpose()
}

async fn ledger_postgres(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    key: &SmIngressAppendKey,
) -> Result<Option<LedgerProof>, AdmissionPinLabError> {
    let row = sqlx::query(&format!("SELECT accepting_stream_id, sequence, custody_payload, original_receipt_at_ms, disposition::bigint FROM {}.sm_ingress_appends WHERE message_key = $1 AND receipt_kind = $2 AND semantic_identity_hash = $3 AND resource = $4 FOR UPDATE", quote_identifier(schema)))
        .bind(key.message_key.to_storage().to_string()).bind(key.kind.to_storage()).bind(key.semantic_identity_hash.as_slice()).bind(key.resource.to_string())
        .fetch_optional(&mut **tx).await.map_err(super::super::postgres_error)?;
    row.map(|row| {
        ledger_proof(
            row.try_get(0)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
            row.try_get(1)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
            row.try_get(2)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
            row.try_get(3)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
            row.try_get(4)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
        )
    })
    .transpose()
}

async fn transition_postgres(
    tx: &mut Transaction<'_, Postgres>,
    schema: &str,
    key: &SmIngressAppendKey,
    transition: Transition<'_>,
) -> Result<PinOutcome, AdmissionPinLabError> {
    if !lock_canonical_postgres(tx, schema, key).await? {
        return Err(AdmissionPinLabError::Rejected);
    }
    let pin = lock_pin_postgres(tx, schema, key)
        .await?
        .ok_or(AdmissionPinLabError::Rejected)?;
    let existing = binding_postgres(tx, schema, pin.admission_id).await?;
    let ledger = ledger_postgres(tx, schema, key).await?;
    let settled = settlement_postgres(tx, schema, pin.admission_id).await?;
    let decision = decide(&pin, existing.as_ref(), ledger, settled, transition)?;
    match decision {
        Decision::Existing => Ok(PinOutcome::Existing),
        Decision::InsertBinding => {
            let binding = transition.binding();
            sqlx::query(&format!("INSERT INTO {}.ingress_pin_allocations (admission_id, accepting_stream_id, sequence) VALUES ($1, $2, $3)", quote_identifier(schema)))
                .bind(pin.admission_id).bind(binding.accepting_stream.as_str()).bind(i64::from(binding.sequence)).execute(&mut **tx).await.map_err(super::super::postgres_error)?;
            Ok(PinOutcome::Created)
        }
        Decision::InsertSettlement(disposition) => {
            sqlx::query(&format!("INSERT INTO {}.ingress_pin_settlements (admission_id, disposition) VALUES ($1, $2)", quote_identifier(schema)))
                .bind(pin.admission_id).bind(disposition.storage() as i32).execute(&mut **tx).await.map_err(super::super::postgres_error)?;
            Ok(PinOutcome::Created)
        }
    }
}

async fn lock_canonical_sqlite(
    tx: &mut crate::db::Transaction<'_>,
    key: &SmIngressAppendKey,
) -> Result<bool, AdmissionPinLabError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM main.ingress_messages WHERE message_key = ?",
            crate::db_params![key.message_key.to_storage().to_string()],
        )
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    Ok(rows
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
        .is_some())
}

async fn binding_sqlite(
    tx: &mut crate::db::Transaction<'_>,
    id: uuid::Uuid,
) -> Result<Option<PinAllocationBinding>, AdmissionPinLabError> {
    let mut rows = tx.query("SELECT accepting_stream_id, sequence FROM main.ingress_pin_allocations WHERE admission_id = ?", crate::db_params![id.to_string()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
    else {
        return Ok(None);
    };
    decode_binding(
        id,
        row.get(0)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        row.get(1)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
    )
    .map(Some)
}

async fn settlement_sqlite(
    tx: &mut crate::db::Transaction<'_>,
    id: uuid::Uuid,
) -> Result<Option<PinTerminalDisposition>, AdmissionPinLabError> {
    let mut rows = tx
        .query(
            "SELECT disposition FROM main.ingress_pin_settlements WHERE admission_id = ?",
            crate::db_params![id.to_string()],
        )
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
    else {
        return Ok(None);
    };
    PinTerminalDisposition::from_storage(
        row.get(0)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
    )
    .map(Some)
}

async fn ledger_sqlite(
    tx: &mut crate::db::Transaction<'_>,
    key: &SmIngressAppendKey,
) -> Result<Option<LedgerProof>, AdmissionPinLabError> {
    let mut rows = tx.query("SELECT accepting_stream_id, sequence, custody_payload, original_receipt_at_ms, disposition FROM main.sm_ingress_appends WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ?", crate::db_params![key.message_key.to_storage().to_string(), key.kind.to_storage(), key.semantic_identity_hash.to_vec(), key.resource.to_string()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
    else {
        return Ok(None);
    };
    ledger_proof(
        row.get(0)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        row.get(1)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        row.get(2)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        row.get(3)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
        row.get(4)
            .map_err(|_| DarkIngressAuthorityError::Database)?,
    )
    .map(Some)
}

async fn transition_sqlite(
    tx: &mut crate::db::Transaction<'_>,
    key: &SmIngressAppendKey,
    transition: Transition<'_>,
) -> Result<PinOutcome, AdmissionPinLabError> {
    if !lock_canonical_sqlite(tx, key).await? {
        return Err(AdmissionPinLabError::Rejected);
    }
    let pin = lookup_sqlite(tx, key)
        .await?
        .ok_or(AdmissionPinLabError::Rejected)?;
    let existing = binding_sqlite(tx, pin.admission_id).await?;
    let ledger = ledger_sqlite(tx, key).await?;
    let settled = settlement_sqlite(tx, pin.admission_id).await?;
    let decision = decide(&pin, existing.as_ref(), ledger, settled, transition)?;
    match decision {
        Decision::Existing => Ok(PinOutcome::Existing),
        Decision::InsertBinding => {
            let binding = transition.binding();
            tx.execute("INSERT INTO main.ingress_pin_allocations (admission_id, accepting_stream_id, sequence) VALUES (?, ?, ?)", crate::db_params![pin.admission_id.to_string(), binding.accepting_stream.as_str().to_string(), i64::from(binding.sequence)]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
            Ok(PinOutcome::Created)
        }
        Decision::InsertSettlement(disposition) => {
            tx.execute("INSERT INTO main.ingress_pin_settlements (admission_id, disposition) VALUES (?, ?)", crate::db_params![pin.admission_id.to_string(), disposition.storage()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
            Ok(PinOutcome::Created)
        }
    }
}
