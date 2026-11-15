//! Witnesses of the canonical host-grant store kept outside PostgreSQL.
//!
//! A restore of PostgreSQL brings back everything the database records
//! about itself, so what tells a restored database from the live one has to
//! be kept elsewhere. Besides the projection markers, a replica keeps four
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
//!   timeline and the flushed WAL position. A position that went back is a
//!   rollback of the database in place or a recovery to an earlier point.
//! - **The regression latch** of the authority, `latch_<authority>`: the
//!   position witness a regression was seen against, and whether it was on
//!   the witnessed timeline. Once a regression is latched it is refused
//!   until a restore is declared (or, for a regression on the witnessed
//!   timeline, an operator acknowledges that exact witness), even after the
//!   server has written past the witnessed position again.
//!
//! Every witness is written after the PostgreSQL commit it describes, never
//! inside it, so SpiceDB stays off the revoke's commit path (ADR-0020 D5).
//! A write is conditioned on the exact witnesses it replaces and only moves
//! a witness forward: a witness the database contradicts is evidence, and no
//! writer replaces it until an operator has declared and reopened the
//! restore (journal heads) or acknowledged the position. Witnesses are best
//! effort: one that was never written detects nothing, and a SpiceDB that
//! was restored or lost together with PostgreSQL leaves none.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};

use kameo::actor::ActorRef;

use super::authority::projection_store_error;
use super::error::HostGrantError;
use super::marker::ProjectionMarker;
use super::model::{Fingerprint, ScopeLifecycleId};
use super::projection::ProjectionWrite;
use super::restore::RestoreEpoch;
use super::store::HostGrantStore;
use crate::permissions::{PermissionActor, ReadHostGrantWitness, ReplaceHostGrantWitnesses};

const HEAD_PREFIX: &str = "head_";
const ROOM_PREFIX: &str = "room_";
const POSITION_PREFIX: &str = "position_";
const LATCH_PREFIX: &str = "latch_";

/// How many times a witness writer reads again after another writer
/// replaced the witness it was conditioned on.
const WITNESS_WRITE_ATTEMPTS: usize = 3;

/// Least time between two warnings about failed witness writes.
const WITNESS_WARNING_INTERVAL_MS: i64 = 60_000;

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

/// A resource of the projection definition that holds more than one
/// relationship, or one that is not a plain marker relationship.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MalformedWitness;

impl ListedWitness {
    /// The one stamp a well-formed resource holds, if any.
    pub(crate) fn single_stamp(&self) -> Result<Option<&str>, MalformedWitness> {
        match (self.well_formed, self.stamps.as_slice()) {
            (true, []) => Ok(None),
            (true, [stamp]) => Ok(Some(stamp.as_str())),
            _ => Err(MalformedWitness),
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
    /// The regression latch of an authority.
    Latch(String),
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
        if let Some(authority) = resource
            .strip_prefix(LATCH_PREFIX)
            .filter(|authority| is_hex(authority, 32))
        {
            return Self::Latch(authority.to_owned());
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

/// The resource of an authority's regression latch.
pub fn latch_witness_resource(authority_id: &str) -> String {
    format!("{LATCH_PREFIX}{authority_id}")
}

/// Where the PostgreSQL server stands in its write-ahead log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostgresPosition {
    /// The cluster's system identifier, as the physical identity spells it
    /// (`pg_control_system().system_identifier` as an unsigned 64-bit
    /// value): fixed when the cluster was created, kept by a physical copy.
    pub system_identifier: u64,
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
        let system_identifier = parts.next()?.parse::<u64>().ok()?;
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

    fn system_and_timeline(&self) -> (u64, u32) {
        (self.position.system_identifier, self.position.timeline)
    }
}

impl fmt::Display for PositionWitness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.stamp())
    }
}

/// An operator's acknowledgement of a PostgreSQL position the witness
/// cannot verify on its own (`waddle-server host-grant
/// acknowledge-position`). It is bound to the exact witness it supersedes
/// and to the system and timeline the server wrote on when it was recorded,
/// so it is honoured at most until the witness moves: once applied it names
/// a witness that no longer exists and is spent, also when a restore of the
/// database brings it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionAcknowledgement {
    /// The position witness the acknowledgement supersedes.
    pub supersedes: PositionWitness,
    /// The system identifier the server wrote on when it was recorded.
    pub system_identifier: u64,
    /// The timeline the server wrote on when it was recorded.
    pub timeline: u32,
}

