use rusqlite::{Connection, OpenFlags};

use crate::SqliteBacking;

/// SQL schema - copied from waddle's `MAM_SCHEMA` to stay byte-compatible.
pub(crate) const MAM_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mam_messages (
    id TEXT PRIMARY KEY,
    room_jid TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    from_jid TEXT NOT NULL,
    to_jid TEXT NOT NULL,
    body TEXT NOT NULL,
    stanza_id TEXT,
    thread_id TEXT,
    reply_to_id TEXT,
    reply_to_jid TEXT,
    origin_id TEXT,
    message_type TEXT NOT NULL DEFAULT 'chat',
    stanza_xml TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_mam_room_timestamp
    ON mam_messages(room_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_sender
    ON mam_messages(room_jid, from_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_id
    ON mam_messages(room_jid, id);
"#;

pub(crate) fn apply_pragmas(conn: &Connection, backing: &SqliteBacking) -> rusqlite::Result<()> {
    // Pragmas differ between backings: WAL is only meaningful on disk, and
    // memory DBs have no fsync cost so `synchronous` is moot. Keep the rest
    // the same (temp_store, cache, busy_timeout).
    if backing.on_disk() {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "wal_autocheckpoint", 1000)?;
    } else {
        // In-memory: skip journal_mode (SQLite will pick MEMORY automatically
        // for :memory: DBs and reject WAL anyway).
        conn.pragma_update(None, "synchronous", "OFF")?;
        // CRITICAL for shared-cache mode: without this, any reader holds a
        // table-level read lock that makes the writer see SQLITE_LOCKED on
        // INSERT. busy_timeout does NOT apply to SQLITE_LOCKED (only BUSY),
        // so writes fail outright. read_uncommitted=1 lets readers proceed
        // without blocking the writer. Cost: readers may see uncommitted
        // rows - fine for a benchmark; waddle production is disk+WAL anyway.
        conn.pragma_update(None, "read_uncommitted", 1i64)?;
    }
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "mmap_size", 268_435_456i64)?;
    conn.pragma_update(None, "cache_size", -64_000i64)?; // 64 MiB
    conn.pragma_update(None, "busy_timeout", 5_000i64)?;
    Ok(())
}

/// r2d2 connection manager that opens connections via URI + explicit flags.
/// r2d2_sqlite's built-in manager uses `Connection::open` which doesn't let
/// us pass `SQLITE_OPEN_URI`, so we roll a minimal one.
pub(crate) struct UriManager {
    pub(crate) uri: String,
    pub(crate) flags: OpenFlags,
    pub(crate) backing: SqliteBacking,
}

impl r2d2::ManageConnection for UriManager {
    type Connection = Connection;
    type Error = rusqlite::Error;

    fn connect(&self) -> Result<Connection, rusqlite::Error> {
        let c = Connection::open_with_flags(&self.uri, self.flags)?;
        apply_pragmas(&c, &self.backing)?;
        Ok(c)
    }

    fn is_valid(&self, c: &mut Connection) -> Result<(), rusqlite::Error> {
        c.execute_batch("SELECT 1")
    }

    fn has_broken(&self, _c: &mut Connection) -> bool {
        false
    }
}
