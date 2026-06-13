use std::collections::HashSet;

use jid::BareJid;
use sqlx::postgres::PgPool;
use sqlx::sqlite::SqlitePool;
use sqlx::{Postgres, QueryBuilder, Sqlite};
use waddle_xmpp_core::mam::{ArchivedMessage, MamQuery};

use crate::mam::storage::MamStorageError;

use super::decode::{decode_postgres_message_row, decode_sqlite_message_row};
use super::schema::SELECT_COLUMNS;
use crate::mam::storage::query_semantics::missing_requested_id_in_set;

/// Pre-computed bind values for the SQL `with` filter.
///
/// XEP-0313 §4.3.1 distinguishes bare and full `with` semantics:
/// bare matches the archived JID's bare form (resource may be any /
/// absent); full matches only exact equality. Pre-computing both
/// values keeps the bind site lifetime stable for sqlx and makes the
/// matching shape readable at the SQL emit site.
pub(super) struct WithFilter {
    /// The bare form of the query `with`. Used for both equality
    /// (against an archived bare JID) and as the prefix in
    /// `LIKE 'bare/%'` (against an archived full JID).
    bare: String,
    /// `Some(full_form)` when the query `with` carries a resource;
    /// in that case the SQL emits an exact-equality match against the
    /// full form. `None` when the query `with` is bare.
    full: Option<String>,
}

impl WithFilter {
    pub(super) fn from_with(with: &jid::Jid) -> Self {
        Self {
            bare: with.to_bare().to_string(),
            full: with.resource().is_some().then(|| with.to_string()),
        }
    }

    /// SQL `LIKE` pattern matching `<bare>/<any-resource>`. The bare
    /// portion is escaped so any `%`, `_`, or `\` characters in the
    /// localpart (legal per RFC 7622) are treated literally instead of
    /// as LIKE wildcards. Pair the produced pattern with `ESCAPE '\\'`
    /// at the SQL site (see [`LIKE_ESCAPE`]).
    fn bare_resource_prefix(&self) -> String {
        format!("{}/%", escape_like_pattern(&self.bare))
    }
}

/// Escape character used with the SQL `LIKE ... ESCAPE '\'` clause. A
/// single backslash, doubled to escape the Rust string literal.
const LIKE_ESCAPE: &str = "\\";

