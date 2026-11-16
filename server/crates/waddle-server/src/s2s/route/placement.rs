//! Where a request for a local room waits while the replica that holds the
//! room is found (ADR-0020 D10).
//!
//! In a deployment of several replicas a room's actor lives on one of them,
//! and finding it, and handing a request to it, asks another replica. That
//! replica may be slow or gone. The workers that hand requests to the room
//! host serve every remote domain's rooms in turn, so none of them may wait
//! for another replica: a request is placed here first, off those workers,
//! and reaches them only once it is known to be decided on this replica.
//!
//! Requests for one room wait in one lane, in arrival order, and are placed
//! one at a time by the lane's task, so a room's requests reach its queue,
//! or its holder, in the order they arrived. Lanes are placed concurrently,
//! within a bound for the replica and a smaller one for each remote domain:
//! the lanes of a domain whose rooms are held by a replica that does not
//! answer cannot take the places every other domain's lanes need. What
//! waits here is bounded for the replica, for each room and for each remote
//! domain. A stanza beyond the replica's or its domain's bound holds its
//! stream until it fits, as for the queues behind this stage; one beyond
//! its room's is handed back at once ([`Full::Room`]), so that a room whose
//! lane is full holds back no other room of its domain. A stanza that ends
//! a seat does not count against its room's lane.
//!
//! A room whose holder did not answer in time is remembered for a short
//! while, and requests for it are answered at once meanwhile, so that one
//! silent holder costs each of its rooms one wait and not one for every
//! stanza.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use jid::BareJid;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::room_delivery::{Full, LocalRoomRequest};

/// Rooms remembered as held by a replica that did not answer, before the
/// entries whose time has passed are dropped.
const SILENT_ROOMS: usize = 4096;

/// A request waiting to be placed.
#[derive(Debug)]
pub(super) struct Waiting {
    pub request: LocalRoomRequest,
    /// The remote domain whose share the request counts against.
    pub peer: String,
    /// When the request reached this stage.
    pub arrived: Instant,
    /// Whether the request counts against its room's lane: everything but
    /// a stanza that ends a seat.
    pub in_room: bool,
    /// For a leave or an error that could not be placed before and is
    /// placed again: until when it was first kept. Placing it again does
    /// not extend that.
    pub kept_until: Option<Instant>,
}

/// Bounds on what waits to be placed and on how many lanes are placed at
/// once.
#[derive(Debug, Clone, Copy)]
pub(super) struct PlacementBounds {
    /// Requests waiting, for every room.
    pub waiting: usize,
    /// Requests waiting for one room.
    pub waiting_per_room: usize,
    /// Requests waiting from one remote domain.
    pub waiting_per_peer: usize,
    /// Rooms being placed at once.
    pub placing: usize,
    /// Rooms being placed at once for requests of one remote domain.
    pub placing_per_peer: usize,
}

#[derive(Debug, Default)]
struct Lanes {
    /// The requests waiting for each room, oldest first. A room has an
    /// entry, possibly empty, exactly while a task places its requests.
    rooms: HashMap<BareJid, VecDeque<Waiting>>,
    waiting: usize,
    /// Of those waiting, the requests from each remote domain.
    peers: HashMap<String, usize>,
    /// Rooms whose holder did not answer, and until when requests for them
    /// are answered without asking it again.
    silent: HashMap<BareJid, Instant>,
}

/// What became of a request offered to the stage.
#[derive(Debug)]
pub(super) enum Offered {
    /// It waits behind the room's earlier requests.
    Queued,
    /// It waits, and its room had no lane: the caller starts the lane's
    /// task.
    Started,
    /// A bound refused it.
    Refused(Box<Waiting>, Full),
}

/// What became of a request offered to the stage by a caller that waits
/// while the stage or its domain's share is full.
#[derive(Debug)]
pub(super) enum Laned {
    /// It waits; `started` when its room had no lane and the caller starts
    /// the lane's task.
    Queued { started: bool },
    /// Its room's lane is full: handed back at once.
    RoomFull(Box<Waiting>),
    /// The deadline passed; it was not queued.
    TimedOut,
    /// The router is stopping; it was not queued.
    Stopped,
}

