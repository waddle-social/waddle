//! The bounded queues between inbound streams and the occupants of remote
//! rooms.
//!
//! A stanza from a remote JID to a local full JID may be room traffic, and
//! deciding that needs the shared occupancy table, which a stream's task
//! must not wait for. Such stanzas are queued here and decided, applied and
//! delivered by a few workers. Each room bare JID always lands on the same
//! shard and is served in arrival order, so an occupant sees a room's
//! presence and messages in the room's order, and the room's own presence
//! moves the occupancy on before the room's next stanza is decided. Within
//! a shard the rooms with queued work take turns, one stanza each, so one
//! busy room does not hold back the others.
//!
//! A room's queue, each shard, and each remote domain's share of a shard
//! are bounded. A stanza beyond a bound is not dropped: the stream that
//! carried it waits, reading nothing more from its peer, until the stanza
//! fits ([`RoomQueues::push_or_wait`]), up to a deadline for each stanza,
//! so a room's burst, such as the
//! presence of every occupant before the joining occupant's own (XEP-0045
//! §7.2.3), arrives in full and in order. One remote domain cannot hold
//! more than its share of a shard, so the rooms of other domains on that
//! shard keep moving. Work that ends an occupancy because a stream failed
//! is exempt from every bound and counts towards none: there is at most one
//! such item per occupancy, and the occupant must be told.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::BuildHasher;
use std::sync::Mutex;

use jid::{BareJid, FullJid, Jid};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::Stanza;

use super::occupancy::Occupancy;
use crate::s2s::conn::VerifiedPeerStream;

/// What a worker does for one room.
#[derive(Debug)]
pub(super) enum RoomWork {
    /// A stanza from `from` for the local full JID `occupant`, which
    /// arrived on `stream`: delivered if it is the traffic of a room the
    /// occupant is in, refused otherwise.
    Traffic(Box<InboundTraffic>),
    /// An occupancy whose stream to the room's domain failed: ended, if it
    /// is still as read, and the occupant told (XEP-0045 §11.1).
    Ended(Box<Occupancy>),
}

/// A stanza that may be a room's traffic, as it arrived.
#[derive(Debug)]
pub(super) struct InboundTraffic {
    pub stream: VerifiedPeerStream,
    pub from: Jid,
    pub occupant: FullJid,
    pub stanza: Stanza,
}

/// Identifies the end of one occupancy, so that it is queued once however
/// many failed streams carried it.
type EndKey = (FullJid, BareJid, String);

fn end_key(occupancy: &Occupancy) -> EndKey {
    (
        occupancy.occupant.clone(),
        occupancy.room.clone(),
        occupancy.attempt.clone(),
    )
}

/// One queued item: the work, and the remote domain whose share it counts
/// against, or `None` for exempt work.
#[derive(Debug)]
struct Item {
    work: RoomWork,
    peer: Option<String>,
}

#[derive(Debug, Default)]
struct RoomQueue {
    items: VecDeque<Item>,
    /// Queued items that count against the room's bound.
    bounded: usize,
}

#[derive(Debug, Default)]
struct Shard {
    rooms: HashMap<BareJid, RoomQueue>,
    /// Rooms with queued work, in the order they take their turn.
    turns: VecDeque<BareJid>,
    /// Queued work that counts against the shard's bound.
    bounded: usize,
    /// Of that, the work from each remote domain.
    peers: HashMap<String, usize>,
    /// The occupancy ends queued.
    ended: HashSet<EndKey>,
}

impl Shard {
    /// Queue `work` for `room`, counted against `peer`'s share (exempt when
    /// `None`), or give it back when a bound refuses it.
    fn push(
        &mut self,
        room: BareJid,
        work: RoomWork,
        peer: Option<String>,
        bounds: &Bounds,
    ) -> Result<(), RoomWork> {
        match &peer {
            Some(peer) => {
                let in_room = self.rooms.get(&room).map_or(0, |queue| queue.bounded);
                let of_peer = self.peers.get(peer).copied().unwrap_or(0);
                if self.bounded >= bounds.per_shard
                    || in_room >= bounds.per_room
                    || of_peer >= bounds.per_peer
                {
                    return Err(work);
                }
                self.bounded += 1;
                *self.peers.entry(peer.clone()).or_insert(0) += 1;
            }
            None => {
                // The same occupancy, ended by a second failed stream.
                if let RoomWork::Ended(occupancy) = &work {
                    if !self.ended.insert(end_key(occupancy)) {
                        return Ok(());
                    }
                }
            }
        }
        let queue = self.rooms.entry(room.clone()).or_default();
        if queue.items.is_empty() {
            self.turns.push_back(room);
        }
        if peer.is_some() {
            queue.bounded += 1;
        }
        queue.items.push_back(Item { work, peer });
        Ok(())
    }

