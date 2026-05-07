use chrono::Utc;
use jid::BareJid;

use crate::mam::storage::MamStorageError;

use super::{MamDatabaseBackend, SqlxMamStorage};

impl SqlxMamStorage {
    /// Test-only escape hatch that inserts a row directly via raw SQL,
    /// bypassing the typed encode path. Used to construct deliberately
    /// malformed rows (e.g. orphan `parent_thread_id` with NULL
    /// `thread_id`) so the decode-side hard-error contract can be
    /// tested. Gated behind `cfg(test)` for in-crate tests and the
    /// `test-utils` Cargo feature for cross-crate integration tests;
    /// sqlite-only.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub async fn insert_raw_thread_columns_for_test(
        &self,
        archive_jid: &BareJid,
        archive_id: &str,
        thread_id: Option<&str>,
        parent_thread_id: Option<&str>,
    ) -> Result<(), MamStorageError> {
        let MamDatabaseBackend::Sqlite(pool) = &self.backend else {
            return Err(MamStorageError::Database(
                "insert_raw_thread_columns_for_test is sqlite-only".to_string(),
            ));
        };
        let archive_jid_str = archive_jid.to_string();
        sqlx::query(
            "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) VALUES (?, ?, ?, ?, ?, NULL, NULL, ?, NULL, NULL, NULL, ?, NULL, NULL, NULL, ?)",
        )
        .bind(archive_id)
        .bind(archive_jid_str.as_str())
        .bind(Utc::now().to_rfc3339())
        .bind(archive_jid_str.as_str())
        .bind(archive_jid_str.as_str())
        .bind(thread_id)
        .bind("chat")
        .bind(parent_thread_id)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Test-only escape hatch that inserts a row with a deliberately
    /// malformed `from_jid` text column, bypassing the typed encode
    /// path. Used to construct rows that exercise the decode-side
    /// hard-error contract for `parse_archived_addressing` (a parse
    /// failure surfaces as `MamStorageError::Serialization` rather
    /// than collapsing to a sentinel JID, the data-loss bug
    /// `parse_message_jid` papered over). Gated behind `cfg(test)` for
    /// in-crate tests and the `test-utils` Cargo feature for
    /// cross-crate integration tests; sqlite-only.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub async fn insert_raw_from_jid_for_test(
        &self,
        archive_jid: &BareJid,
        archive_id: &str,
        raw_from: &str,
    ) -> Result<(), MamStorageError> {
        let MamDatabaseBackend::Sqlite(pool) = &self.backend else {
            return Err(MamStorageError::Database(
                "insert_raw_from_jid_for_test is sqlite-only".to_string(),
            ));
        };
        let archive_jid_str = archive_jid.to_string();
        sqlx::query(
            "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) VALUES (?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind(archive_id)
        .bind(archive_jid_str.as_str())
        .bind(Utc::now().to_rfc3339())
        .bind(raw_from)
        .bind(archive_jid_str.as_str())
        .bind("chat")
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Test-only escape hatch mirroring
    /// [`Self::insert_raw_thread_columns_for_test`] for the XEP-0461
    /// reply columns. Used to construct deliberately malformed rows
    /// (e.g. orphan `reply_to_jid` with NULL `reply_to_id`) so the
    /// decode-side hard-error contract for the collapsed
    /// `Option<ArchivedReply>` field can be tested. Gated behind
    /// `cfg(test)` for in-crate tests and the `test-utils` Cargo
    /// feature for cross-crate integration tests; sqlite-only.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub async fn insert_raw_reply_columns_for_test(
        &self,
        archive_jid: &BareJid,
        archive_id: &str,
        reply_to_id: Option<&str>,
        reply_to_jid: Option<&str>,
    ) -> Result<(), MamStorageError> {
        let MamDatabaseBackend::Sqlite(pool) = &self.backend else {
            return Err(MamStorageError::Database(
                "insert_raw_reply_columns_for_test is sqlite-only".to_string(),
            ));
        };
        let archive_jid_str = archive_jid.to_string();
        sqlx::query(
            "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) VALUES (?, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind(archive_id)
        .bind(archive_jid_str.as_str())
        .bind(Utc::now().to_rfc3339())
        .bind(archive_jid_str.as_str())
        .bind(archive_jid_str.as_str())
        .bind(reply_to_id)
        .bind(reply_to_jid)
        .bind("chat")
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Test-only escape hatch mirroring
    /// [`Self::insert_raw_thread_columns_for_test`] for the
    /// `message_type` column. Used to construct rows whose
    /// `message_type` SQL value is outside the closed RFC 6121
    /// §5.2.2 set (`chat`, `error`, `groupchat`, `headline`,
    /// `normal`) so the decode-side hard-error contract for the
    /// typed [`xmpp_parsers::message::MessageType`] field can be
    /// tested. Gated behind `cfg(test)` for in-crate tests and the
    /// `test-utils` Cargo feature for cross-crate integration tests;
    /// sqlite-only.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub async fn insert_raw_message_type_for_test(
        &self,
        archive_jid: &BareJid,
        archive_id: &str,
        raw_message_type: &str,
    ) -> Result<(), MamStorageError> {
        let MamDatabaseBackend::Sqlite(pool) = &self.backend else {
            return Err(MamStorageError::Database(
                "insert_raw_message_type_for_test is sqlite-only".to_string(),
            ));
        };
        let archive_jid_str = archive_jid.to_string();
        sqlx::query(
            "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) VALUES (?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, NULL, ?, NULL, NULL, NULL, NULL)",
        )
        .bind(archive_id)
        .bind(archive_jid_str.as_str())
        .bind(Utc::now().to_rfc3339())
        .bind(archive_jid_str.as_str())
        .bind(archive_jid_str.as_str())
        .bind(raw_message_type)
        .execute(pool)
        .await?;
        Ok(())
    }
}
