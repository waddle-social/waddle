-- Remote rooms local resources occupy, shared by every replica (ADR-0020
-- D7's room exception; XEP-0045). One row per local full JID and remote
-- room bare JID: where the occupancy stands (`phase`), the nickname, the
-- affiliation the room last stated, the join it is waiting on (`attempt`
-- and the join presence's `join_id`), leaves whose answer is still to be
-- absorbed, the replica that last routed the resource's stanzas for the
-- room (`owner_id`, the router's correlation owner), and when an
-- unanswered verifying, joining or leaving occupancy ends. Any replica
-- admits the room's traffic for the occupant from this table and ends the
-- occupancy when the resource ends or a stream to the room's domain fails.
-- Every write after the insert is a compare-and-set on `revision`.
--
-- `account_jid` serves the per-account bound, `local_domain` and
-- `room_domain` the cleanup after a stream between the two fails. The
-- addresses are bounded in bytes so that the primary key's index entry
-- stays below PostgreSQL's B-tree limit, 2704 bytes with the default 8 kB
-- page.
--
-- `s2s_remote_room_verdicts` caches, until `expires_at_ms`, per remote
-- domain (its JID in `room_jid`) whether its XEP-0030 disco#info named it
-- a multi-user chat service, and per bare JID at such a service whether
-- its disco#info named it a room. A join to a room not known to be one is
-- held until the domain, and then the room, answers.
-- See server/docs/planning/s2s-routing-2047.md.
CREATE TABLE s2s_remote_room_occupancies (
    occupant_jid TEXT NOT NULL,
    room_jid TEXT NOT NULL,
    account_jid TEXT NOT NULL,
    local_domain TEXT NOT NULL,
    room_domain TEXT NOT NULL,
    nick TEXT NOT NULL,
    phase TEXT NOT NULL,
    affiliation TEXT NOT NULL,
    attempt TEXT NOT NULL,
    join_id TEXT,
    leaves_pending BIGINT NOT NULL,
    leave_until_ms BIGINT,
    owner_id TEXT NOT NULL,
    deadline_ms BIGINT,
    updated_at_ms BIGINT NOT NULL,
    revision BIGINT NOT NULL,
    PRIMARY KEY (occupant_jid, room_jid),
    CONSTRAINT s2s_remote_room_occupancies_phase CHECK (phase IN ('verifying', 'joining', 'joined', 'leaving')),
    CONSTRAINT s2s_remote_room_occupancies_affiliation CHECK (affiliation IN ('owner', 'admin', 'member', 'outcast', 'none')),
    CONSTRAINT s2s_remote_room_occupancies_occupant_jid CHECK (octet_length(occupant_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_remote_room_occupancies_room_jid CHECK (octet_length(room_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_remote_room_occupancies_nick CHECK (octet_length(nick) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_remote_room_occupancies_join_id CHECK (join_id IS NULL OR octet_length(join_id) <= 256),
    CONSTRAINT s2s_remote_room_occupancies_leaves_pending CHECK (leaves_pending >= 0)
);

CREATE INDEX s2s_remote_room_occupancies_account ON s2s_remote_room_occupancies (account_jid);
CREATE INDEX s2s_remote_room_occupancies_pair ON s2s_remote_room_occupancies (local_domain, room_domain);
CREATE INDEX s2s_remote_room_occupancies_deadline ON s2s_remote_room_occupancies (deadline_ms);
CREATE INDEX s2s_remote_room_occupancies_updated ON s2s_remote_room_occupancies (updated_at_ms);

CREATE TABLE s2s_remote_room_verdicts (
    room_jid TEXT PRIMARY KEY,
    verdict TEXT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    CONSTRAINT s2s_remote_room_verdicts_verdict CHECK (verdict IN ('room', 'not_room')),
    CONSTRAINT s2s_remote_room_verdicts_room_jid CHECK (octet_length(room_jid) BETWEEN 1 AND 1023)
);

CREATE INDEX s2s_remote_room_verdicts_expiry ON s2s_remote_room_verdicts (expires_at_ms);
