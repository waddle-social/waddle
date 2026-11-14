//! Witnesses of the canonical host-grant store kept outside PostgreSQL.
//!
//! A restore of PostgreSQL brings back everything the database records
//! about itself, so what tells a restored database from the live one has to
//! be kept elsewhere. Besides the projection markers, a replica keeps three
//! kinds of witness in SpiceDB, in the projection's own definition and
//! relation (`waddle_grant_projection#marker`), each under a resource ID
//! that no lifecycle identity can take:
//!
//! - **The journal head** of each scope, `head_<lifecycle>`: the stamp of
//!   the scope's latest recorded operation, in the marker's spelling. It is
//!   written after every committed operation, whether or not the projection
//!   followed, so it advances while the projection is stuck. A head the
//!   journal does not record shows that the database lost that operation.
//! - **The room bindings** of each projected scope, `room_<digest>` naming
//!   the lifecycle, written in the same request as the projection. A room
//!   whose witness names a lifecycle the database does not hold belongs to a
//!   lost enrollment; its acceptance is refused rather than treated as a
//!   room no scope manages.
//! - **The PostgreSQL position** of the authority, `position_<authority>`:
//!   the declared restore epoch, the PostgreSQL system identifier, the
//!   timeline and the flushed WAL position read after a committed operation
//!   or a passed fence. A position that went back on the same timeline is a
//!   rollback of the database in place or a recovery to an earlier point.
//!
//! Every witness is written after the PostgreSQL commit it describes, never
//! inside it, so SpiceDB stays off the revoke's commit path (ADR-0020 D5).
//! A write is conditioned on the exact witness it replaces and only moves a
//! witness forward: a witness the database contradicts is evidence, and no
//! writer replaces it until an operator has declared and reopened the
//! restore (journal heads) or acknowledged the position. Witnesses are best
//! effort: one that was never written detects nothing, and a SpiceDB that
//! was restored or lost together with PostgreSQL leaves none.

use std::fmt;

use kameo::actor::ActorRef;

use super::authority::projection_store_error;
use super::error::HostGrantError;
use super::marker::ProjectionMarker;
use super::model::{Fingerprint, ScopeLifecycleId};
use super::projection::ProjectionWrite;
use super::restore::RestoreEpoch;
use super::store::HostGrantStore;
use crate::permissions::{PermissionActor, ReadHostGrantWitness, ReplaceHostGrantWitness};

const HEAD_PREFIX: &str = "head_";
const ROOM_PREFIX: &str = "room_";
const POSITION_PREFIX: &str = "position_";

/// How many times a witness writer reads again after another writer
/// replaced the witness it was conditioned on.
const WITNESS_WRITE_ATTEMPTS: usize = 3;

/// One resource of the projection definition as SpiceDB holds it: its ID
/// and the subject ID of each relationship it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListedWitness {
    pub resource: String,
    pub stamps: Vec<String>,
    /// Every relationship is a plain `marker` relationship to a resource of
    /// the projection definition, without a caveat.
    pub well_formed: bool,
}

impl ListedWitness {
    /// The one stamp a well-formed resource holds; `Err` when it holds more
    /// than one, or one that is not a plain marker relationship.
    pub(crate) fn single_stamp(&self) -> Result<Option<&str>, ()> {
        match (self.well_formed, self.stamps.as_slice()) {
            (true, []) => Ok(None),
            (true, [stamp]) => Ok(Some(stamp.as_str())),
            _ => Err(()),
        }
    }
}

/// What a resource of the projection definition is, by its ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WitnessResource {
    /// A scope's projection marker.
    Marker(ScopeLifecycleId),
    /// A scope's journal-head witness.
    Head(ScopeLifecycleId),
    /// A room binding: the digest of the room's authorization object.
    Room(String),
    /// The PostgreSQL position witness of an authority.
    Position(String),
    /// Nothing this build writes.
    Unknown,
}

