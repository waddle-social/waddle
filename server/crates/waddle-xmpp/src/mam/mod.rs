//! Message Archive Management (MAM) implementation.
//!
//! Implements XEP-0313 for message history storage and retrieval.
//!
//! ## Storage
//!
//! The [`storage`] module provides persistent storage backends for archived
//! messages. The primary implementation uses sqlx-backed storage for SQLite and Postgres.
//!
//! ## IQ Handling
//!
//! Shared query/result models and reusable XML parsing/building helpers live in
//! `waddle-xmpp-core` so server and client crates can share typed history
//! primitives without runtime coupling.

pub mod projection;
pub mod storage;

pub use storage::{
    store_archived_message_on_connection, store_archived_message_on_sqlite_connection,
    ArchiveExpectation, InMemoryMamStorage, MamArchiveKind, MamStorage, MamStorageError,
    MamTxStoreError, MamTxStoreOutcome, SqlxMamStorage, StoreOutcome, TerminalTombstoneOutcome,
};
pub use waddle_xmpp_core::mam::{
    archived_inner_message, build_fin_iq, build_query_form_iq, build_result_messages, is_mam_query,
    is_mam_query_form_request, parse_mam_query, ArchivedMention, ArchivedMessage,
    ArchivedModeration, ArchivedReactionSet, ArchivedReference, ArchivedReply, ArchivedRetraction,
    ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone, MamQuery, MamResult,
    RichMessageId, RichText, ThreadId, FULLTEXT_MAM_FIELD, FULLTEXT_MAM_NS, MAM_NS, RSM_NS,
    STANZA_ID_NS,
};
