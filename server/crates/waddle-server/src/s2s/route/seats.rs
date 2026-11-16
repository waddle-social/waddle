//! The seats remote occupants hold in local rooms (ADR-0020 D10; XEP-0045).
//!
//! A seat is one remote full JID's occupancy of one local room, established
//! by a join an inbound S2S stream carried. It records what that stream
//! proved ([`StreamEvidence`]), so that a decision naming the occupant
//! without a request of its own decides it on that evidence
//! ([`SeatEvidence`]), and the occupancy generation the room actor seats it
//! under.
//!
//! Each seat is one row of `s2s_local_room_seats` in the deployment's
//! global database ([`SqlSeatStore`]), which every replica shares, and one
//! entry of the [`SeatIndex`] of the replica that holds the room and seated
//! the occupant. That replica alone sends the room's traffic to the seat,
//! and it sends only to seats its index holds: the index is what bounds a
//! room's fan-out to remote JIDs to those it admitted. The row serves the
//! bounds across replicas, the end of a seat whose stream another replica
//! lost, and the removal of seats a stopped replica left.
//!
//! The decisions are recorded in `server/docs/planning/s2s-routing-2047.md`
//! (inbound MUC).

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;

use crate::db::{Database, DatabaseDriver, Row};
use crate::permissions::host_grant::{SeatEvidence, StreamEvidence};
use crate::s2s::conn::AuthenticationStrength;
use crate::s2s::verify::TrustProfileKind;

/// The longest room or occupant address, or nickname, a seat stores, in
/// bytes. Two such addresses keep the primary key below PostgreSQL's
/// B-tree entry limit (2704 bytes with the default 8 kB page).
pub const MAX_SEAT_ADDRESS_BYTES: usize = 1023;

/// One remote full JID's occupancy of one local room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    pub room: BareJid,
    pub occupant: FullJid,
    /// The nickname the room seats the occupant under.
    pub nick: String,
    /// The occupancy generation the room actor seats the occupant under.
    pub session: OccupancySessionGeneration,
    /// What the stream that carried the join proved.
    pub evidence: StreamEvidence,
    /// The S2S configuration snapshot that stream was admitted under.
    pub snapshot: Uuid,
    /// The replica that holds the room and seated the occupant.
    pub owner: String,
    /// When another replica lost a stream of the seat's domain pair, in
    /// milliseconds since the Unix epoch.
    pub lost_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

/// Bounds on the seats a join may add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeatLimits {
    /// Remote occupants of one room.
    pub per_room: usize,
    /// Seats of one remote domain, in every room.
    pub per_peer: usize,
    /// Seats of one remote account, in every room and under every
    /// resource.
    pub per_account: usize,
    /// Seats of the deployment.
    pub total: usize,
}

/// What inserting a seat did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatInsert {
    Inserted,
    /// A seat for the same room and occupant exists.
    Exists,
    /// The seat would exceed a bound.
    Bounds,
}

/// Why the seat table could not be used.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SeatStoreError {
    /// An address or nickname is longer than the table stores.
    #[error("the seat's addresses are too long to store")]
    TooLong,
    /// The store failed; the outcome of a write is unknown.
    #[error("the seat store failed: {0}")]
    Storage(String),
}

fn storage(error: impl std::fmt::Display) -> SeatStoreError {
    SeatStoreError::Storage(error.to_string())
}

/// The seats of remote occupants, shared by every replica.
#[async_trait]
pub trait SeatStore: Send + Sync {
    /// Insert `seat` unless one for its room and occupant exists or it
    /// would exceed `limits`.
    async fn insert(&self, seat: &Seat, limits: &SeatLimits) -> Result<SeatInsert, SeatStoreError>;

    /// The seat `occupant` holds in `room`, if any.
    async fn get(&self, room: &BareJid, occupant: &FullJid)
        -> Result<Option<Seat>, SeatStoreError>;

    /// Remove `seat`'s row if it still names `seat.session` and
    /// `seat.owner`. `true` when this call removed it.
    async fn remove(&self, seat: &Seat) -> Result<bool, SeatStoreError>;

    /// Record `nick` for `seat` if its row still names `seat.session` and
    /// `seat.owner`.
    async fn rename(&self, seat: &Seat, nick: &str, now_ms: i64) -> Result<bool, SeatStoreError>;

