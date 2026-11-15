-- Host-grant restore incarnation (ADR-0020 D5 condition B7, gate G13).
--
-- A restore of this database brings back an older authority incarnation
-- with its older grants and `active` flags. The columns added here bind the
-- incarnation to values that a restore of this database cannot bring back:
--
-- - `restore_epoch`: the deployment's declared restore epoch the current
--   incarnation was established under. Every replica and the owner phase
--   carry the configured epoch (WADDLE_HOST_GRANT_RESTORE_EPOCH) and refuse
--   a database that records another one, so a database restored to before
--   the latest restore transition is not served.
-- - `lineage_uuid`: the database lineage (`_lineage`) the incarnation is
--   bound to. A logical restore into another database is adopted, which
--   rotates the lineage, and the owner phase then requires a declared
--   restore before the authority is served again.
-- - `authority_id`: a fixed identity of this authority, minted here and
--   never changed. The PostgreSQL position witness a replica keeps in
--   SpiceDB is filed under it, so a restored copy of this database finds the
--   witness of the database it was copied from.
-- - `position_acknowledgement`: an operator's acknowledgement
--   (`waddle-server host-grant acknowledge-position`) of the exact position
--   witness it supersedes, `<epoch>_<system>_<timeline>_<lsn>`, for the
--   PostgreSQL system and timeline the server wrote on when it was recorded,
--   `/<system>_<timeline>`: a promotion the position witness cannot verify
--   on its own, or a regression on the witnessed timeline the operator
--   accepts. It applies only while the witness it names is current, so once
--   applied it is spent.
--
-- `host_grant_scopes.lost_operations` marks a scope a restore lost
-- operations in, as a fence pass, a reconciliation or a reconstruction
-- found from SpiceDB. Such a scope is reopened only by name, with the
-- operator's confirmation that what was lost has been applied again; the
-- reopen clears the mark.
--
-- A declared restore closes every scope as `restored`; reconstruction
-- leaves it `reconstructed` until an operator reopens it (`reopen`); a
-- scope whose projection or witness records an operation the journal does
-- not is closed as `fenced` (`fence`) until a restore is declared. See
-- server/docs/planning/host-grant-authority-2047.md, "Restore incarnation
-- decisions".

ALTER TABLE host_grant_authority
    ADD COLUMN restore_epoch BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN lineage_uuid TEXT,
    ADD COLUMN authority_id TEXT NOT NULL DEFAULT replace(gen_random_uuid()::text, '-', ''),
    ADD COLUMN position_acknowledgement TEXT;

ALTER TABLE host_grant_authority
    ADD CONSTRAINT host_grant_authority_restore_epoch
        CHECK (restore_epoch >= 0 AND restore_epoch <= 4294967295),
    ADD CONSTRAINT host_grant_authority_id CHECK (authority_id ~ '^[0-9a-f]{32}$'),
    ADD CONSTRAINT host_grant_authority_position_acknowledgement
        CHECK (position_acknowledgement ~
            '^(0|[1-9][0-9]{0,9})_(0|[1-9][0-9]{0,19})_(0|[1-9][0-9]{0,9})_(0|[1-9][0-9]{0,19})/(0|[1-9][0-9]{0,19})_(0|[1-9][0-9]{0,9})$');

ALTER TABLE host_grant_scopes ADD COLUMN lost_operations BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE host_grant_scopes DROP CONSTRAINT host_grant_scopes_reason;
ALTER TABLE host_grant_scopes ADD CONSTRAINT host_grant_scopes_reason CHECK (
    (admission_state = 'active' AND closed_reason IS NULL AND projected_generation > 0)
    OR (admission_state = 'closed'
        AND closed_reason IN (
            'enrolled', 'retired', 'exhausted', 'restored', 'reconstructed', 'fenced'
        ))
);

ALTER TABLE host_grant_operations DROP CONSTRAINT host_grant_operations_kind;
ALTER TABLE host_grant_operations ADD CONSTRAINT host_grant_operations_kind CHECK (
    kind IN (
        'enroll', 'set_affiliation', 'retire', 'exhaust', 'restore', 'reconstruct', 'fence',
        'reopen'
    )
);

-- Startup compares each projection marker's and journal witness's
-- incarnation with the incarnations a scope's journal records.
CREATE INDEX idx_host_grant_operations_incarnation
    ON host_grant_operations (lifecycle_id, authority_incarnation);