/// The lanes, their bounds and the places for concurrent placements.
pub(super) struct Placements {
    lanes: Mutex<Lanes>,
    /// A place under the waiting bounds freed.
    space: Notify,
    bounds: PlacementBounds,
    placing: Arc<Semaphore>,
    placing_of: Mutex<HashMap<String, Arc<Semaphore>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Nothing here can fail while it holds a lock. A poisoned lock means
    // the counts may no longer match the lanes, and a stage that miscounts
    // would hold back or admit without bound: the process stops instead.
    mutex
        .lock()
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
}

impl Placements {
    pub(super) fn new(bounds: PlacementBounds) -> Self {
        let bounds = PlacementBounds {
            waiting: bounds.waiting.max(1),
            waiting_per_room: bounds.waiting_per_room.max(1),
            waiting_per_peer: bounds.waiting_per_peer.max(1),
            placing: bounds.placing.max(1),
            placing_per_peer: bounds.placing_per_peer.clamp(1, bounds.placing.max(1)),
        };
        Self {
            lanes: Mutex::new(Lanes::default()),
            space: Notify::new(),
            placing: Arc::new(Semaphore::new(bounds.placing)),
            placing_of: Mutex::new(HashMap::new()),
            bounds,
        }
    }

    /// Queue `waiting` behind the earlier requests for `room`, without
    /// waiting.
    pub(super) fn offer(&self, room: &BareJid, waiting: Waiting) -> Offered {
        let mut lanes = lock(&self.lanes);
        let of_peer = lanes.peers.get(&waiting.peer).copied().unwrap_or(0);
        let of_room = lanes.rooms.get(room).map_or(0, |lane| {
            lane.iter().filter(|waiting| waiting.in_room).count()
        });
        if waiting.in_room && of_room >= self.bounds.waiting_per_room {
            return Offered::Refused(Box::new(waiting), Full::Room);
        }
        if lanes.waiting >= self.bounds.waiting || of_peer >= self.bounds.waiting_per_peer {
            return Offered::Refused(Box::new(waiting), Full::Shared);
        }
        lanes.waiting += 1;
        *lanes.peers.entry(waiting.peer.clone()).or_insert(0) += 1;
        match lanes.rooms.get_mut(room) {
            Some(lane) => {
                lane.push_back(waiting);
                Offered::Queued
            }
            None => {
                lanes.rooms.insert(room.clone(), VecDeque::from([waiting]));
                Offered::Started
            }
        }
    }

    /// Queue `waiting` for `room`, waiting for a place while the stage or
    /// the domain's share is full, until `deadline` or until `stop` is
    /// cancelled. A full lane is not waited for.
    pub(super) async fn offer_or_wait(
        &self,
        room: &BareJid,
        mut waiting: Waiting,
        deadline: Instant,
        stop: &CancellationToken,
    ) -> Laned {
        loop {
            // Registered before the attempt, so a place freed between the
            // attempt and the wait is not missed.
            let freed = self.space.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            match self.offer(room, waiting) {
                Offered::Queued => return Laned::Queued { started: false },
                Offered::Started => return Laned::Queued { started: true },
                Offered::Refused(refused, Full::Room) => return Laned::RoomFull(refused),
                Offered::Refused(refused, Full::Shared) => waiting = *refused,
            }
            tokio::select! {
                biased;
                () = stop.cancelled() => return Laned::Stopped,
                () = tokio::time::sleep_until(deadline) => return Laned::TimedOut,
                () = &mut freed => {}
            }
        }
    }

