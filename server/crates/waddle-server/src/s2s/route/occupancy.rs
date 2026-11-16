//! The remote rooms local resources occupy, shared by every replica
//! (ADR-0020 D7's room exception; XEP-0045).
//!
//! One row per local full JID and remote room bare JID records where the
//! occupancy stands ([`OccupancyPhase`]), the nickname, the affiliation the
//! room last stated, and the replica whose router last routed the
//! resource's stanzas for the room. Any replica reads it to admit the
//! room's traffic for the occupant, whichever replica the peer's stream
//! reaches, and any replica ends it when the resource ends or a stream to
//! the room's domain fails. Every write but the first insert is a
//! compare-and-set on the row's revision, so concurrent writers on
//! different replicas never overwrite each other's change unseen.
//!
//! The table also caches whether a remote domain is a multi-user chat
//! service, keyed by the domain's JID, and whether a bare JID at such a
//! service is a room, keyed by that JID, as their XEP-0030 `disco#info`
//! answered ([`RoomVerdict`]).
//!
//! The decisions are recorded in `server/docs/planning/s2s-routing-2047.md`
//! (outbound MUC).

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use thiserror::Error;
use xmpp_parsers::muc::user::Affiliation;

use crate::db::{Database, DatabaseDriver, Row};

/// The longest occupant or room address a row stores, in bytes. Two such
/// addresses keep the primary key below PostgreSQL's B-tree entry limit
/// (2704 bytes with the default 8 kB page).
pub const MAX_OCCUPANCY_ADDRESS_BYTES: usize = 1023;

/// The longest join presence id a row keeps to answer the join, in bytes;
/// a longer id is not kept and the answer carries none.
pub const MAX_JOIN_ID_BYTES: usize = 256;

/// Where an occupancy stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OccupancyPhase {
    /// The join is held while this server asks the room's domain whether
    /// it is a multi-user chat service; nothing was sent to the room.
    Verifying,
    /// The join presence was sent; the room has not yet sent the
    /// occupant's own presence.
    Joining,
    /// The room sent the occupant's own available presence.
    Joined,
    /// The occupant sent unavailable presence. The room's answer and the
    /// traffic it sent before reading the leave are admitted until the
    /// row's deadline.
    Leaving,
}

impl OccupancyPhase {
    fn code(self) -> &'static str {
        match self {
            Self::Verifying => "verifying",
            Self::Joining => "joining",
            Self::Joined => "joined",
            Self::Leaving => "leaving",
        }
    }

    fn parse(code: &str) -> Option<Self> {
        match code {
            "verifying" => Some(Self::Verifying),
            "joining" => Some(Self::Joining),
            "joined" => Some(Self::Joined),
            "leaving" => Some(Self::Leaving),
            _ => None,
        }
    }

    /// Whether the room seats the occupant or is about to: the join left.
    pub fn occupies(self) -> bool {
        matches!(self, Self::Joining | Self::Joined)
    }
}

fn affiliation_code(affiliation: &Affiliation) -> &'static str {
    match affiliation {
        Affiliation::Owner => "owner",
        Affiliation::Admin => "admin",
        Affiliation::Member => "member",
        Affiliation::Outcast => "outcast",
        Affiliation::None => "none",
    }
}

/// One local resource's occupancy of one remote room.
#[derive(Debug, Clone, PartialEq)]
pub struct Occupancy {
    pub occupant: FullJid,
    pub room: BareJid,
    /// The nickname requested while joining, then the one the room's own
    /// presence names (XEP-0045 §7.2.2 lets the room assign another).
    pub nick: String,
    pub phase: OccupancyPhase,
    /// The affiliation the room last stated in the occupant's own
    /// presence.
    pub affiliation: Affiliation,
    /// Names the join that put the occupancy in its current verifying or
    /// joining phase; a later join replaces it. Settling a held join or a
    /// failed join acts only while the row still names that join.
    pub attempt: String,
    /// The id of that join presence, which its error answer carries.
    pub join_id: Option<String>,
    /// Leaves sent before the current join whose answer, the occupant's
    /// own unavailable presence, has not yet arrived. Each such answer is
    /// absorbed, not taken as the end of the occupancy, until
    /// `leave_until_ms`.
    pub leaves_pending: i64,
    pub leave_until_ms: Option<i64>,
    /// The replica whose router last routed the resource's stanzas for the
    /// room.
    pub owner: String,
    /// When a verifying, joining or leaving occupancy ends unanswered, in
    /// milliseconds since the Unix epoch.
    pub deadline_ms: Option<i64>,
    pub updated_at_ms: i64,
    /// Incremented by every write; a write applies only to the revision
    /// it read.
    pub revision: i64,
}

