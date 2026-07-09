use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use sqlx::Row;
use waddle_xmpp_core::mam::ArchivedMessage;

use crate::mam::storage::MamStorageError;

pub(super) fn decode_sqlite_message_row(
    row: &SqliteRow,
) -> Result<ArchivedMessage, MamStorageError> {
    let timestamp = DateTime::parse_from_rfc3339(&row.try_get::<String, _>(2)?)
        .map_err(|error| MamStorageError::Serialization(format!("Invalid timestamp: {error}")))?
        .with_timezone(&Utc);

    let rich_payload: Option<String> = row.try_get(13)?;
    let nickname_generation = decode_nickname_generation(row.try_get::<Option<i64>, _>(14)?)?;
    let thread_id_raw: Option<String> = row.try_get(7)?;
    let parent_thread_id_raw: Option<String> = row.try_get(15)?;
    let thread = decode_thread_columns(thread_id_raw, parent_thread_id_raw)?;
    let reply_to_id_raw: Option<String> = row.try_get(8)?;
    let reply_to_jid_raw: Option<String> = row.try_get(9)?;
    let reply = decode_reply_columns(reply_to_id_raw, reply_to_jid_raw)?;
    let from_raw: String = row.try_get(3)?;
    let to_raw: String = row.try_get(4)?;
    let archive_jid_raw: String = row.try_get(1)?;
    let archive_jid_for_decode = parse_archived_addressing("room_jid", &archive_jid_raw)?;
    let stanza_id_raw: Option<String> = row.try_get(6)?;
    let origin_id_raw: Option<String> = row.try_get(10)?;
    let message_type_raw: String = row.try_get(11)?;
    Ok(ArchivedMessage {
        id: row.try_get(0)?,
        timestamp,
        from: parse_archived_addressing("from_jid", &from_raw)?,
        to: parse_archived_addressing("to_jid", &to_raw)?,
        // Nullable TEXT — preserves the wire-fidelity distinction
        // between `NULL` (no `<body>` element) and `''` (empty
        // `<body></body>`). Explicit type to avoid ambiguity with
        // sqlx's inference.
        body: row.try_get::<Option<String>, _>(5)?,
        stanza_id: stanza_id_raw
            .map(|id| waddle_xmpp_core::xep0359::StanzaId::new(id, archive_jid_for_decode.clone())),
        thread,
        reply,
        origin_id: origin_id_raw.map(waddle_xmpp_core::xep0359::OriginId::new),
        message_type: parse_archived_message_type(&message_type_raw)?,
        stanza_xml: row.try_get(12)?,
        rich: decode_rich_payload(rich_payload.as_deref())?,
        nickname_generation,
    })
}

pub(super) fn decode_postgres_message_row(row: &PgRow) -> Result<ArchivedMessage, MamStorageError> {
    let rich_payload: Option<String> = row.try_get(13)?;
    let nickname_generation = decode_nickname_generation(row.try_get::<Option<i64>, _>(14)?)?;
    let thread_id_raw: Option<String> = row.try_get(7)?;
    let parent_thread_id_raw: Option<String> = row.try_get(15)?;
    let thread = decode_thread_columns(thread_id_raw, parent_thread_id_raw)?;
    let reply_to_id_raw: Option<String> = row.try_get(8)?;
    let reply_to_jid_raw: Option<String> = row.try_get(9)?;
    let reply = decode_reply_columns(reply_to_id_raw, reply_to_jid_raw)?;
    let from_raw: String = row.try_get(3)?;
    let to_raw: String = row.try_get(4)?;
    let archive_jid_raw: String = row.try_get(1)?;
    let archive_jid_for_decode = parse_archived_addressing("room_jid", &archive_jid_raw)?;
    let stanza_id_raw: Option<String> = row.try_get(6)?;
    let origin_id_raw: Option<String> = row.try_get(10)?;
    let message_type_raw: String = row.try_get(11)?;
    Ok(ArchivedMessage {
        id: row.try_get(0)?,
        timestamp: row.try_get(2)?,
        from: parse_archived_addressing("from_jid", &from_raw)?,
        to: parse_archived_addressing("to_jid", &to_raw)?,
        // See `decode_sqlite_message_row` — nullable, explicit type
        // for the wire-fidelity NULL/'' distinction.
        body: row.try_get::<Option<String>, _>(5)?,
        stanza_id: stanza_id_raw
            .map(|id| waddle_xmpp_core::xep0359::StanzaId::new(id, archive_jid_for_decode.clone())),
        thread,
        reply,
        origin_id: origin_id_raw.map(waddle_xmpp_core::xep0359::OriginId::new),
        message_type: parse_archived_message_type(&message_type_raw)?,
        stanza_xml: row.try_get(12)?,
        rich: decode_rich_payload(rich_payload.as_deref())?,
        nickname_generation,
    })
}