impl WitnessResource {
    pub(crate) fn classify(resource: &str) -> Self {
        if let Some(lifecycle) = ScopeLifecycleId::from_storage(resource) {
            return Self::Marker(lifecycle);
        }
        if let Some(lifecycle) = resource
            .strip_prefix(HEAD_PREFIX)
            .and_then(ScopeLifecycleId::from_storage)
        {
            return Self::Head(lifecycle);
        }
        if let Some(digest) = resource
            .strip_prefix(ROOM_PREFIX)
            .filter(|digest| is_hex(digest, 64))
        {
            return Self::Room(digest.to_owned());
        }
        if let Some(authority) = resource
            .strip_prefix(POSITION_PREFIX)
            .filter(|authority| is_hex(authority, 32))
        {
            return Self::Position(authority.to_owned());
        }
        Self::Unknown
    }
}

fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The resource of a scope's journal-head witness.
pub fn head_witness_resource(lifecycle: ScopeLifecycleId) -> String {
    format!("{HEAD_PREFIX}{}", lifecycle.to_storage())
}

/// The digest a room binding witness is filed under: SHA-256 of the room's
/// authorization object, so the witness names no room.
pub fn room_witness_digest(channel_id: &str) -> String {
    let mut fingerprint = Fingerprint::new("waddle:host-grant:room-witness:v1");
    fingerprint.text(channel_id);
    fingerprint.finish()
}

/// The resource of a room binding witness.
pub fn room_witness_resource(channel_id: &str) -> String {
    format!("{ROOM_PREFIX}{}", room_witness_digest(channel_id))
}

/// The resource of an authority's PostgreSQL position witness.
pub fn position_witness_resource(authority_id: &str) -> String {
    format!("{POSITION_PREFIX}{authority_id}")
}

/// Where the PostgreSQL server stands in its write-ahead log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostgresPosition {
    /// `pg_control_system().system_identifier`: fixed when the cluster was
    /// created, kept by a physical copy.
    pub system_identifier: i64,
    /// The timeline the server writes on; a promotion or a recovery to a
    /// point in time starts a new one.
    pub timeline: u32,
    /// `pg_current_wal_flush_lsn()`, in bytes.
    pub lsn: u64,
}

/// The position witness: the declared restore epoch it was written under
/// and the position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionWitness {
    pub epoch: RestoreEpoch,
    pub position: PostgresPosition,
}

impl PositionWitness {
    /// `<epoch>_<system identifier>_<timeline>_<lsn>`, in decimal.
    pub fn stamp(&self) -> String {
        format!(
            "{}_{}_{}_{}",
            self.epoch.value(),
            self.position.system_identifier,
            self.position.timeline,
            self.position.lsn
        )
    }

    /// Parse exactly the spelling [`Self::stamp`] writes.
    pub fn parse(stamp: &str) -> Option<Self> {
        let mut parts = stamp.split('_');
        let epoch = RestoreEpoch::parse(parts.next()?)?;
        let system_identifier = parts.next()?.parse::<i64>().ok()?;
        let timeline = parts.next()?.parse::<u32>().ok()?;
        let lsn = parts.next()?.parse::<u64>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        let witness = Self {
            epoch,
            position: PostgresPosition {
                system_identifier,
                timeline,
                lsn,
            },
        };
        (witness.stamp() == stamp).then_some(witness)
    }
}

impl fmt::Display for PositionWitness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.stamp())
    }
}

/// How the current PostgreSQL position relates to its witness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionStanding {
    /// No witness is recorded; the current position becomes it.
    Unrecorded,
    /// The witness is the current position.
    Current,
    /// The server wrote further on the witnessed timeline.
    Advanced,
    /// A restore was declared since the witness, or an operator acknowledged
    /// the current system and timeline: the witness is replaced.
    Rebased,
    /// The position went back: the database was rolled back in place,
    /// recovered to an earlier point, or replaced by an older copy.
    Regressed,
    /// The system or the timeline changed (a promotion, a recovery to a
    /// point in time, another cluster) and whether the witnessed position is
    /// part of the current history cannot be read; an operator
    /// acknowledgement or a declared restore is required.
    Unverified,
}