/// Bounds on the rows a join may add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomLimits {
    pub per_resource: usize,
    pub per_account: usize,
    pub total: usize,
}

/// What inserting a new occupancy did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    /// A row for the same occupant and room exists.
    Exists,
    /// The insert would exceed a bound.
    Bounds,
}

/// Whether a remote domain is a multi-user chat service, or a bare JID at
/// one is a room, as its `disco#info` answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomVerdict {
    /// A domain that names the `conference/text` identity and the MUC
    /// feature and no `server` identity; a bare JID that names a
    /// `conference` identity, or answers `item-not-found` at such a
    /// service.
    Room,
    /// It answered, and is not a multi-user chat service, or not a room.
    NotRoom,
}

impl RoomVerdict {
    fn code(self) -> &'static str {
        match self {
            Self::Room => "room",
            Self::NotRoom => "not_room",
        }
    }
}

/// Why the occupancy table could not be used.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RoomStoreError {
    /// An address or nickname is longer than the table stores.
    #[error("the occupancy's addresses are too long to store")]
    TooLong,
    /// The store failed; the outcome of a write is unknown.
    #[error("the remote room store failed: {0}")]
    Storage(String),
}

fn storage(error: impl std::fmt::Display) -> RoomStoreError {
    RoomStoreError::Storage(error.to_string())
}

/// The remote rooms local resources occupy, shared by every replica.
#[async_trait]
pub trait RemoteRoomStore: Send + Sync {
    /// The occupancy of `room` by `occupant`, if any.
    async fn get(
        &self,
        occupant: &FullJid,
        room: &BareJid,
    ) -> Result<Option<Occupancy>, RoomStoreError>;

    /// Insert `occupancy` unless a row for its occupant and room exists or
    /// the row would exceed `limits`.
    async fn insert(
        &self,
        occupancy: &Occupancy,
        limits: &RoomLimits,
    ) -> Result<InsertOutcome, RoomStoreError>;

    /// Replace the row with `occupancy` if it is still at
    /// `occupancy.revision`, and store it at the next revision. `true`
    /// when it was replaced.
    async fn replace(&self, occupancy: &Occupancy) -> Result<bool, RoomStoreError>;

    /// Remove `occupancy`'s row if it is still at `occupancy.revision` and
    /// still names `occupancy.attempt`. `true` when this call removed it;
    /// only that caller acts on its end. A row removed and inserted again
    /// for the same occupant and room starts its revisions again, but a new
    /// row always names a new attempt, so a removal decided on the old row
    /// never removes the new one.
    async fn remove(&self, occupancy: &Occupancy) -> Result<bool, RoomStoreError>;

    /// Record that `owner` now routes the occupant's stanzas for `room`.
    async fn adopt(
        &self,
        occupant: &FullJid,
        room: &BareJid,
        owner: &str,
        now_ms: i64,
    ) -> Result<(), RoomStoreError>;

    /// Record that `owner` now routes every stanza of `occupant`, whose
    /// session resumed there.
    async fn adopt_resource(
        &self,
        occupant: &FullJid,
        owner: &str,
        now_ms: i64,
    ) -> Result<(), RoomStoreError>;

    /// Every occupancy of `occupant`.
    async fn of_resource(&self, occupant: &FullJid) -> Result<Vec<Occupancy>, RoomStoreError>;

