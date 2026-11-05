use crate::mam::MamArchiveKind;
use async_trait::async_trait;
use jid::BareJid;
use tracing::{instrument, Span};
use waddle_xmpp_core::mam::{ArchivedMessage, MamQuery, MamResult};
use waddle_xmpp_core::xep0359::OriginId;

use crate::mam::storage::{MamStorage, MamStorageError, StoreOutcome, TerminalTombstoneOutcome};
use crate::muc::RoomClaimFenceContext;

use super::{MamDatabaseBackend, SqlxMamStorage};

#[async_trait]
impl MamStorage for SqlxMamStorage {
    #[instrument(
        skip(self, message, archive_jid),
        err,
        fields(
            archive = %archive_jid,
            message_id = tracing::field::Empty,
            room = tracing::field::Empty,
        )
    )]
    async fn store_message(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<StoreOutcome, MamStorageError> {
        record_archive_span_fields(archive_jid, message);
        super::write::store_message(&self.backend, archive_jid, message).await
    }

    #[instrument(
        skip(self, message, fence, archive_jid),
        err,
        fields(
            archive = %archive_jid,
            message_id = tracing::field::Empty,
            room = tracing::field::Empty,
        )
    )]
    async fn store_message_fenced(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
        fence: &RoomClaimFenceContext,
    ) -> Result<StoreOutcome, MamStorageError> {
        record_archive_span_fields(archive_jid, message);
        if !self.fencing_enabled {
            return self.store_message(archive_jid, message).await;
        }
        super::write::store_message_fenced(&self.backend, archive_jid, message, fence).await
    }

    #[instrument(skip(self, archive_jid), fields(archive = %archive_jid))]
    async fn query_messages(
        &self,
        archive_jid: &BareJid,
        archive_kind: MamArchiveKind,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut conn = pool.acquire().await?;
                super::query_connection::query_sqlite_connection(
                    &mut conn,
                    archive_jid,
                    archive_kind,
                    query,
                )
                .await
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut conn = pool.acquire().await?;
                super::query_connection::query_postgres_connection(
                    &mut conn,
                    None,
                    archive_jid,
                    archive_kind,
                    query,
                )
                .await
            }
        }
    }

    #[instrument(skip(self))]
    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut conn = pool.acquire().await?;
                super::lookup::get_sqlite(&mut conn, archive_id).await
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut conn = pool.acquire().await?;
                super::lookup::get_postgres(&mut conn, None, archive_id).await
            }
        }
    }

    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut conn = pool.acquire().await?;
                super::lookup::get_sqlite_by_stanza_id(&mut conn, archive_jid, stanza_id).await
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut conn = pool.acquire().await?;
                super::lookup::get_postgres_by_stanza_id(&mut conn, None, archive_jid, stanza_id)
                    .await
            }
        }
    }

    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut conn = pool.acquire().await?;
                super::lookup::get_sqlite_by_message_id(&mut conn, archive_jid, message_id).await
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut conn = pool.acquire().await?;
                super::lookup::get_postgres_by_message_id(&mut conn, None, archive_jid, message_id)
                    .await
            }
        }
    }

    async fn get_message_by_sender_and_origin_id(
        &self,
        archive_jid: &BareJid,
        archive_kind: MamArchiveKind,
        sender: &jid::Jid,
        origin_id: &OriginId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut conn = pool.acquire().await?;
                super::lookup::get_sqlite_by_sender_and_origin_id(
                    &mut conn,
                    archive_jid,
                    archive_kind,
                    sender,
                    origin_id,
                )
                .await
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut conn = pool.acquire().await?;
                super::lookup::get_postgres_by_sender_and_origin_id(
                    &mut conn,
                    None,
                    archive_jid,
                    archive_kind,
                    sender,
                    origin_id,
                )
                .await
            }
        }
    }

    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut conn = pool.acquire().await?;
                super::lookup::sqlite(&mut conn, archive_jid, stanza_id).await
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut conn = pool.acquire().await?;
                super::lookup::postgres(&mut conn, None, archive_jid, stanza_id).await
            }
        }
    }

    #[instrument(skip(self, room_jid), fields(room = %room_jid))]
    async fn count_messages(&self, room_jid: &BareJid) -> Result<u32, MamStorageError> {
        match &self.backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut conn = pool.acquire().await?;
                super::query_connection::count_sqlite_connection(&mut conn, room_jid).await
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut conn = pool.acquire().await?;
                super::query_connection::count_postgres_connection(&mut conn, None, room_jid).await
            }
        }
    }

    #[instrument(skip(self, tombstone))]
    async fn replace_with_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        super::write::replace_with_tombstone(&self.backend, archive_id, tombstone).await
    }

    #[instrument(skip(self, tombstone))]
    async fn replace_with_terminal_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<TerminalTombstoneOutcome, MamStorageError> {
        super::write::replace_with_terminal_tombstone(&self.backend, archive_id, tombstone).await
    }
}

/// Fill in the deferred span attributes for an archive write: the
/// `room` label only for groupchat archives, and `message_id` only when
/// the message actually carries a stanza id — an id-less write leaves
/// the field unset rather than exporting `message_id=""` (#1439).
fn record_archive_span_fields(archive_jid: &BareJid, message: &ArchivedMessage) {
    if message.message_type == xmpp_parsers::message::MessageType::Groupchat {
        Span::current().record("room", tracing::field::display(archive_jid));
    }
    crate::telemetry::messages::record_span_message_id(
        message
            .stanza_id
            .as_ref()
            .map(|id| id.id.as_str())
            .unwrap_or(""),
    );
}
