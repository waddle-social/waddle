pub use super::extension_grants::ExtensionGrantRepository;
use crate::db::Transaction;
use crate::inbox::InboxTxError;
use crate::ingress::EffectReceiptKey;
use crate::ingress_substrate::EnteredTransaction;
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use jid::BareJid;
#[cfg(all(test, not(feature = "clustering")))]
use jid::FullJid;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::inbox::storage::GroupchatNotificationRecovery;
use waddle_xmpp::inbox::InboxEntry;
use waddle_xmpp::ingress::{
    AliasResolution, DeliveryKey, IngressEffectIntent, IngressEffectKey, IngressEffectKind,
    IngressOrdinal, MessageKey, NormalizedTarget, SemanticDigest, SmIngressId, WireHandledCount,
};
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage, MamTxStoreOutcome};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp_core::xep0359::OriginId;

use crate::db::{Database, DatabaseDriver};
use crate::ingress_substrate::{
    self, DefinerApiCandidateContext, EffectReceiptKind, MessageEnvelope, MessageWriteOutcome,
    TerminalizeOutcome,
};
use crate::ingress_uow::{IngressUowError, IngressUowTransaction};
use crate::server::routes::interpret::effects::room::PlannedGroupchatNotificationRecovery;
#[cfg(all(test, not(feature = "clustering")))]
use waddle_xmpp::ingress::{DigestContext, DigestInput, FrozenStanzaError};

/// Repository for MAM archive rows written inside the ingress transaction.
#[derive(Debug, Default, Clone, Copy)]
pub struct MamArchiveRepository;

impl MamArchiveRepository {
    /// Lock every archive's ordinal counter in canonical order before any
    /// archive row is written, so two transactions that write the same pair
    /// of archives in opposite plan order (a DM and its reply, a MUC private
    /// message and its answer) queue on the counters instead of deadlocking.
    pub async fn lock_sequences(
        transaction: &mut IngressUowTransaction<'_>,
        archives: &[BareJid],
    ) -> Result<(), IngressUowError> {
        let mut ordered: Vec<&BareJid> = archives.iter().collect();
        ordered.sort();
        ordered.dedup();
        let schema = transaction.receipt_schema().map(str::to_owned);
        for archive in ordered {
            if let Some(connection) = transaction.transaction_mut().postgres_connection() {
                match schema.as_deref() {
                    Some(schema) => {
                        waddle_xmpp::mam::lock_archive_sequence_on_connection_in_schema(
                            connection, schema, archive,
                        )
                        .await
                    }
                    None => {
                        waddle_xmpp::mam::lock_archive_sequence_on_connection(connection, archive)
                            .await
                    }
                }
                .map_err(waddle_xmpp::mam::MamTxStoreError::Database)?;
            } else {
                let connection = transaction
                    .transaction_mut()
                    .sqlite_connection()
                    .ok_or(IngressUowError::PostgresRequired)?;
                waddle_xmpp::mam::lock_archive_sequence_on_sqlite_connection(connection, archive)
                    .await
                    .map_err(waddle_xmpp::mam::MamTxStoreError::Database)?;
            }
        }
        Ok(())
    }

    pub async fn store(
        transaction: &mut IngressUowTransaction<'_>,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
        expectation: ArchiveExpectation,
    ) -> Result<MamTxStoreOutcome, IngressUowError> {
        let schema = transaction.receipt_schema().map(str::to_owned);
        let outcome = if let Some(connection) = transaction.transaction_mut().postgres_connection()
        {
            match schema.as_deref() {
                Some(schema) => {
                    waddle_xmpp::mam::store_archived_message_on_connection_in_schema(
                        connection,
                        schema,
                        archive_jid,
                        message,
                        expectation,
                    )
                    .await?
                }
                None => {
                    waddle_xmpp::mam::store_archived_message_on_connection(
                        connection,
                        archive_jid,
                        message,
                        expectation,
                    )
                    .await?
                }
            }
        } else {
            let connection = transaction
                .transaction_mut()
                .sqlite_connection()
                .ok_or(IngressUowError::PostgresRequired)?;
            waddle_xmpp::mam::store_archived_message_on_sqlite_connection(
                connection,
                archive_jid,
                message,
                expectation,
            )
            .await?
        };
        Ok(outcome)
    }

    /// Store a room archive row only after the exact room claim has been
    /// asserted in this transaction.
    #[cfg(feature = "clustering")]
    pub async fn store_fenced(
        transaction: &mut IngressUowTransaction<'_>,
        fence: &RoomClaimFence<'_>,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
        expectation: ArchiveExpectation,
    ) -> Result<MamTxStoreOutcome, IngressUowError> {
        if fence.transaction_identity != transaction.identity() || fence.room != *archive_jid {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        Self::store(transaction, archive_jid, message, expectation).await
    }
}

/// Repository for inbox projections written inside the ingress transaction.
#[derive(Debug, Default, Clone, Copy)]
pub struct InboxRepository;

impl InboxRepository {
    pub async fn upsert(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<InboxEntry, IngressUowError> {
        Self::apply_once(transaction, message_key, user, entry, increment_unread).await
    }

    /// Apply a message's projection once, including its unread/reply increments.
    pub async fn apply_once(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<InboxEntry, IngressUowError> {
        Self::apply_projection(transaction, message_key, user, entry, increment_unread)
            .await
            .map(|(entry, _)| entry)
    }

    async fn apply_projection(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<(InboxEntry, bool), IngressUowError> {
        let thread = entry
            .thread_id
            .as_ref()
            .filter(|thread| !thread.is_empty())
            .map(|thread| {
                waddle_xmpp_core::mam::ThreadId::try_new(thread.clone())
                    .ok_or(InboxTxError::InvalidProjectionThread)
            })
            .transpose()?;
        let apply = Self::record_projection(
            transaction,
            message_key,
            user,
            &entry.partner,
            thread.as_ref(),
        )
        .await?;
        let (tx, prepared) = transaction.prepared_parts();
        let entry = if apply {
            crate::inbox::upsert_in_prepared_transaction(
                tx,
                prepared,
                user,
                entry,
                increment_unread,
            )
            .await?
        } else {
            crate::inbox::get_in_prepared_transaction(tx, prepared, user, &entry).await?
        };
        Ok((entry, apply))
    }

    pub(super) async fn record_projection(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        user: &BareJid,
        partner: &BareJid,
        thread: Option<&waddle_xmpp_core::mam::ThreadId>,
    ) -> Result<bool, IngressUowError> {
        let key = DeliveryKey::inbox_projection(message_key, user, (partner, thread));
        let recorded = DeliveryEffectRepository::lookup(transaction, key).await?;
        if recorded.is_some_and(|recorded| recorded != message_key) {
            return Err(ingress_substrate::IngressSubstrateError::DeliveryKeyConflict.into());
        }
        Ok(if recorded.is_some() {
            false
        } else {
            // Recording locks the canonical row before the projection write.
            // A concurrent retry that wins that lock first returns AlreadyRecorded.
            match DeliveryEffectRepository::record(transaction, key, message_key).await? {
                MessageWriteOutcome::Recorded => true,
                MessageWriteOutcome::AlreadyRecorded => false,
                MessageWriteOutcome::MessageVanished => {
                    return Err(InboxTxError::ProjectionMessageMissing.into());
                }
            }
        })
    }

    /// Upsert an inbox row and its groupchat notification recovery item once in
    /// the same ingress transaction.
    pub async fn upsert_with_groupchat_notification_recovery(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
        recovery: PlannedGroupchatNotificationRecovery,
    ) -> Result<InboxEntry, IngressUowError> {
        let (entry, _) =
            Self::apply_projection(transaction, message_key, user, entry, increment_unread).await?;
        let recovery = GroupchatNotificationRecovery {
            message_key,
            key: recovery.key,
            sender_jid: recovery.sender_jid,
            is_live_occupant: recovery.is_live_occupant,
            room_members_only: recovery.room_members_only,
            sender_can_broadcast_channel_mention: recovery.sender_can_broadcast_channel_mention,
            created_at_ms: recovery.created_at_ms,
        };
        crate::inbox::insert_groupchat_notification_recovery_in_transaction(
            transaction.transaction_mut(),
            recovery,
        )
        .await?;
        Ok(entry)
    }
}

/// Repository for canonical ingress messages and their origin-id aliases.
#[derive(Debug, Default, Clone, Copy)]
pub struct CanonicalMessageRepository;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoomOwnerDecision {
    Undecided,
    Accepted,
    Rejected,
}

/// Owner-result state of one canonical message, read under its row lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RoomOwnerState {
    /// Whether the message was created with an authenticated source binding.
    /// Only a bound message can carry an owner decision.
    pub source_bound: bool,
    /// `None` until the origin opens the decision with its dispatch intent.
    pub decision: Option<RoomOwnerDecision>,
}

#[cfg(all(test, not(feature = "clustering")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RelayedSourceAuthority {
    pub decision: RoomOwnerDecision,
    pub rejection_child: Option<MessageKey>,
}

#[cfg(all(test, not(feature = "clustering")))]
#[derive(Debug)]
pub(crate) struct ClosedRoomOwnerResultSource<'a> {
    message_key: MessageKey,
    sender: &'a FullJid,
    room: &'a BareJid,
    digest: &'a SemanticDigest,
    binding: &'a ingress_substrate::CanonicalSourceBinding,
    stanza_lang: Option<&'a xmpp_parsers::message::Lang>,
}

#[cfg(all(test, not(feature = "clustering")))]
impl<'a> ClosedRoomOwnerResultSource<'a> {
    pub(crate) fn new(
        message_key: MessageKey,
        sender: &'a FullJid,
        room: &'a BareJid,
        digest: &'a SemanticDigest,
        binding: &'a ingress_substrate::CanonicalSourceBinding,
        stanza_lang: Option<&'a xmpp_parsers::message::Lang>,
    ) -> Self {
        Self {
            message_key,
            sender,
            room,
            digest,
            binding,
            stanza_lang,
        }
    }
}

#[cfg(all(test, not(feature = "clustering")))]
#[derive(Debug)]
pub(crate) enum ClosedRoomOwnerResultProposal {
    Accepted {
        owner_effects: Vec<IngressEffectIntent>,
    },
    Rejected {
        error: FrozenStanzaError,
    },
}

#[cfg(all(test, not(feature = "clustering")))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ClosedRoomOwnerResultOutcome {
    pub decision: RoomOwnerDecision,
    pub rejection_child: Option<MessageKey>,
    /// Exact receipt identities still pending delivery. This API does not
    /// write completion receipts for external effects.
    pub pending_effect_receipts: Vec<EffectReceiptKey>,
}

