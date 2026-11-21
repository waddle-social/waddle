-- Remote occupants of local rooms, shared by every replica (ADR-0020 D10;
-- XEP-0045). One row per local room bare JID and remote occupant full JID:
-- a seat, established by a join an inbound S2S stream carried.
--
-- A seat records what the stream that carried its join proved (`strength`,
-- and `trust_profile` when the remote domain presented a certificate) and
-- the S2S configuration snapshot that stream was admitted under. Decisions
-- that name the occupant without a request of its own (the audience of a
-- room acceptance, the revocation sweep, the seats restored after a room's
-- claim moved) decide it on that evidence. Peer admission and trusted-home
-- status are not stored: they are evaluated at each decision.
--
-- `session_id` is the occupancy generation the room actor seats the
-- occupant under. `owner_id` is the replica that holds the room and seated
-- the occupant (its router's correlation owner): only that replica sends
-- the room's traffic to the seat and ends it. Another replica that loses a
-- stream of the seat's domain pair sets `lost_at_ms`, and the owner ends
-- the seat at its next sweep; a seat whose owner stopped recording that it
-- is alive is removed by any replica.
--
-- `account_jid` and `occupant_domain` serve the per-account and per-peer
-- bounds, `room_domain` with `occupant_domain` the cleanup after a stream
-- between the two fails. The addresses are bounded in bytes so that the
-- primary key's index entry stays below PostgreSQL's B-tree limit, 2704
-- bytes with the default 8 kB page.
-- See server/docs/planning/s2s-routing-2047.md.
CREATE TABLE s2s_local_room_seats (
    room_jid TEXT NOT NULL,
    occupant_jid TEXT NOT NULL,
    account_jid TEXT NOT NULL,
    room_domain TEXT NOT NULL,
    occupant_domain TEXT NOT NULL,
    nick TEXT NOT NULL,
    session_id TEXT NOT NULL,
    strength TEXT NOT NULL,
    trust_profile TEXT,
    snapshot_generation TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    lost_at_ms BIGINT,
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (room_jid, occupant_jid),
    CONSTRAINT s2s_local_room_seats_strength CHECK (strength IN ('pkix', 'dialback')),
    CONSTRAINT s2s_local_room_seats_trust_profile CHECK (trust_profile IS NULL OR trust_profile IN ('public_webpki', 'private_ca')),
    CONSTRAINT s2s_local_room_seats_room_jid CHECK (octet_length(room_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_local_room_seats_occupant_jid CHECK (octet_length(occupant_jid) BETWEEN 1 AND 1023),
    CONSTRAINT s2s_local_room_seats_nick CHECK (octet_length(nick) BETWEEN 1 AND 1023)
);

CREATE INDEX s2s_local_room_seats_account ON s2s_local_room_seats (account_jid);
CREATE INDEX s2s_local_room_seats_peer ON s2s_local_room_seats (occupant_domain, room_domain);
CREATE INDEX s2s_local_room_seats_owner ON s2s_local_room_seats (owner_id);
CREATE INDEX s2s_local_room_seats_updated ON s2s_local_room_seats (updated_at_ms);