/// Escape SQL `LIKE` pattern metacharacters in `value` so they match
/// literally. Mirrors the `ESCAPE '\\'` clause emitted alongside the
/// resulting bind. Order matters: the escape character itself must be
/// doubled first so the subsequent `%`/`_` substitutions don't double-
/// escape.
fn escape_like_pattern(value: &str) -> String {
    value
        .replace(LIKE_ESCAPE, "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

macro_rules! push_common_mam_filters {
    ($builder:expr, $query:expr, $with_filter:expr) => {{
        if let Some(with) = $with_filter {
            // Bare `with`: archived JID may be bare (`= bare`) or full
            // with any resource (`LIKE 'bare/%'`). The resource prefix
            // MUST include the `/` separator so domain prefix
            // collisions (`example.com` vs `example.com.evil`) cannot
            // match.
            //
            // Full `with`: archived JID must equal exactly.
            if let Some(full) = with.full.as_deref() {
                $builder
                    .push(" AND (from_jid = ")
                    .push_bind(full)
                    .push(" OR to_jid = ")
                    .push_bind(full)
                    .push(")");
            } else {
                let bare = with.bare.as_str();
                let resource_prefix = with.bare_resource_prefix();
                $builder
                    .push(" AND (from_jid = ")
                    .push_bind(bare)
                    .push(" OR from_jid LIKE ")
                    .push_bind(resource_prefix.clone())
                    .push(" ESCAPE '\\' OR to_jid = ")
                    .push_bind(bare)
                    .push(" OR to_jid LIKE ")
                    .push_bind(resource_prefix)
                    .push(" ESCAPE '\\')");
            }
        }
        if !$query.ids.is_empty() {
            $builder.push(" AND id IN (");
            let mut ids = $builder.separated(", ");
            for id in &$query.ids {
                ids.push_bind(id.as_str());
            }
            ids.push_unseparated(")");
        }
        if !$query.stanza_ids.is_empty() {
            // Room pins pass archive-primary IDs. DM pins pass the pair-stable
            // logical message ID, stored as the wire `<message id>` on each
            // participant's personal archive row. Match both so the same MAM
            // filter hydrates pinned bodies for rooms and DMs.
            $builder.push(" AND (id IN (");
            let mut ids = $builder.separated(", ");
            for id in &$query.stanza_ids {
                ids.push_bind(id.as_str());
            }
            ids.push_unseparated(") OR stanza_id IN (");
            let mut stanza_ids = $builder.separated(", ");
            for id in &$query.stanza_ids {
                stanza_ids.push_bind(id.as_str());
            }
            stanza_ids.push_unseparated("))");
        }
        if let Some(thread_id) = $query.thread_id.as_ref() {
            $builder
                .push(" AND (id = ")
                .push_bind(thread_id.as_str())
                .push(" OR stanza_id = ")
                .push_bind(thread_id.as_str())
                .push(" OR thread_id = ")
                .push_bind(thread_id.as_str())
                .push(" OR (thread_id IS NULL AND reply_to_id = ")
                .push_bind(thread_id.as_str())
                .push("))");
        }
        if let Some(fulltext) = $query.fulltext.as_ref() {
            for term in fulltext.as_str().split_whitespace() {
                $builder
                    .push(" AND LOWER(body) LIKE ")
                    .push_bind(format!("%{}%", term.to_lowercase()));
            }
        }
    }};
}

pub(super) fn push_sqlite_mam_filters<'args>(
    builder: &mut QueryBuilder<'args, Sqlite>,
    archive_jid: &'args str,
    query: &'args MamQuery,
    with_filter: Option<&'args WithFilter>,
    filter_before: Option<&'args ArchivedMessage>,
    filter_after: Option<&'args ArchivedMessage>,
) {
    builder.push_bind(archive_jid);
    if let Some(start) = query.start {
        builder
            .push(" AND timestamp >= ")
            .push_bind(start.to_rfc3339());
    }
    if let Some(end) = query.end {
        builder
            .push(" AND timestamp <= ")
            .push_bind(end.to_rfc3339());
    }
    if let Some(cursor) = filter_before {
        builder
            .push(" AND (timestamp < ")
            .push_bind(cursor.timestamp.to_rfc3339())
            .push(" OR (timestamp = ")
            .push_bind(cursor.timestamp.to_rfc3339())
            .push(" AND id < ")
            .push_bind(cursor.id.as_str())
            .push("))");
    }
    if let Some(cursor) = filter_after {
        builder
            .push(" AND (timestamp > ")
            .push_bind(cursor.timestamp.to_rfc3339())
            .push(" OR (timestamp = ")
            .push_bind(cursor.timestamp.to_rfc3339())
            .push(" AND id > ")
            .push_bind(cursor.id.as_str())
            .push("))");
    }
    push_common_mam_filters!(builder, query, with_filter);
}

pub(super) fn push_postgres_mam_filters<'args>(
    builder: &mut QueryBuilder<'args, Postgres>,
    archive_jid: &'args str,
    query: &'args MamQuery,
    with_filter: Option<&'args WithFilter>,
    filter_before: Option<&'args ArchivedMessage>,
    filter_after: Option<&'args ArchivedMessage>,
) {
    builder.push_bind(archive_jid);
    if let Some(start) = query.start {
        builder.push(" AND timestamp >= ").push_bind(start);
    }
    if let Some(end) = query.end {
        builder.push(" AND timestamp <= ").push_bind(end);
    }
    if let Some(cursor) = filter_before {
        builder
            .push(" AND (timestamp < ")
            .push_bind(cursor.timestamp)
            .push(" OR (timestamp = ")
            .push_bind(cursor.timestamp)
            .push(" AND id < ")
            .push_bind(cursor.id.as_str())
            .push("))");
    }
    if let Some(cursor) = filter_after {
        builder
            .push(" AND (timestamp > ")
            .push_bind(cursor.timestamp)
            .push(" OR (timestamp = ")
            .push_bind(cursor.timestamp)
            .push(" AND id > ")
            .push_bind(cursor.id.as_str())
            .push("))");
    }
    push_common_mam_filters!(builder, query, with_filter);
}

