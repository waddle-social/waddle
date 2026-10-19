//! Bounded round-robin admission. Wake hints grant a finite burst of claims;
//! an empty room cannot perpetually reschedule itself.
use std::collections::{HashMap, VecDeque};

use jid::BareJid;

const MAX_QUEUED_ROOMS: usize = 512;

pub(super) struct Scheduler {
    pending: VecDeque<BareJid>,
    demand: HashMap<BareJid, usize>,
    active: HashMap<BareJid, usize>,
    per_room: usize,
}

impl Scheduler {
    pub(super) fn new(installation_limit: u32) -> Self {
        Self {
            pending: VecDeque::new(),
            demand: HashMap::new(),
            active: HashMap::new(),
            per_room: (installation_limit as usize).clamp(1, 4),
        }
    }

    pub(super) fn wake(&mut self, room: BareJid) {
        self.request(room, self.per_room);
    }

    fn request(&mut self, room: BareJid, count: usize) {
        if let Some(demand) = self.demand.get_mut(&room) {
            *demand = (*demand).max(count).min(self.per_room);
        } else if self.demand.len() < MAX_QUEUED_ROOMS {
            self.demand.insert(room.clone(), count);
            self.pending.push_back(room);
        }
    }

    pub(super) fn next(&mut self) -> Option<BareJid> {
        // Examine each queued room once. Rooms at their limit keep their place
        // for the next completion without spinning or blocking another room.
        for _ in 0..self.pending.len() {
            let room = self.pending.pop_front()?;
            if self.active.get(&room).copied().unwrap_or_default() >= self.per_room {
                self.pending.push_back(room);
                continue;
            }
            let demand = self.demand.get_mut(&room)?;
            *demand -= 1;
            if *demand == 0 {
                self.demand.remove(&room);
            } else {
                self.pending.push_back(room.clone());
            }
            *self.active.entry(room.clone()).or_default() += 1;
            return Some(room);
        }
        None
    }

    pub(super) fn completed(&mut self, room: BareJid, more_work: bool) {
        if let Some(count) = self.active.get_mut(&room) {
            *count -= 1;
            if *count == 0 {
                self.active.remove(&room);
            }
        }
        if more_work {
            self.request(room, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(name: &str) -> BareJid {
        format!("{name}@rooms.example.test").parse().unwrap()
    }

    #[test]
    fn hot_room_uses_four_slots_without_starving_other_rooms() {
        let mut scheduler = Scheduler::new(8);
        let hot = room("hot");
        let other = room("other");
        scheduler.wake(hot.clone());
        assert_eq!(scheduler.next(), Some(hot.clone()));
        scheduler.wake(other.clone());
        assert_eq!(scheduler.next(), Some(hot.clone()));
        assert_eq!(scheduler.next(), Some(other.clone()));
        assert_eq!(scheduler.next(), Some(hot.clone()));
        assert_eq!(scheduler.next(), Some(other.clone()));
        assert_eq!(scheduler.next(), Some(hot.clone()));
        scheduler.wake(hot.clone());
        assert_eq!(scheduler.next(), Some(other.clone()));
        assert_eq!(scheduler.next(), Some(other));
        assert_eq!(scheduler.next(), None);
        scheduler.completed(hot.clone(), true);
        assert_eq!(scheduler.next(), Some(hot));
        assert_eq!(scheduler.next(), None);
    }

    #[test]
    fn idle_room_stops_after_a_bounded_burst() {
        let mut scheduler = Scheduler::new(8);
        let room = room("empty");
        scheduler.wake(room.clone());
        for _ in 0..4 {
            assert_eq!(scheduler.next(), Some(room.clone()));
            scheduler.completed(room.clone(), false);
        }
        assert_eq!(scheduler.next(), None);
        assert!(scheduler.active.is_empty());
        assert!(scheduler.demand.is_empty());
    }

    #[test]
    fn installation_limit_and_wake_capacity_are_respected() {
        let mut scheduler = Scheduler::new(1);
        let room = room("one");
        scheduler.wake(room.clone());
        assert_eq!(scheduler.next(), Some(room.clone()));
        scheduler.wake(room.clone());
        assert_eq!(scheduler.next(), None);
        scheduler.completed(room.clone(), false);
        assert_eq!(scheduler.next(), Some(room));
        for index in 0..MAX_QUEUED_ROOMS + 1 {
            scheduler.wake(format!("room{index}@rooms.example.test").parse().unwrap());
        }
        assert_eq!(scheduler.demand.len(), MAX_QUEUED_ROOMS);
    }
}
