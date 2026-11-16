//! The replica's in-memory copy of the enrolled set: which channels, room
//! addresses and Space nodes enrolled host-grant scopes manage.
//!
//! Every surface first asks whether an object is enrolled, and most objects
//! are not. Answering that from PostgreSQL on every request would make every
//! unmanaged room depend on the canonical store being readable, and would
//! put PostgreSQL I/O inside the permission actor's mailbox for every legacy
//! read. Instead the set is loaded when the replica starts and refreshed in
//! the background every [`ENROLLED_SET_REFRESH_INTERVAL`], off every
//! request path:
//!
//! - an object in the set is enrolled, and the surface decides it from the
//!   canonical grants, which it reads from PostgreSQL and fails closed on;
//! - an object outside the set is unmanaged and keeps its legacy checks,
//!   even while PostgreSQL cannot be read: a failed refresh keeps the last
//!   set it loaded, and so does a refresh that finds the host-grant
//!   relations gone after a load;
//! - until the first load succeeds nothing is known, and every object of a
//!   kind a scope can enroll (a channel, a room address, a Space node) is
//!   undecided and fails closed. Startup waits for that first load before
//!   the replica serves. A schema without the host-grant relations holds no
//!   scope, and every object is unmanaged there.
//!
//! Enrollment only ever adds to the set: a room binding is immutable and a
//! retired scope's objects stay enrolled. A load reads one snapshot taken
//! after the load started, so the set it installs holds every scope whose
//! enrollment committed before that start. A replica therefore honours a
//! scope once a load that started after the enrollment committed has
//! succeeded there. The owner-phase enrollment procedure relies on a scope
//! only once every serving replica reports such a load (see the "Writer
//! fence and sink contracts" section of
//! `server/docs/planning/host-grant-authority-2047.md`). A reload that finds
//! fewer objects than the set it replaces is logged at warn with the counts
//! only.
//!
//! Every successful load records when it started, by the wall clock, as the
//! `waddle.host_grant.enrolled_set.load_started` gauge (unix seconds). The
//! value is recorded by the load itself and keeps being exported while later
//! refreshes fail or hang, so `time() - load_started` is the set's age
//! whatever the refresh task is doing; alerting reads it from there. Each
//! refresh tick also records that age as `waddle.host_grant.enrolled_set.age`
//! and, once it exceeds [`ENROLLED_SET_STALE_AFTER`] while the set holds
//! the host-grant relations, logs a rate-limited warning.
//!
//! Readiness does not depend on the set's age. A set that cannot be
//! refreshed still decides every unmanaged room the way it was decided
//! before any scope existed, and a managed room reads its grants from
//! PostgreSQL per decision; taking replicas out of service while PostgreSQL
//! is unreadable would refuse the unmanaged rooms too.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, SystemTime};

use jid::BareJid;
use tokio::sync::watch;
use tokio::time::Instant;
use waddle_xmpp::identity::CanonicalBareJid;

use super::error::HostGrantError;
use super::model::ScopeLifecycleId;
use super::rate_limited::RateLimitedWarning;
use super::store::HostGrantStore;
use crate::permissions::{Object, ObjectType};

/// How often a replica reloads the enrolled set once it has loaded it.
pub const ENROLLED_SET_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// How old the loaded set may grow, while refreshes fail, before a
/// rate-limited warning says it is stale: six refresh intervals. Nothing
/// else depends on it; readiness does not.
pub const ENROLLED_SET_STALE_AFTER: Duration = Duration::from_secs(30);

/// How often the first load is retried until it succeeds.
const FIRST_LOAD_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// How long a reader waits for the first load before it answers
/// [`Enrollment::Undecided`].
const FIRST_LOAD_READER_WAIT: Duration = Duration::from_secs(2);

/// At most one warning a minute that a refresh failed.
static REFRESH_FAILED: RateLimitedWarning = RateLimitedWarning::new();
/// At most one warning a minute that the set is older than
/// [`ENROLLED_SET_STALE_AFTER`].
static STALE: RateLimitedWarning = RateLimitedWarning::new();

