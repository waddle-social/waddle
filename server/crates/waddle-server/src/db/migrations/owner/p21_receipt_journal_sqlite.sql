CREATE TABLE p21_receipt_operation_results (
    operation_id TEXT PRIMARY KEY NOT NULL,
    message_key TEXT NOT NULL,
    receipt_kind INTEGER NOT NULL,
    semantic_identity_hash BLOB NOT NULL CHECK (length(semantic_identity_hash) = 32),
    inserted INTEGER NOT NULL CHECK (inserted IN (0, 1))
);
