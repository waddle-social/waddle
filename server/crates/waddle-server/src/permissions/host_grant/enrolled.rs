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
//!   set it loaded;
//! - until the first load succeeds nothing is known, and every object of a
//!   kind a scope can enroll (a channel, a room address, a Space node) is
//!   undecided and fails closed. Startup waits for that first load before
//!   the replica serves. A schema without the host-grant relations holds no
//!   scope, and every object is unmanaged there.
//!
//! Enrollment only ever adds to the set: a room binding is immutable and a
//! retired scope's objects stay enrolled. A scope enrolled on any replica is
//! therefore managed on each replica within one refresh interval, and the
//! owner-phase enrollment procedure waits at least that long before the
//! scope is relied on (see the "Writer fence and sink contracts" section of
//! `server/docs/planning/host-grant-authority-2047.md`).

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use jid::BareJid;
use tokio::sync::watch;
use waddle_xmpp::identity::CanonicalBareJid;

use super::error::HostGrantError;
use super::model::ScopeLifecycleId;
use super::rate_limited::RateLimitedWarning;
use super::store::HostGrantStore;
use crate::permissions::{Object, ObjectType};

/// How often a replica reloads the enrolled set once it has loaded it.
pub const ENROLLED_SET_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// How often the first load is retried until it succeeds.
const FIRST_LOAD_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// How long a reader waits for the first load before it answers
/// [`Enrollment::Undecided`].
const FIRST_LOAD_READER_WAIT: Duration = Duration::from_secs(2);

/// At most one warning a minute that a refresh failed.
static REFRESH_FAILED: RateLimitedWarning = RateLimitedWarning::new();

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
    store: Option<HostGrantStore>,
}

impl Default for EnrolledScopes {
    /// The set of a deployment without a canonical store: every object is
    /// unmanaged.
    fn default() -> Self {
        Self {
            state: Arc::new(watch::Sender::new(EnrolledState::RelationsAbsent)),
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
            store: Some(store.clone()),
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(refresh_until_dropped(
                store,
                Arc::downgrade(&scopes.state),
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
        load_into(store, &self.state).await
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

async fn load_into(
    store: &HostGrantStore,
    state: &watch::Sender<EnrolledState>,
) -> Result<(), HostGrantError> {
    let loaded = match store.enrolled_set().await? {
        Some(set) => EnrolledState::Loaded(Arc::new(set)),
        None => EnrolledState::RelationsAbsent,
    };
    state.send_replace(loaded);
    Ok(())
}

/// Reload the set every `interval` (every [`FIRST_LOAD_RETRY_INTERVAL`]
/// until the first load succeeds) while any handle on it is alive.
async fn refresh_until_dropped(
    store: HostGrantStore,
    state: Weak<watch::Sender<EnrolledState>>,
    interval: Duration,
) {
    loop {
        let Some(sender) = state.upgrade() else {
            return;
        };
        let outcome = load_into(&store, &sender).await;
        let loaded = !matches!(*sender.borrow(), EnrolledState::Pending);
        drop(sender);
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