/// When a load started, by the monotonic clock (for the set's age) and by
/// the wall clock (for the exported start time).
#[derive(Clone, Copy, Debug)]
struct LoadStart {
    instant: Instant,
    wall: SystemTime,
}

impl LoadStart {
    fn now() -> Self {
        Self {
            instant: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}

/// The enrolled objects of every scope, as one snapshot of the store read
/// them, each with the lifecycle of the scope that enrolled it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnrolledSet {
    /// By canonical room address.
    rooms: BTreeMap<String, ScopeLifecycleId>,
    /// By channel identifier (the room's authorization object).
    channels: BTreeMap<String, ScopeLifecycleId>,
    /// By Space node.
    space_nodes: BTreeMap<String, ScopeLifecycleId>,
}

impl EnrolledSet {
    pub(super) fn insert_scope(&mut self, lifecycle: ScopeLifecycleId, space_node: String) {
        self.space_nodes.insert(space_node, lifecycle);
    }

    pub(super) fn insert_room(
        &mut self,
        lifecycle: ScopeLifecycleId,
        room: String,
        channel_id: String,
    ) {
        self.rooms.insert(room, lifecycle);
        self.channels.insert(channel_id, lifecycle);
    }

    /// The scope managing the room whose canonical address is `room`.
    pub fn room(&self, room: &str) -> Option<ScopeLifecycleId> {
        self.rooms.get(room).copied()
    }

    /// The scope managing the room whose authorization object is `channel_id`.
    pub fn channel(&self, channel_id: &str) -> Option<ScopeLifecycleId> {
        self.channels.get(channel_id).copied()
    }

    /// The scope whose Space node is `node`.
    pub fn space_node(&self, node: &str) -> Option<ScopeLifecycleId> {
        self.space_nodes.get(node).copied()
    }

    /// How many rooms, channels and Space nodes of this set `newer` does
    /// not hold.
    fn missing_from(&self, newer: &Self) -> (usize, usize, usize) {
        fn missing<V>(older: &BTreeMap<String, V>, newer: &BTreeMap<String, V>) -> usize {
            older.keys().filter(|key| !newer.contains_key(*key)).count()
        }
        (
            missing(&self.rooms, &newer.rooms),
            missing(&self.channels, &newer.channels),
            missing(&self.space_nodes, &newer.space_nodes),
        )
    }

    /// Whether no scope is enrolled.
    pub fn is_empty(&self) -> bool {
        self.space_nodes.is_empty() && self.rooms.is_empty()
    }

    /// The canonical addresses of the rooms `lifecycle` manages.
    pub fn scope_rooms(&self, lifecycle: ScopeLifecycleId) -> Vec<String> {
        self.rooms
            .iter()
            .filter(|(_, scope)| **scope == lifecycle)
            .map(|(room, _)| room.clone())
            .collect()
    }

    /// The channel identifiers of the rooms `lifecycle` manages.
    pub fn scope_channels(&self, lifecycle: ScopeLifecycleId) -> Vec<String> {
        self.channels
            .iter()
            .filter(|(_, scope)| **scope == lifecycle)
            .map(|(channel, _)| channel.clone())
            .collect()
    }
}

/// What the replica knows of the enrolled set.
#[derive(Clone, Debug)]
enum EnrolledState {
    /// The first load has not succeeded.
    Pending,
    /// The schema holds no host-grant relation, so no scope is enrolled.
    RelationsAbsent,
    Loaded(Arc<EnrolledSet>),
}

/// Whether an enrolled scope manages an object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enrollment {
    /// No enrolled scope manages it; its legacy checks apply.
    Unmanaged,
    /// An enrolled scope manages it; the canonical grants decide.
    Enrolled,
    /// It cannot be told: the enrolled set has not loaded yet, or the
    /// address reaches an enrolled room in a form that is not canonical.
    /// The caller fails closed.
    Undecided,
}