    /// Every occupancy of a resource at `local_domain` in a room at
    /// `room_domain`, only those `owner` routes when it is given.
    async fn of_pair(
        &self,
        local_domain: &str,
        room_domain: &str,
        owner: Option<&str>,
    ) -> Result<Vec<Occupancy>, RoomStoreError>;

    /// At most `limit` occupancies whose deadline is at or before `now_ms`.
    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<Occupancy>, RoomStoreError>;

    /// At most `limit` occupancies not written since `before_ms` whose
    /// owner has not recorded that it is alive since then, such as those a
    /// stopped replica left.
    async fn orphaned(
        &self,
        before_ms: i64,
        limit: usize,
    ) -> Result<Vec<Occupancy>, RoomStoreError>;

    /// Occupancies of every resource, in every phase.
    async fn count(&self) -> Result<usize, RoomStoreError>;

    /// The unexpired verdict on `room`, a service's domain JID or a room's
    /// bare JID, if any.
    async fn verdict(
        &self,
        room: &BareJid,
        now_ms: i64,
    ) -> Result<Option<RoomVerdict>, RoomStoreError>;

    /// Record `verdict` on `room` until `expires_at_ms`.
    async fn record_verdict(
        &self,
        room: &BareJid,
        verdict: RoomVerdict,
        expires_at_ms: i64,
    ) -> Result<(), RoomStoreError>;

    /// Remove verdicts that expired at or before `now_ms`.
    async fn prune_verdicts(&self, now_ms: i64) -> Result<u64, RoomStoreError>;
}

/// The occupancy table in the deployment's global database
/// (`s2s_remote_room_occupancies`, with `s2s_remote_room_verdicts`), which
/// every replica shares.
#[derive(Clone)]
pub struct SqlRemoteRoomStore {
    db: Database,
}

const COLUMNS: &str = "occupant_jid, room_jid, nick, phase, affiliation, attempt, join_id, \
     leaves_pending, leave_until_ms, owner_id, deadline_ms, updated_at_ms, revision";

const KEY_CLAUSE: &str = "occupant_jid = ? AND room_jid = ?";

/// The seed of the PostgreSQL advisory lock key a join takes on its
/// account (`hashtextextended(account, seed)`), which keeps those keys
/// apart from the other advisory locks of the schema.
const ACCOUNT_LOCK_SEED: i64 = 0x7332_735f_726f_6f6d;

impl SqlRemoteRoomStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    async fn execute(
        &self,
        sql: &str,
        params: Vec<crate::db::Value>,
    ) -> Result<u64, RoomStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        guard.execute(sql, params).await.map_err(storage)
    }

    async fn select(
        &self,
        sql: &str,
        params: Vec<crate::db::Value>,
    ) -> Result<Vec<Occupancy>, RoomStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard.query(sql, params).await.map_err(storage)?;
        let mut occupancies = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            occupancies.push(decode(&row)?);
        }
        Ok(occupancies)
    }
}

fn fits(occupancy: &Occupancy) -> bool {
    occupancy.occupant.as_str().len() <= MAX_OCCUPANCY_ADDRESS_BYTES
        && occupancy.room.as_str().len() <= MAX_OCCUPANCY_ADDRESS_BYTES
        && !occupancy.nick.is_empty()
        && occupancy.nick.len() <= MAX_OCCUPANCY_ADDRESS_BYTES
        && occupancy
            .join_id
            .as_ref()
            .is_none_or(|id| id.len() <= MAX_JOIN_ID_BYTES)
}

fn key_params(occupant: &FullJid, room: &BareJid) -> Vec<crate::db::Value> {
    crate::db_params![occupant.as_str(), room.as_str()]
}