impl PositionAcknowledgement {
    /// `<witness stamp>/<system identifier>_<timeline>`, as
    /// `host_grant_authority.position_acknowledgement` holds it.
    pub fn spelling(&self) -> String {
        format!(
            "{}/{}_{}",
            self.supersedes.stamp(),
            self.system_identifier,
            self.timeline
        )
    }

    /// Parse exactly the spelling [`Self::spelling`] writes.
    pub fn parse(text: &str) -> Option<Self> {
        let (witness, current) = text.split_once('/')?;
        let (system, timeline) = current.split_once('_')?;
        let acknowledgement = Self {
            supersedes: PositionWitness::parse(witness)?,
            system_identifier: system.parse::<u64>().ok()?,
            timeline: timeline.parse::<u32>().ok()?,
        };
        (acknowledgement.spelling() == text).then_some(acknowledgement)
    }

    /// Whether an acknowledgement of this shape can ever be honoured: on
    /// the witnessed system, for a later timeline (a promotion) or for the
    /// witnessed timeline itself (a regression on it). Another system, or
    /// an earlier timeline, takes a declared restore.
    pub fn admissible(&self) -> bool {
        let witnessed = self.supersedes.position;
        witnessed.system_identifier == self.system_identifier && self.timeline >= witnessed.timeline
    }
}

/// Which kind of regression a latch records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatchKind {
    /// The position went back on the witnessed timeline: the database was
    /// rolled back in place or replaced by an older copy of itself. Only a
    /// declared restore, or an acknowledgement bound to the latched
    /// witness on that same timeline, clears it.
    OnTimeline,
    /// The server writes on another timeline below the witnessed position:
    /// a promotion or a recovery that cannot contain it. Only a declared
    /// restore clears it.
    AcrossTimelines,
}

impl LatchKind {
    fn as_storage(self) -> &'static str {
        match self {
            Self::OnTimeline => "timeline",
            Self::AcrossTimelines => "branch",
        }
    }

    fn from_storage(text: &str) -> Option<Self> {
        match text {
            "timeline" => Some(Self::OnTimeline),
            "branch" => Some(Self::AcrossTimelines),
            _ => None,
        }
    }
}

/// A regression seen against a position witness, kept until a declared
/// restore or a bound acknowledgement clears it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionLatch {
    pub kind: LatchKind,
    /// The position witness the regression was seen against.
    pub witness: PositionWitness,
}

impl PositionLatch {
    /// `<kind>_<witness stamp>`.
    pub fn stamp(&self) -> String {
        format!("{}_{}", self.kind.as_storage(), self.witness.stamp())
    }

    /// Parse exactly the spelling [`Self::stamp`] writes.
    pub fn parse(stamp: &str) -> Option<Self> {
        let (kind, witness) = stamp.split_once('_')?;
        let latch = Self {
            kind: LatchKind::from_storage(kind)?,
            witness: PositionWitness::parse(witness)?,
        };
        (latch.stamp() == stamp).then_some(latch)
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
    /// A restore was declared since the witness: the witness is replaced.
    Rebased,
    /// An operator acknowledged this exact witness for the current system
    /// and timeline: the witness is replaced, and the acknowledgement is
    /// spent with it.
    Acknowledged,
    /// The position went back: the database was rolled back in place,
    /// recovered to an earlier point, or replaced by an older copy; or a
    /// regression seen earlier is latched.
    Regressed,
    /// The system or the timeline changed (a promotion, a recovery to a
    /// point in time, another cluster) and whether the witnessed position is
    /// part of the current history cannot be read; an operator
    /// acknowledgement (a later timeline of the same system) or a declared
    /// restore is required.
    Unverified,
}

impl PositionStanding {
    /// Whether the witness is written to the current position.
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::Unrecorded | Self::Advanced | Self::Rebased | Self::Acknowledged
        )
    }
}