/// Combine the raw `thread_id` / `parent_thread_id` columns into a
/// typed [`waddle_xmpp_core::xep0201::ThreadInfo`].
///
/// SQL schema preserves the two columns; the in-memory representation
/// is collapsed (#228 commit 4). A row with `parent_thread_id` set
/// but `thread_id` NULL is malformed (RFC 6121 §5.2.5: parent is
/// meaningful only as a back-reference from a thread that has its own
/// id) and the typed shape would otherwise paper over the corruption,
/// so we hard-reject it as a serialization error rather than silently
/// dropping the parent.
fn decode_thread_columns(
    thread_id: Option<String>,
    parent_thread_id: Option<String>,
) -> Result<Option<waddle_xmpp_core::xep0201::ThreadInfo>, MamStorageError> {
    use waddle_xmpp_core::mam::ThreadId;
    use waddle_xmpp_core::xep0201::ThreadInfo;

    match (thread_id, parent_thread_id) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(MamStorageError::Serialization(
            "orphan parent_thread_id without thread_id".to_string(),
        )),
        (Some(raw_id), parent_raw) => {
            let id = ThreadId::new(raw_id).ok_or_else(|| {
                MamStorageError::Serialization("invalid (empty) thread_id".to_string())
            })?;
            let parent = match parent_raw {
                None => None,
                Some(raw_parent) => Some(ThreadId::new(raw_parent).ok_or_else(|| {
                    MamStorageError::Serialization("invalid (empty) parent_thread_id".to_string())
                })?),
            };
            Ok(Some(ThreadInfo { id, parent }))
        }
    }
}

/// Combine the raw `reply_to_id` / `reply_to_jid` columns into a typed
/// [`waddle_xmpp_core::mam::ArchivedReply`].
///
/// SQL schema preserves the two columns plus the
/// `idx_mam_room_reply_to` index; the in-memory representation is
/// collapsed (#228 commit 5). A row with `reply_to_jid` set but
/// `reply_to_id` NULL is malformed (XEP-0461 §3 makes `id` MUST and
/// `to` SHOULD — a `to` without an `id` cannot identify which message
/// is being replied to) and the typed shape would otherwise paper
/// over the corruption, so we hard-reject it as a serialization error
/// rather than silently dropping the orphan sender JID.
fn decode_reply_columns(
    reply_to_id: Option<String>,
    reply_to_jid: Option<String>,
) -> Result<Option<waddle_xmpp_core::mam::ArchivedReply>, MamStorageError> {
    use waddle_xmpp_core::mam::{ArchivedReply, RichMessageId};

    match (reply_to_id, reply_to_jid) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(MamStorageError::Serialization(
            "orphan reply_to_jid without reply_to_id".to_string(),
        )),
        (Some(id_raw), to_raw) => {
            let id = RichMessageId::new(id_raw)
                .ok_or_else(|| MamStorageError::Serialization("empty reply_to_id".to_string()))?;
            let to = to_raw
                .map(|raw| raw.parse::<jid::Jid>())
                .transpose()
                .map_err(|error| {
                    MamStorageError::Serialization(format!("invalid reply_to_jid: {error}"))
                })?;
            Ok(Some(ArchivedReply { id, to }))
        }
    }
}

/// Parse a `from_jid` / `to_jid` SQL column value into a typed
/// [`jid::Jid`]. Per the typed-decode hard-error policy, an
/// unparseable value is surfaced as `MamStorageError::Serialization`
/// — never silently substituted with a sentinel JID. This closes the
/// `parse_message_jid` "unknown@invalid" data-loss bug at the
/// storage decode boundary as well.
fn parse_archived_addressing(
    column: &'static str,
    value: &str,
) -> Result<jid::Jid, MamStorageError> {
    value.parse::<jid::Jid>().map_err(|error| {
        MamStorageError::Serialization(format!("Invalid {column} value '{value}': {error}"))
    })
}

