//! Per-resource completion beneath a recorded remote carbon obligation.
use crate::db::Database;
use crate::ingress_substrate::MessageEnvelope;
use jid::FullJid;
use waddle_xmpp::ingress::MessageKey;

use super::{CanonicalMessageRepository, IngressUnitOfWork, IngressUowError};
use crate::db::DatabaseDriver;
use crate::ingress::decision::EffectReceiptKey;

pub(crate) struct CarbonReceiptRepository;

impl CarbonReceiptRepository {
    /// A lost owner reply must not erase a resource merely because its socket
    /// disappeared. Include unresolved starts and durable acceptance evidence.
    pub(crate) async fn attempted_resources(
        db: &Database,
        message: MessageKey,
        receipt: &EffectReceiptKey,
    ) -> Result<Vec<FullJid>, IngressUowError> {
        let mut tx = db.begin().await?;
        let (key, clock) = if tx.driver() == DatabaseDriver::Postgres {
            (
                "?::uuid",
                "(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint",
            )
        } else {
            (
                "?",
                "CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)",
            )
        };
        let sql = format!(
            "SELECT recipient FROM ingress_send_attempts WHERE message_key = {key} AND kind = ? AND semantic_identity_hash = ? AND (state = 2 OR expires_at_ms > {clock}) UNION SELECT resource FROM sm_ingress_appends WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? UNION SELECT recipient FROM ingress_carbon_receipts WHERE message_key = {key} AND kind = ? AND semantic_identity_hash = ?"
        );
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![
                    message.to_storage().to_string(),
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash.to_vec(),
                    message.to_storage().to_string(),
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash.to_vec(),
                    message.to_storage().to_string(),
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash.to_vec()
                ],
            )
            .await?;
        let mut recipients = Vec::new();
        while let Some(row) = rows.next().await? {
            let recipient: String = row.get(0)?;
            recipients.push(
                recipient
                    .parse()
                    .map_err(|_| IngressUowError::InvalidStoredCarbonRecipient)?,
            );
        }
        drop(rows);
        tx.commit().await?;
        Ok(recipients)
    }

    /// Read immutable canonical carbon authority without taking a writer lock.
    pub(crate) async fn load_authority(
        db: &Database,
        message: MessageKey,
    ) -> Result<
        (
            MessageEnvelope,
            Vec<waddle_xmpp::ingress::IngressEffectIntent>,
        ),
        IngressUowError,
    > {
        let mut tx = db.begin().await?;
        let envelope = crate::ingress_substrate::load_envelope(&mut tx, message)
            .await?
            .ok_or(IngressUowError::EffectIntentMessageMissing)?;
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            "SELECT kind::int, semantic_identity_hash, payload_version::int, payload FROM ingress_effect_intents WHERE message_key = ?::uuid"
        } else {
            "SELECT kind, semantic_identity_hash, payload_version, payload FROM ingress_effect_intents WHERE message_key = ?"
        };
        let mut rows = tx
            .query(sql, crate::db_params![message.to_storage().to_string()])
            .await?;
        let mut intents = Vec::new();
        while let Some(row) = rows.next().await? {
            let kind = i32::try_from(row.get::<i64>(0)?)
                .map_err(|_| IngressUowError::EffectIntentConflict)?;
            let hash: Vec<u8> = row.get(1)?;
            let version: i64 = row.get(2)?;
            let payload: Vec<u8> = row.get(3)?;
            if version != 1 {
                return Err(IngressUowError::EffectIntentConflict);
            }
            let intent = waddle_xmpp::ingress::IngressEffectIntent::decode_v1(kind, &payload)?;
            if hash.as_slice() != crate::ingress::receipt_key(&intent)?.semantic_identity_hash {
                return Err(IngressUowError::EffectIntentConflict);
            }
            intents.push(intent);
        }
        drop(rows);
        tx.commit().await?;
        Ok((envelope, intents))
    }

    pub(crate) async fn load(
        uow: &IngressUnitOfWork,
        message: MessageKey,
        receipt: &EffectReceiptKey,
    ) -> Result<Vec<FullJid>, IngressUowError> {
        let mut tx = uow.begin().await?;
        let recipients = if tx.uses_definer_api_candidate() {
            let (_, prepared) = tx.prepared_parts();
            let prepared = prepared
                .ok_or(crate::ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let api = crate::ingress_substrate::DefinerApiCandidateContext::from_prepared(prepared)
                .ok_or(crate::ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let query = format!(
                "SELECT * FROM {}(?::uuid, ?::integer, ?::bytea, ?::text, ?::integer)",
                api.qualified_function("waddle_ingress_list_carbon_receipts"),
            );
            let page_size = super::DEFINER_API_PAGE_SIZE;
            let mut after_resource: Option<String> = None;
            let mut recipients = Vec::new();
            loop {
                let mut rows = tx
                    .transaction_mut()
                    .query(
                        &query,
                        crate::db_params![
                            message.to_storage().to_string(),
                            receipt.kind.to_storage(),
                            receipt.semantic_identity_hash.to_vec(),
                            after_resource.clone(),
                            page_size
                        ],
                    )
                    .await?;
                let mut page_count = 0_i64;
                let mut last = None;
                while let Some(row) = rows.next().await? {
                    let recipient: String = row.get(0)?;
                    recipients.push(
                        recipient
                            .parse()
                            .map_err(|_| IngressUowError::InvalidStoredCarbonRecipient)?,
                    );
                    last = Some(recipient);
                    page_count += 1;
                }
                drop(rows);
                if page_count < page_size {
                    break;
                }
                after_resource = last;
            }
            recipients
        } else {
            let query = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
                "SELECT recipient FROM ingress_carbon_receipts WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? ORDER BY recipient"
            } else {
                "SELECT recipient FROM ingress_carbon_receipts WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? ORDER BY recipient"
            };
            let mut rows = tx
                .transaction_mut()
                .query(
                    query,
                    crate::db_params![
                        message.to_storage().to_string(),
                        receipt.kind.to_storage(),
                        receipt.semantic_identity_hash.to_vec()
                    ],
                )
                .await?;
            let mut recipients = Vec::new();
            while let Some(row) = rows.next().await? {
                let recipient: String = row.get(0)?;
                recipients.push(
                    recipient
                        .parse()
                        .map_err(|_| IngressUowError::InvalidStoredCarbonRecipient)?,
                );
            }
            drop(rows);
            recipients
        };
        tx.commit().await?;
        Ok(recipients)
    }

    pub(crate) async fn record(
        uow: &IngressUnitOfWork,
        message: MessageKey,
        receipt: &EffectReceiptKey,
        recipients: &[FullJid],
    ) -> Result<(), IngressUowError> {
        Self::record_progress(uow, message, receipt, recipients, false).await?;
        Ok(())
    }

    /// Serialize aggregate completion with every resource's start transition.
    /// A concurrent or vanished ambiguous target must keep the fanout pending.
    pub(crate) async fn record_progress(
        uow: &IngressUnitOfWork,
        message: MessageKey,
        receipt: &EffectReceiptKey,
        recipients: &[FullJid],
        complete: bool,
    ) -> Result<bool, IngressUowError> {
        let mut tx = uow.begin().await?;
        if !CanonicalMessageRepository::lock(&mut tx, message).await? {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        {
            let (transaction, prepared) = tx.prepared_parts();
            let candidate_api = match prepared
                .filter(|prepared| prepared.uses_definer_api_candidate())
            {
                Some(prepared) => Some(
                    crate::ingress_substrate::DefinerApiCandidateContext::from_prepared(prepared)
                        .ok_or(
                        crate::ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult,
                    )?,
                ),
                None => None,
            };
            let sql = if let Some(api) = &candidate_api {
                format!(
                    "SELECT {}(?::uuid, ?::integer, ?::bytea, ?::text)",
                    api.qualified_function("waddle_ingress_insert_carbon_receipt"),
                )
            } else if transaction.driver() == DatabaseDriver::Postgres {
                "INSERT INTO ingress_carbon_receipts (message_key, kind, semantic_identity_hash, recipient) VALUES (?::uuid, ?, ?, ?) ON CONFLICT DO NOTHING".to_owned()
            } else {
                "INSERT INTO ingress_carbon_receipts (message_key, kind, semantic_identity_hash, recipient) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING".to_owned()
            };
            for recipient in recipients {
                let params = crate::db_params![
                    message.to_storage().to_string(),
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash.to_vec(),
                    recipient.to_string()
                ];
                if candidate_api.is_some() {
                    let _ = super::candidate_boolean(transaction, &sql, params).await?;
                } else {
                    transaction.execute(&sql, params).await?;
                }
            }
        }
        let mut settled = !complete;
        if complete {
            let (key, clock) = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
                (
                    "?::uuid",
                    "(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint",
                )
            } else {
                (
                    "?",
                    "CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)",
                )
            };
            let pending = {
                let (transaction, prepared) = tx.prepared_parts();
                let candidate_api = if let Some(prepared) =
                    prepared.filter(|prepared| prepared.uses_definer_api_candidate())
                {
                    Some(
                        crate::ingress_substrate::DefinerApiCandidateContext::from_prepared(
                            prepared,
                        )
                        .ok_or(crate::ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?,
                    )
                } else {
                    None
                };
                let mut pending = Vec::new();
                if let Some(api) = candidate_api {
                    let query = format!(
                        "SELECT * FROM {}(?::uuid, ?::integer, ?::bytea, ?::text, ?::integer)",
                        api.qualified_function("waddle_ingress_list_pending_send_attempts"),
                    );
                    let page_size = super::DEFINER_API_PAGE_SIZE;
                    let mut after_resource: Option<String> = None;
                    loop {
                        let mut rows = transaction
                            .query(
                                &query,
                                crate::db_params![
                                    message.to_storage().to_string(),
                                    receipt.kind.to_storage(),
                                    receipt.semantic_identity_hash.to_vec(),
                                    after_resource.clone(),
                                    page_size
                                ],
                            )
                            .await?;
                        let mut page_count = 0_i64;
                        let mut last = None;
                        while let Some(row) = rows.next().await? {
                            let recipient: String = row.get(0)?;
                            pending.push(
                                recipient
                                    .parse()
                                    .map_err(|_| IngressUowError::InvalidStoredCarbonRecipient)?,
                            );
                            last = Some(recipient);
                            page_count += 1;
                        }
                        drop(rows);
                        if page_count < page_size {
                            break;
                        }
                        after_resource = last;
                    }
                } else {
                    let pending_attempts = format!(
                        "SELECT recipient FROM ingress_send_attempts WHERE message_key = {key} AND kind = ? AND semantic_identity_hash = ? AND (state IN (0, 1) AND expires_at_ms > {clock})"
                    );
                    let mut rows = transaction
                        .query(
                            &pending_attempts,
                            crate::db_params![
                                message.to_storage().to_string(),
                                receipt.kind.to_storage(),
                                receipt.semantic_identity_hash.to_vec()
                            ],
                        )
                        .await?;
                    while let Some(row) = rows.next().await? {
                        let recipient: String = row.get(0)?;
                        pending.push(
                            recipient
                                .parse()
                                .map_err(|_| IngressUowError::InvalidStoredCarbonRecipient)?,
                        );
                    }
                    drop(rows);
                }
                pending
            };
            settled = true;
            for recipient in pending {
                let obligation = super::SendObligation {
                    message,
                    receipt: receipt.clone(),
                    recipient,
                };
                if !super::SendAttemptRepository::has_custody(&mut tx, &obligation).await?
                    && !super::SendAttemptRepository::has_resource_receipt(&mut tx, &obligation)
                        .await?
                {
                    settled = false;
                    break;
                }
            }
            if settled {
                super::EffectReceiptRepository::record_receipt(
                    &mut tx,
                    message,
                    receipt.kind,
                    &receipt.semantic_identity_hash,
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(settled)
    }
}