/// The regression `current` shows against `witness`, if its WAL position
/// is below the witnessed one on the same system and declared epoch.
pub fn regression(witness: &PositionWitness, current: &PositionWitness) -> Option<LatchKind> {
    let (seen, now) = (witness.position, current.position);
    if witness.epoch != current.epoch
        || seen.system_identifier != now.system_identifier
        || now.lsn >= seen.lsn
    {
        return None;
    }
    Some(if seen.timeline == now.timeline {
        LatchKind::OnTimeline
    } else {
        LatchKind::AcrossTimelines
    })
}

/// Compare the current position with its witness.
///
/// - A witness of an earlier declared epoch is replaced: the declared
///   restore is the acknowledgement of what the database lost. One of a
///   later epoch belongs to a database the current one predates.
/// - A latch that names the witness keeps the regression it recorded,
///   whatever the position is now. One on the witnessed timeline yields to
///   an acknowledgement bound to that witness and timeline; one across
///   timelines only to a declared restore.
/// - On the witnessed system and timeline, a lower position is a
///   regression, which an acknowledgement bound to that witness and
///   timeline excuses, and a higher one an advance.
/// - On another timeline of the same system, a position below the
///   witnessed one cannot contain it and is a regression no acknowledgement
///   excuses. Otherwise the switch point of the timeline decides, which is
///   in its history file a runtime role cannot read: the position is
///   unverified unless an operator acknowledged that exact witness for the
///   current, later, timeline.
/// - Another system is unverified; only a declared restore replaces it.
pub fn position_standing(
    witness: Option<&PositionWitness>,
    current: &PositionWitness,
    acknowledgement: Option<&PositionAcknowledgement>,
    latch: Option<&PositionLatch>,
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
    let same_timeline = witness.system_and_timeline() == current.system_and_timeline();
    let acknowledged = acknowledgement.is_some_and(|acknowledgement| {
        acknowledgement.supersedes == *witness
            && (acknowledgement.system_identifier, acknowledgement.timeline)
                == current.system_and_timeline()
    });
    if let Some(latch) = latch.filter(|latch| latch.witness == *witness) {
        return if latch.kind == LatchKind::OnTimeline && same_timeline && acknowledged {
            PositionStanding::Acknowledged
        } else {
            PositionStanding::Regressed
        };
    }
    if same_timeline {
        return match now.lsn.cmp(&seen.lsn) {
            std::cmp::Ordering::Less if acknowledged => PositionStanding::Acknowledged,
            std::cmp::Ordering::Less => PositionStanding::Regressed,
            std::cmp::Ordering::Equal => PositionStanding::Current,
            std::cmp::Ordering::Greater => PositionStanding::Advanced,
        };
    }
    if seen.system_identifier != now.system_identifier {
        return PositionStanding::Unverified;
    }
    if now.lsn < seen.lsn {
        return PositionStanding::Regressed;
    }
    if acknowledged && now.timeline > seen.timeline {
        return PositionStanding::Acknowledged;
    }
    PositionStanding::Unverified
}

/// What a witness writer did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WitnessSync {
    /// The witness was written.
    Written,
    /// A regression was latched; the position witness was kept.
    Latched,
    /// The witness already held the current value.
    Current,
    /// The witness holds something the canonical store contradicts or does
    /// not reach; it is evidence and was left as it is.
    Kept,
    /// The witness resource is malformed and was left as it is.
    Malformed,
    /// Nothing to write: the scope is unknown, the backend holds no
    /// projection, or a first position witness waits for a fence pass.
    Skipped,
}

/// One change of a witness write: `resource` holds exactly `previous` (or
/// nothing) when the request applies, and holds `next` (or nothing) after
/// it. Equal values only condition the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WitnessChange {
    pub resource: String,
    pub previous: Option<String>,
    pub next: Option<String>,
}

