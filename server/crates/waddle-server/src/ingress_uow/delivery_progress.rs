//! Transaction-scoped progress beneath an exact recorded delivery obligation.
use jid::FullJid;
use waddle_xmpp::ingress::MessageKey;

use super::{IngressUowError, IngressUowTransaction};
use crate::{db::DatabaseDriver, ingress::decision::EffectReceiptKey};

pub(crate) struct DeliveryProgressRepository;

impl DeliveryProgressRepository {
    pub(crate) async fn load(
        tx: &mut IngressUowTransaction<'_>,
        message: MessageKey,
        receipt: &EffectReceiptKey,
    ) -> Result<Vec<FullJid>, IngressUowError> {
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "SELECT resource FROM ingress_delivery_receipts WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? ORDER BY resource"
        } else {
            "SELECT resource FROM ingress_delivery_receipts WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? ORDER BY resource"
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
                    .map_err(|_| IngressUowError::InvalidStoredDeliveryResource)?,
            );
        }
        drop(rows);
        Ok(recipients)
    }

    pub(crate) async fn record(
        tx: &mut IngressUowTransaction<'_>,
        message: MessageKey,
        receipt: &EffectReceiptKey,
        recipients: &[FullJid],
    ) -> Result<(), IngressUowError> {
        if recipients.is_empty() {
            return Ok(());
        }
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "INSERT INTO ingress_delivery_receipts (message_key, kind, semantic_identity_hash, resource) VALUES (?::uuid, ?, ?, ?) ON CONFLICT DO NOTHING"
        } else {
            "INSERT INTO ingress_delivery_receipts (message_key, kind, semantic_identity_hash, resource) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING"
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
        Ok(())
    }
}