/// Decode a stored `message_type` column value into the typed
/// [`xmpp_parsers::message::MessageType`] enum.
///
/// `xmpp-parsers` generates `FromStr` for `MessageType` via the
/// `generate_attribute!` macro (variants: `chat`, `error`,
/// `groupchat`, `headline`, `normal`). Any value outside that closed
/// set is database corruption — a write site bypassed the typed
/// encoder (`message_type_wire_str`) or the column was edited
/// manually. Per the typed-decode hard-error policy (#228 Q7) we
/// surface these as `MamStorageError::Serialization` rather than
/// papering over with a sentinel default. The error message echoes
/// the bad value and the column name so DB-corruption signatures are
/// visible at the boundary, mirroring `parse_archived_addressing`'s
/// pattern for `from_jid` / `to_jid`.
fn parse_archived_message_type(
    value: &str,
) -> Result<xmpp_parsers::message::MessageType, MamStorageError> {
    xmpp_parsers::message::MessageType::from_str(value).map_err(|error| {
        MamStorageError::Serialization(format!("Invalid message_type value '{value}': {error}"))
    })
}

pub(super) fn encode_rich_payload(
    message: &ArchivedMessage,
) -> Result<Option<String>, MamStorageError> {
    message
        .rich
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| MamStorageError::Serialization(error.to_string()))
}

pub(super) fn decode_rich_payload(
    value: Option<&str>,
) -> Result<Option<waddle_xmpp_core::mam::ArchivedRichMessage>, MamStorageError> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| MamStorageError::Serialization(error.to_string()))
}

/// Convert the database's signed `nickname_generation` column to the
/// typed `u64`. Negative values would only appear from corruption,
/// manual edits, or a write that bypassed `encode_nickname_generation`
/// — refuse them rather than wrap silently with `as u64`.
fn decode_nickname_generation(value: Option<i64>) -> Result<Option<u64>, MamStorageError> {
    value.map(u64::try_from).transpose().map_err(|error| {
        MamStorageError::Serialization(format!(
            "negative nickname_generation column value rejected: {error}"
        ))
    })
}

/// Convert a typed `u64` generation to the SQL backend's signed `i64`,
/// rejecting values outside `i64` range so the column never stores a
/// negative wrapped value that would later round-trip incorrectly.
pub(super) fn encode_nickname_generation(
    value: Option<u64>,
) -> Result<Option<i64>, MamStorageError> {
    value.map(i64::try_from).transpose().map_err(|error| {
        MamStorageError::Serialization(format!(
            "nickname_generation overflow on store ({error}) — exceeds i64::MAX"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_serialization_error(
        result: Result<Option<waddle_xmpp_core::xep0201::ThreadInfo>, MamStorageError>,
        expected_parts: &[&str],
    ) {
        let message = match result {
            Err(MamStorageError::Serialization(message)) => message,
            other => panic!("expected serialization error, got: {other:?}"),
        };
        for part in expected_parts {
            assert!(
                message.contains(part),
                "decode error should mention {part:?}; got: {message}"
            );
        }
    }

    #[test]
    fn xep_0201_decode_rejects_present_but_empty_thread_id_row() {
        // Q7 hard-error policy (PR #331 review): a non-NULL but empty
        // `thread_id` is malformed and must not be folded into `None`.
        assert_serialization_error(
            decode_thread_columns(Some(String::new()), Some("orphan-parent".to_string())),
            &["thread_id"],
        );
    }

    #[test]
    fn xep_0201_decode_rejects_present_but_empty_parent_thread_id_row() {
        assert_serialization_error(
            decode_thread_columns(Some("real-thread".to_string()), Some(String::new())),
            &["parent_thread_id"],
        );
    }

    #[test]
    fn xep_0201_decode_rejects_orphan_parent_thread_id_row() {
        assert_serialization_error(
            decode_thread_columns(None, Some("orphan-parent".to_string())),
            &["orphan", "parent_thread_id"],
        );
    }
}