/// Move a scope's journal-head witness to the operation the journal records
/// last, when that is a move forward.
///
/// The witness is read before the journal, so a newer head another writer
/// recorded is never mistaken for one the journal lost. It is replaced when
/// the journal records it at an older generation, or, when the journal does
/// not record it, only once its incarnation, which the journal held, was
/// superseded by a declared restore that an operator has reopened under the
/// current incarnation. A head of an incarnation the journal never held is
/// kept: no declared restore of this journal superseded it.
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
                previous.incarnation != head.marker.incarnation
                    && head.reopened
                    && store
                        .journal_holds_incarnation(lifecycle, previous.incarnation)
                        .await?
            };
            if !forward {
                return Ok(WitnessSync::Kept);
            }
        }
        let change = WitnessChange {
            resource: resource.clone(),
            previous: previous.as_ref().map(ProjectionMarker::stamp),
            next: Some(head.marker.stamp()),
        };
        match apply_witness_changes(permissions, vec![change]).await? {
            Some(ProjectionWrite::Applied) => return Ok(WitnessSync::Written),
            Some(ProjectionWrite::PreconditionFailed) => continue,
            None => return Ok(WitnessSync::Skipped),
        }
    }
    Ok(WitnessSync::Kept)
}

/// Who asks the position witness writer to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionWriter {
    /// A fence pass that passed: it may write the first position witness,
    /// because the fence found the projection the witness belongs beside.
    Fence,
    /// Any other writer: a committed mutation or an operator's act. It
    /// moves an existing witness and never writes the first one, so a
    /// SpiceDB that holds nothing of this deployment's is not given a
    /// witness by it.
    Commit,
}

/// Move the authority's PostgreSQL position witness forward when the
/// current position does not contradict it, or latch a regression it shows.
/// The witnesses are read before the position, so the position read is at
/// least the one any writer recorded.
///
/// A move that replaces the witness also removes the latch, in the same
/// request; a latch is written conditioned on the exact position witness it
/// names. `latched` is a regression this replica saw and keeps in memory;
/// it counts like a stored latch that names the same witness.
pub async fn sync_position_witness(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
    writer: PositionWriter,
    latched: Option<&PositionLatch>,
) -> Result<(WitnessSync, Option<PositionStanding>), HostGrantError> {
    let Some(authority) = store.position_authority().await? else {
        return Ok((WitnessSync::Skipped, None));
    };
    let resource = position_witness_resource(&authority.authority_id);
    let latch_resource = latch_witness_resource(&authority.authority_id);
    for _ in 0..WITNESS_WRITE_ATTEMPTS {
        let Some(listed) = read_witness(permissions, &resource).await? else {
            return Ok((WitnessSync::Skipped, None));
        };
        let Some(listed_latch) = read_witness(permissions, &latch_resource).await? else {
            return Ok((WitnessSync::Skipped, None));
        };
        let (Ok(stamp), Ok(latch_stamp)) = (listed.single_stamp(), listed_latch.single_stamp())
        else {
            return Ok((WitnessSync::Malformed, None));
        };
        let previous = match stamp {
            None => None,
            Some(stamp) => match PositionWitness::parse(stamp) {
                Some(witness) => Some(witness),
                None => return Ok((WitnessSync::Malformed, None)),
            },
        };
        let stored_latch = match latch_stamp {
            None => None,
            Some(stamp) => match PositionLatch::parse(stamp) {
                Some(latch) => Some(latch),
                None => return Ok((WitnessSync::Malformed, None)),
            },
        };
        let current = PositionWitness {
            epoch: authority.restore_epoch,
            position: store.postgres_position().await?,
        };
        let latch = effective_latch(previous.as_ref(), stored_latch.as_ref(), latched);
        let standing = position_standing(
            previous.as_ref(),
            &current,
            authority.position_acknowledgement.as_ref(),
            latch.as_ref(),
        );
        let changes = match standing {
            PositionStanding::Unrecorded if writer == PositionWriter::Commit => {
                return Ok((WitnessSync::Skipped, Some(standing)));
            }
            standing if standing.writes() => {
                let mut changes = vec![WitnessChange {
                    resource: resource.clone(),
                    previous: previous.as_ref().map(PositionWitness::stamp),
                    next: Some(current.stamp()),
                }];
                if let Some(stored) = latch_stamp {
                    changes.push(WitnessChange {
                        resource: latch_resource.clone(),
                        previous: Some(stored.to_owned()),
                        next: None,
                    });
                }
                changes
            }
            PositionStanding::Regressed => {
                let Some(seen) = previous else {
                    return Ok((WitnessSync::Kept, Some(standing)));
                };
                let stored_names_witness =
                    stored_latch.is_some_and(|stored| stored.witness == seen);
                let latch = match latch {
                    Some(_) if stored_names_witness => {
                        return Ok((WitnessSync::Kept, Some(standing)))
                    }
                    Some(latch) => latch,
                    None => match regression(&seen, &current) {
                        Some(kind) => PositionLatch {
                            kind,
                            witness: seen,
                        },
                        None => return Ok((WitnessSync::Kept, Some(standing))),
                    },
                };
                latch_changes(&resource, &latch_resource, &latch, latch_stamp)
            }
            PositionStanding::Current => return Ok((WitnessSync::Current, Some(standing))),
            _ => return Ok((WitnessSync::Kept, Some(standing))),
        };
        let latching = standing == PositionStanding::Regressed;
        match apply_witness_changes(permissions, changes).await? {
            Some(ProjectionWrite::Applied) if latching => {
                return Ok((WitnessSync::Latched, Some(standing)))
            }
            Some(ProjectionWrite::Applied) => return Ok((WitnessSync::Written, Some(standing))),
            Some(ProjectionWrite::PreconditionFailed) => continue,
            None => return Ok((WitnessSync::Skipped, None)),
        }
    }
    Ok((WitnessSync::Kept, None))
}

