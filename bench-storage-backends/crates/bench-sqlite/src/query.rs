use bench_core::message::{ArchivedMessage, MamQuery};
use bench_core::store::StoreError;
use r2d2::Pool;

use crate::schema::UriManager;

pub(crate) fn run_query(
    pool: &Pool<UriManager>,
    q: &MamQuery,
) -> Result<Vec<ArchivedMessage>, StoreError> {
    let conn = pool.get().map_err(StoreError::backend)?;
    let mut sql = String::from(
        "SELECT id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, \
         reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml \
         FROM mam_messages WHERE room_jid = ?1",
    );
    let mut params_dyn: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(q.room_jid.clone())];
    let mut idx: usize = 2;
    if let Some(start) = q.start {
        sql.push_str(&format!(" AND timestamp >= ?{idx}"));
        params_dyn.push(Box::new(start.to_rfc3339()));
        idx += 1;
    }
    if let Some(end) = q.end {
        sql.push_str(&format!(" AND timestamp <= ?{idx}"));
        params_dyn.push(Box::new(end.to_rfc3339()));
        idx += 1;
    }
    if let Some(from) = &q.from_jid {
        sql.push_str(&format!(" AND from_jid = ?{idx}"));
        params_dyn.push(Box::new(from.clone()));
        idx += 1;
    }
    if let Some(before) = &q.before_id {
        sql.push_str(&format!(" AND id < ?{idx}"));
        params_dyn.push(Box::new(before.clone()));
        idx += 1;
    }
    if let Some(after) = &q.after_id {
        sql.push_str(&format!(" AND id > ?{idx}"));
        params_dyn.push(Box::new(after.clone()));
        idx += 1;
    }
    sql.push_str(" ORDER BY timestamp DESC");
    let limit = q.limit.max(1);
    sql.push_str(&format!(" LIMIT ?{idx}"));
    params_dyn.push(Box::new(limit as i64));

    let refs: Vec<&dyn rusqlite::ToSql> = params_dyn.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(StoreError::backend)?;
    let rows = stmt
        .query_map(refs.as_slice(), row_to_message)
        .map_err(StoreError::backend)?;
    let mut out = Vec::with_capacity(limit as usize);
    for r in rows {
        out.push(r.map_err(StoreError::backend)?);
    }
    Ok(out)
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArchivedMessage> {
    let ts: String = row.get(2)?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(&ts)
        .map(|d| d.with_timezone(&chrono::Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?;
    Ok(ArchivedMessage {
        id: row.get(0)?,
        room_jid: row.get(1)?,
        timestamp,
        from: row.get(3)?,
        to: row.get(4)?,
        body: row.get(5)?,
        stanza_id: row.get(6)?,
        thread_id: row.get(7)?,
        reply_to_id: row.get(8)?,
        reply_to_jid: row.get(9)?,
        origin_id: row.get(10)?,
        message_type: row.get(11)?,
        stanza_xml: row.get(12)?,
    })
}