impl CanonicalMessageRepository {
    /// Protect a room's current claim through a nested sink acceptance without
    /// acquiring a second node publication guard. The inner sink owns that
    /// guard; retaining one here would deadlock a queued identity rotation.
    /// Epoch is already locked by the UoW. This takes only the exact claim row,
    /// never the canonical row that the inner SM transaction must acquire.
    #[cfg(feature = "clustering")]
    pub(crate) async fn protect_room_acceptance(
        transaction: &mut IngressUowTransaction<'_>,
        room: &BareJid,
        fence: &waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> Result<(), IngressUowError> {
        let identity = transaction
            .bound_node_identity()
            .ok_or(IngressUowError::NodeIdentityUnbound)?
            .current();
        if !identity.is_active()
            || identity != fence.owner
            || fence.entity
                != waddle_xmpp::ownership::Entity::new(EntityType::RoomActor, room.to_string())
        {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        let (tx, prepared) = transaction.prepared_parts();
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Claims,
            (
                "SELECT 1 FROM ",
                " WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
            ),
            "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?",
        );
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![
                    format!("{}:{room}", EntityType::RoomActor.as_db_str()),
                    identity.node_id,
                    identity.node_epoch,
                    fence.epoch.0
                ],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        drop(rows);
        let current = transaction
            .bound_node_identity()
            .ok_or(IngressUowError::NodeIdentityUnbound)?
            .current();
        if !current.is_active() || current != fence.owner {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        Ok(())
    }

    pub async fn lock(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<bool, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::lock_message_for_child_in_prepared_transaction(tx, prepared, message_key)
            .await
            .map_err(|error| match error {
                ingress_substrate::IngressSubstrateError::Timeout => IngressUowError::Timeout,
                other => other.into(),
            })
    }

    /// Check durable terminal state without taking the canonical row lock.
    pub async fn is_terminal(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<bool, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
            let function = api.qualified_function("waddle_ingress_message_is_terminal");
            return super::candidate_boolean(
                tx,
                &format!("SELECT {function}(?::uuid)"),
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await;
        }
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            format!(
                "SELECT 1 FROM {} WHERE message_key = ?::uuid AND terminal_at IS NOT NULL",
                ingress_substrate::prepared_ingress_relation(
                    prepared,
                    ingress_substrate::PreparedIngressRelation::Messages
                )
            )
        } else {
            "SELECT 1 FROM ingress_messages WHERE message_key = ? AND terminal_at IS NOT NULL"
                .to_owned()
        };
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    pub async fn record_message(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        digest: &SemanticDigest,
        envelope: Option<&MessageEnvelope>,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::record_message_in_prepared_transaction(
            tx,
            prepared,
            message_key,
            digest,
            envelope,
            None,
        )
        .await
        .map_err(Into::into)
    }

    /// Record message content together with its immutable authenticated
    /// source. A message created this way can later carry a room owner's
    /// decision; one created without a source never can.
    pub(crate) async fn record_message_with_source(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        digest: &SemanticDigest,
        envelope: Option<&MessageEnvelope>,
        source: &ingress_substrate::CanonicalSourceBinding,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::record_message_in_prepared_transaction(
            tx,
            prepared,
            message_key,
            digest,
            envelope,
            Some(source),
        )
        .await
        .map_err(Into::into)
    }

    pub(crate) async fn assert_room_snapshot(
        transaction: &mut IngressUowTransaction<'_>,
        room: &BareJid,
        coordinates: waddle_xmpp::muc::durable::RoomCommittedCoordinates,
    ) -> Result<(), IngressUowError> {
        let sql = dialect_sql(
            transaction,
            "SELECT lifecycle_id, revision FROM clustering_muc_rooms WHERE room_jid = ? FOR SHARE NOWAIT",
            "SELECT lifecycle_id, revision FROM clustering_muc_rooms WHERE room_jid = ?",
        );
        let mut rows = transaction
            .transaction_mut()
            .query(sql, crate::db_params![room.to_string()])
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(IngressUowError::RoomGenerationStale)?;
        let lifecycle: Option<String> = row.get(0)?;
        let revision: Option<i64> = row.get(1)?;
        if lifecycle.as_deref() != Some(coordinates.lifecycle.to_string().as_str())
            || revision != Some(coordinates.revision.as_i64())
        {
            return Err(IngressUowError::RoomGenerationStale);
        }
        Ok(())
    }

    pub async fn record_room_canonical_envelope(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        envelope: &MessageEnvelope,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::record_room_envelope_in_prepared_transaction(
            tx,
            prepared,
            message_key,
            envelope,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn load_envelope(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<Option<MessageEnvelope>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::load_envelope_in_prepared_transaction(tx, prepared, message_key)
            .await
            .map_err(Into::into)
    }

    /// Read the original acceptance time while holding the canonical lock.
    pub async fn created_at(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<DateTime<Utc>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
            let function = api.qualified_function("waddle_ingress_message_created_at");
            let mut rows = tx
                .query(
                    &format!("SELECT created_at FROM {function}(?::uuid)"),
                    crate::db_params![message_key.to_storage().to_string()],
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(IngressUowError::EffectIntentMessageMissing)?;
            let timestamp: String = row.get(0)?;
            if rows.next().await?.is_some() {
                return Err(
                    ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into(),
                );
            }
            return DateTime::parse_from_rfc3339(&timestamp)
                .map(|timestamp| timestamp.with_timezone(&Utc))
                .map_err(|_| IngressUowError::InvalidStoredMessageTimestamp);
        }
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            format!(
                "SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM {} WHERE message_key = ?::uuid",
                ingress_substrate::prepared_ingress_relation(
                    prepared,
                    ingress_substrate::PreparedIngressRelation::Messages
                )
            )
        } else {
            "SELECT created_at FROM ingress_messages WHERE message_key = ?".to_owned()
        };
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(IngressUowError::EffectIntentMessageMissing)?;
        let timestamp: String = row.get(0)?;
        DateTime::parse_from_rfc3339(&timestamp)
            .map(|timestamp| timestamp.with_timezone(&Utc))
            .map_err(|_| IngressUowError::InvalidStoredMessageTimestamp)
    }

    /// Resolve a prepared PostgreSQL origin while owning the complete UOW.
    /// Every error and cancellation drops the outer transaction, including an
    /// in-flight savepoint; the transaction returns only on full success.
    pub(crate) async fn resolve_prepared_origin_owned<'a>(
        mut transaction: IngressUowTransaction<'a>,
        sender: &BareJid,
        target: &NormalizedTarget,
        origin_id: &OriginId,
        digest: &SemanticDigest,
    ) -> Result<(IngressUowTransaction<'a>, AliasResolution), IngressUowError> {
        let scope = super::OwnedAliasScope(());
        let resolved = {
            let (tx, prepared) = transaction.prepared_parts();
            if tx.driver() != DatabaseDriver::Postgres {
                return Err(IngressUowError::PostgresRequired);
            }
            let prepared = prepared.ok_or(IngressUowError::AttestedPoolRequired)?;
            ingress_substrate::resolve_prepared_origin_in_scope(
                tx,
                prepared,
                &scope,
                sender,
                target,
                origin_id,
                ingress_substrate::OriginCandidate {
                    digest,
                    source: None,
                },
            )
            .await?
        };
        Ok((transaction, resolved))
    }

    /// [`Self::resolve_prepared_origin_owned`] for a message that must carry
    /// its authenticated source: a first insert binds `source`, and an
    /// existing alias is returned as it is.
    pub(crate) async fn resolve_prepared_origin_owned_with_source<'a>(
        mut transaction: IngressUowTransaction<'a>,
        sender: &BareJid,
        target: &NormalizedTarget,
        origin_id: &OriginId,
        digest: &SemanticDigest,
        source: &ingress_substrate::CanonicalSourceBinding,
    ) -> Result<(IngressUowTransaction<'a>, AliasResolution), IngressUowError> {
        let scope = super::OwnedAliasScope(());
        let resolved = {
            let (tx, prepared) = transaction.prepared_parts();
            if tx.driver() != DatabaseDriver::Postgres {
                return Err(IngressUowError::PostgresRequired);
            }
            let prepared = prepared.ok_or(IngressUowError::AttestedPoolRequired)?;
            ingress_substrate::resolve_prepared_origin_in_scope(
                tx,
                prepared,
                &scope,
                sender,
                target,
                origin_id,
                ingress_substrate::OriginCandidate {
                    digest,
                    source: Some(source),
                },
            )
            .await?
        };
        Ok((transaction, resolved))
    }

    pub async fn resolve_and_record_alias(
        transaction: &mut IngressUowTransaction<'_>,
        sender: &BareJid,
        target: &NormalizedTarget,
        origin_id: &OriginId,
        digest: &SemanticDigest,
        mint: impl FnOnce() -> MessageKey,
    ) -> Result<AliasResolution, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::resolve_alias_in_prepared_transaction(
            tx, prepared, sender, target, origin_id, digest, None, mint,
        )
        .await
        .map_err(Into::into)
    }