/// Decode one row selected with [`COLUMNS`]. Every stored address was
/// written from a parsed JID; one that no longer parses fails the whole
/// read rather than being taken for a missing occupancy.
fn decode(row: &Row) -> Result<Occupancy, RoomStoreError> {
    let occupant: String = row.get(0).map_err(storage)?;
    let room: String = row.get(1).map_err(storage)?;
    let phase: String = row.get(3).map_err(storage)?;
    let affiliation: String = row.get(4).map_err(storage)?;
    Ok(Occupancy {
        occupant: occupant.parse().map_err(storage)?,
        room: room.parse().map_err(storage)?,
        nick: row.get(2).map_err(storage)?,
        phase: OccupancyPhase::parse(&phase)
            .ok_or_else(|| RoomStoreError::Storage("an unknown occupancy phase".to_owned()))?,
        affiliation: affiliation
            .parse::<Affiliation>()
            .map_err(|_| RoomStoreError::Storage("an unknown affiliation".to_owned()))?,
        attempt: row.get(5).map_err(storage)?,
        join_id: row.get(6).map_err(storage)?,
        leaves_pending: row.get(7).map_err(storage)?,
        leave_until_ms: row.get(8).map_err(storage)?,
        owner: row.get(9).map_err(storage)?,
        deadline_ms: row.get(10).map_err(storage)?,
        updated_at_ms: row.get(11).map_err(storage)?,
        revision: row.get(12).map_err(storage)?,
    })
}

