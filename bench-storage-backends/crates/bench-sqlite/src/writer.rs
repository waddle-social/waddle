use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bench_core::message::ArchivedMessage;
use bench_core::store::StoreError;
use hdrhistogram::Histogram;
use rusqlite::{params, Connection, OpenFlags};

use crate::schema::{apply_pragmas, MAM_SCHEMA};
use crate::SqliteBacking;

/// Message sent to the writer thread.
pub(crate) struct WriteJob {
    pub(crate) msg: ArchivedMessage,
    /// Instant at which the caller pushed this job onto the queue.
    pub(crate) enqueued_at: Instant,
    pub(crate) reply: tokio::sync::oneshot::Sender<Result<(), StoreError>>,
}

pub(crate) fn writer_loop(
    uri: String,
    flags: OpenFlags,
    backing: SqliteBacking,
    rx: mpsc::Receiver<WriteJob>,
    queue_wait: Arc<Mutex<Histogram<u64>>>,
    sql_exec: Arc<Mutex<Histogram<u64>>>,
) {
    let conn = match Connection::open_with_flags(&uri, flags) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "writer failed to open db");
            return;
        }
    };
    if let Err(e) = apply_pragmas(&conn, &backing) {
        tracing::error!(error = %e, "writer failed to apply pragmas");
        return;
    }
    // Writer is authoritative for DDL. `CREATE IF NOT EXISTS` is idempotent
    // so running it again when the keepalive also created it is harmless.
    if let Err(e) = conn.execute_batch(MAM_SCHEMA) {
        tracing::error!(error = %e, "writer failed to init schema");
        return;
    }

    const INSERT_SQL: &str = r#"INSERT INTO mam_messages
            (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id,
             thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#;

    while let Ok(job) = rx.recv() {
        let qw = job.enqueued_at.elapsed();
        if let Ok(mut h) = queue_wait.lock() {
            let _ = h.record(qw.as_nanos().min(u64::MAX as u128) as u64);
        }

        let m = &job.msg;
        let ts = m.timestamp.to_rfc3339();
        let exec_start = Instant::now();
        let result = conn
            .prepare_cached(INSERT_SQL)
            .and_then(|mut stmt| {
                stmt.execute(params![
                    m.id,
                    m.room_jid,
                    ts,
                    m.from,
                    m.to,
                    m.body,
                    m.stanza_id,
                    m.thread_id,
                    m.reply_to_id,
                    m.reply_to_jid,
                    m.origin_id,
                    m.message_type,
                    m.stanza_xml,
                ])
                .map(|_| ())
            })
            .map_err(StoreError::backend);
        let exec = exec_start.elapsed();
        if let Ok(mut h) = sql_exec.lock() {
            let _ = h.record(exec.as_nanos().min(u64::MAX as u128) as u64);
        }
        let _ = job.reply.send(result);
    }
}
