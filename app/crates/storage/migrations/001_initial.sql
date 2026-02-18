CREATE TABLE IF NOT EXISTS _migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    from_jid TEXT NOT NULL,
    to_jid TEXT NOT NULL,
    body TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    message_type TEXT NOT NULL,
    thread TEXT,
    read INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_messages_from ON messages(from_jid);
CREATE INDEX IF NOT EXISTS idx_messages_to ON messages(to_jid);
CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp);

CREATE TABLE IF NOT EXISTS roster (
    jid TEXT PRIMARY KEY,
    name TEXT,
    subscription TEXT NOT NULL,
    groups TEXT
);

CREATE TABLE IF NOT EXISTS muc_rooms (
    room_jid TEXT PRIMARY KEY,
    nick TEXT NOT NULL,
    joined INTEGER NOT NULL DEFAULT 0,
    subject TEXT
);

CREATE TABLE IF NOT EXISTS plugin_kv (
    plugin_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value BLOB NOT NULL,
    PRIMARY KEY (plugin_id, key)
);
