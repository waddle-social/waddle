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

pub mod storage;

pub use storage::{InMemoryMamStorage, MamStorage, MamStorageError, SqlxMamStorage};
pub use waddle_xmpp_core::mam::{
    build_fin_iq, build_result_messages, is_mam_query, parse_mam_query, ArchiveId, ArchivedMention,
    ArchivedMessage, ArchivedModeration, ArchivedReactionSet, ArchivedReference, ArchivedReply,
    ArchivedRetraction, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone, MamQuery,
    MamResult, RichMessageId, RichText, MAM_NS, RSM_NS, STANZA_ID_NS,
};
pub use waddle_xmpp_core::xep::xep0359::add_stanza_id;