/// The latch that counts for `witness`: a stored one that names it, or else
/// the one this replica keeps in memory when it names it.
pub(crate) fn effective_latch(
    witness: Option<&PositionWitness>,
    stored: Option<&PositionLatch>,
    latched: Option<&PositionLatch>,
) -> Option<PositionLatch> {
    let witness = witness?;
    stored
        .filter(|latch| latch.witness == *witness)
        .or_else(|| latched.filter(|latch| latch.witness == *witness))
        .copied()
}

/// The request that stores `latch`: the position witness it names must be
/// exactly that witness, and the latch resource exactly what was read.
pub(crate) fn latch_changes(
    resource: &str,
    latch_resource: &str,
    latch: &PositionLatch,
    stored: Option<&str>,
) -> Vec<WitnessChange> {
    let witness = latch.witness.stamp();
    vec![
        WitnessChange {
            resource: resource.to_owned(),
            previous: Some(witness.clone()),
            next: Some(witness),
        },
        WitnessChange {
            resource: latch_resource.to_owned(),
            previous: stored.map(str::to_owned),
            next: Some(latch.stamp()),
        },
    ]
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
        witness_write_failed(&error, "journal");
    }
    if let Err(error) =
        sync_position_witness(&store, &permissions, PositionWriter::Commit, None).await
    {
        witness_write_failed(&error, "position");
    }
}

/// What a witness pass after a passed fence did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WitnessPass {
    /// Scopes whose journal witness differed from the journal's head and
    /// was brought up to date or kept as evidence.
    pub stale_heads: usize,
    /// Of those, the heads written.
    pub written: usize,
    /// Writes that failed; each was counted and logged by kind.
    pub failed: usize,
}

/// Bring the position witness and every stale journal witness up to date
/// after a fence pass that passed. `witnessed` holds the journal witness
/// stamps the pass listed; a scope whose witness already names its journal
/// head is not visited, so a pass over a deployment whose witnesses are
/// current costs one PostgreSQL read besides the position. Every scope
/// counts, retired and exhausted ones included, so an operator's act on any
/// of them (a retirement, an enrollment, a reopen, a revoke) is witnessed
/// by the next pass that passes.
pub async fn sync_witnesses_after_fence(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
    witnessed: &BTreeMap<ScopeLifecycleId, String>,
    latched: Option<&PositionLatch>,
) -> WitnessPass {
    let mut pass = WitnessPass::default();
    if let Err(error) =
        sync_position_witness(store, permissions, PositionWriter::Fence, latched).await
    {
        witness_write_failed(&error, "position");
        pass.failed += 1;
    }
    let heads = match store.journal_heads().await {
        Ok(heads) => heads,
        Err(error) => {
            witness_write_failed(&error, "journal");
            pass.failed += 1;
            return pass;
        }
    };
    for (lifecycle, head) in heads {
        if witnessed.get(&lifecycle) == Some(&head) {
            continue;
        }
        pass.stale_heads += 1;
        match sync_journal_witness(store, permissions, lifecycle).await {
            Ok(WitnessSync::Written) => pass.written += 1,
            Ok(_) => {}
            Err(error) => {
                witness_write_failed(&error, "journal");
                pass.failed += 1;
            }
        }
    }
    pass
}

