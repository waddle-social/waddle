CREATE TABLE IF NOT EXISTS mam_sync_state (
    jid TEXT PRIMARY KEY,
    last_stanza_id TEXT NOT NULL,
    last_sync_at TEXT NOT NULL
);
