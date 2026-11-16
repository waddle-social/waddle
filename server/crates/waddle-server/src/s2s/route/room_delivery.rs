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
//! A room's queue and each shard are bounded; a stanza beyond either bound
//! is dropped and counted. Work that ends an occupancy because a stream
//! failed is exempt from both bounds: there is at most one such item per
//! occupancy, and the occupant must be told.

use std::collections::{HashMap, VecDeque};
use std::hash::BuildHasher;
use std::sync::Mutex;

use jid::{BareJid, FullJid, Jid};
use tokio::sync::Notify;
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

#[derive(Debug, Default)]
struct Shard {
    rooms: HashMap<BareJid, VecDeque<(RoomWork, bool)>>,
    /// Rooms with queued work, in the order they take their turn.
    turns: VecDeque<BareJid>,
    /// Queued work that counts against the shard's bound.
    bounded: usize,
}

impl Shard {
    fn push(&mut self, room: BareJid, work: RoomWork, exempt: bool, bounds: &Bounds) -> bool {
        let queued = self.rooms.get(&room).map_or(0, VecDeque::len);
        if !exempt && (self.bounded >= bounds.per_shard || queued >= bounds.per_room) {
            return false;
        }
        if !exempt {
            self.bounded += 1;
        }
        if queued == 0 {
            self.turns.push_back(room.clone());
        }
        self.rooms
            .entry(room)
            .or_default()
            .push_back((work, exempt));
        true
    }

    fn pop(&mut self) -> Option<RoomWork> {
        let room = self.turns.pop_front()?;
        let queue = self.rooms.get_mut(&room)?;
        let (work, exempt) = queue.pop_front()?;
        if queue.is_empty() {
            self.rooms.remove(&room);
        } else {
            self.turns.push_back(room);
        }
        if !exempt {
            self.bounded = self.bounded.saturating_sub(1);
        }
        Some(work)
    }
}

#[derive(Debug, Clone, Copy)]
struct Bounds {
    per_shard: usize,
    per_room: usize,
}

/// The shards and their bounds.
pub(super) struct RoomQueues {
    shards: Vec<(Mutex<Shard>, Notify)>,
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

impl RoomQueues {
    /// `workers` shards sharing `total` queued stanzas, at most `per_room`
    /// of them for one room.
    pub(super) fn new(workers: usize, total: usize, per_room: usize) -> Self {
        let workers = workers.max(1);
        Self {
            shards: (0..workers)
                .map(|_| (Mutex::new(Shard::default()), Notify::new()))
                .collect(),
            hasher: std::collections::hash_map::RandomState::new(),
            bounds: Bounds {
                per_shard: (total / workers).max(1),
                per_room: per_room.max(1),
            },
        }
    }

    /// How many shards, one worker each.
    pub(super) fn shards(&self) -> usize {
        self.shards.len()
    }

    fn shard_of(&self, room: &BareJid) -> usize {
        let hash = self.hasher.hash_one(room.as_str());
        usize::try_from(hash % self.shards.len() as u64).unwrap_or(0)
    }

    /// Queue `work` for `room`; `false` when a bound refused it. `exempt`
    /// work is never refused.
    pub(super) fn push(&self, room: BareJid, work: RoomWork, exempt: bool) -> bool {
        let Some((shard, notify)) = self.shards.get(self.shard_of(&room)) else {
            return false;
        };
        let pushed = lock(shard).push(room, work, exempt, &self.bounds);
        if pushed {
            notify.notify_one();
        }
        pushed
    }

    /// The next work of shard `index`, waiting for some.
    pub(super) async fn next(&self, index: usize) -> RoomWork {
        let Some((shard, notify)) = self.shards.get(index) else {
            return std::future::pending().await;
        };
        loop {
            if let Some(work) = lock(shard).pop() {
                return work;
            }
            notify.notified().await;
        }
    }
}