    /// Make `owner` the owner of the seat `occupant` holds in `room`, if
    /// its owner has not recorded that it is alive since `before_ms`, and
    /// return it as adopted. `None` when no such seat exists or its owner
    /// is alive.
    async fn adopt(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        owner: &str,
        before_ms: i64,
        now_ms: i64,
    ) -> Result<Option<Seat>, SeatStoreError>;

    /// Mark every seat of an occupant at `occupant_domain` in a room at
    /// `room_domain` that `owner` does not own as lost at `now_ms`. The
    /// owner of each ends it at its next sweep.
    async fn mark_lost(
        &self,
        room_domain: &str,
        occupant_domain: &str,
        owner: &str,
        now_ms: i64,
    ) -> Result<u64, SeatStoreError>;

    /// Remove every seat `owner` owns: the replica is stopping and seats
    /// nobody any more.
    async fn remove_owned(&self, owner: &str) -> Result<u64, SeatStoreError>;

    /// At most `limit` seats `owner` owns that another replica marked
    /// lost.
    async fn lost(&self, owner: &str, limit: usize) -> Result<Vec<Seat>, SeatStoreError>;

    /// At most `limit` seats not written since `before_ms` whose owner has
    /// not recorded that it is alive since then: those a stopped replica
    /// left.
    async fn orphaned(&self, before_ms: i64, limit: usize) -> Result<Vec<Seat>, SeatStoreError>;

    /// Seats of the deployment.
    async fn count(&self) -> Result<usize, SeatStoreError>;
}

fn strength_code(strength: AuthenticationStrength) -> &'static str {
    match strength {
        AuthenticationStrength::Pkix => "pkix",
        AuthenticationStrength::Dialback => "dialback",
    }
}

fn trust_profile_code(profile: TrustProfileKind) -> &'static str {
    match profile {
        TrustProfileKind::PublicWebPki => "public_webpki",
        TrustProfileKind::PrivateCa => "private_ca",
    }
}

/// The seat table in the deployment's global database
/// (`s2s_local_room_seats`).
#[derive(Clone)]
pub struct SqlSeatStore {
    db: Database,
}

const COLUMNS: &str = "room_jid, occupant_jid, nick, session_id, strength, trust_profile, \
     snapshot_generation, owner_id, lost_at_ms, updated_at_ms";

const KEY_CLAUSE: &str = "room_jid = ? AND occupant_jid = ?";

/// The seed of the PostgreSQL advisory lock key a join takes on its remote
/// domain (`hashtextextended(domain, seed)`). Every advisory lock of the
/// database shares one 64-bit key space; a collision with another lock
/// only makes the two holders wait for each other.
const PEER_LOCK_SEED: i64 = 0x7332_735f_7365_6174;

impl SqlSeatStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    async fn execute(
        &self,
        sql: &str,
        params: Vec<crate::db::Value>,
    ) -> Result<u64, SeatStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        guard.execute(sql, params).await.map_err(storage)
    }

    async fn select(
        &self,
        sql: &str,
        params: Vec<crate::db::Value>,
    ) -> Result<Vec<Seat>, SeatStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard.query(sql, params).await.map_err(storage)?;
        let mut seats = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            seats.push(decode(&row)?);
        }
        Ok(seats)
    }
}

fn fits(seat: &Seat) -> bool {
    seat.room.as_str().len() <= MAX_SEAT_ADDRESS_BYTES
        && seat.occupant.as_str().len() <= MAX_SEAT_ADDRESS_BYTES
        && !seat.nick.is_empty()
        && seat.nick.len() <= MAX_SEAT_ADDRESS_BYTES
}

fn key_params(room: &BareJid, occupant: &FullJid) -> Vec<crate::db::Value> {
    crate::db_params![room.as_str(), occupant.as_str()]
}

