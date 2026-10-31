-- Outbox table before #1732 — retained as an upgrade fixture.
CREATE TABLE IF NOT EXISTS call_teardown_outbox (
    intent_id TEXT PRIMARY KEY,
    call_id TEXT NOT NULL,
    identity TEXT NULL CHECK (identity IS NULL OR identity <> ''),
    room_jid TEXT NULL CHECK (room_jid IS NULL OR room_jid <> ''),
    action TEXT NOT NULL CHECK (action IN ('remove_participant','delete_room','muji_presence_clear','muji_room_sweep','call_thread_end_retry')),
    generation INTEGER NULL CHECK (generation IS NULL OR generation > 0),
    occupant TEXT NULL CHECK (occupant IS NULL OR occupant <> ''),
    room_sid TEXT NULL,
    participant_sid TEXT NULL,
    session_binding TEXT NULL CHECK (session_binding IS NULL OR session_binding <> ''),
    thread_id TEXT NULL CHECK (thread_id IS NULL OR thread_id <> ''),
    anchor_origin_id TEXT NULL CHECK (anchor_origin_id IS NULL OR anchor_origin_id <> ''),
    thread_started_at_ms INTEGER NULL,
    thread_ended_at_ms INTEGER NULL,
    producing_node TEXT NULL CHECK (producing_node IS NULL OR producing_node <> ''),
    status TEXT NOT NULL CHECK (status IN ('queued','in-progress','done','failed')),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT NULL,
    next_attempt_at_ms INTEGER NULL,
    claimed_at_ms INTEGER NULL,
    claim_token TEXT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK (
    (action = 'remove_participant' AND identity IS NOT NULL AND room_jid IS NULL)
    OR (action = 'delete_room' AND identity IS NULL AND room_jid IS NULL AND participant_sid IS NULL)
    OR (action = 'muji_presence_clear' AND identity IS NOT NULL AND room_jid IS NOT NULL)
    OR (action = 'muji_room_sweep' AND identity IS NULL AND room_jid IS NOT NULL AND room_sid IS NOT NULL AND participant_sid IS NULL)
    OR (action = 'call_thread_end_retry' AND identity IS NULL AND room_jid IS NOT NULL AND participant_sid IS NULL AND thread_id IS NOT NULL AND anchor_origin_id IS NOT NULL AND thread_started_at_ms IS NOT NULL AND thread_ended_at_ms IS NOT NULL)
    ),
    CHECK ((thread_id IS NULL AND anchor_origin_id IS NULL AND thread_started_at_ms IS NULL AND thread_ended_at_ms IS NULL) OR action = 'call_thread_end_retry')
    );