pub(super) async fn fetch_sqlite_cursor(
    pool: &SqlitePool,
    archive_jid: &BareJid,
    cursor_id: &str,
) -> Result<ArchivedMessage, MamStorageError> {
    let mut builder = QueryBuilder::<Sqlite>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
    ));
    builder.push_bind(archive_jid.to_string());
    builder.push(" AND id = ").push_bind(cursor_id);

    let row = builder.build().fetch_optional(pool).await?;
    row.as_ref()
        .map(decode_sqlite_message_row)
        .transpose()?
        .ok_or_else(|| MamStorageError::NotFound(cursor_id.to_string()))
}

pub(super) async fn fetch_postgres_cursor(
    pool: &PgPool,
    archive_jid: &BareJid,
    cursor_id: &str,
) -> Result<ArchivedMessage, MamStorageError> {
    let mut builder = QueryBuilder::<Postgres>::new(format!(
        "SELECT {SELECT_COLUMNS} FROM mam_messages WHERE room_jid = "
    ));
    builder.push_bind(archive_jid.to_string());
    builder.push(" AND id = ").push_bind(cursor_id);

    let row = builder.build().fetch_optional(pool).await?;
    row.as_ref()
        .map(decode_postgres_message_row)
        .transpose()?
        .ok_or_else(|| MamStorageError::NotFound(cursor_id.to_string()))
}

pub(super) async fn ensure_sqlite_requested_ids_exist(
    pool: &SqlitePool,
    archive_jid: &BareJid,
    requested_ids: &[String],
) -> Result<(), MamStorageError> {
    if requested_ids.is_empty() {
        return Ok(());
    }

    let mut builder = QueryBuilder::<Sqlite>::new("SELECT id FROM mam_messages WHERE room_jid = ");
    builder.push_bind(archive_jid.to_string());
    builder.push(" AND id IN (");
    let mut ids = builder.separated(", ");
    for id in requested_ids {
        ids.push_bind(id.as_str());
    }
    ids.push_unseparated(")");

    let available_ids = builder
        .build_query_scalar::<String>()
        .fetch_all(pool)
        .await?;
    let available_ids = available_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if let Some(missing_id) = missing_requested_id_in_set(&available_ids, requested_ids) {
        return Err(MamStorageError::NotFound(missing_id));
    }

    Ok(())
}

pub(super) async fn ensure_postgres_requested_ids_exist(
    pool: &PgPool,
    archive_jid: &BareJid,
    requested_ids: &[String],
) -> Result<(), MamStorageError> {
    if requested_ids.is_empty() {
        return Ok(());
    }

    let mut builder =
        QueryBuilder::<Postgres>::new("SELECT id FROM mam_messages WHERE room_jid = ");
    builder.push_bind(archive_jid.to_string());
    builder.push(" AND id IN (");
    let mut ids = builder.separated(", ");
    for id in requested_ids {
        ids.push_bind(id.as_str());
    }
    ids.push_unseparated(")");

    let available_ids = builder
        .build_query_scalar::<String>()
        .fetch_all(pool)
        .await?;
    let available_ids = available_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if let Some(missing_id) = missing_requested_id_in_set(&available_ids, requested_ids) {
        return Err(MamStorageError::NotFound(missing_id));
    }

    Ok(())
}