/// The occupancies matching `clause`, counted through no more than `bound`
/// rows: a count at the bound means "at least the bound".
async fn count_where(
    transaction: &mut crate::db::Transaction<'_>,
    clause: &str,
    mut params: Vec<crate::db::Value>,
    bound: usize,
) -> Result<usize, RoomStoreError> {
    let sql = [
        "SELECT COUNT(*) FROM (SELECT 1 FROM s2s_remote_room_occupancies",
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
impl RemoteRoomStore for SqlRemoteRoomStore {
    async fn get(
        &self,
        occupant: &FullJid,
        room: &BareJid,
    ) -> Result<Option<Occupancy>, RoomStoreError> {
        let sql = [
            "SELECT ",
            COLUMNS,
            " FROM s2s_remote_room_occupancies WHERE ",
            KEY_CLAUSE,
        ]
        .concat();
        Ok(self
            .select(&sql, key_params(occupant, room))
            .await?
            .into_iter()
            .next())
    }

    async fn insert(
        &self,
        occupancy: &Occupancy,
        limits: &RoomLimits,
    ) -> Result<InsertOutcome, RoomStoreError> {
        if !fits(occupancy) {
            return Err(RoomStoreError::TooLong);
        }
        let account = occupancy.occupant.to_bare();
        // The counts and the insert are one transaction. On SQLite it holds
        // the write lock throughout. On PostgreSQL it first takes a
        // transaction advisory lock on the account, so the joins of one
        // account, and so of each of its resources, are counted and
        // inserted one at a time and their bounds hold exactly; at READ
        // COMMITTED each count then sees every join committed before the
        // lock was granted. Joins of different accounts run concurrently,
        // so the deployment's total can be exceeded briefly by the joins
        // in flight at once. Each count reads at most its bound's rows
        // through an index (the primary key, the account index), and the
        // total reads at most `limits.total`, so no join counts the whole
        // table.
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
                    crate::db_params![account.as_str(), ACCOUNT_LOCK_SEED],
                )
                .await
                .map_err(storage)?;
        }
        let resource = count_where(
            &mut transaction,
            " WHERE occupant_jid = ?",
            crate::db_params![occupancy.occupant.as_str()],
            limits.per_resource,
        )
        .await?;
        let held_by_account = count_where(
            &mut transaction,
            " WHERE account_jid = ?",
            crate::db_params![account.as_str()],
            limits.per_account,
        )
        .await?;
        let total = count_where(&mut transaction, "", Vec::new(), limits.total).await?;
        if resource >= limits.per_resource
            || held_by_account >= limits.per_account
            || total >= limits.total
        {
            transaction.rollback().await.map_err(storage)?;
            return Ok(InsertOutcome::Bounds);
        }
        let inserted = transaction
            .execute(
                "INSERT INTO s2s_remote_room_occupancies \
                 (occupant_jid, room_jid, account_jid, local_domain, room_domain, nick, phase, \
                 affiliation, attempt, join_id, leaves_pending, leave_until_ms, owner_id, \
                 deadline_ms, updated_at_ms, revision) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
                crate::db_params![
                    occupancy.occupant.as_str(),
                    occupancy.room.as_str(),
                    account.as_str(),
                    occupancy.occupant.domain().as_str(),
                    occupancy.room.domain().as_str(),
                    occupancy.nick.as_str(),
                    occupancy.phase.code(),
                    affiliation_code(&occupancy.affiliation),
                    occupancy.attempt.as_str(),
                    occupancy.join_id.as_deref(),
                    occupancy.leaves_pending,
                    occupancy.leave_until_ms,
                    occupancy.owner.as_str(),
                    occupancy.deadline_ms,
                    occupancy.updated_at_ms,
                    occupancy.revision,
                ],
            )
            .await
            .map_err(storage)?;
        transaction.commit().await.map_err(storage)?;
        Ok(if inserted == 1 {
            InsertOutcome::Inserted
        } else {
            InsertOutcome::Exists
        })
    }

    async fn replace(&self, occupancy: &Occupancy) -> Result<bool, RoomStoreError> {
        if !fits(occupancy) {
            return Err(RoomStoreError::TooLong);
        }
        let mut params = crate::db_params![
            occupancy.nick.as_str(),
            occupancy.phase.code(),
            affiliation_code(&occupancy.affiliation),
            occupancy.attempt.as_str(),
            occupancy.join_id.as_deref(),
            occupancy.leaves_pending,
            occupancy.leave_until_ms,
            occupancy.owner.as_str(),
            occupancy.deadline_ms,
            occupancy.updated_at_ms,
            occupancy.revision.saturating_add(1),
        ];
        params.extend(key_params(&occupancy.occupant, &occupancy.room));
        params.push(occupancy.revision.into());
        let replaced = self
            .execute(
                &[
                    "UPDATE s2s_remote_room_occupancies SET nick = ?, phase = ?, \
                     affiliation = ?, attempt = ?, join_id = ?, leaves_pending = ?, \
                     leave_until_ms = ?, owner_id = ?, deadline_ms = ?, updated_at_ms = ?, \
                     revision = ? WHERE ",
                    KEY_CLAUSE,
                    " AND revision = ?",
                ]
                .concat(),
                params,
            )
            .await?;
        Ok(replaced == 1)
    }

    async fn remove(&self, occupancy: &Occupancy) -> Result<bool, RoomStoreError> {
        let mut params = key_params(&occupancy.occupant, &occupancy.room);
        params.push(occupancy.revision.into());
        params.push(occupancy.attempt.as_str().into());
        let removed = self
            .execute(
                &[
                    "DELETE FROM s2s_remote_room_occupancies WHERE ",
                    KEY_CLAUSE,
                    " AND revision = ? AND attempt = ?",
                ]
                .concat(),
                params,
            )
            .await?;
        Ok(removed == 1)
    }

    async fn adopt(
        &self,
        occupant: &FullJid,
        room: &BareJid,
        owner: &str,
        now_ms: i64,
    ) -> Result<(), RoomStoreError> {
        let mut params = crate::db_params![owner, now_ms];
        params.extend(key_params(occupant, room));
        params.push(owner.into());
        self.execute(
            &[
                "UPDATE s2s_remote_room_occupancies SET owner_id = ?, updated_at_ms = ?, \
                 revision = revision + 1 WHERE ",
                KEY_CLAUSE,
                " AND owner_id <> ?",
            ]
            .concat(),
            params,
        )
        .await
        .map(|_| ())
    }

    async fn adopt_resource(
        &self,
        occupant: &FullJid,
        owner: &str,
        now_ms: i64,
    ) -> Result<(), RoomStoreError> {
        self.execute(
            "UPDATE s2s_remote_room_occupancies SET owner_id = ?, updated_at_ms = ?, \
             revision = revision + 1 WHERE occupant_jid = ? AND owner_id <> ?",
            crate::db_params![owner, now_ms, occupant.as_str(), owner],
        )
        .await
        .map(|_| ())
    }

    async fn of_resource(&self, occupant: &FullJid) -> Result<Vec<Occupancy>, RoomStoreError> {
        let sql = [
            "SELECT ",
            COLUMNS,
            " FROM s2s_remote_room_occupancies WHERE occupant_jid = ?",
        ]
        .concat();
        self.select(&sql, crate::db_params![occupant.as_str()])
            .await
    }

    async fn of_pair(
        &self,
        local_domain: &str,
        room_domain: &str,
        owner: Option<&str>,
    ) -> Result<Vec<Occupancy>, RoomStoreError> {
        let mut params = crate::db_params![local_domain, room_domain];
        let mut sql = [
            "SELECT ",
            COLUMNS,
            " FROM s2s_remote_room_occupancies WHERE local_domain = ? AND room_domain = ?",
        ]
        .concat();
        if let Some(owner) = owner {
            sql.push_str(" AND owner_id = ?");
            params.push(owner.into());
        }
        self.select(&sql, params).await
    }

    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<Occupancy>, RoomStoreError> {
        let sql = [
            "SELECT ",
            COLUMNS,
            " FROM s2s_remote_room_occupancies WHERE deadline_ms <= ? \
             ORDER BY deadline_ms LIMIT ?",
        ]
        .concat();
        self.select(&sql, crate::db_params![now_ms, limit]).await
    }

    async fn orphaned(
        &self,
        before_ms: i64,
        limit: usize,
    ) -> Result<Vec<Occupancy>, RoomStoreError> {
        let sql = [
            "SELECT ",
            COLUMNS,
            " FROM s2s_remote_room_occupancies WHERE updated_at_ms <= ? AND owner_id NOT IN \
             (SELECT owner_id FROM s2s_iq_correlation_owners WHERE seen_at_ms > ?) \
             ORDER BY updated_at_ms LIMIT ?",
        ]
        .concat();
        self.select(&sql, crate::db_params![before_ms, before_ms, limit])
            .await
    }

    async fn count(&self) -> Result<usize, RoomStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT COUNT(*) FROM s2s_remote_room_occupancies",
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

    async fn verdict(
        &self,
        room: &BareJid,
        now_ms: i64,
    ) -> Result<Option<RoomVerdict>, RoomStoreError> {
        let guard = self.db.guard().await.map_err(storage)?;
        let mut rows = guard
            .query(
                "SELECT verdict FROM s2s_remote_room_verdicts \
                 WHERE room_jid = ? AND expires_at_ms > ?",
                crate::db_params![room.as_str(), now_ms],
            )
            .await
            .map_err(storage)?;
        let Some(row) = rows.next().await.map_err(storage)? else {
            return Ok(None);
        };
        let verdict: String = row.get(0).map_err(storage)?;
        Ok(match verdict.as_str() {
            "room" => Some(RoomVerdict::Room),
            "not_room" => Some(RoomVerdict::NotRoom),
            _ => None,
        })
    }

    async fn record_verdict(
        &self,
        room: &BareJid,
        verdict: RoomVerdict,
        expires_at_ms: i64,
    ) -> Result<(), RoomStoreError> {
        if room.as_str().len() > MAX_OCCUPANCY_ADDRESS_BYTES {
            return Err(RoomStoreError::TooLong);
        }
        self.execute(
            "INSERT INTO s2s_remote_room_verdicts (room_jid, verdict, expires_at_ms) \
             VALUES (?, ?, ?) ON CONFLICT (room_jid) DO UPDATE SET \
             verdict = excluded.verdict, expires_at_ms = excluded.expires_at_ms",
            crate::db_params![room.as_str(), verdict.code(), expires_at_ms],
        )
        .await
        .map(|_| ())
    }

    async fn prune_verdicts(&self, now_ms: i64) -> Result<u64, RoomStoreError> {
        self.execute(
            "DELETE FROM s2s_remote_room_verdicts WHERE expires_at_ms <= ?",
            crate::db_params![now_ms],
        )
        .await
    }
}