    /// [`Self::resolve_and_record_alias`] for a message that must carry its
    /// authenticated source.
    pub(crate) async fn resolve_and_record_alias_with_source(
        transaction: &mut IngressUowTransaction<'_>,
        sender: &BareJid,
        target: &NormalizedTarget,
        origin_id: &OriginId,
        digest: &SemanticDigest,
        source: &ingress_substrate::CanonicalSourceBinding,
        mint: impl FnOnce() -> MessageKey,
    ) -> Result<AliasResolution, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::resolve_alias_in_prepared_transaction(
            tx,
            prepared,
            sender,
            target,
            origin_id,
            digest,
            Some(source),
            mint,
        )
        .await
        .map_err(Into::into)
    }

    /// Assert an already-recorded origin alias without creating or repairing it.
    /// PostgreSQL retains parent and alias row locks through the transaction;
    /// SQLite relies on the transaction's BEGIN IMMEDIATE writer serialization.
    pub(crate) async fn assert_relayed_alias(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        sender: &BareJid,
        target: &NormalizedTarget,
        origin_id: &OriginId,
        digest: &SemanticDigest,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::assert_relayed_alias_in_prepared_transaction(
            tx,
            prepared,
            message_key,
            sender,
            target,
            origin_id,
            digest,
        )
        .await
        .map_err(Into::into)
    }

    /// Lock and validate the exact canonical source before the room owner
    /// resolves acceptance versus a child rejection. This is called after the
    /// epoch, principal and room-claim fences and before any child row.
    #[cfg(all(test, not(feature = "clustering")))]
    pub(crate) async fn assert_relayed_source(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        sender: &FullJid,
        room: &BareJid,
        digest: &SemanticDigest,
        binding: &ingress_substrate::CanonicalSourceBinding,
        stanza_lang: Option<&xmpp_parsers::message::Lang>,
    ) -> Result<RelayedSourceAuthority, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            format!(
                "SELECT digest_version, digest, source_binding_version::int, source_bare_jid, source_auth_context_id::text, source_auth_context_version::text, source_auth_epoch::text, owner_decision::int, source_sm_ingress_id::text, source_ingress_ordinal::text FROM {} WHERE message_key = ?::uuid FOR UPDATE",
                ingress_substrate::prepared_ingress_relation(
                    prepared,
                    ingress_substrate::PreparedIngressRelation::Messages,
                )
            )
        } else {
            "SELECT digest_version, digest, source_binding_version, source_bare_jid, source_auth_context_id, source_auth_context_version, source_auth_epoch, owner_decision, source_sm_ingress_id, source_ingress_ordinal FROM ingress_messages WHERE message_key = ?".to_owned()
        };
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        };
        let digest_version: i32 = row.get(0)?;
        let digest_bytes: Vec<u8> = row.get(1)?;
        let source_version: Option<i32> = row.get(2)?;
        let source_bare: Option<String> = row.get(3)?;
        let source_context: Option<String> = row.get(4)?;
        let source_context_version: Option<String> = row.get(5)?;
        let source_epoch: Option<String> = row.get(6)?;
        let owner_decision: Option<i32> = row.get(7)?;
        let source_sm_ingress_id: Option<String> = row.get(8)?;
        let source_ingress_ordinal: Option<String> = row.get(9)?;
        let (offered_version, offered_digest) = digest.to_storage();
        let principal = binding.principal();
        let expected_sm_occurrence = binding.sm_occurrence().map(|(id, ordinal)| {
            (
                id.to_storage().to_string(),
                ordinal.to_storage().to_string(),
            )
        });
        let stored_sm_occurrence = source_sm_ingress_id.zip(source_ingress_ordinal);
        if i32::from(offered_version) != digest_version
            || digest_bytes.as_slice() != offered_digest
            || source_version != Some(1)
            || source_bare.as_deref() != Some(sender.to_bare().to_string().as_str())
            || source_bare.as_deref() != Some(principal.bare_jid().to_string().as_str())
            || source_context.as_deref()
                != Some(principal.auth_context_id().as_uuid().to_string().as_str())
            || source_context_version.as_deref()
                != Some(principal.auth_context_version().get().to_string().as_str())
            || source_epoch.as_deref() != Some(principal.auth_epoch().get().to_string().as_str())
            || stored_sm_occurrence != expected_sm_occurrence
        {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        }
        drop(rows);

        let envelope = Self::load_envelope(transaction, message_key)
            .await?
            .ok_or(IngressUowError::EffectIntentMessageMissing)?;
        let source_message = envelope.message();
        let exact_sender = source_message
            .from
            .as_ref()
            .and_then(|jid| jid.try_as_full().ok());
        let Some(target) = source_message.to.as_ref() else {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        };
        if exact_sender != Some(sender) || target.resource().is_some() || target.to_bare() != *room
        {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        }
        let digest_context = DigestContext {
            target: NormalizedTarget::Bare(target.to_bare()),
            server_authorities: vec![sender.to_bare(), room.clone()],
            stanza_lang: stanza_lang.cloned(),
        };
        let mut digest_message = (*source_message).clone();
        for authority in &digest_context.server_authorities {
            waddle_xmpp_core::xep0359::remove_stanza_ids_by(
                &mut digest_message,
                &authority.clone().into(),
            );
        }
        let source_digest_input = DigestInput::from_parsed(&digest_message, &digest_context)
            .map_err(|_| ingress_substrate::IngressSubstrateError::RelayedAliasUnproven)?;
        if waddle_xmpp::ingress::digest::v1::digest(&source_digest_input) != *digest {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        }

        let recorded = EffectIntentRepository::load(transaction, message_key).await?;
        if !recorded.iter().any(|intent| {
            matches!(intent, IngressEffectIntent::DispatchToRoomRemote { room: saved, .. } if saved == room)
        }) {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        }

        let rejection_child = Self::rejection_child(transaction, message_key).await?;
        let decision = match owner_decision {
            Some(0) if rejection_child.is_none() => RoomOwnerDecision::Undecided,
            Some(1) if rejection_child.is_none() => RoomOwnerDecision::Accepted,
            Some(2) if rejection_child.is_some() => RoomOwnerDecision::Rejected,
            _ => return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into()),
        };
        if let Some(child) = rejection_child {
            if !Self::lock(transaction, child).await? {
                return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
            }
        }
        Ok(RelayedSourceAuthority {
            decision,
            rejection_child,
        })
    }

    /// Close the first owner result in one caller-owned UOW. This deliberately
    /// has no production caller: activation needs the claim/alias boundary,
    /// planner provenance, runtime grants, and the independent PostgreSQL
    /// commit-unknown pilot. This test harness composes stored effects and
    /// latches atomically; it does not establish those activation authorities.
    #[cfg(all(test, not(feature = "clustering")))]
    pub(crate) async fn apply_closed_room_owner_result(
        transaction: &mut IngressUowTransaction<'_>,
        source: ClosedRoomOwnerResultSource<'_>,
        proposal: ClosedRoomOwnerResultProposal,
    ) -> Result<ClosedRoomOwnerResultOutcome, IngressUowError> {
        let ClosedRoomOwnerResultSource {
            message_key,
            sender,
            room,
            digest,
            binding,
            stanza_lang,
        } = source;
        if sender.to_bare() != *binding.principal().bare_jid() {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        }
        let authority = Self::assert_relayed_source(
            transaction,
            message_key,
            sender,
            room,
            digest,
            binding,
            stanza_lang,
        )
        .await?;

        // A replay is decided by the immutable latch and stored effects. Do
        // not inspect or reconcile today's proposal before returning it.
        if authority.decision != RoomOwnerDecision::Undecided {
            return Self::replay_closed_room_owner_result(
                transaction,
                message_key,
                sender,
                room,
                stanza_lang,
                authority,
            )
            .await;
        }

        match proposal {
            ClosedRoomOwnerResultProposal::Accepted { owner_effects } => {
                validate_room_owner_effects(&owner_effects, room)?;
                let mut planned = EffectIntentRepository::load(transaction, message_key).await?;
                let stored_owner_effects = planned
                    .iter()
                    .filter(|effect| {
                        matches!(effect, IngressEffectIntent::RouteMucGroupchat { .. })
                    })
                    .collect::<Vec<_>>();
                if stored_owner_effects.len() > 1
                    || stored_owner_effects
                        .iter()
                        .any(|stored| !owner_effects.contains(stored))
                {
                    return Err(IngressUowError::EffectIntentConflict);
                }
                for effect in &owner_effects {
                    if !planned.contains(effect) {
                        planned.push(effect.clone());
                    }
                }
                match EffectIntentRepository::reconcile(transaction, message_key, &planned, true)
                    .await?
                {
                    ReconcileVerdict::Contradiction { .. } | ReconcileVerdict::Divergent { .. } => {
                        return Err(IngressUowError::EffectIntentConflict);
                    }
                    ReconcileVerdict::FirstCommit
                    | ReconcileVerdict::Consistent
                    | ReconcileVerdict::Repaired { .. } => {}
                }
                let (tx, prepared) = transaction.prepared_parts();
                ingress_substrate::invalidate_retention_in_prepared_transaction(
                    tx,
                    prepared,
                    message_key,
                )
                .await?;
                Self::decide_room_owner(transaction, message_key, RoomOwnerDecision::Accepted)
                    .await?;
                pending_room_effect_receipts(transaction, message_key, &owner_effects)
                    .await
                    .map(|pending_effect_receipts| ClosedRoomOwnerResultOutcome {
                        decision: RoomOwnerDecision::Accepted,
                        rejection_child: None,
                        pending_effect_receipts,
                    })
            }
            ClosedRoomOwnerResultProposal::Rejected { error } => {
                let source_effects = EffectIntentRepository::load(transaction, message_key).await?;
                if source_effects
                    .iter()
                    .any(|effect| matches!(effect, IngressEffectIntent::RouteMucGroupchat { .. }))
                {
                    return Err(IngressUowError::EffectIntentConflict);
                }
                let source = Self::load_envelope(transaction, message_key)
                    .await?
                    .ok_or(IngressUowError::EffectIntentMessageMissing)?;
                let error_message = owner_error_reply(source.message(), sender, room, &error)?;
                let child = MessageKey::new();
                let digest_context = DigestContext {
                    target: NormalizedTarget::Full(sender.clone()),
                    server_authorities: vec![sender.to_bare(), room.clone()],
                    stanza_lang: stanza_lang.cloned(),
                };
                let mut digest_message = error_message.clone();
                for authority in &digest_context.server_authorities {
                    waddle_xmpp_core::xep0359::remove_stanza_ids_by(
                        &mut digest_message,
                        &authority.clone().into(),
                    );
                }
                let child_digest_input = DigestInput::from_parsed(&digest_message, &digest_context)
                    .map_err(|_| IngressUowError::EffectIntentConflict)?;
                let child_digest = waddle_xmpp::ingress::digest::v1::digest(&child_digest_input);
                let child_envelope = MessageEnvelope::new(error_message);
                Self::record_message(transaction, child, &child_digest, Some(&child_envelope))
                    .await?;
                let error_intent = IngressEffectIntent::ErrorReply {
                    recipient: sender.clone(),
                    error,
                };
                match EffectIntentRepository::reconcile(
                    transaction,
                    child,
                    std::slice::from_ref(&error_intent),
                    false,
                )
                .await?
                {
                    ReconcileVerdict::FirstCommit | ReconcileVerdict::Consistent => {}
                    ReconcileVerdict::Repaired { .. }
                    | ReconcileVerdict::Contradiction { .. }
                    | ReconcileVerdict::Divergent { .. } => {
                        return Err(IngressUowError::EffectIntentConflict);
                    }
                }
                Self::record_owner_rejection(transaction, message_key, child).await?;
                Self::decide_room_owner(transaction, message_key, RoomOwnerDecision::Rejected)
                    .await?;
                let pending_effect_receipts = pending_room_effect_receipts(
                    transaction,
                    child,
                    std::slice::from_ref(&error_intent),
                )
                .await?;
                Ok(ClosedRoomOwnerResultOutcome {
                    decision: RoomOwnerDecision::Rejected,
                    rejection_child: Some(child),
                    pending_effect_receipts,
                })
            }
        }
    }

    #[cfg(all(test, not(feature = "clustering")))]
    async fn replay_closed_room_owner_result(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        sender: &FullJid,
        room: &BareJid,
        stanza_lang: Option<&xmpp_parsers::message::Lang>,
        authority: RelayedSourceAuthority,
    ) -> Result<ClosedRoomOwnerResultOutcome, IngressUowError> {
        match authority.decision {
            RoomOwnerDecision::Undecided => {
                Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into())
            }
            RoomOwnerDecision::Accepted => {
                let owner_effects = EffectIntentRepository::load(transaction, message_key)
                    .await?
                    .into_iter()
                    .filter(|intent| {
                        matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. })
                    })
                    .collect::<Vec<_>>();
                validate_room_owner_effects(&owner_effects, room)?;
                let pending_effect_receipts =
                    pending_room_effect_receipts(transaction, message_key, &owner_effects).await?;
                Ok(ClosedRoomOwnerResultOutcome {
                    decision: RoomOwnerDecision::Accepted,
                    rejection_child: None,
                    pending_effect_receipts,
                })
            }
            RoomOwnerDecision::Rejected => {
                let child = authority
                    .rejection_child
                    .ok_or(IngressUowError::EffectIntentConflict)?;
                if !Self::lock(transaction, child).await? {
                    return Err(
                        ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into(),
                    );
                }
                let source = Self::load_envelope(transaction, message_key)
                    .await?
                    .ok_or(IngressUowError::EffectIntentMessageMissing)?;
                let child_envelope = Self::load_envelope(transaction, child)
                    .await?
                    .ok_or(IngressUowError::EffectIntentMessageMissing)?;
                let child_intents = EffectIntentRepository::load(transaction, child).await?;
                let [IngressEffectIntent::ErrorReply { recipient, error }] =
                    child_intents.as_slice()
                else {
                    return Err(IngressUowError::EffectIntentConflict);
                };
                if recipient != sender
                    || owner_error_reply(source.message(), sender, room, error)?
                        != *child_envelope.message()
                {
                    return Err(IngressUowError::EffectIntentConflict);
                }
                let digest_context = DigestContext {
                    target: NormalizedTarget::Full(sender.clone()),
                    server_authorities: vec![sender.to_bare(), room.clone()],
                    stanza_lang: stanza_lang.cloned(),
                };
                let mut digest_message = (*child_envelope.message()).clone();
                for authority in &digest_context.server_authorities {
                    waddle_xmpp_core::xep0359::remove_stanza_ids_by(
                        &mut digest_message,
                        &authority.clone().into(),
                    );
                }
                let child_digest_input = DigestInput::from_parsed(&digest_message, &digest_context)
                    .map_err(|_| IngressUowError::EffectIntentConflict)?;
                let expected_digest = waddle_xmpp::ingress::digest::v1::digest(&child_digest_input);
                if Self::stored_digest(transaction, child).await? != expected_digest {
                    return Err(IngressUowError::EffectIntentConflict);
                }
                let pending_effect_receipts =
                    pending_room_effect_receipts(transaction, child, &child_intents).await?;
                Ok(ClosedRoomOwnerResultOutcome {
                    decision: RoomOwnerDecision::Rejected,
                    rejection_child: Some(child),
                    pending_effect_receipts,
                })
            }
        }
    }

    #[cfg(all(test, not(feature = "clustering")))]
    async fn stored_digest(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<SemanticDigest, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Messages,
            (
                "SELECT digest_version::int, digest FROM ",
                " WHERE message_key = ?::uuid",
            ),
            "SELECT digest_version, digest FROM ingress_messages WHERE message_key = ?",
        );
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(IngressUowError::EffectIntentMessageMissing);
        };
        let version: i32 = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| ingress_substrate::IngressSubstrateError::InvalidStoredDigest)?;
        let version = u8::try_from(version)
            .map_err(|_| ingress_substrate::IngressSubstrateError::InvalidStoredDigest)?;
        SemanticDigest::from_storage(version, bytes)
            .map_err(|_| ingress_substrate::IngressSubstrateError::InvalidStoredDigest.into())
    }

    /// The rejection child mapped to `source`, if its owner rejected it.
    pub(crate) async fn rejection_child(
        transaction: &mut IngressUowTransaction<'_>,
        source: MessageKey,
    ) -> Result<Option<MessageKey>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
            let function = api.qualified_function("waddle_ingress_owner_rejection_child");
            let mut rows = tx
                .query(
                    &format!("SELECT {function}(?::uuid)"),
                    crate::db_params![source.to_storage().to_string()],
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let child: Option<String> = row.get(0)?;
            if rows.next().await?.is_some() {
                return Err(
                    ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into(),
                );
            }
            return child
                .map(|child| {
                    child
                        .parse::<Uuid>()
                        .map(MessageKey::from_storage)
                        .map_err(|_| {
                            ingress_substrate::IngressSubstrateError::InvalidStoredMessageKey.into()
                        })
                })
                .transpose();
        }
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            format!(
                "SELECT child_message_key::text FROM {} WHERE source_message_key = ?::uuid",
                ingress_substrate::prepared_ingress_relation(
                    prepared,
                    ingress_substrate::PreparedIngressRelation::OwnerRejections,
                )
            )
        } else {
            "SELECT child_message_key FROM ingress_owner_rejections WHERE source_message_key = ?"
                .to_owned()
        };
        let mut rows = tx
            .query(&sql, crate::db_params![source.to_storage().to_string()])
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let child: String = row.get(0)?;
        child
            .parse::<Uuid>()
            .map(MessageKey::from_storage)
            .map(Some)
            .map_err(|_| ingress_substrate::IngressSubstrateError::InvalidStoredMessageKey.into())
    }

    /// Map a rejected source to the child message that holds its frozen
    /// error. The source must be bound and undecided.
    pub(crate) async fn record_owner_rejection(
        transaction: &mut IngressUowTransaction<'_>,
        source: MessageKey,
        child: MessageKey,
    ) -> Result<(), IngressUowError> {
        // The caller proves and locks the source before writing its child.
        // Re-lock and recheck here as a local guard against a late mapping
        // after an acceptance latch has already committed.
        if !Self::lock(transaction, source).await? {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        }
        let (tx, prepared) = transaction.prepared_parts();
        if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
            let function = api.qualified_function("waddle_ingress_record_owner_rejection");
            if !super::candidate_boolean(
                tx,
                &format!("SELECT {function}(?::uuid, ?::uuid)"),
                crate::db_params![
                    source.to_storage().to_string(),
                    child.to_storage().to_string(),
                ],
            )
            .await?
            {
                return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
            }
            ingress_substrate::invalidate_retention_in_prepared_transaction(tx, prepared, source)
                .await?;
            return Ok(());
        }
        let state_sql = if tx.driver() == DatabaseDriver::Postgres {
            format!(
                "SELECT source_binding_version::int, owner_decision::int FROM {} WHERE message_key = ?::uuid",
                ingress_substrate::prepared_ingress_relation(
                    prepared,
                    ingress_substrate::PreparedIngressRelation::Messages,
                )
            )
        } else {
            "SELECT source_binding_version, owner_decision FROM ingress_messages WHERE message_key = ?"
                .to_owned()
        };
        let mut rows = tx
            .query(
                &state_sql,
                crate::db_params![source.to_storage().to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        };
        let source_version: Option<i32> = row.get(0)?;
        let owner_decision: Option<i32> = row.get(1)?;
        drop(rows);
        if source_version != Some(1) || owner_decision != Some(0) {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        }
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            format!(
                "INSERT INTO {} (source_message_key, child_message_key) VALUES (?::uuid, ?::uuid)",
                ingress_substrate::prepared_ingress_relation(
                    prepared,
                    ingress_substrate::PreparedIngressRelation::OwnerRejections,
                )
            )
        } else {
            "INSERT INTO ingress_owner_rejections (source_message_key, child_message_key) VALUES (?, ?)"
                .to_owned()
        };
        let inserted = tx
            .execute(
                &sql,
                crate::db_params![
                    source.to_storage().to_string(),
                    child.to_storage().to_string()
                ],
            )
            .await?;
        if inserted != 1 {
            return Err(IngressUowError::EffectIntentConflict);
        }
        ingress_substrate::invalidate_retention_in_prepared_transaction(tx, prepared, source)
            .await?;
        Ok(())
    }

    /// Read the owner-result state of a canonical message. The caller holds
    /// the message's row lock, so the state cannot change before commit.
    pub(crate) async fn room_owner_state(
        transaction: &mut IngressUowTransaction<'_>,
        source: MessageKey,
    ) -> Result<Option<RoomOwnerState>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            format!(
                "SELECT source_binding_version::int, owner_decision::int FROM {} WHERE message_key = ?::uuid",
                ingress_substrate::prepared_ingress_relation(
                    prepared,
                    ingress_substrate::PreparedIngressRelation::Messages,
                )
            )
        } else {
            "SELECT source_binding_version, owner_decision FROM ingress_messages WHERE message_key = ?"
                .to_owned()
        };
        let mut rows = tx
            .query(&sql, crate::db_params![source.to_storage().to_string()])
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let source_version: Option<i32> = row.get(0)?;
        let owner_decision: Option<i32> = row.get(1)?;
        let decision = match owner_decision {
            None => None,
            Some(0) => Some(RoomOwnerDecision::Undecided),
            Some(1) => Some(RoomOwnerDecision::Accepted),
            Some(2) => Some(RoomOwnerDecision::Rejected),
            Some(_) => {
                return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into())
            }
        };
        Ok(Some(RoomOwnerState {
            source_bound: source_version == Some(1),
            decision,
        }))
    }

    /// The authenticated principal a bound message was created by and the
    /// origin id its alias records, or `None` for a message without a source
    /// binding. A room owner that works from a mailbox entry rebuilds the
    /// sender's admission from these.
    pub(crate) async fn acceptance_source(
        transaction: &mut IngressUowTransaction<'_>,
        source: MessageKey,
    ) -> Result<Option<(AuthenticatedPrincipalRef, Option<OriginId>)>, IngressUowError> {
        use waddle_xmpp::auth::{AuthContextId, AuthContextVersion, PrincipalAuthEpoch};
        let malformed = || ingress_substrate::IngressSubstrateError::RelayedAliasUnproven;
        let (tx, prepared) = transaction.prepared_parts();
        if tx.driver() != DatabaseDriver::Postgres {
            return Err(IngressUowError::PostgresRequired);
        }
        let messages = ingress_substrate::prepared_ingress_relation(
            prepared,
            ingress_substrate::PreparedIngressRelation::Messages,
        );
        let aliases = ingress_substrate::prepared_ingress_relation(
            prepared,
            ingress_substrate::PreparedIngressRelation::Aliases,
        );
        let mut rows = tx
            .query(
                &format!(
                    "SELECT m.source_binding_version::int, m.source_bare_jid, \
                     m.source_auth_context_id::text, m.source_auth_context_version::text, \
                     m.source_auth_epoch::text, \
                     (SELECT a.origin_id FROM {aliases} a WHERE a.message_key = m.message_key LIMIT 1) \
                     FROM {messages} m WHERE m.message_key = ?::uuid"
                ),
                crate::db_params![source.to_storage().to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let version: Option<i32> = row.get(0)?;
        let bare: Option<String> = row.get(1)?;
        let context: Option<String> = row.get(2)?;
        let context_version: Option<String> = row.get(3)?;
        let epoch: Option<String> = row.get(4)?;
        let origin_id: Option<String> = row.get(5)?;
        let (Some(1), Some(bare), Some(context), Some(context_version), Some(epoch)) =
            (version, bare, context, context_version, epoch)
        else {
            return Ok(None);
        };
        let principal = AuthenticatedPrincipalRef::new(
            bare.parse().map_err(|_| malformed())?,
            AuthContextId::new(context.parse::<Uuid>().map_err(|_| malformed())?),
            AuthContextVersion::new(context_version.parse().map_err(|_| malformed())?),
            PrincipalAuthEpoch::new(epoch.parse().map_err(|_| malformed())?),
        );
        let origin_id = origin_id.map(OriginId::new);
        Ok(Some((principal, origin_id)))
    }

    /// Open the owner decision of a bound source that records a dispatch to
    /// its remote room. The decision starts undecided.
    pub(crate) async fn initialize_room_owner_decision(
        transaction: &mut IngressUowTransaction<'_>,
        source: MessageKey,
        room: &BareJid,
    ) -> Result<(), IngressUowError> {
        let recorded = EffectIntentRepository::load(transaction, source).await?;
        if !recorded.iter().any(|intent| {
            matches!(intent, IngressEffectIntent::DispatchToRoomRemote { room: saved, .. } if saved == room)
        }) {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        }
        let (tx, prepared) = transaction.prepared_parts();
        if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
            let function = api.qualified_function("waddle_ingress_initialize_owner_decision");
            if !super::candidate_boolean(
                tx,
                &format!("SELECT {function}(?::uuid)"),
                crate::db_params![source.to_storage().to_string()],
            )
            .await?
            {
                return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
            }
            return Ok(());
        }
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            format!(
                "UPDATE {} SET owner_decision = 0 WHERE message_key = ?::uuid AND owner_decision IS NULL AND source_binding_version = 1",
                ingress_substrate::prepared_ingress_relation(
                    prepared,
                    ingress_substrate::PreparedIngressRelation::Messages,
                )
            )
        } else {
            "UPDATE ingress_messages SET owner_decision = 0 WHERE message_key = ? AND owner_decision IS NULL AND source_binding_version = 1".to_owned()
        };
        let updated = tx
            .execute(&sql, crate::db_params![source.to_storage().to_string()])
            .await?;
        if updated != 1 {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        }
        Ok(())
    }

    /// Latch the owner's first decision on an undecided source. A rejection
    /// requires its child mapping; an acceptance forbids one.
    pub(crate) async fn decide_room_owner(
        transaction: &mut IngressUowTransaction<'_>,
        source: MessageKey,
        decision: RoomOwnerDecision,
    ) -> Result<(), IngressUowError> {
        let value = match decision {
            RoomOwnerDecision::Accepted => 1,
            RoomOwnerDecision::Rejected => 2,
            RoomOwnerDecision::Undecided => {
                return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
            }
        };
        let (tx, prepared) = transaction.prepared_parts();
        if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
            let function = api.qualified_function("waddle_ingress_decide_owner");
            if !super::candidate_boolean(
                tx,
                &format!("SELECT {function}(?::uuid, ?::integer)"),
                crate::db_params![value, source.to_storage().to_string()],
            )
            .await?
            {
                return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
            }
            ingress_substrate::invalidate_retention_in_prepared_transaction(tx, prepared, source)
                .await?;
            ingress_substrate::refresh_retention_in_prepared_transaction_with_db_clock(
                tx, prepared, source,
            )
            .await?;
            return Ok(());
        }
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            format!(
                "UPDATE {} SET owner_decision = ? WHERE message_key = ?::uuid AND owner_decision = 0",
                ingress_substrate::prepared_ingress_relation(
                    prepared,
                    ingress_substrate::PreparedIngressRelation::Messages,
                )
            )
        } else {
            "UPDATE ingress_messages SET owner_decision = ? WHERE message_key = ? AND owner_decision = 0".to_owned()
        };
        let updated = tx
            .execute(
                &sql,
                crate::db_params![value, source.to_storage().to_string()],
            )
            .await?;
        if updated != 1 {
            return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
        }
        // The immutable result changes the replay authority. Start a fresh
        // source tail once its own obligations are settled, even when a
        // rejection child must remain paired with it until collection.
        ingress_substrate::invalidate_retention_in_prepared_transaction(tx, prepared, source)
            .await?;
        ingress_substrate::refresh_retention_in_prepared_transaction_with_db_clock(
            tx, prepared, source,
        )
        .await?;
        Ok(())
    }

    /// Reopen a canonical row when reconciliation adds a new obligation.
    pub async fn clear_terminal(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<(), IngressUowError> {
        if !Self::lock(transaction, message_key).await? {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        let (tx, prepared) = transaction.prepared_parts();
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            format!(
                "UPDATE {} SET terminal_at = NULL, retention_eligible_at = NULL WHERE message_key = ?::uuid AND terminal_at IS NOT NULL",
                ingress_substrate::prepared_ingress_relation(
                    prepared,
                    ingress_substrate::PreparedIngressRelation::Messages
                )
            )
        } else {
            "UPDATE ingress_messages SET terminal_at = NULL, retention_eligible_at = NULL WHERE message_key = ? AND terminal_at IS NOT NULL".to_owned()
        };
        tx.execute(
            &sql,
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await?;
        Ok(())
    }

    pub async fn terminalize(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        proven_terminal_at: DateTime<Utc>,
    ) -> Result<TerminalizeOutcome, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::terminalize_message_in_prepared_transaction(
            tx,
            prepared,
            message_key,
            proven_terminal_at,
        )
        .await
        .map_err(Into::into)
    }
}