/// A shared handle on the replica's enrolled set.
///
/// Cloning shares the set. The background refresh stops once every handle
/// is dropped.
#[derive(Clone)]
pub struct EnrolledScopes {
    state: Arc<watch::Sender<EnrolledState>>,
    /// When the load that installed the current set started. Written under
    /// the same lock as the set it describes.
    last_load: Arc<Mutex<Option<LoadStart>>>,
    store: Option<HostGrantStore>,
}

impl Default for EnrolledScopes {
    /// The set of a deployment without a canonical store: every object is
    /// unmanaged.
    fn default() -> Self {
        Self {
            state: Arc::new(watch::Sender::new(EnrolledState::RelationsAbsent)),
            last_load: Arc::default(),
            store: None,
        }
    }
}

impl std::fmt::Debug for EnrolledScopes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match &*self.state.borrow() {
            EnrolledState::Pending => "pending",
            EnrolledState::RelationsAbsent => "relations_absent",
            EnrolledState::Loaded(_) => "loaded",
        };
        f.debug_struct("EnrolledScopes")
            .field("state", &state)
            .finish_non_exhaustive()
    }
}

impl EnrolledScopes {
    /// The enrolled set of `store`, loaded and refreshed by a background
    /// task of the current Tokio runtime. Outside a runtime nothing loads
    /// it, and every object a scope can enroll stays undecided.
    pub fn watch(store: HostGrantStore) -> Self {
        let scopes = Self {
            state: Arc::new(watch::Sender::new(EnrolledState::Pending)),
            last_load: Arc::default(),
            store: Some(store.clone()),
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(refresh_until_dropped(
                store,
                Arc::downgrade(&scopes.state),
                Arc::clone(&scopes.last_load),
                ENROLLED_SET_REFRESH_INTERVAL,
            ));
        }
        scopes
    }

