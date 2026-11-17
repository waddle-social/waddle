-- Ending a retired account's open sessions on every replica (ADR-0020 D8).
-- See docs/operations/account-retirement.md and
-- server/docs/planning/host-grant-authority-2047.md.
--
-- `waddle-server account retire` records a request here after the
-- retirement committed. Every serving replica holds a lease row, reads the
-- requests of the last day it has not completed, ends the account's
-- streams and detached stream-management sessions it holds, and records
-- that in an acknowledgement. The command waits for an acknowledgement of
-- every replica whose lease counts, or for the lease to run out.
--
-- Times are milliseconds since the Unix epoch on the clock of the process
-- that wrote them, on both engines. The runtime role inserts and renews its
-- lease and inserts and completes its acknowledgements; it reads requests
-- and deletes nothing. The owner removes old rows when the command runs.

-- One row per registration of a serving replica: a process registers once
-- when it starts and renews the lease while it runs. `node` names the
-- replica in the command's report: its clustering node identity, or its
-- host and process.
CREATE TABLE account_session_ending_replicas (
    replica_id TEXT PRIMARY KEY,
    node TEXT NOT NULL,
    registered_at_ms BIGINT NOT NULL,
    lease_expires_at_ms BIGINT NOT NULL,
    CONSTRAINT account_session_ending_replicas_id CHECK (replica_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT account_session_ending_replicas_node CHECK (octet_length(node) BETWEEN 1 AND 255)
);

CREATE INDEX account_session_ending_replicas_lease ON account_session_ending_replicas (lease_expires_at_ms);

-- One row per run of the command: end the open sessions of the retired
-- account `account_jid`, its bare JID in canonical form (the key of
-- `retired_account_jids`).
CREATE TABLE account_session_ending_requests (
    request_id TEXT PRIMARY KEY,
    account_jid TEXT NOT NULL,
    requested_at_ms BIGINT NOT NULL,
    CONSTRAINT account_session_ending_requests_id CHECK (request_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT account_session_ending_requests_account_jid CHECK (octet_length(account_jid) BETWEEN 3 AND 2047)
);

CREATE INDEX account_session_ending_requests_requested ON account_session_ending_requests (requested_at_ms);

-- A replica's progress on a request: `observed_at_ms` when it took the
-- account into its set of retired accounts, `completed_at_ms` once it had
-- ended every stream and detached session of the account it held, with how
-- many of each it ended.
CREATE TABLE account_session_ending_acks (
    request_id TEXT NOT NULL,
    replica_id TEXT NOT NULL,
    observed_at_ms BIGINT NOT NULL,
    completed_at_ms BIGINT,
    streams_closed BIGINT NOT NULL DEFAULT 0,
    detached_sessions_ended BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (request_id, replica_id),
    CONSTRAINT account_session_ending_acks_request CHECK (request_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT account_session_ending_acks_replica CHECK (replica_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT account_session_ending_acks_counts CHECK (streams_closed >= 0 AND detached_sessions_ended >= 0)
);

CREATE INDEX account_session_ending_acks_replica ON account_session_ending_acks (replica_id);