#[cfg(all(test, not(feature = "clustering")))]
fn validate_room_owner_effects(
    effects: &[IngressEffectIntent],
    room: &BareJid,
) -> Result<(), IngressUowError> {
    if effects.len() != 1
        || !effects.iter().all(|effect| {
            matches!(effect, IngressEffectIntent::RouteMucGroupchat { room: saved, .. } if saved == room)
        })
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    Ok(())
}

#[cfg(all(test, not(feature = "clustering")))]
fn owner_error_reply(
    source: &xmpp_parsers::message::Message,
    sender: &FullJid,
    room: &BareJid,
    error: &FrozenStanzaError,
) -> Result<xmpp_parsers::message::Message, IngressUowError> {
    if source.from.as_ref().and_then(|jid| jid.try_as_full().ok()) != Some(sender)
        || source.to.as_ref().map(jid::Jid::to_bare) != Some(room.clone())
    {
        return Err(ingress_substrate::IngressSubstrateError::RelayedAliasUnproven.into());
    }
    let mut reply = (*source).clone();
    reply.to = Some(sender.clone().into());
    reply.from = source.to.clone();
    reply.type_ = xmpp_parsers::message::MessageType::Error;
    reply.payloads.push(error.to_xmpp().into());
    Ok(reply)
}

#[cfg(all(test, not(feature = "clustering")))]
async fn pending_room_effect_receipts(
    transaction: &mut IngressUowTransaction<'_>,
    message_key: MessageKey,
    effects: &[IngressEffectIntent],
) -> Result<Vec<EffectReceiptKey>, IngressUowError> {
    let mut pending = Vec::new();
    for effect in effects {
        let receipt = crate::ingress::receipt_key(effect)?;
        if !EffectReceiptRepository::contains(
            transaction,
            message_key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await?
            && !pending.contains(&receipt)
        {
            pending.push(receipt);
        }
    }
    Ok(pending)
}

/// Repository for exact stream-management ingress references.
#[derive(Debug, Default, Clone, Copy)]
pub struct SmIngressRepository;

impl SmIngressRepository {
    pub async fn insert_sm_ref(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        ordinal: IngressOrdinal,
        wire_h: WireHandledCount,
        message_key: MessageKey,
    ) -> Result<MessageWriteOutcome, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::insert_sm_ref_in_prepared_transaction(
            tx,
            prepared,
            sm_ingress_id,
            ordinal,
            wire_h,
            message_key,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn lookup_wire_binding(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        wire_h: WireHandledCount,
    ) -> Result<Option<(MessageKey, IngressOrdinal)>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::lookup_wire_binding_in_prepared_transaction(
            tx,
            prepared,
            sm_ingress_id,
            wire_h,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn lookup(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        ordinal: IngressOrdinal,
    ) -> Result<Option<MessageKey>, IngressUowError> {
        const POSTGRES: &str = "SELECT message_key::text FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid AND ingress_ordinal = ?::numeric";
        const SQLITE: &str = "SELECT message_key FROM ingress_sm_refs WHERE sm_ingress_id = ? AND ingress_ordinal = ?";
        let sql = dialect_sql(transaction, POSTGRES, SQLITE);
        lookup_message_key(
            transaction,
            sql,
            crate::db_params![
                sm_ingress_id.to_storage().to_string(),
                ordinal.to_storage().to_string(),
            ],
        )
        .await
    }

    /// Read an indexed, bounded reference page, retaining the exact ordinals
    /// so retirement cannot delete references outside this page.
    pub async fn refs_for_stream_batch(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        limit: u32,
    ) -> Result<Vec<(IngressOrdinal, MessageKey)>, IngressUowError> {
        const POSTGRES: &str = "SELECT ingress_ordinal::text, message_key::text FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid ORDER BY ingress_ordinal LIMIT ?";
        const SQLITE: &str = "SELECT ingress_ordinal, message_key FROM ingress_sm_refs WHERE sm_ingress_id = ? ORDER BY ingress_ordinal LIMIT ?";
        let sql = dialect_sql(transaction, POSTGRES, SQLITE);
        let mut rows = transaction
            .transaction_mut()
            .query(
                sql,
                crate::db_params![sm_ingress_id.to_storage().to_string(), i64::from(limit)],
            )
            .await?;
        let mut refs = Vec::new();
        while let Some(row) = rows.next().await? {
            let ordinal: String = row.get(0)?;
            let ordinal = ordinal
                .parse::<u64>()
                .ok()
                .and_then(|value| IngressOrdinal::from_storage(value).ok())
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidStoredStream)?;
            let key: String = row.get(1)?;
            let key = key
                .parse::<Uuid>()
                .map(MessageKey::from_storage)
                .map_err(|_| ingress_substrate::IngressSubstrateError::InvalidStoredMessageKey)?;
            refs.push((ordinal, key));
        }
        Ok(refs)
    }

    pub async fn delete_stream_ref(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        ordinal: IngressOrdinal,
    ) -> Result<u64, IngressUowError> {
        let message_key = Self::lookup(transaction, sm_ingress_id, ordinal).await?;
        if let Some(key) = message_key {
            CanonicalMessageRepository::lock(transaction, key).await?;
        }
        let (tx, prepared) = transaction.prepared_parts();
        let deleted = if let Some(prepared) =
            prepared.filter(|prepared| prepared.uses_definer_api_candidate())
        {
            let function = format!(
                "{}.waddle_ingress_delete_sm_ref",
                ingress_substrate::quote_postgres_identifier(prepared.schema())
            );
            let mut rows = tx
                .query(
                    &format!("SELECT {function}(?::uuid, ?::numeric)"),
                    crate::db_params![
                        sm_ingress_id.to_storage().to_string(),
                        ordinal.to_storage().to_string()
                    ],
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let deleted: bool = row.get(0)?;
            if rows.next().await?.is_some() {
                return Err(
                    ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into(),
                );
            }
            u64::from(deleted)
        } else {
            const POSTGRES: &str = "DELETE FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid AND ingress_ordinal = ?::numeric";
            const SQLITE: &str =
                "DELETE FROM ingress_sm_refs WHERE sm_ingress_id = ? AND ingress_ordinal = ?";
            let sql = if tx.driver() == DatabaseDriver::Postgres {
                POSTGRES
            } else {
                SQLITE
            };
            tx.execute(
                sql,
                crate::db_params![
                    sm_ingress_id.to_storage().to_string(),
                    ordinal.to_storage().to_string()
                ],
            )
            .await?
        };
        if let Some(key) = message_key {
            ingress_substrate::refresh_retention_with_db_clock(tx, key).await?;
        }
        Ok(deleted)
    }
}

pub use ingress_substrate::FrontierOutcome;

/// Repository for ingress-stream enrollment and its contiguous frontier.
#[derive(Debug, Default, Clone, Copy)]
pub struct SmIngressStreamRepository;

impl SmIngressStreamRepository {
    /// Enroll the identity reserved by ingress admission, retaining the stream uniqueness check.
    pub async fn mint_reserved(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
        id: SmIngressId,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
            let function = format!(
                "{}.waddle_ingress_insert_sm_stream",
                ingress_substrate::quote_postgres_identifier(prepared.schema())
            );
            let mut rows = tx
                .query(
                    &format!("SELECT {function}(?::uuid, ?::text)"),
                    crate::db_params![id.to_storage().to_string(), stream_id.as_str().to_owned()],
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let _: bool = row.get(0)?;
            if rows.next().await?.is_some() {
                return Err(
                    ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into(),
                );
            }
            return Ok(());
        }
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::SmStreams,
            (
                "INSERT INTO ",
                " (sm_ingress_id, stream_id) VALUES (?::uuid, ?) ON CONFLICT (stream_id) DO NOTHING",
            ),
            "INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id) VALUES (?, ?) ON CONFLICT (stream_id) DO NOTHING",
        );
        tx.execute(
            &sql,
            crate::db_params![id.to_storage().to_string(), stream_id.as_str().to_owned()],
        )
        .await?;
        Ok(())
    }

    /// Bounded keyset scan; stream rows themselves retain retirement responsibility.
    pub async fn retirement_candidates(
        transaction: &mut IngressUowTransaction<'_>,
        after: Option<&SmSessionId>,
        limit: u32,
    ) -> Result<Vec<SmSessionId>, IngressUowError> {
        let mut rows = match after {
            Some(after) => transaction.transaction_mut().query(
                "SELECT stream_id FROM ingress_sm_streams WHERE stream_id > ? ORDER BY stream_id LIMIT ?",
                crate::db_params![after.as_str(), i64::from(limit)],
            ).await?,
            None => transaction.transaction_mut().query(
                "SELECT stream_id FROM ingress_sm_streams ORDER BY stream_id LIMIT ?",
                crate::db_params![i64::from(limit)],
            ).await?,
        };
        let mut streams = Vec::new();
        while let Some(row) = rows.next().await? {
            streams.push(SmSessionId::new(row.get::<String>(0)?));
        }
        Ok(streams)
    }

    /// Mint the one durable ingress row for a freshly SM-enabled stream.
    pub async fn mint(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<SmIngressId, IngressUowError> {
        const POSTGRES: (&str, &str) = (
            "INSERT INTO ",
            " (sm_ingress_id, stream_id) VALUES (?::uuid, ?) ON CONFLICT (stream_id) DO NOTHING",
        );
        const SQLITE: &str = "INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id) VALUES (?, ?) ON CONFLICT (stream_id) DO NOTHING";
        let (tx, prepared) = transaction.prepared_parts();
        let minted = SmIngressId::new();
        if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
            let function = format!(
                "{}.waddle_ingress_insert_sm_stream",
                ingress_substrate::quote_postgres_identifier(prepared.schema())
            );
            let mut rows = tx
                .query(
                    &format!("SELECT {function}(?::uuid, ?::text)"),
                    crate::db_params![
                        minted.to_storage().to_string(),
                        stream_id.as_str().to_string()
                    ],
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let inserted: bool = row.get(0)?;
            if rows.next().await?.is_some() {
                return Err(
                    ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into(),
                );
            }
            if inserted {
                return Ok(minted);
            }
            let lookup = format!(
                "{}.waddle_ingress_lookup_sm_stream",
                ingress_substrate::quote_postgres_identifier(prepared.schema())
            );
            let mut rows = tx
                .query(
                    &format!("SELECT {lookup}(?::text)::text"),
                    crate::db_params![stream_id.as_str().to_string()],
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let stored: Option<String> = row.get(0)?;
            if rows.next().await?.is_some() {
                return Err(
                    ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into(),
                );
            }
            let stored = stored.ok_or(IngressUowError::SmIngressStreamMissing)?;
            return stored
                .parse::<Uuid>()
                .map(SmIngressId::from_storage)
                .map_err(|_| IngressUowError::InvalidStoredSmIngressId);
        }
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::SmStreams,
            POSTGRES,
            SQLITE,
        );
        let inserted = tx
            .execute(
                &sql,
                crate::db_params![
                    minted.to_storage().to_string(),
                    stream_id.as_str().to_string()
                ],
            )
            .await?;
        if inserted == 1 {
            return Ok(minted);
        }
        const SELECT_POSTGRES: (&str, &str) =
            ("SELECT sm_ingress_id::text FROM ", " WHERE stream_id = ?");
        const SELECT_SQLITE: &str =
            "SELECT sm_ingress_id FROM ingress_sm_streams WHERE stream_id = ?";
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::SmStreams,
            SELECT_POSTGRES,
            SELECT_SQLITE,
        );
        let mut rows = tx
            .query(&sql, crate::db_params![stream_id.as_str().to_string()])
            .await?;
        let stored: String = rows
            .next()
            .await?
            .ok_or(IngressUowError::SmIngressStreamMissing)?
            .get(0)?;
        stored
            .parse::<Uuid>()
            .map(SmIngressId::from_storage)
            .map_err(|_| IngressUowError::InvalidStoredSmIngressId)
    }

    /// Lock an existing enrolled stream. This path never enrolls a stream.
    #[cfg(feature = "clustering")]
    pub async fn lock(
        transaction: &mut IngressUowTransaction<'_>,
        fence: &SmClaimFence<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<(SmIngressId, u64)>, IngressUowError> {
        if fence.transaction_identity != transaction.identity() || fence.stream_id != *stream_id {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        Self::lock_stream(transaction, stream_id).await
    }

    /// Lock a stream protected by this database's single-node write transaction.
    pub async fn lock_single_node(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<(SmIngressId, u64)>, IngressUowError> {
        if !matches!(transaction.fencing(), super::IngressFencing::SingleNode) {
            return Err(IngressUowError::SingleNodeFencingRequired);
        }
        Self::lock_stream(transaction, stream_id).await
    }

    async fn lock_stream(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<(SmIngressId, u64)>, IngressUowError> {
        const POSTGRES: (&str, &str) = (
            "SELECT sm_ingress_id::text, handled_ordinal::text FROM ",
            " WHERE stream_id = ? FOR UPDATE",
        );
        const SQLITE: &str = "SELECT sm_ingress_id, CAST(handled_ordinal AS TEXT) FROM ingress_sm_streams WHERE stream_id = ?";
        let (tx, prepared) = transaction.prepared_parts();
        if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
            let function = format!(
                "{}.waddle_ingress_lock_sm_stream_by_name",
                ingress_substrate::quote_postgres_identifier(prepared.schema())
            );
            let mut rows = tx
                .query(
                    &format!(
                        "SELECT sm_ingress_id::text, handled_ordinal::text \
                         FROM {function}(?::text)"
                    ),
                    crate::db_params![stream_id.as_str().to_string()],
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Ok(None);
            };
            let id: String = row.get(0)?;
            let frontier: String = row.get(1)?;
            if rows.next().await?.is_some() {
                return Err(
                    ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into(),
                );
            }
            let id = id
                .parse::<Uuid>()
                .map(SmIngressId::from_storage)
                .map_err(|_| IngressUowError::InvalidStoredSmIngressId)?;
            let frontier = frontier
                .parse::<u64>()
                .map_err(|_| IngressUowError::InvalidStoredFrontier)?;
            return Ok(Some((id, frontier)));
        }
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::SmStreams,
            POSTGRES,
            SQLITE,
        );
        let mut rows = tx
            .query(&sql, crate::db_params![stream_id.as_str().to_string()])
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let id: String = row.get(0)?;
        let frontier: String = row.get(1)?;
        let id = id
            .parse::<Uuid>()
            .map(SmIngressId::from_storage)
            .map_err(|_| IngressUowError::InvalidStoredSmIngressId)?;
        let frontier = frontier
            .parse::<u64>()
            .map_err(|_| IngressUowError::InvalidStoredFrontier)?;
        Ok(Some((id, frontier)))
    }

    pub async fn lookup_unclaimed(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<SmIngressId>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        Self::lookup_unclaimed_in_transaction(tx, prepared, stream_id).await
    }

    /// Retirement still uses unqualified scans, deletion and retention refresh.
    /// Keep its lookup on the same legacy path until that whole closure is qualified.
    pub(crate) async fn lookup_unclaimed_for_retirement(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<SmIngressId>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        // Retirement's ref scan and stream delete are unqualified, so the
        // lookup they act on stays unqualified too. Only the definer-API
        // candidate routes it through its function.
        let prepared = prepared.filter(|prepared| prepared.uses_definer_api_candidate());
        Self::lookup_unclaimed_in_transaction(tx, prepared, stream_id).await
    }

    async fn lookup_unclaimed_in_transaction(
        tx: &mut Transaction<'_>,
        prepared: Option<&ingress_substrate::PreparedAttestedReceiptWriter>,
        stream_id: &SmSessionId,
    ) -> Result<Option<SmIngressId>, IngressUowError> {
        const POSTGRES: (&str, &str) = (
            "SELECT sm_ingress_id::text FROM ",
            " WHERE stream_id = ? FOR UPDATE",
        );
        const SQLITE: &str = "SELECT sm_ingress_id FROM ingress_sm_streams WHERE stream_id = ?";
        if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
            let function = format!(
                "{}.waddle_ingress_lock_sm_stream_by_name",
                ingress_substrate::quote_postgres_identifier(prepared.schema())
            );
            let mut rows = tx
                .query(
                    &format!("SELECT sm_ingress_id::text FROM {function}(?::text)"),
                    crate::db_params![stream_id.as_str().to_string()],
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Ok(None);
            };
            let id: String = row.get(0)?;
            if rows.next().await?.is_some() {
                return Err(
                    ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into(),
                );
            }
            return id
                .parse::<Uuid>()
                .map(SmIngressId::from_storage)
                .map(Some)
                .map_err(|_| IngressUowError::InvalidStoredSmIngressId);
        }
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::SmStreams,
            POSTGRES,
            SQLITE,
        );
        let mut rows = tx
            .query(&sql, crate::db_params![stream_id.as_str().to_string()])
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let id: String = row.get(0)?;
        id.parse::<Uuid>()
            .map(SmIngressId::from_storage)
            .map(Some)
            .map_err(|_| IngressUowError::InvalidStoredSmIngressId)
    }

    /// Fence a retirement's unclaimed check against concurrent claim writes.
    /// A missing row cannot be protected by a row lock, so this rare cleanup
    /// path holds the claims table's SHARE lock through its ingress deletion.
    #[cfg(feature = "clustering")]
    pub async fn fence_claim_absence_for_retirement(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<bool, IngressUowError> {
        if transaction.bound_node_identity().is_none() {
            return Err(IngressUowError::NodeIdentityUnbound);
        }
        let (tx, prepared) = transaction.prepared_parts();
        let claims = ingress_substrate::prepared_ingress_relation(
            prepared,
            ingress_substrate::PreparedIngressRelation::Claims,
        );
        let lock = format!("LOCK TABLE {claims} IN SHARE MODE");
        tx.execute(&lock, ()).await?;
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Claims,
            ("SELECT 1 FROM ", " WHERE entity = ? AND entity_type = ?"),
            "SELECT 1 FROM clustering_claims WHERE entity = ? AND entity_type = ?",
        );
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![
                    format!("sm_session:{}", stream_id.as_str()),
                    "sm_session".to_string(),
                ],
            )
            .await?;
        Ok(rows.next().await?.is_none())
    }

    /// Advance the contiguous ordinal and its exposable wire checkpoint atomically.
    pub async fn advance_frontier(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        offered: IngressOrdinal,
        checkpoint_h: WireHandledCount,
    ) -> Result<FrontierOutcome, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::advance_frontier_in_prepared_transaction(
            tx,
            prepared,
            sm_ingress_id,
            offered,
            checkpoint_h,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn flush_checkpoint(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        h: WireHandledCount,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::flush_checkpoint_in_prepared_transaction(tx, prepared, sm_ingress_id, h)
            .await
            .map_err(Into::into)
    }

    pub async fn load_stream_checkpoint(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
    ) -> Result<Option<WireHandledCount>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::load_stream_checkpoint_in_prepared_transaction(
            tx,
            prepared,
            sm_ingress_id,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn delete_unclaimed(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<u64, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
            let function = format!(
                "{}.waddle_ingress_delete_sm_stream",
                ingress_substrate::quote_postgres_identifier(prepared.schema())
            );
            let mut rows = tx
                .query(
                    &format!("SELECT {function}(?::text)"),
                    crate::db_params![stream_id.as_str().to_string()],
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let deleted: bool = row.get(0)?;
            if rows.next().await?.is_some() {
                return Err(
                    ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into(),
                );
            }
            return Ok(u64::from(deleted));
        }
        tx.execute(
            "DELETE FROM ingress_sm_streams WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(Into::into)
    }
}

/// Repository for completed durable and post-commit ingress effects.
#[derive(Debug, Default, Clone, Copy)]
pub struct EffectReceiptRepository;

impl EffectReceiptRepository {
    pub async fn contains(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        kind: EffectReceiptKind,
        hash: &[u8; 32],
    ) -> Result<bool, IngressUowError> {
        if transaction.uses_definer_api_candidate() {
            let (tx, prepared) = transaction.prepared_parts();
            let prepared = prepared
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let api = ingress_substrate::DefinerApiCandidateContext::from_prepared(prepared)
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let query = format!(
                "SELECT {}(?::uuid, ?::integer, ?::bytea, NULL::text)",
                api.qualified_function("waddle_ingress_effect_receipt_exists"),
            );
            return super::candidate_boolean(
                tx,
                &query,
                crate::db_params![
                    message_key.to_storage().to_string(),
                    kind.to_storage(),
                    hash.to_vec()
                ],
            )
            .await;
        }
        let driver = transaction.transaction_mut().driver();
        let schema = receipt_schema(transaction);
        let sql = if driver == DatabaseDriver::Postgres {
            match schema {
                Some(schema) => format!(
                    "SELECT 1 FROM {}.ingress_effect_receipts WHERE message_key = ?::uuid \
                     AND kind = ? AND semantic_identity_hash = ?",
                    ingress_substrate::quote_postgres_identifier(schema)
                ),
                None => "SELECT 1 FROM ingress_effect_receipts WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ?".to_owned(),
            }
        } else {
            "SELECT 1 FROM ingress_effect_receipts WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ?".to_owned()
        };
        let mut rows = transaction
            .transaction_mut()
            .query(
                &sql,
                crate::db_params![
                    message_key.to_storage().to_string(),
                    kind.to_storage(),
                    hash.to_vec()
                ],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    pub async fn record_receipt(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        kind: EffectReceiptKind,
        hash: &[u8; 32],
    ) -> Result<(), IngressUowError> {
        if transaction.uses_definer_api_candidate() {
            let (tx, prepared) = transaction.prepared_parts();
            let prepared = prepared
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let api = ingress_substrate::DefinerApiCandidateContext::from_prepared(prepared)
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let query = format!(
                "SELECT {}(?::uuid, ?::integer, ?::bytea)",
                api.qualified_function("waddle_ingress_record_effect_receipt"),
            );
            let _ = super::candidate_boolean(
                tx,
                &query,
                crate::db_params![
                    message_key.to_storage().to_string(),
                    kind.to_storage(),
                    hash.to_vec()
                ],
            )
            .await?;
            return Ok(());
        }
        let schema = receipt_schema(transaction).map(str::to_owned);
        if let Some(schema) = schema {
            ingress_substrate::record_receipt_in_prepared_transaction(
                transaction.transaction_mut(),
                message_key,
                kind,
                hash,
                Some(&schema),
            )
            .await?;
            Ok(())
        } else {
            ingress_substrate::record_receipt(
                transaction.transaction_mut(),
                message_key,
                kind,
                hash,
            )
            .await
            .map_err(Into::into)
        }
    }

    pub async fn receipts_complete(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<bool, IngressUowError> {
        if transaction.uses_definer_api_candidate() {
            let (tx, prepared) = transaction.prepared_parts();
            let prepared = prepared
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let api = ingress_substrate::DefinerApiCandidateContext::from_prepared(prepared)
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let query = format!(
                "SELECT {}(?::uuid)",
                api.qualified_function("waddle_ingress_receipts_complete"),
            );
            return super::candidate_boolean(
                tx,
                &query,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await;
        }
        let schema = receipt_schema(transaction).map(str::to_owned);
        ingress_substrate::receipts_complete_in_schema(
            transaction.transaction_mut(),
            message_key,
            schema.as_deref(),
        )
        .await
        .map_err(Into::into)
    }

    pub(crate) async fn receipts_complete_on_transaction(
        transaction: &mut EnteredTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<bool, ingress_substrate::IngressSubstrateError> {
        ingress_substrate::receipts_complete(transaction, message_key).await
    }

    /// Every receipt identity recorded for the row, loaded in one query so a
    /// high-fanout row (thousands of intents) can be classified inside a
    /// bounded maintenance deadline instead of one lookup per intent.
    pub async fn keys(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<Vec<EffectReceiptKey>, IngressUowError> {
        if transaction.uses_definer_api_candidate() {
            let (_, prepared) = transaction.prepared_parts();
            let prepared = prepared
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let api = ingress_substrate::DefinerApiCandidateContext::from_prepared(prepared)
                .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
            let query = format!(
                "SELECT * FROM {}(?::uuid, ?::integer, ?::bytea, ?::integer)",
                api.qualified_function("waddle_ingress_list_effect_receipts"),
            );
            let page_size = super::DEFINER_API_PAGE_SIZE;
            let mut after: Option<(i64, Vec<u8>)> = None;
            let mut keys = Vec::new();
            loop {
                let mut rows = transaction
                    .transaction_mut()
                    .query(
                        &query,
                        crate::db_params![
                            message_key.to_storage().to_string(),
                            after.as_ref().map(|(kind, _)| *kind),
                            after.as_ref().map(|(_, hash)| hash.clone()),
                            page_size
                        ],
                    )
                    .await?;
                let mut page_count = 0_i64;
                let mut last = None;
                while let Some(row) = rows.next().await? {
                    let kind: i64 = row.get(0)?;
                    let hash: Vec<u8> = row.get(1)?;
                    let semantic_identity_hash: [u8; 32] = hash
                        .as_slice()
                        .try_into()
                        .map_err(|_| IngressUowError::InvalidStoredReceiptHash)?;
                    keys.push(EffectReceiptKey {
                        kind: EffectReceiptKind::from_storage(
                            i32::try_from(kind)
                                .map_err(|_| IngressUowError::InvalidStoredReceiptHash)?,
                        ),
                        semantic_identity_hash,
                    });
                    last = Some((kind, hash));
                    page_count += 1;
                }
                drop(rows);
                if page_count < page_size {
                    break;
                }
                after = last;
            }
            return Ok(keys);
        }
        let driver = transaction.transaction_mut().driver();
        let schema = receipt_schema(transaction);
        let sql = if driver == DatabaseDriver::Postgres {
            match schema {
                Some(schema) => format!(
                    "SELECT kind, semantic_identity_hash FROM {}.ingress_effect_receipts \
                     WHERE message_key = ?::uuid",
                    ingress_substrate::quote_postgres_identifier(schema)
                ),
                None => "SELECT kind, semantic_identity_hash FROM ingress_effect_receipts WHERE message_key = ?::uuid".to_owned(),
            }
        } else {
            "SELECT kind, semantic_identity_hash FROM ingress_effect_receipts WHERE message_key = ?"
                .to_owned()
        };
        let mut rows = transaction
            .transaction_mut()
            .query(
                &sql,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await?;
        let mut keys = Vec::new();
        while let Some(row) = rows.next().await? {
            let kind: i64 = row.get(0)?;
            let hash: Vec<u8> = row.get(1)?;
            let semantic_identity_hash: [u8; 32] = hash
                .try_into()
                .map_err(|_| IngressUowError::InvalidStoredReceiptHash)?;
            keys.push(EffectReceiptKey {
                kind: EffectReceiptKind::from_storage(
                    i32::try_from(kind).map_err(|_| IngressUowError::InvalidStoredReceiptHash)?,
                ),
                semantic_identity_hash,
            });
        }
        Ok(keys)
    }

    /// Receipts present for the row, for maintenance progress accounting.
    pub async fn record_receipt_pooled(
        db: &Database,
        message_key: MessageKey,
        kind: EffectReceiptKind,
        hash: &[u8; 32],
    ) -> Result<(), IngressUowError> {
        #[cfg(test)]
        let _ =
            super::settlement::POOLED_RECEIPT_WRITES.try_with(|count| count.set(count.get() + 1));
        ingress_substrate::record_receipt_pooled(db, message_key, kind, hash)
            .await
            .map_err(Into::into)
    }
}

/// Reconciliation preserves committed authority while repairing missing effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileVerdict {
    FirstCommit,
    Consistent,
    Repaired {
        inserted: Vec<IngressEffectKey>,
    },
    Contradiction {
        kind: IngressEffectKind,
        recorded: IngressEffectKey,
        planned: IngressEffectKey,
    },
    Divergent {
        kinds: Vec<IngressEffectKind>,
    },
}

/// Repository for inert, deterministic effect-intent rows.
#[derive(Debug, Default, Clone, Copy)]
pub struct EffectIntentRepository;

impl EffectIntentRepository {
    /// Report a changed retry plan without allowing it to revise a sealed audience.
    pub(crate) fn compare_sealed_replan(
        recorded: &[IngressEffectIntent],
        planned: &[IngressEffectIntent],
    ) -> Result<ReconcileVerdict, IngressUowError> {
        let recorded = recorded
            .iter()
            .cloned()
            .map(|intent| RecordedEffect { ordinal: 0, intent })
            .collect::<Vec<_>>();
        let planned = canonical_effects(planned)?;
        let (verdict, omissions) = compare_effects(&recorded, &planned, true);
        Ok(match verdict {
            ReconcileVerdict::Repaired { .. } => ReconcileVerdict::Divergent {
                kinds: omissions
                    .iter()
                    .map(|intent| intent.kind())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            },
            verdict => verdict,
        })
    }

    /// Load payload-complete recorded authority while holding the canonical row lock.
    pub async fn load(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<Vec<IngressEffectIntent>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Messages,
            ("SELECT 1 FROM ", " WHERE message_key = ?::uuid FOR UPDATE"),
            "SELECT 1 FROM ingress_messages WHERE message_key = ?",
        );
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        drop(rows);
        Ok(load_effects(tx, prepared, message_key)
            .await?
            .into_iter()
            .map(|row| row.intent)
            .collect())
    }

    pub async fn reconcile(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        planned: &[IngressEffectIntent],
        existing_authority: bool,
    ) -> Result<ReconcileVerdict, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        Self::reconcile_prepared(tx, prepared, message_key, planned, existing_authority).await
    }

    #[cfg(test)]
    pub(super) async fn reconcile_on_transaction(
        transaction: &mut EnteredTransaction<'_>,
        message_key: MessageKey,
        planned: &[IngressEffectIntent],
        existing_authority: bool,
    ) -> Result<ReconcileVerdict, IngressUowError> {
        Self::reconcile_prepared(transaction, None, message_key, planned, existing_authority).await
    }

    async fn reconcile_prepared(
        transaction: &mut EnteredTransaction<'_>,
        prepared: Option<&ingress_substrate::PreparedAttestedReceiptWriter>,
        message_key: MessageKey,
        planned: &[IngressEffectIntent],
        existing_authority: bool,
    ) -> Result<ReconcileVerdict, IngressUowError> {
        let sql = ingress_substrate::prepared_ingress_sql(
            transaction.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Messages,
            ("SELECT 1 FROM ", " WHERE message_key = ?::uuid FOR UPDATE"),
            "SELECT 1 FROM ingress_messages WHERE message_key = ?",
        );
        let mut message = transaction
            .query(
                &sql,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await?;
        if message.next().await?.is_none() {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        drop(message);

        let planned = canonical_effects(planned)?;
        let recorded = load_effects(transaction, prepared, message_key).await?;
        if let Some(manifest) = ingress_substrate::load_envelope_in_prepared_transaction(
            transaction,
            prepared,
            message_key,
        )
        .await?
        .and_then(|envelope| envelope.room_manifest().cloned())
        {
            // Deferred system archives seal before their position is assigned.
            // Only a None -> stored ordinal refinement may differ from that
            // seal; proposed values must match the current authoritative row.
            if recorded.iter().any(|row| {
                !manifest.recorded_intents().iter().any(|frozen| {
                    super::archive_ordinal::archive_intent_matches(&row.intent, frozen)
                })
            }) || manifest.recorded_intents().iter().any(|intent| {
                !recorded
                    .iter()
                    .any(|row| super::archive_ordinal::archive_intent_matches(&row.intent, intent))
            }) || planned.iter().any(|intent| {
                !recorded
                    .iter()
                    .any(|row| super::archive_ordinal::archive_intent_matches(&row.intent, intent))
            }) {
                return Err(IngressUowError::EffectIntentConflict);
            }
        }

        let (verdict, omissions) = compare_effects(&recorded, &planned, existing_authority);
        if matches!(verdict, ReconcileVerdict::Contradiction { .. }) {
            return Ok(verdict);
        }
        let mut ordinal = recorded
            .iter()
            .map(|row| row.ordinal)
            .max()
            .map(|ordinal| {
                ordinal
                    .checked_add(1)
                    .ok_or(IngressUowError::EffectIntentOrdinalOverflow)
            })
            .transpose()?
            .unwrap_or(0);
        if !omissions.is_empty() {
            ingress_substrate::invalidate_retention_in_prepared_transaction(
                transaction,
                prepared,
                message_key,
            )
            .await?;
        }
        for intent in omissions {
            insert_effect(transaction, prepared, message_key, ordinal, intent).await?;
            ordinal = ordinal
                .checked_add(1)
                .ok_or(IngressUowError::EffectIntentOrdinalOverflow)?;
        }
        Ok(verdict)
    }
}

struct RecordedEffect {
    ordinal: u64,
    intent: IngressEffectIntent,
}

fn canonical_effects(
    planned: &[IngressEffectIntent],
) -> Result<Vec<IngressEffectIntent>, IngressUowError> {
    let mut canonical = BTreeMap::new();
    for intent in planned {
        // Normalize recipient ordering through the frozen codec before comparing.
        let normalized = intent.with_encoded_v1(IngressEffectIntent::decode_v1)??;
        let key = normalized.semantic_key();
        if let Some(previous) = canonical.insert(key, normalized.clone()) {
            if previous != normalized {
                return Err(IngressUowError::EffectIntentConflict);
            }
        }
    }
    Ok(canonical.into_values().collect())
}

async fn load_effects(
    transaction: &mut Transaction<'_>,
    prepared: Option<&ingress_substrate::PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
) -> Result<Vec<RecordedEffect>, IngressUowError> {
    if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
        let api = ingress_substrate::DefinerApiCandidateContext::from_prepared(prepared)
            .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
        let query = format!(
            "SELECT * FROM {}(?::uuid, ?::numeric, ?::integer)",
            api.qualified_function("waddle_ingress_list_effect_intents"),
        );
        let page_size = super::DEFINER_API_PAGE_SIZE;
        let mut after_ordinal: Option<String> = None;
        let mut recorded = Vec::new();
        loop {
            let mut rows = transaction
                .query(
                    &query,
                    crate::db_params![
                        message_key.to_storage().to_string(),
                        after_ordinal.clone(),
                        page_size
                    ],
                )
                .await?;
            let mut page_count = 0_i64;
            let mut last_ordinal = None;
            while let Some(row) = rows.next().await? {
                let kind = i32::try_from(row.get::<i64>(0)?)
                    .map_err(|_| IngressUowError::EffectIntentConflict)?;
                let hash: Vec<u8> = row.get(1)?;
                let ordinal: String = row.get(2)?;
                let version: i64 = row.get(3)?;
                let payload: Vec<u8> = row.get(4)?;
                if version != 1 {
                    return Err(IngressUowError::EffectIntentConflict);
                }
                let intent = IngressEffectIntent::decode_v1(kind, &payload)?;
                if hash.as_slice() != semantic_identity_hash(&intent) {
                    return Err(IngressUowError::EffectIntentConflict);
                }
                recorded.push(RecordedEffect {
                    ordinal: ordinal
                        .parse()
                        .map_err(|_| IngressUowError::EffectIntentConflict)?,
                    intent,
                });
                last_ordinal = Some(ordinal);
                page_count += 1;
            }
            drop(rows);
            if page_count < page_size {
                break;
            }
            after_ordinal = last_ordinal;
        }
        return Ok(recorded);
    }

    let sql = ingress_substrate::prepared_ingress_sql(
        transaction.driver(),
        prepared,
        ingress_substrate::PreparedIngressRelation::Effects,
        (
            "SELECT kind::int, semantic_identity_hash, effect_ordinal::text, payload_version::int, payload FROM ",
            " WHERE message_key = ?::uuid ORDER BY effect_ordinal",
        ),
        "SELECT kind, semantic_identity_hash, CAST(effect_ordinal AS TEXT), payload_version, payload FROM ingress_effect_intents WHERE message_key = ? ORDER BY effect_ordinal",
    );
    let mut rows = transaction
        .query(
            &sql,
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await?;
    let mut recorded = Vec::new();
    while let Some(row) = rows.next().await? {
        let kind =
            i32::try_from(row.get::<i64>(0)?).map_err(|_| IngressUowError::EffectIntentConflict)?;
        let hash: Vec<u8> = row.get(1)?;
        let ordinal: String = row.get(2)?;
        let version: i64 = row.get(3)?;
        let payload: Vec<u8> = row.get(4)?;
        if version != 1 {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let intent = IngressEffectIntent::decode_v1(kind, &payload)?;
        if hash.as_slice() != semantic_identity_hash(&intent) {
            return Err(IngressUowError::EffectIntentConflict);
        }
        recorded.push(RecordedEffect {
            ordinal: ordinal
                .parse()
                .map_err(|_| IngressUowError::EffectIntentConflict)?,
            intent,
        });
    }
    Ok(recorded)
}

fn compare_effects<'a>(
    recorded: &[RecordedEffect],
    planned: &'a [IngressEffectIntent],
    existing_authority: bool,
) -> (ReconcileVerdict, Vec<&'a IngressEffectIntent>) {
    let mut omissions = Vec::new();
    let mut divergent = BTreeSet::new();
    for intent in planned {
        let authority = intent.authority_key();
        let matches = recorded
            .iter()
            .filter(|row| row.intent.authority_key() == authority)
            .collect::<Vec<_>>();
        if matches.is_empty() {
            // A planned effect under a new authority that reuses a recorded
            // semantic identity is a contradiction: the same immutable identity
            // cannot be claimed by two assigning authorities. Surfacing it here
            // keeps the insert below from failing on the identity primary key.
            if let Some(same_identity) = recorded
                .iter()
                .find(|row| row.intent.semantic_key() == intent.semantic_key())
            {
                return (
                    ReconcileVerdict::Contradiction {
                        kind: intent.kind(),
                        recorded: same_identity.intent.semantic_key(),
                        planned: intent.semantic_key(),
                    },
                    Vec::new(),
                );
            }
            // Two proposed archive identities under one authority are contradictory
            // even on a first commit, before any row has been inserted.
            if intent.kind() == IngressEffectKind::ArchiveAuthoritative {
                if let Some(other) = planned
                    .iter()
                    .find(|other| other.authority_key() == authority && *other != intent)
                {
                    return (
                        ReconcileVerdict::Contradiction {
                            kind: intent.kind(),
                            recorded: other.semantic_key(),
                            planned: intent.semantic_key(),
                        },
                        Vec::new(),
                    );
                }
            }
            // Replays cannot invent obligations for an empty acceptance or
            // add observers and carbon audiences from current policy. A remote
            // room owner may still establish its authority on first acceptance.
            if (existing_authority && recorded.is_empty())
                || (!recorded.is_empty()
                    && (notification_candidate_has_recorded_recovery(recorded, intent)
                        || matches!(
                            intent,
                            IngressEffectIntent::RelayCarbons { .. }
                                | IngressEffectIntent::Carbons { .. }
                        )
                        || matches!(intent, IngressEffectIntent::RoomObserver { room, plugin, .. }
                        if !room_observer_authority_pending(recorded, room, plugin))
                        || !inbox_omission_is_recorded_audience(recorded, intent, planned)))
            {
                divergent.insert(intent.kind());
            } else {
                omissions.push(intent);
            }
        } else if !matches
            .iter()
            .any(|row| super::archive_ordinal::archive_intent_matches(&row.intent, intent))
        {
            if intent.kind() == IngressEffectKind::ArchiveAuthoritative {
                return (
                    ReconcileVerdict::Contradiction {
                        kind: intent.kind(),
                        recorded: matches[0].intent.semantic_key(),
                        planned: intent.semantic_key(),
                    },
                    Vec::new(),
                );
            }
            divergent.insert(intent.kind());
        }
    }
    for row in recorded {
        if !planned
            .iter()
            .any(|intent| super::archive_ordinal::archive_intent_matches(&row.intent, intent))
        {
            divergent.insert(row.intent.kind());
        }
    }
    let verdict = if recorded.is_empty() && !existing_authority {
        ReconcileVerdict::FirstCommit
    } else if !divergent.is_empty() {
        ReconcileVerdict::Divergent {
            kinds: divergent.into_iter().collect(),
        }
    } else if !omissions.is_empty() {
        ReconcileVerdict::Repaired {
            inserted: omissions
                .iter()
                .map(|intent| intent.semantic_key())
                .collect(),
        }
    } else {
        ReconcileVerdict::Consistent
    };
    (verdict, omissions)
}

fn notification_candidate_has_recorded_recovery(
    recorded: &[RecordedEffect],
    planned: &IngressEffectIntent,
) -> bool {
    let IngressEffectIntent::NotificationActivityPreview {
        owner,
        mutation:
            waddle_xmpp::ingress::NotificationActivityMutation::NotificationCandidate {
                conversation,
                archive_stanza_id,
                ..
            },
    } = planned
    else {
        return false;
    };
    // Recovery owns this frozen decision, including a deferred decision later
    // settled by the sweep. Fresh policy cannot add a candidate obligation.
    recorded.iter().any(|row| {
        matches!(&row.intent, IngressEffectIntent::GroupchatNotificationRecovery { mutation }
            if &mutation.recipient == owner
                && &mutation.room == conversation
                && &mutation.archive_stanza_id == archive_stanza_id)
    })
}

fn room_observer_authority_pending(
    recorded: &[RecordedEffect],
    target_room: &BareJid,
    target_plugin: &waddle_extensions::PluginId,
) -> bool {
    let mut recorded_plugins = recorded.iter().filter_map(|row| match &row.intent {
        IngressEffectIntent::RoomObserver { room, plugin, .. } if room == target_room => {
            Some(plugin)
        }
        _ => None,
    });
    room_authority_pending(recorded, target_room)
        && recorded_plugins.next().is_none_or(|plugin| {
            plugin == target_plugin && recorded_plugins.all(|other| other == plugin)
        })
}

fn room_authority_pending(recorded: &[RecordedEffect], target_room: &BareJid) -> bool {
    recorded.iter().any(|row| {
        matches!(&row.intent,
        IngressEffectIntent::DispatchToRoomRemote { room, .. } if room == target_room)
    }) && !recorded.iter().any(|row| {
        matches!(&row.intent,
            IngressEffectIntent::ArchiveAuthoritative { archive, .. } if archive == target_room)
    })
}

/// A later room snapshot cannot turn a newly joined member into an original
/// recipient. Repair inbox obligations only within the committed authority and
/// audience. Remote owner first acceptance introduces a new room authority.
fn inbox_omission_is_recorded_audience(
    recorded: &[RecordedEffect],
    planned: &IngressEffectIntent,
    planned_intents: &[IngressEffectIntent],
) -> bool {
    use waddle_xmpp::ingress::EffectAuthorityKey;
    // Inbox pushes have their own RouteDirect identity. They cannot add a
    // recipient whose associated projection was rejected as audience growth.
    if let IngressEffectIntent::RouteDirect { recipient, .. } = planned {
        return planned_intents.iter().all(|intent| match intent {
            IngressEffectIntent::InboxProject { owner, .. } if owner == recipient => {
                inbox_omission_is_recorded_audience(recorded, intent, planned_intents)
            }
            _ => true,
        });
    }
    let (owner, partner) = match planned.authority_key() {
        EffectAuthorityKey::Inbox { owner, partner, .. } => (owner, partner),
        EffectAuthorityKey::Recovery {
            recipient, room, ..
        } => (recipient, room),
        EffectAuthorityKey::Conversation {
            owner,
            conversation,
        } if recorded.iter().any(|row| {
            matches!(&row.intent,
                IngressEffectIntent::RouteMucGroupchat { room, .. } if room == &conversation)
        }) =>
        {
            (owner, conversation)
        }
        _ => return true,
    };
    // A relayed owner is accepting the room authority for the first time, not
    // replanning an already committed room audience.
    if room_authority_pending(recorded, &partner) {
        return true;
    }
    let has_inbox_authority = recorded.iter().any(|row| {
        matches!(row.intent.authority_key(), EffectAuthorityKey::Inbox { partner: recorded_partner, .. }
            if recorded_partner == partner)
    });
    let owner_in_audience = recorded.iter().any(|row| match &row.intent {
        IngressEffectIntent::RouteMucGroupchat {
            room, occupants, ..
        } => room == &partner && occupants.iter().any(|occupant| occupant.to_bare() == owner),
        IngressEffectIntent::InboxProject { .. } => matches!(
            row.intent.authority_key(),
            EffectAuthorityKey::Inbox { owner: recorded_owner, partner: recorded_partner, .. }
                if recorded_owner == owner && recorded_partner == partner
        ),
        _ => false,
    });
    has_inbox_authority && owner_in_audience
}

async fn insert_effect(
    transaction: &mut EnteredTransaction<'_>,
    prepared: Option<&ingress_substrate::PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
    ordinal: u64,
    intent: &IngressEffectIntent,
) -> Result<(), IngressUowError> {
    let (kind, payload) = intent.with_encoded_v1(|kind, payload| (kind, payload.to_vec()))?;
    if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
        let api = ingress_substrate::DefinerApiCandidateContext::from_prepared(prepared)
            .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
        let query = format!(
            "SELECT {}(?::uuid, ?::numeric, ?::integer, ?::bytea, ?::bytea)",
            api.qualified_function("waddle_ingress_insert_effect_intent"),
        );
        let mut rows = transaction
            .query(
                &query,
                crate::db_params![
                    message_key.to_storage().to_string(),
                    ordinal.to_string(),
                    i64::from(kind),
                    semantic_identity_hash(intent).to_vec(),
                    payload
                ],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult)?;
        let inserted: bool = row.get(0)?;
        if rows.next().await?.is_some() || !inserted {
            return Err(ingress_substrate::IngressSubstrateError::InvalidDefinerApiResult.into());
        }
        return Ok(());
    }
    let sql = ingress_substrate::prepared_ingress_sql(
        transaction.driver(),
        prepared,
        ingress_substrate::PreparedIngressRelation::Effects,
        (
            "INSERT INTO ",
            " (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES (?::uuid, ?::numeric, ?, ?, 1, ?)",
        ),
        "INSERT INTO ingress_effect_intents (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES (?, ?, ?, ?, 1, ?)",
    );
    transaction
        .execute(
            &sql,
            crate::db_params![
                message_key.to_storage().to_string(),
                ordinal.to_string(),
                i64::from(kind),
                semantic_identity_hash(intent).to_vec(),
                payload
            ],
        )
        .await?;
    Ok(())
}

fn semantic_identity_hash(intent: &IngressEffectIntent) -> [u8; 32] {
    let identity = intent.semantic_key().storage_identity();
    Sha256::digest(identity.as_bytes()).into()
}

/// Result of checking a persisted authenticated principal under a share lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalAssertion {
    Asserted,
    PrincipalAssertionFailed,
}

/// Repository for the durable authority check of an authenticated principal.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrincipalRepository;

impl PrincipalRepository {
    pub async fn assert_principal(
        transaction: &mut IngressUowTransaction<'_>,
        principal: &AuthenticatedPrincipalRef,
    ) -> Result<PrincipalAssertion, IngressUowError> {
        const POSTGRES: (&str, &str) = (
            "SELECT expires_at FROM ",
            " WHERE user_jid = ? AND auth_context_id = ? AND auth_context_version = ? AND principal_auth_epoch = ? FOR SHARE",
        );
        const SQLITE: &str = "SELECT expires_at FROM sessions WHERE user_jid = ? AND auth_context_id = ? AND auth_context_version = ? AND principal_auth_epoch = ?";
        let (tx, prepared) = transaction.prepared_parts();
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Sessions,
            POSTGRES,
            SQLITE,
        );
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![
                    principal.bare_jid().to_string(),
                    principal.auth_context_id().as_uuid().to_string(),
                    i64::try_from(principal.auth_context_version().get())
                        .map_err(|_| IngressUowError::PrincipalReferenceOutOfRange)?,
                    i64::try_from(principal.auth_epoch().get())
                        .map_err(|_| IngressUowError::PrincipalReferenceOutOfRange)?,
                ],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(PrincipalAssertion::PrincipalAssertionFailed);
        };
        let expires_at: Option<String> = row.get(0)?;
        let expired = expires_at
            .map(|raw| {
                DateTime::parse_from_rfc3339(&raw)
                    .map(|expires_at| Utc::now() >= expires_at.with_timezone(&Utc))
                    .map_err(|_| IngressUowError::InvalidStoredPrincipalExpiry)
            })
            .transpose()?
            .unwrap_or(false);
        Ok(if expired {
            PrincipalAssertion::PrincipalAssertionFailed
        } else {
            PrincipalAssertion::Asserted
        })
    }
}

/// Repository for durable delivery/effect identities.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeliveryEffectRepository;

impl DeliveryEffectRepository {
    /// Bind a host-owned key to the persisted effect/target authority.
    pub async fn bind_effect(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        effect: &IngressEffectKey,
    ) -> Result<super::EffectDeliveryBinding, IngressUowError> {
        super::EffectDescendantRepository::bind_effect(transaction, message_key, effect).await
    }

    pub async fn record(
        transaction: &mut IngressUowTransaction<'_>,
        delivery_key: DeliveryKey,
        message_key: MessageKey,
    ) -> Result<MessageWriteOutcome, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        ingress_substrate::record_delivery_in_prepared_transaction(
            tx,
            prepared,
            delivery_key,
            message_key,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn lookup(
        transaction: &mut IngressUowTransaction<'_>,
        delivery_key: DeliveryKey,
    ) -> Result<Option<MessageKey>, IngressUowError> {
        const SQLITE: &str = "SELECT message_key FROM ingress_deliveries WHERE delivery_key = ?";
        let (tx, prepared) = transaction.prepared_parts();
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Deliveries,
            (
                "SELECT message_key::text FROM ",
                " WHERE delivery_key = ?::uuid",
            ),
            SQLITE,
        );
        lookup_message_key(
            transaction,
            &sql,
            crate::db_params![delivery_key.to_storage().to_string()],
        )
        .await
    }
}

fn dialect_sql(
    transaction: &mut IngressUowTransaction<'_>,
    postgres: &'static str,
    sqlite: &'static str,
) -> &'static str {
    match transaction.transaction_mut().driver() {
        DatabaseDriver::Postgres => postgres,
        DatabaseDriver::Sqlite => sqlite,
    }
}

fn receipt_schema<'transaction, 'database>(
    transaction: &'transaction IngressUowTransaction<'database>,
) -> Option<&'transaction str> {
    transaction.receipt_schema()
}

async fn lookup_message_key(
    transaction: &mut IngressUowTransaction<'_>,
    sql: &str,
    params: impl crate::db::IntoParams,
) -> Result<Option<MessageKey>, IngressUowError> {
    let sqlite_sql;
    let sql = if transaction
        .transaction_mut()
        .postgres_connection()
        .is_some()
    {
        sql
    } else {
        sqlite_sql = sql
            .replace("::text", "")
            .replace("::uuid", "")
            .replace("::numeric", "");
        &sqlite_sql
    };
    let mut rows = transaction.transaction_mut().query(sql, params).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let stored: String = row.get(0)?;
    let key = stored
        .parse::<Uuid>()
        .map(MessageKey::from_storage)
        .map_err(|_| ingress_substrate::IngressSubstrateError::InvalidStoredMessageKey)?;
    Ok(Some(key))
}

#[cfg(feature = "clustering")]
use std::marker::PhantomData;
#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::{ClaimEpoch, EntityType, NodeIdentity};

/// Repository that proves exact SM ownership before a fenced SM write.
#[cfg(feature = "clustering")]
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaimRepository;

/// Non-forgeable proof that this transaction holds one exact SM claim under
/// current local node authority.
///
/// It can only be minted after both the node-authority currency check
/// (against the canonical `SharedNodeIdentity` bound at
/// [`super::IngressUnitOfWork::open_with_node_identity`]) and the
/// `FOR SHARE` claim assertion. The minted `CurrentNodeIdentityGuard` is
/// retained by the transaction itself — not this independently droppable
/// value — so identity rotation or terminal disable cannot complete until
/// the transaction commits or rolls back, the same
/// publication-after-revocation gate the fenced SM persistence path holds
/// through its transactions. Its lifetime is tied to the unit of work so a
/// caller cannot carry it into another one.
#[cfg(feature = "clustering")]
#[derive(Debug)]
pub struct SmClaimFence<'transaction> {
    stream_id: SmSessionId,
    transaction_identity: Uuid,
    _transaction: PhantomData<&'transaction IngressUowTransaction<'transaction>>,
}

/// Non-forgeable proof that this transaction holds one exact room claim under
/// current local node authority.
///
/// It can only be minted after both the node-authority currency check
/// (against the canonical `SharedNodeIdentity` bound at
/// [`super::IngressUnitOfWork::open_with_node_identity`]) and the
/// `FOR SHARE` claim assertion. The minted `CurrentNodeIdentityGuard` is
/// retained by the transaction itself — not this independently droppable
/// value — so identity rotation or terminal disable cannot complete until
/// the transaction commits or rolls back, the same
/// publication-after-revocation gate the fenced room archive path holds
/// through its transactions. Its lifetime is tied to the unit of work so a
/// caller cannot carry it into another one.
#[cfg(feature = "clustering")]
#[derive(Debug)]
pub struct RoomClaimFence<'transaction> {
    room: BareJid,
    transaction_identity: Uuid,
    _transaction: PhantomData<&'transaction IngressUowTransaction<'transaction>>,
}

#[cfg(feature = "clustering")]
impl ClaimRepository {
    /// Assert the exact `(entity, node id, node incarnation, claim epoch)`
    /// row under `FOR SHARE`, after proving `owner` is still this node's
    /// current, active identity per the transaction's bound canonical
    /// identity source. Locks are taken in the fixed order
    /// epoch (at [`super::IngressUnitOfWork::begin`]) → exact claim
    /// (here) → `sm_sessions`/child rows (fenced repositories).
    pub async fn assert_sm_claim<'transaction>(
        transaction: &mut IngressUowTransaction<'transaction>,
        stream_id: &SmSessionId,
        owner: &NodeIdentity,
        claim_epoch: ClaimEpoch,
    ) -> Result<SmClaimFence<'transaction>, IngressUowError> {
        let authority = transaction.acquire_authority(owner).await?;
        let entity = format!(
            "{}:{}",
            EntityType::SmSession.as_db_str(),
            stream_id.as_str()
        );
        let (tx, prepared) = transaction.prepared_parts();
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Claims,
            (
                "SELECT 1 FROM ",
                " WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
            ),
            "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?",
        );
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![
                    entity,
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    claim_epoch.0,
                ],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        drop(rows);
        if let Some(authority) = authority {
            transaction.retain_authority(authority);
        }
        Ok(SmClaimFence {
            stream_id: stream_id.clone(),
            transaction_identity: transaction.identity(),
            _transaction: PhantomData,
        })
    }

    /// Assert the exact `(entity, node id, node incarnation, claim epoch)`
    /// row under `FOR SHARE`, after proving `owner` is still this node's
    /// current, active identity per the transaction's bound canonical
    /// identity source. Locks are taken in the fixed order
    /// epoch (at [`super::IngressUnitOfWork::begin`]) → exact claim
    /// (here) → room archive/child rows (fenced repositories).
    pub async fn assert_room_claim<'transaction>(
        transaction: &mut IngressUowTransaction<'transaction>,
        room: &BareJid,
        owner: &NodeIdentity,
        claim_epoch: ClaimEpoch,
    ) -> Result<RoomClaimFence<'transaction>, IngressUowError> {
        let authority = transaction.acquire_authority(owner).await?;
        let entity = format!("{}:{}", EntityType::RoomActor.as_db_str(), room);
        let (tx, prepared) = transaction.prepared_parts();
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Claims,
            (
                "SELECT 1 FROM ",
                " WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
            ),
            "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?",
        );
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![
                    entity,
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    claim_epoch.0,
                ],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        drop(rows);
        if let Some(authority) = authority {
            transaction.retain_authority(authority);
        }
        Ok(RoomClaimFence {
            room: room.clone(),
            transaction_identity: transaction.identity(),
            _transaction: PhantomData,
        })
    }

    /// Read which node incarnation holds the `UserActor` claim of an
    /// account, and its epoch, without locking the claim row. The result is
    /// a fence to present, not a proof that this node holds the claim.
    pub(crate) async fn user_claim(
        transaction: &mut IngressUowTransaction<'_>,
        account: &BareJid,
    ) -> Result<Option<(NodeIdentity, ClaimEpoch)>, IngressUowError> {
        let entity = format!("{}:{}", EntityType::UserActor.as_db_str(), account);
        let (tx, prepared) = transaction.prepared_parts();
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Claims,
            (
                "SELECT node_id, node_epoch, claim_epoch FROM ",
                " WHERE entity = ?",
            ),
            "SELECT node_id, node_epoch, claim_epoch FROM clustering_claims WHERE entity = ?",
        );
        let mut rows = tx.query(&sql, crate::db_params![entity]).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let node_id: String = row.get(0)?;
        let node_epoch: String = row.get(1)?;
        let claim_epoch: i64 = row.get(2)?;
        Ok(Some((
            NodeIdentity::new(node_id, node_epoch),
            ClaimEpoch(claim_epoch),
        )))
    }

    /// Assert the exact `UserActor` claim row of `account` under `FOR
    /// SHARE`, after proving `owner` is still this node's current, active
    /// identity. The claim cannot move before the transaction ends.
    pub async fn assert_user_claim(
        transaction: &mut IngressUowTransaction<'_>,
        account: &BareJid,
        owner: &NodeIdentity,
        claim_epoch: ClaimEpoch,
    ) -> Result<(), IngressUowError> {
        let authority = transaction.acquire_authority(owner).await?;
        let entity = format!("{}:{}", EntityType::UserActor.as_db_str(), account);
        let (tx, prepared) = transaction.prepared_parts();
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Claims,
            (
                "SELECT 1 FROM ",
                " WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
            ),
            "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?",
        );
        let mut rows = tx
            .query(
                &sql,
                crate::db_params![
                    entity,
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    claim_epoch.0,
                ],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        drop(rows);
        if let Some(authority) = authority {
            transaction.retain_authority(authority);
        }
        Ok(())
    }

    /// Read which node incarnation holds a room's claim, and its epoch,
    /// without locking the claim row. The result is a fence to present, not
    /// a proof that this node holds the claim.
    pub(crate) async fn room_claim(
        transaction: &mut IngressUowTransaction<'_>,
        room: &BareJid,
    ) -> Result<Option<(NodeIdentity, ClaimEpoch)>, IngressUowError> {
        let entity = format!("{}:{}", EntityType::RoomActor.as_db_str(), room);
        let (tx, prepared) = transaction.prepared_parts();
        let sql = ingress_substrate::prepared_ingress_sql(
            tx.driver(),
            prepared,
            ingress_substrate::PreparedIngressRelation::Claims,
            (
                "SELECT node_id, node_epoch, claim_epoch FROM ",
                " WHERE entity = ?",
            ),
            "SELECT node_id, node_epoch, claim_epoch FROM clustering_claims WHERE entity = ?",
        );
        let mut rows = tx.query(&sql, crate::db_params![entity]).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let node_id: String = row.get(0)?;
        let node_epoch: String = row.get(1)?;
        let claim_epoch: i64 = row.get(2)?;
        Ok(Some((
            NodeIdentity::new(node_id, node_epoch),
            ClaimEpoch(claim_epoch),
        )))
    }
}

#[cfg(test)]
#[path = "reconciliation_authority_tests.rs"]
mod reconciliation_authority_tests;