    /// Reload the set from the store now. A failure keeps the set that was
    /// loaded before. Startup and tests use it; the background task calls it
    /// on its own schedule.
    pub async fn refresh(&self) -> Result<(), HostGrantError> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        load_into(store, &self.state, &self.last_load).await
    }

    /// How long ago the load that installed the current set started;
    /// `None` before the first load and on a deployment without a canonical
    /// store.
    pub fn age(&self) -> Option<Duration> {
        self.last_load_start().map(|start| start.instant.elapsed())
    }

    /// When the load that installed the current set started, by the wall
    /// clock; `None` before the first load and on a deployment without a
    /// canonical store. Every scope whose enrollment committed before it is
    /// in the set.
    pub fn load_started(&self) -> Option<SystemTime> {
        self.last_load_start().map(|start| start.wall)
    }

    /// Whether a set that holds the host-grant relations was installed by a
    /// load that started more than `bound` ago. A schema without the
    /// relations, a deployment without a canonical store and a set that has
    /// not loaded are never older than any bound.
    pub fn older_than(&self, bound: Duration) -> bool {
        matches!(*self.state.borrow(), EnrolledState::Loaded(_))
            && self.age().is_some_and(|age| age > bound)
    }

    fn last_load_start(&self) -> Option<LoadStart> {
        self.store.as_ref()?;
        *self
            .last_load
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Wait until the first load has succeeded, for at most `limit`.
    /// `true` once the set is known.
    pub async fn wait_loaded(&self, limit: Duration) -> bool {
        let mut receiver = self.state.subscribe();
        tokio::time::timeout(
            limit,
            receiver.wait_for(|state| !matches!(state, EnrolledState::Pending)),
        )
        .await
        .is_ok_and(|changed| changed.is_ok())
    }

    /// Whether the set is known, without waiting.
    pub fn loaded(&self) -> bool {
        !matches!(*self.state.borrow(), EnrolledState::Pending)
    }

    /// The enrollment of the channel `channel_id`, without waiting.
    pub fn channel(&self, channel_id: &str) -> Enrollment {
        self.answer(|set| set.channel(channel_id).is_some())
    }

    /// The enrollment of the Space node `node`, without waiting.
    pub fn space_node(&self, node: &str) -> Enrollment {
        self.answer(|set| set.space_node(node).is_some())
    }

    /// The enrollment of the room at `room`, without waiting.
    ///
    /// Room bindings are keyed by the canonical address. An address that is
    /// not canonical but canonicalizes to an enrolled room is undecided:
    /// a surface must not serve a managed room under another spelling. One
    /// that canonicalizes to no enrolled room, or not at all, names no
    /// managed room and keeps its legacy checks.
    pub fn room(&self, room: &BareJid) -> Enrollment {
        let canonical = match CanonicalBareJid::new(room.clone()) {
            Ok(canonical) => Ok(canonical),
            Err(_) => CanonicalBareJid::canonicalize(room).map_err(|_| ()),
        };
        let exact = canonical
            .as_ref()
            .is_ok_and(|canonical| canonical.as_str() == room.as_str());
        let enrollment = self.answer(|set| {
            canonical
                .as_ref()
                .is_ok_and(|canonical| set.room(canonical.as_str()).is_some())
        });
        match enrollment {
            Enrollment::Enrolled if !exact => Enrollment::Undecided,
            other => other,
        }
    }

    /// The enrollment of an authorization object: a channel or a Space.
    /// No scope enrolls any other kind of object.
    pub fn object(&self, object: &Object) -> Enrollment {
        match object.object_type {
            ObjectType::Channel => self.channel(&object.id),
            ObjectType::Space => self.space_node(&object.id),
            ObjectType::Server | ObjectType::Message | ObjectType::Dm | ObjectType::Role => {
                Enrollment::Unmanaged
            }
        }
    }

    /// The loaded set itself, or `None` while it is unknown. A schema
    /// without host-grant relations has an empty set.
    pub fn snapshot(&self) -> Option<Arc<EnrolledSet>> {
        match &*self.state.borrow() {
            EnrolledState::Pending => None,
            EnrolledState::RelationsAbsent => Some(Arc::default()),
            EnrolledState::Loaded(set) => Some(Arc::clone(set)),
        }
    }

    /// Like [`Self::channel`], after waiting a bounded time for the first
    /// load.
    pub async fn settled_channel(&self, channel_id: &str) -> Enrollment {
        self.settle().await;
        self.channel(channel_id)
    }

    /// Like [`Self::space_node`], after waiting a bounded time for the first
    /// load.
    pub async fn settled_space_node(&self, node: &str) -> Enrollment {
        self.settle().await;
        self.space_node(node)
    }

    /// Like [`Self::room`], after waiting a bounded time for the first load.
    pub async fn settled_room(&self, room: &BareJid) -> Enrollment {
        self.settle().await;
        self.room(room)
    }

    /// Like [`Self::object`], after waiting a bounded time for the first
    /// load.
    pub async fn settled_object(&self, object: &Object) -> Enrollment {
        self.settle().await;
        self.object(object)
    }

    /// Like [`Self::snapshot`], after waiting a bounded time for the first
    /// load.
    pub async fn settled_snapshot(&self) -> Option<Arc<EnrolledSet>> {
        self.settle().await;
        self.snapshot()
    }

    /// Wait for the first load, for at most [`FIRST_LOAD_READER_WAIT`].
    /// Once the set is known this returns at once.
    async fn settle(&self) {
        if !self.loaded() {
            self.wait_loaded(FIRST_LOAD_READER_WAIT).await;
        }
    }

    fn answer(&self, enrolled: impl FnOnce(&EnrolledSet) -> bool) -> Enrollment {
        match &*self.state.borrow() {
            EnrolledState::Pending => Enrollment::Undecided,
            EnrolledState::RelationsAbsent => Enrollment::Unmanaged,
            EnrolledState::Loaded(set) if enrolled(set) => Enrollment::Enrolled,
            EnrolledState::Loaded(_) => Enrollment::Unmanaged,
        }
    }
}