/// Decode one row selected with [`COLUMNS`]. Every stored value was written
/// from a typed one; a row that no longer decodes fails the whole read
/// rather than being taken for a missing seat, and never yields evidence
/// the row does not hold.
fn decode(row: &Row) -> Result<Seat, SeatStoreError> {
    let room: String = row.get(0).map_err(storage)?;
    let occupant: String = row.get(1).map_err(storage)?;
    let session: String = row.get(3).map_err(storage)?;
    let strength: String = row.get(4).map_err(storage)?;
    let trust_profile: Option<String> = row.get(5).map_err(storage)?;
    let snapshot: String = row.get(6).map_err(storage)?;
    let strength = match strength.as_str() {
        "pkix" => AuthenticationStrength::Pkix,
        "dialback" => AuthenticationStrength::Dialback,
        _ => return Err(storage("an unknown authentication strength")),
    };
    let trust_profile = match trust_profile.as_deref() {
        None => None,
        Some("public_webpki") => Some(TrustProfileKind::PublicWebPki),
        Some("private_ca") => Some(TrustProfileKind::PrivateCa),
        Some(_) => return Err(storage("an unknown trust profile")),
    };
    Ok(Seat {
        room: room.parse().map_err(storage)?,
        occupant: occupant.parse().map_err(storage)?,
        nick: row.get(2).map_err(storage)?,
        session: session.parse().map_err(storage)?,
        evidence: StreamEvidence::recorded(strength, trust_profile),
        snapshot: snapshot.parse().map_err(storage)?,
        owner: row.get(7).map_err(storage)?,
        lost_at_ms: row.get(8).map_err(storage)?,
        updated_at_ms: row.get(9).map_err(storage)?,
    })
}