impl PositionStanding {
    /// Whether the witness is written to the current position.
    pub fn writes(self) -> bool {
        matches!(self, Self::Unrecorded | Self::Advanced | Self::Rebased)
    }
}

/// Compare the current position with its witness.
///
/// - On the witnessed system and timeline, a lower position is a
///   regression and a higher one an advance.
/// - A witness of an earlier declared epoch is replaced: the declared
///   restore is the acknowledgement of what the database lost.
/// - On another system or timeline, the position is accepted only when an
///   operator acknowledged exactly that system and timeline. Otherwise a
///   position below the witnessed one on the same system cannot contain it
///   and is a regression, and anything else is unverified: the switch point
///   of a timeline is in its history file, which a runtime role cannot read.
pub fn position_standing(
    witness: Option<&PositionWitness>,
    current: &PositionWitness,
    acknowledged: Option<(i64, u32)>,
) -> PositionStanding {
    let Some(witness) = witness else {
        return PositionStanding::Unrecorded;
    };
    if witness.epoch < current.epoch {
        return PositionStanding::Rebased;
    }
    if witness.epoch > current.epoch {
        return PositionStanding::Regressed;
    }
    let (seen, now) = (witness.position, current.position);
    if (seen.system_identifier, seen.timeline) == (now.system_identifier, now.timeline) {
        return match now.lsn.cmp(&seen.lsn) {
            std::cmp::Ordering::Less => PositionStanding::Regressed,
            std::cmp::Ordering::Equal => PositionStanding::Current,
            std::cmp::Ordering::Greater => PositionStanding::Advanced,
        };
    }
    if acknowledged == Some((now.system_identifier, now.timeline)) {
        return PositionStanding::Rebased;
    }
    if seen.system_identifier == now.system_identifier && now.lsn < seen.lsn {
        return PositionStanding::Regressed;
    }
    PositionStanding::Unverified
}

/// What a witness writer did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WitnessSync {
    /// The witness was written.
    Written,
    /// The witness already held the current value.
    Current,
    /// The witness holds something the canonical store contradicts or does
    /// not reach; it is evidence and was left as it is.
    Kept,
    /// The witness resource is malformed and was left as it is.
    Malformed,
    /// Nothing to write: the scope is unknown, or the backend holds no
    /// projection.
    Skipped,
}

/// Move a scope's journal-head witness to the operation the journal records
/// last, when that is a move forward.
///
/// The witness is read before the journal, so a newer head another writer
/// recorded is never mistaken for one the journal lost. It is replaced when
/// the journal records it at an older generation, or, when the journal does
/// not record it, only once its incarnation was superseded by a declared
/// restore that an operator has reopened under the current incarnation.
pub async fn sync_journal_witness(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
    lifecycle: ScopeLifecycleId,
) -> Result<WitnessSync, HostGrantError> {
    let resource = head_witness_resource(lifecycle);
    for _ in 0..WITNESS_WRITE_ATTEMPTS {
        let Some(listed) = read_witness(permissions, &resource).await? else {
            return Ok(WitnessSync::Skipped);
        };
        let Ok(stamp) = listed.single_stamp() else {
            return Ok(WitnessSync::Malformed);
        };
        let previous = match stamp {
            None => None,
            Some(stamp) => match ProjectionMarker::parse(lifecycle, stamp) {
                Some(marker) => Some(marker),
                None => return Ok(WitnessSync::Malformed),
            },
        };
        let Some(head) = store.journal_head(lifecycle).await? else {
            return Ok(WitnessSync::Skipped);
        };
        if let Some(previous) = &previous {
            if previous == &head.marker {
                return Ok(WitnessSync::Current);
            }
            let recorded = store
                .recorded_marker(lifecycle, previous.generation)
                .await?
                .as_ref()
                == Some(previous);
            let forward = if recorded {
                previous.generation < head.marker.generation
            } else {
                previous.incarnation != head.marker.incarnation && head.reopened
            };
            if !forward {
                return Ok(WitnessSync::Kept);
            }
        }
        let next = head.marker.stamp();
        match replace_witness(
            permissions,
            &resource,
            previous.as_ref().map(ProjectionMarker::stamp),
            next,
        )
        .await?
        {
            Some(ProjectionWrite::Applied) => return Ok(WitnessSync::Written),
            Some(ProjectionWrite::PreconditionFailed) => continue,
            None => return Ok(WitnessSync::Skipped),
        }
    }
    Ok(WitnessSync::Kept)
}

