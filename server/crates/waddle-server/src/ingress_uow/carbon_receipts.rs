//! Per-resource completion beneath a recorded remote carbon obligation.
use jid::FullJid;
use waddle_xmpp::ingress::MessageKey;

use super::{CanonicalMessageRepository, IngressUnitOfWork, IngressUowError};
use crate::{db::DatabaseDriver, ingress::decision::EffectReceiptKey};

pub(crate) struct CarbonReceiptRepository;

impl CarbonReceiptRepository {
    pub(crate) async fn load(
        uow: &IngressUnitOfWork,
        message: MessageKey,
        receipt: &EffectReceiptKey,
    ) -> Result<Vec<FullJid>, IngressUowError> {
        let mut tx = uow.begin().await?;
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "SELECT recipient FROM ingress_carbon_receipts WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? ORDER BY recipient"
        } else {
            "SELECT recipient FROM ingress_carbon_receipts WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? ORDER BY recipient"
        };
        let mut rows = tx
            .transaction_mut()
            .query(
                sql,
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
        tx.commit().await?;
        Ok(recipients)
    }

    pub(crate) async fn record(
        uow: &IngressUnitOfWork,
        message: MessageKey,
        receipt: &EffectReceiptKey,
        recipients: &[FullJid],
    ) -> Result<(), IngressUowError> {
        if recipients.is_empty() {
            return Ok(());
        }
        let mut tx = uow.begin().await?;
        if !CanonicalMessageRepository::lock(&mut tx, message).await? {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "INSERT INTO ingress_carbon_receipts (message_key, kind, semantic_identity_hash, recipient) VALUES (?::uuid, ?, ?, ?) ON CONFLICT DO NOTHING"
        } else {
            "INSERT INTO ingress_carbon_receipts (message_key, kind, semantic_identity_hash, recipient) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING"
        };
        for recipient in recipients {
            tx.transaction_mut()
                .execute(
                    sql,
                    crate::db_params![
                        message.to_storage().to_string(),
                        receipt.kind.to_storage(),
                        receipt.semantic_identity_hash.to_vec(),
                        recipient.to_string()
                    ],
                )
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