/// The seats matching `clause`, counted through no more than `bound` rows:
/// a count at the bound means "at least the bound".
async fn count_where(
    transaction: &mut crate::db::Transaction<'_>,
    clause: &str,
    mut params: Vec<crate::db::Value>,
    bound: usize,
) -> Result<usize, SeatStoreError> {
    let sql = [
        "SELECT COUNT(*) FROM (SELECT 1 FROM s2s_local_room_seats",
        clause,
        " LIMIT ?) AS bounded",
    ]
    .concat();
    params.push(bound.into());
    let mut rows = transaction.query(&sql, params).await.map_err(storage)?;
    let count: i64 = match rows.next().await.map_err(storage)? {
        Some(row) => row.get(0).map_err(storage)?,
        None => 0,
    };
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

#[async_trait]
impl SeatStore for SqlSeatStore {
    async fn insert(&self, seat: &Seat, limits: &SeatLimits) -> Result<SeatInsert, SeatStoreError> {
        if !fits(seat) {
            return Err(SeatStoreError::TooLong);
        }
        let account = seat.occupant.to_bare();
        let peer = seat.occupant.domain().as_str();
        // The counts and the insert are one transaction. On SQLite it holds
        // the write lock throughout. On PostgreSQL it first takes a
        // transaction advisory lock on the remote domain, so the joins of
        // one peer, and so of each of its accounts, are counted and
        // inserted one at a time and the per-peer and per-account bounds
        // hold exactly; at READ COMMITTED each count then sees every join
        // committed before the lock was granted. Joins of different peers
        // run concurrently, so a room's bound and the deployment's total
        // can be exceeded briefly by the joins of other peers in flight at
        // once. Each count reads at most its bound's rows through an index.
        let mut transaction = self.db.begin_immediate().await.map_err(storage)?;
        if self.db.driver() == DatabaseDriver::Postgres {
            transaction
                .execute(
                    "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
                    crate::db_params![],
                )
                .await
                .map_err(storage)?;
            transaction
                .query(
                    "SELECT pg_catalog.pg_advisory_xact_lock(\
                     pg_catalog.hashtextextended(?, ?))",
                    crate::db_params![peer, PEER_LOCK_SEED],
                )
                .await
                .map_err(storage)?;
        }
        let in_room = count_where(
            &mut transaction,
            " WHERE room_jid = ?",
            crate::db_params![seat.room.as_str()],
            limits.per_room,
        )
        .await?;
        let of_peer = count_where(
            &mut transaction,
            " WHERE occupant_domain = ?",
            crate::db_params![peer],
            limits.per_peer,
        )
        .await?;
        let of_account = count_where(
            &mut transaction,
            " WHERE account_jid = ?",
            crate::db_params![account.as_str()],
            limits.per_account,
        )
        .await?;
        let total = count_where(&mut transaction, "", Vec::new(), limits.total).await?;
        if in_room >= limits.per_room
            || of_peer >= limits.per_peer
            || of_account >= limits.per_account
            || total >= limits.total
        {
            transaction.rollback().await.map_err(storage)?;
            return Ok(SeatInsert::Bounds);
        }
        let inserted = transaction
            .execute(
                "INSERT INTO s2s_local_room_seats \
                 (room_jid, occupant_jid, account_jid, room_domain, occupant_domain, nick, \
                 session_id, strength, trust_profile, snapshot_generation, owner_id, \
                 lost_at_ms, updated_at_ms) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?) ON CONFLICT DO NOTHING",
                crate::db_params![
                    seat.room.as_str(),
                    seat.occupant.as_str(),
                    account.as_str(),
                    seat.room.domain().as_str(),
                    peer,
                    seat.nick.as_str(),
                    seat.session.to_string(),
                    strength_code(seat.evidence.strength()),
                    seat.evidence.trust_profile().map(trust_profile_code),
                    seat.snapshot.to_string(),
                    seat.owner.as_str(),
                    seat.updated_at_ms,
                ],
            )
            .await
            .map_err(storage)?;
        transaction.commit().await.map_err(storage)?;
        Ok(if inserted == 1 {
            SeatInsert::Inserted
        } else {
            SeatInsert::Exists
        })
    }

    async fn get(
        &self,
        room: &BareJid,
        occupant: &FullJid,
    ) -> Result<Option<Seat>, SeatStoreError> {
        let sql = [
            "SELECT ",
            COLUMNS,
            " FROM s2s_local_room_seats WHERE ",
            KEY_CLAUSE,
        ]
        .concat();
        Ok(self
            .select(&sql, key_params(room, occupant))
            .await?
            .into_iter()
            .next())
    }

    async fn remove(&self, seat: &Seat) -> Result<bool, SeatStoreError> {
        let mut params = key_params(&seat.room, &seat.occupant);
        params.push(seat.session.to_string().into());
        params.push(seat.owner.as_str().into());
        let removed = self
            .execute(
                &[
                    "DELETE FROM s2s_local_room_seats WHERE ",
                    KEY_CLAUSE,
                    " AND session_id = ? AND owner_id = ?",
                ]
                .concat(),
                params,
            )
            .await?;
        Ok(removed == 1)
    }

    async fn rename(&self, seat: &Seat, nick: &str, now_ms: i64) -> Result<bool, SeatStoreError> {
        if nick.is_empty() || nick.len() > MAX_SEAT_ADDRESS_BYTES {
            return Err(SeatStoreError::TooLong);
        }
        let mut params = crate::db_params![nick, now_ms];
        params.extend(key_params(&seat.room, &seat.occupant));
        params.push(seat.session.to_string().into());
        params.push(seat.owner.as_str().into());
        let renamed = self
            .execute(
                &[
                    "UPDATE s2s_local_room_seats SET nick = ?, updated_at_ms = ? WHERE ",
                    KEY_CLAUSE,
                    " AND session_id = ? AND owner_id = ?",
                ]
                .concat(),
                params,
            )
            .await?;
        Ok(renamed == 1)
    }

    async fn adopt(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        owner: &str,
        before_ms: i64,
        now_ms: i64,
    ) -> Result<Option<Seat>, SeatStoreError> {
        let mut params = crate::db_params![owner, now_ms];
        params.extend(key_params(room, occupant));
        params.push(owner.into());
        params.push(before_ms.into());
        let adopted = self
            .execute(
                &[
                    "UPDATE s2s_local_room_seats SET owner_id = ?, lost_at_ms = NULL, \
                     updated_at_ms = ? WHERE ",
                    KEY_CLAUSE,
                    " AND owner_id <> ? AND owner_id NOT IN \
                     (SELECT owner_id FROM s2s_iq_correlation_owners WHERE seen_at_ms > ?)",
                ]
                .concat(),
                params,
            )
            .await?;
        if adopted != 1 {
            return Ok(None);
        }
        Ok(self
            .get(room, occupant)
            .await?
            .filter(|seat| seat.owner == owner))
    }

    async fn mark_lost(
        &self,
        room_domain: &str,
        occupant_domain: &str,
        owner: &str,
        now_ms: i64,
    ) -> Result<u64, SeatStoreError> {
        self.execute(
            "UPDATE s2s_local_room_seats SET lost_at_ms = ? \
             WHERE occupant_domain = ? AND room_domain = ? AND owner_id <> ? \
             AND lost_at_ms IS NULL",
            crate::db_params![now_ms, occupant_domain, room_domain, owner],
        )
        .await
    }

    async fn remove_owned(&self, owner: &str) -> Result<u64, SeatStoreError> {
        self.execute(
            "DELETE FROM s2s_local_room_seats WHERE owner_id = ?",
            crate::db_params![owner],
        )
        .await
    }

    async fn lost(&self, owner: &str, limit: usize) -> Result<Vec<Seat>, SeatStoreError> {
        let sql = [
            "SELECT ",
            COLUMNS,
            " FROM s2s_local_room_seats WHERE owner_id = ? AND lost_at_ms IS NOT NULL \
             ORDER BY lost_at_ms LIMIT ?",
        ]
        .concat();
        self.select(&sql, crate::db_params![owner, limit]).await
    }

    async fn orphaned(&self, before_ms: i64, limit: usize) -> Result<Vec<Seat>, SeatStoreError> {
        let sql = [
            "SELECT ",
            COLUMNS,
            " FROM s2s_local_room_seats WHERE updated_at_ms <= ? AND owner_id NOT IN \
             (SELECT owner_id FROM s2s_iq_correlation_owners WHERE seen_at_ms > ?) \
             ORDER BY updated_at_ms LIMIT ?",
        ]
        .concat();
        self.select(&sql, crate::db_params![before_ms, before_ms, limit])
            .await
    }

    async fn count(&self) -> Result<usize, SeatStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT COUNT(*) FROM s2s_local_room_seats",
                crate::db_params![],
            )
            .await
            .map_err(storage)?;
        let count: i64 = match rows.next().await.map_err(storage)? {
            Some(row) => row.get(0).map_err(storage)?,
            None => 0,
        };
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }
}