/// Move the authority's PostgreSQL position witness forward, when the
/// current position does not contradict it. The witness is read before the
/// position, so the position read is at least the one any writer recorded.
pub async fn sync_position_witness(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
) -> Result<(WitnessSync, Option<PositionStanding>), HostGrantError> {
    let Some(authority) = store.position_authority().await? else {
        return Ok((WitnessSync::Skipped, None));
    };
    let resource = position_witness_resource(&authority.authority_id);
    for _ in 0..WITNESS_WRITE_ATTEMPTS {
        let Some(listed) = read_witness(permissions, &resource).await? else {
            return Ok((WitnessSync::Skipped, None));
        };
        let Ok(stamp) = listed.single_stamp() else {
            return Ok((WitnessSync::Malformed, None));
        };
        let previous = match stamp {
            None => None,
            Some(stamp) => match PositionWitness::parse(stamp) {
                Some(witness) => Some(witness),
                None => return Ok((WitnessSync::Malformed, None)),
            },
        };
        let current = PositionWitness {
            epoch: authority.restore_epoch,
            position: store.postgres_position().await?,
        };
        let standing = position_standing(
            previous.as_ref(),
            &current,
            authority.position_acknowledgement,
        );
        if !standing.writes() {
            let sync = if standing == PositionStanding::Current {
                WitnessSync::Current
            } else {
                WitnessSync::Kept
            };
            return Ok((sync, Some(standing)));
        }
        match replace_witness(
            permissions,
            &resource,
            previous.as_ref().map(PositionWitness::stamp),
            current.stamp(),
        )
        .await?
        {
            Some(ProjectionWrite::Applied) => return Ok((WitnessSync::Written, Some(standing))),
            Some(ProjectionWrite::PreconditionFailed) => continue,
            None => return Ok((WitnessSync::Skipped, None)),
        }
    }
    Ok((WitnessSync::Kept, None))
}

/// Bring a scope's journal head and the authority's position witnesses up
/// to date after a committed operation, logging only failure kinds. It is
/// what a writer spawns after its commit, so the commit never waits for
/// SpiceDB.
pub async fn sync_witnesses_after_commit(
    store: HostGrantStore,
    permissions: ActorRef<PermissionActor>,
    lifecycle: ScopeLifecycleId,
) {
    if let Err(error) = sync_journal_witness(&store, &permissions, lifecycle).await {
        tracing::debug!(
            kind = error.kind(),
            "the host-grant journal witness was not brought up to date"
        );
    }
    if let Err(error) = sync_position_witness(&store, &permissions).await {
        tracing::debug!(
            kind = error.kind(),
            "the host-grant position witness was not brought up to date"
        );
    }
}

async fn read_witness(
    permissions: &ActorRef<PermissionActor>,
    resource: &str,
) -> Result<Option<ListedWitness>, HostGrantError> {
    permissions
        .ask(ReadHostGrantWitness {
            resource: resource.to_owned(),
        })
        .await
        .map_err(projection_store_error)
}

async fn replace_witness(
    permissions: &ActorRef<PermissionActor>,
    resource: &str,
    previous: Option<String>,
    next: String,
) -> Result<Option<ProjectionWrite>, HostGrantError> {
    permissions
        .ask(ReplaceHostGrantWitness {
            resource: resource.to_owned(),
            previous,
            next,
        })
        .await
        .map_err(projection_store_error)
}