/// Reload the set into `state`.
///
/// The start of the load is taken before the store is read, so the snapshot
/// the load reads, and the set it installs, holds every scope enrolled
/// before that start. The set and its start are installed together under
/// `last_load`'s lock. A load that finishes after a load that started later
/// has already installed its set installs nothing: the later set holds
/// everything it read.
///
/// A schema that held the host-grant relations and no longer does is a
/// failed reload ([`HostGrantError::RelationsMissing`]): it keeps the set
/// loaded before and that set's start, so the set does not count as fresher
/// than its last successful load. Migrations only add the relations, so
/// their absence after a load is never read as "no scope is enrolled",
/// which would hand every managed room back to the legacy stores.
async fn load_into(
    store: &HostGrantStore,
    state: &watch::Sender<EnrolledState>,
    last_load: &Mutex<Option<LoadStart>>,
) -> Result<(), HostGrantError> {
    let started = LoadStart::now();
    let read = store.enrolled_set().await?;
    let mut last = last_load.lock().unwrap_or_else(PoisonError::into_inner);
    if last.is_some_and(|installed| installed.instant > started.instant) {
        return Ok(());
    }
    let loaded = match read {
        Some(set) => {
            if let EnrolledState::Loaded(previous) = &*state.borrow() {
                let (rooms, channels, space_nodes) = previous.missing_from(&set);
                if rooms + channels + space_nodes > 0 {
                    tracing::warn!(
                        rooms,
                        channels,
                        space_nodes,
                        "A reload of the host-grant enrolled set holds fewer objects than the set \
                         it replaces"
                    );
                }
            }
            EnrolledState::Loaded(Arc::new(set))
        }
        None if matches!(*state.borrow(), EnrolledState::Loaded(_)) => {
            return Err(HostGrantError::RelationsMissing);
        }
        None => EnrolledState::RelationsAbsent,
    };
    state.send_replace(loaded);
    *last = Some(started);
    drop(last);
    record_load_started(started.wall);
    Ok(())
}

/// Export when the load that installed the current set started, as unix
/// seconds. The gauge keeps its value until the next successful load, so
/// its age is readable while refreshes fail or hang.
fn record_load_started(started: SystemTime) {
    let unix_seconds = started
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    waddle_xmpp::gauge_record!(
        "waddle.host_grant.enrolled_set.load_started",
        "s",
        "Unix time at which the load that installed this replica's host-grant enrolled set started.",
        unix_seconds,
    );
}

/// Reload the set every `interval` (every [`FIRST_LOAD_RETRY_INTERVAL`]
/// until the first load succeeds) while any handle on it is alive.
async fn refresh_until_dropped(
    store: HostGrantStore,
    state: Weak<watch::Sender<EnrolledState>>,
    last_load: Arc<Mutex<Option<LoadStart>>>,
    interval: Duration,
) {
    loop {
        let Some(sender) = state.upgrade() else {
            return;
        };
        let outcome = load_into(&store, &sender, &last_load).await;
        let loaded = !matches!(*sender.borrow(), EnrolledState::Pending);
        let holds_scopes = matches!(*sender.borrow(), EnrolledState::Loaded(_));
        drop(sender);
        // Recorded on every tick that completes, failed or not, so that the
        // gauge grows while refreshes fail. A tick that hangs records
        // nothing; `waddle.host_grant.enrolled_set.load_started` covers it.
        let age = last_load
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map(|start| start.instant.elapsed());
        if let Some(age) = age {
            waddle_xmpp::gauge_record!(
                "waddle.host_grant.enrolled_set.age",
                "s",
                "Time since the load that installed this replica's host-grant enrolled set started.",
                age.as_secs(),
            );
            if holds_scopes && age > ENROLLED_SET_STALE_AFTER && STALE.due() {
                tracing::warn!(
                    kind = "enrolled_set_stale",
                    age_seconds = age.as_secs(),
                    "The host-grant enrolled set has not been reloaded within its staleness \
                     bound; a scope enrolled since may still be decided as unmanaged here"
                );
            }
        }
        if let Err(error) = outcome {
            if REFRESH_FAILED.due() {
                tracing::warn!(
                    kind = error.kind(),
                    loaded,
                    "The host-grant enrolled set could not be refreshed; the last loaded set \
                     still applies"
                );
            }
        }
        tokio::time::sleep(if loaded {
            interval
        } else {
            FIRST_LOAD_RETRY_INTERVAL
        })
        .await;
    }
}