/// How many archive identifiers of the copies handed to one seat are
/// remembered, so that a copy offered again is not sent twice.
const RECENT_COPIES: usize = 64;

/// How many of a room's stanzas are kept for a seat whose join is still
/// being answered. One beyond this ends the seat.
const PENDING_STANZAS: usize = 256;

/// A seat this replica holds, with what its egress remembers for it.
#[derive(Debug, Clone)]
struct Held {
    seat: Seat,
    /// The archive identifiers of the room messages most recently handed
    /// to the seat's queue, oldest first.
    recent: VecDeque<String>,
    /// While the join that opened the seat is being answered: the room's
    /// stanzas for the occupant that arrived meanwhile, kept until the
    /// join's own answer is queued so that they follow it (XEP-0045 §7.1).
    pending: Option<Vec<Stanza>>,
}

/// What became of a room's stanza offered for a seat whose join may still
/// be under way.
#[derive(Debug)]
pub enum Deferred {
    /// The seat's join is answered: the stanza goes on now.
    Ready(Stanza),
    /// Kept until the join's answer is queued.
    Kept,
    /// Too many stanzas are kept already: the seat cannot be kept in step.
    Overflow,
    /// No such seat is held here.
    NotHeld(Stanza),
}

/// The seats of the remote occupants this replica seats, by room.
///
/// A room's stanza leaves for a remote JID only while that JID holds a seat
/// here, so the index bounds what a local room sends over S2S to the
/// occupants it admitted, one stanza per seat. It also answers what stream
/// evidence each seat was admitted under ([`SeatEvidence`]).
#[derive(Debug, Default)]
pub struct SeatIndex {
    rooms: Mutex<HashMap<BareJid, HashMap<FullJid, Held>>>,
}

impl SeatIndex {
    fn rooms(&self) -> MutexGuard<'_, HashMap<BareJid, HashMap<FullJid, Held>>> {
        // Every update leaves the maps consistent, so a poisoned lock still
        // holds usable state.
        match self.rooms.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Hold `seat`, replacing any seat of the same room and occupant.
    pub fn hold(&self, seat: Seat) {
        self.insert(seat, None);
    }

    /// Hold `seat` while the join that opened it is answered: the room's
    /// stanzas for it are kept until [`Self::settle`].
    pub fn hold_pending(&self, seat: Seat) {
        self.insert(seat, Some(Vec::new()));
    }

    fn insert(&self, seat: Seat, pending: Option<Vec<Stanza>>) {
        self.rooms().entry(seat.room.clone()).or_default().insert(
            seat.occupant.clone(),
            Held {
                seat,
                recent: VecDeque::new(),
                pending,
            },
        );
    }

    /// Keep `stanza` for the seat `occupant` holds in `room` if its join is
    /// still being answered, or hand it back to go on now.
    pub fn defer(&self, room: &BareJid, occupant: &FullJid, stanza: Stanza) -> Deferred {
        let mut rooms = self.rooms();
        let Some(held) = rooms
            .get_mut(room)
            .and_then(|seats| seats.get_mut(occupant))
        else {
            return Deferred::NotHeld(stanza);
        };
        match &mut held.pending {
            None => Deferred::Ready(stanza),
            Some(kept) if kept.len() >= PENDING_STANZAS => Deferred::Overflow,
            Some(kept) => {
                kept.push(stanza);
                Deferred::Kept
            }
        }
    }