/// Bring every scope's journal witness up to date with no listing to
/// compare with first: for an operator's act whose writer holds SpiceDB.
pub async fn sync_journal_witnesses(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
) -> WitnessPass {
    let mut pass = WitnessPass::default();
    let heads = match store.journal_heads().await {
        Ok(heads) => heads,
        Err(error) => {
            witness_write_failed(&error, "journal");
            pass.failed += 1;
            return pass;
        }
    };
    for lifecycle in heads.into_keys() {
        match sync_journal_witness(store, permissions, lifecycle).await {
            Ok(WitnessSync::Written) => {
                pass.stale_heads += 1;
                pass.written += 1;
            }
            Ok(WitnessSync::Kept | WitnessSync::Malformed) => pass.stale_heads += 1,
            Ok(_) => {}
            Err(error) => {
                witness_write_failed(&error, "journal");
                pass.failed += 1;
            }
        }
    }
    pass
}

/// The authority's position witness and regression latch as SpiceDB holds
/// them, for an operator. `None` without the host-grant relations or
/// without a projection backend.
pub async fn read_position_witness(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
) -> Result<Option<(Option<PositionWitness>, Option<PositionLatch>)>, HostGrantError> {
    let Some(authority_id) = store.authority_id().await? else {
        return Ok(None);
    };
    let read = |resource: String| async move {
        read_witness(permissions, &resource).await.map(|listed| {
            listed.map(|listed| listed.single_stamp().map(|stamp| stamp.map(str::to_owned)))
        })
    };
    let (Some(position), Some(latch)) = (
        read(position_witness_resource(&authority_id)).await?,
        read(latch_witness_resource(&authority_id)).await?,
    ) else {
        return Ok(None);
    };
    let malformed = || HostGrantError::CorruptRow {
        field: "position witness",
    };
    let position = position
        .map_err(|_| malformed())?
        .map(|stamp| PositionWitness::parse(&stamp).ok_or_else(malformed))
        .transpose()?;
    let latch = latch
        .map_err(|_| malformed())?
        .map(|stamp| PositionLatch::parse(&stamp).ok_or_else(malformed))
        .transpose()?;
    Ok(Some((position, latch)))
}

/// Count a failed witness write and warn about it at most once a minute,
/// by kind only.
pub(crate) fn witness_write_failed(error: &HostGrantError, witness: &'static str) {
    static LAST_WARNING_MS: AtomicI64 = AtomicI64::new(i64::MIN);
    waddle_xmpp::counter_add!(
        "waddle.host_grant.witness.write_failures",
        "{failure}",
        "Host-grant witness writes that did not complete; a witness that is not written detects \
         nothing, so a sustained nonzero rate widens the restore detection window",
        1,
    );
    if rate_limited(&LAST_WARNING_MS, WITNESS_WARNING_INTERVAL_MS) {
        tracing::warn!(
            kind = error.kind(),
            witness,
            "a host-grant witness was not brought up to date \
             (docs/operations/authz-host-grant-restore.md)"
        );
    }
}

/// Whether at least `interval_ms` passed since the last time this answered
/// yes for `last`; a yes records now.
pub(crate) fn rate_limited(last: &AtomicI64, interval_ms: i64) -> bool {
    let now = chrono::Utc::now().timestamp_millis();
    let previous = last.load(Ordering::Acquire);
    if previous != i64::MIN && now.saturating_sub(previous) < interval_ms {
        return false;
    }
    last.compare_exchange(previous, now, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

pub(crate) async fn read_witness(
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

pub(crate) async fn apply_witness_changes(
    permissions: &ActorRef<PermissionActor>,
    changes: Vec<WitnessChange>,
) -> Result<Option<ProjectionWrite>, HostGrantError> {
    permissions
        .ask(ReplaceHostGrantWitnesses { changes })
        .await
        .map_err(projection_store_error)
}