    /// The next item, in the rooms' turns: the work and whether it freed a
    /// place under the bounds.
    fn pop(&mut self) -> Option<(RoomWork, bool)> {
        let room = self.turns.pop_front()?;
        let queue = self.rooms.get_mut(&room)?;
        let Item { work, peer } = queue.items.pop_front()?;
        if peer.is_some() {
            queue.bounded = queue.bounded.saturating_sub(1);
        }
        if queue.items.is_empty() {
            self.rooms.remove(&room);
        } else {
            self.turns.push_back(room);
        }
        match &peer {
            Some(peer) => {
                self.bounded = self.bounded.saturating_sub(1);
                if let Some(count) = self.peers.get_mut(peer) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        self.peers.remove(peer);
                    }
                }
            }
            None => {
                if let RoomWork::Ended(occupancy) = &work {
                    self.ended.remove(&end_key(occupancy));
                }
            }
        }
        Some((work, peer.is_some()))
    }
}

#[derive(Debug, Clone, Copy)]
struct Bounds {
    per_shard: usize,
    per_room: usize,
    per_peer: usize,
}

/// One shard: its queues, the signal that work arrived, and the signal
/// that a place under the bounds freed.
struct ShardSlot {
    shard: Mutex<Shard>,
    work: Notify,
    space: Notify,
}

/// The shards and their bounds.
pub(super) struct RoomQueues {
    shards: Vec<ShardSlot>,
    hasher: std::collections::hash_map::RandomState,
    bounds: Bounds,
}

fn lock(shard: &Mutex<Shard>) -> std::sync::MutexGuard<'_, Shard> {
    // Every update leaves the shard consistent, so a poisoned lock still
    // holds usable state.
    match shard.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Whether a stanza that waited for a place found one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Placed {
    Queued,
    /// The deadline passed; the stanza was not queued.
    TimedOut,
    /// The router is stopping; the stanza was not queued.
    Stopped,
}

impl RoomQueues {
    /// `workers` shards sharing `total` queued stanzas, at most `per_room`
    /// of them for one room and `per_peer` of a shard's for one remote
    /// domain.
    pub(super) fn new(workers: usize, total: usize, per_room: usize, per_peer: usize) -> Self {
        let workers = workers.max(1);
        let per_shard = (total / workers).max(1);
        Self {
            shards: (0..workers)
                .map(|_| ShardSlot {
                    shard: Mutex::new(Shard::default()),
                    work: Notify::new(),
                    space: Notify::new(),
                })
                .collect(),
            hasher: std::collections::hash_map::RandomState::new(),
            bounds: Bounds {
                per_shard,
                per_room: per_room.max(1),
                per_peer: per_peer.clamp(1, per_shard),
            },
        }
    }

    /// How many shards, one worker each.
    pub(super) fn shards(&self) -> usize {
        self.shards.len()
    }

    fn slot(&self, room: &BareJid) -> Option<&ShardSlot> {
        let hash = self.hasher.hash_one(room.as_str());
        let index = usize::try_from(hash % self.shards.len() as u64).unwrap_or(0);
        self.shards.get(index)
    }

    /// Queue `work` for `room` against `peer`'s share, or give it back when
    /// a bound refuses it.
    pub(super) fn try_push(
        &self,
        room: BareJid,
        work: RoomWork,
        peer: &str,
    ) -> Result<(), RoomWork> {
        let Some(slot) = self.slot(&room) else {
            return Err(work);
        };
        lock(&slot.shard).push(room, work, Some(peer.to_owned()), &self.bounds)?;
        slot.work.notify_one();
        Ok(())
    }

    /// Queue `work` for `room` against `peer`'s share, waiting for a place
    /// until `deadline` or until `stop` is cancelled.
    pub(super) async fn push_or_wait(
        &self,
        room: BareJid,
        mut work: RoomWork,
        peer: &str,
        deadline: tokio::time::Instant,
        stop: &CancellationToken,
    ) -> Placed {
        let Some(slot) = self.slot(&room) else {
            return Placed::TimedOut;
        };
        loop {
            // Registered before the attempt, so a place freed between the
            // attempt and the wait is not missed.
            let freed = slot.space.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            let pushed =
                lock(&slot.shard).push(room.clone(), work, Some(peer.to_owned()), &self.bounds);
            match pushed {
                Ok(()) => {
                    slot.work.notify_one();
                    return Placed::Queued;
                }
                Err(refused) => work = refused,
            }
            tokio::select! {
                biased;
                () = stop.cancelled() => return Placed::Stopped,
                () = tokio::time::sleep_until(deadline) => return Placed::TimedOut,
                () = &mut freed => {}
            }
        }
    }

    /// Queue `work`, exempt from every bound. The end of an occupancy
    /// already queued is not queued again.
    pub(super) fn push_exempt(&self, room: BareJid, work: RoomWork) {
        let Some(slot) = self.slot(&room) else {
            return;
        };
        if lock(&slot.shard)
            .push(room, work, None, &self.bounds)
            .is_ok()
        {
            slot.work.notify_one();
        }
    }

    /// The next work of shard `index`, waiting for some.
    pub(super) async fn next(&self, index: usize) -> RoomWork {
        let Some(slot) = self.shards.get(index) else {
            return std::future::pending().await;
        };
        loop {
            let popped = lock(&slot.shard).pop();
            if let Some((work, freed)) = popped {
                if freed {
                    slot.space.notify_waiters();
                }
                return work;
            }
            slot.work.notified().await;
        }
    }
}
