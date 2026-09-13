//! Transaction-scoped progress beneath an exact recorded delivery obligation.
use jid::FullJid;
use waddle_xmpp::ingress::MessageKey;

use super::{IngressUowError, IngressUowTransaction};
use crate::{
    db::DatabaseDriver, ingress::decision::EffectReceiptKey, ingress_substrate::EffectReceiptKind,
};

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

    /// Every recorded resource for the row, grouped by delivery obligation. One
    /// query per row: a lost groupchat row carries one route per inbox owner,
    /// and a per-route read would spend the recovery row deadline before any
    /// recoverable work on it could run.
    pub(crate) async fn load_all(
        tx: &mut IngressUowTransaction<'_>,
        message: MessageKey,
    ) -> Result<Vec<(EffectReceiptKey, Vec<FullJid>)>, IngressUowError> {
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "SELECT kind, semantic_identity_hash, resource FROM ingress_delivery_receipts WHERE message_key = ?::uuid ORDER BY kind, semantic_identity_hash, resource"
        } else {
            "SELECT kind, semantic_identity_hash, resource FROM ingress_delivery_receipts WHERE message_key = ? ORDER BY kind, semantic_identity_hash, resource"
        };
        let mut rows = tx
            .transaction_mut()
            .query(sql, crate::db_params![message.to_storage().to_string()])
            .await?;
        let mut progress: Vec<(EffectReceiptKey, Vec<FullJid>)> = Vec::new();
        while let Some(row) = rows.next().await? {
            let kind: i64 = row.get(0)?;
            let hash: Vec<u8> = row.get(1)?;
            let resource: String = row.get(2)?;
            let receipt = EffectReceiptKey {
                kind: EffectReceiptKind::from_storage(
                    i32::try_from(kind).map_err(|_| IngressUowError::InvalidStoredReceiptHash)?,
                ),
                semantic_identity_hash: hash
                    .try_into()
                    .map_err(|_| IngressUowError::InvalidStoredReceiptHash)?,
            };
            let recipient = resource
                .parse()
                .map_err(|_| IngressUowError::InvalidStoredDeliveryResource)?;
            match progress.last_mut() {
                Some((last, recipients)) if *last == receipt => recipients.push(recipient),
                _ => progress.push((receipt, vec![recipient])),
            }
        }
        drop(rows);
        Ok(progress)
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