    /// The join that opened the seat is answered: take the room's stanzas
    /// kept for it, in order. While any were kept the seat keeps deferring,
    /// so the caller queues them and asks again; once none is left the seat
    /// stops deferring and an empty batch is returned. `None` when the seat
    /// seated under `session` is no longer held.
    pub fn settle(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        session: OccupancySessionGeneration,
    ) -> Option<Vec<Stanza>> {
        let mut rooms = self.rooms();
        let held = rooms.get_mut(room)?.get_mut(occupant)?;
        if held.seat.session != session {
            return None;
        }
        match &mut held.pending {
            Some(kept) if !kept.is_empty() => Some(std::mem::take(kept)),
            _ => {
                held.pending = None;
                Some(Vec::new())
            }
        }
    }

    /// The seat `occupant` holds in `room` here.
    pub fn get(&self, room: &BareJid, occupant: &FullJid) -> Option<Seat> {
        self.rooms()
            .get(room)
            .and_then(|seats| seats.get(occupant))
            .map(|held| held.seat.clone())
    }

    /// Whether `occupant` holds a seat in `room` here.
    pub fn holds(&self, room: &BareJid, occupant: &FullJid) -> bool {
        self.rooms()
            .get(room)
            .is_some_and(|seats| seats.contains_key(occupant))
    }

    /// Stop holding the seat `occupant` holds in `room`, if it is still
    /// the one seated under `session`, and return it.
    pub fn release(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        session: OccupancySessionGeneration,
    ) -> Option<Seat> {
        let mut rooms = self.rooms();
        let seats = rooms.get_mut(room)?;
        if seats.get(occupant)?.seat.session != session {
            return None;
        }
        let released = seats.remove(occupant).map(|held| held.seat);
        if seats.is_empty() {
            rooms.remove(room);
        }
        released
    }

    /// Record the nickname the room now seats `occupant` under.
    pub fn rename(&self, room: &BareJid, occupant: &FullJid, nick: &str) -> Option<Seat> {
        let mut rooms = self.rooms();
        let held = rooms.get_mut(room)?.get_mut(occupant)?;
        held.seat.nick = nick.to_owned();
        Some(held.seat.clone())
    }

    /// Remember that the room message archived as `archive_id` was handed
    /// to the seat's queue: `false` when it already was, so the copy
    /// offered again is not sent twice, or when no such seat is held.
    pub fn note_copy(&self, room: &BareJid, occupant: &FullJid, archive_id: &str) -> bool {
        let mut rooms = self.rooms();
        let Some(held) = rooms
            .get_mut(room)
            .and_then(|seats| seats.get_mut(occupant))
        else {
            return false;
        };
        if held.recent.iter().any(|seen| seen == archive_id) {
            return false;
        }
        if held.recent.len() >= RECENT_COPIES {
            held.recent.pop_front();
        }
        held.recent.push_back(archive_id.to_owned());
        true
    }

    /// Every seat of an occupant at `occupant_domain` in a room at
    /// `room_domain` held here.
    pub fn of_pair(&self, room_domain: &str, occupant_domain: &str) -> Vec<Seat> {
        self.rooms()
            .iter()
            .filter(|(room, _)| room.domain().as_str() == room_domain)
            .flat_map(|(_, seats)| seats.values())
            .filter(|held| held.seat.occupant.domain().as_str() == occupant_domain)
            .map(|held| held.seat.clone())
            .collect()
    }

    /// Every seat held here whose join is answered.
    pub fn settled(&self) -> Vec<Seat> {
        self.rooms()
            .values()
            .flat_map(|seats| seats.values())
            .filter(|held| held.pending.is_none())
            .map(|held| held.seat.clone())
            .collect()
    }

    /// How many seats are held here.
    pub fn len(&self) -> usize {
        self.rooms().values().map(HashMap::len).sum()
    }

    /// Whether no seat is held here.
    pub fn is_empty(&self) -> bool {
        self.rooms().is_empty()
    }
}

impl SeatEvidence for SeatIndex {
    fn of_account(&self, room: &BareJid, account: &BareJid) -> Vec<StreamEvidence> {
        self.rooms()
            .get(room)
            .map(|seats| {
                seats
                    .values()
                    .filter(|held| held.seat.occupant.to_bare() == *account)
                    .map(|held| held.seat.evidence)
                    .collect()
            })
            .unwrap_or_default()
    }
}