    fn released(lanes: &mut Lanes, waiting: &Waiting) {
        lanes.waiting = lanes.waiting.saturating_sub(1);
        if let Some(count) = lanes.peers.get_mut(&waiting.peer) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                lanes.peers.remove(&waiting.peer);
            }
        }
    }

    /// The next request of `room`'s lane. `None` once the lane is empty:
    /// the lane is closed with that answer, and the task that asked ends.
    pub(super) fn next(&self, room: &BareJid) -> Option<Waiting> {
        let mut lanes = lock(&self.lanes);
        let next = lanes.rooms.get_mut(room).and_then(VecDeque::pop_front);
        match &next {
            Some(waiting) => Self::released(&mut lanes, waiting),
            None => {
                lanes.rooms.remove(room);
            }
        }
        drop(lanes);
        if next.is_some() {
            self.space.notify_waiters();
        }
        next
    }

    /// Everything that waits for `room` now, oldest first. The lane stays
    /// open for what arrives later.
    pub(super) fn drain(&self, room: &BareJid) -> Vec<Waiting> {
        let mut lanes = lock(&self.lanes);
        let drained: Vec<Waiting> = lanes
            .rooms
            .get_mut(room)
            .map(|lane| lane.drain(..).collect())
            .unwrap_or_default();
        for waiting in &drained {
            Self::released(&mut lanes, waiting);
        }
        drop(lanes);
        if !drained.is_empty() {
            self.space.notify_waiters();
        }
        drained
    }

    /// Drop from `room`'s lane the requests `withdrawn` selects; how many.
    /// The lane stays open.
    pub(super) fn withdraw(&self, room: &BareJid, withdrawn: impl Fn(&Waiting) -> bool) -> usize {
        let mut lanes = lock(&self.lanes);
        let mut taken = Vec::new();
        if let Some(lane) = lanes.rooms.get_mut(room) {
            let mut kept = VecDeque::with_capacity(lane.len());
            for waiting in lane.drain(..) {
                if withdrawn(&waiting) {
                    taken.push(waiting);
                } else {
                    kept.push_back(waiting);
                }
            }
            *lane = kept;
        }
        for waiting in &taken {
            Self::released(&mut lanes, waiting);
        }
        drop(lanes);
        if !taken.is_empty() {
            self.space.notify_waiters();
        }
        taken.len()
    }

    /// Remember that `room`'s holder did not answer: until `until`,
    /// [`Self::silent`] says so.
    pub(super) fn mark_silent(&self, room: &BareJid, until: Instant) {
        let mut lanes = lock(&self.lanes);
        if lanes.silent.len() >= SILENT_ROOMS {
            let now = Instant::now();
            lanes.silent.retain(|_, until| *until > now);
        }
        if lanes.silent.len() < SILENT_ROOMS {
            lanes.silent.insert(room.clone(), until);
        }
    }

    /// Whether `room`'s holder did not answer a short while ago.
    pub(super) fn silent(&self, room: &BareJid) -> bool {
        let mut lanes = lock(&self.lanes);
        match lanes.silent.get(room) {
            Some(until) if *until > Instant::now() => true,
            Some(_) => {
                lanes.silent.remove(room);
                false
            }
            None => false,
        }
    }

    /// A place among the rooms being placed, for a request of `peer`: one
    /// of the domain's places first, then one of the replica's, so a
    /// domain that waits for its own places holds none of the others'.
    /// `None` when the stage is closed.
    pub(super) async fn place(
        &self,
        peer: &str,
    ) -> Option<(OwnedSemaphorePermit, OwnedSemaphorePermit)> {
        let of_peer = {
            let mut placing_of = lock(&self.placing_of);
            if placing_of.len() >= SILENT_ROOMS && !placing_of.contains_key(peer) {
                // A domain none of whose places is taken or waited for
                // needs no entry.
                let all = self.bounds.placing_per_peer;
                placing_of.retain(|_, places| {
                    Arc::strong_count(places) > 1 || places.available_permits() < all
                });
            }
            Arc::clone(
                placing_of
                    .entry(peer.to_owned())
                    .or_insert_with(|| Arc::new(Semaphore::new(self.bounds.placing_per_peer))),
            )
        };
        let of_peer = of_peer.acquire_owned().await.ok()?;
        let of_all = Arc::clone(&self.placing).acquire_owned().await.ok()?;
        Some((of_peer, of_all))
    }

    /// How many requests wait to be placed.
    pub(super) fn waiting(&self) -> usize {
        lock(&self.lanes).waiting
    }
}
