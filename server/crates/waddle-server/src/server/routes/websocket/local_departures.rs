//! Retained local responsibility for MUC departures that could not be projected.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use jid::{BareJid, FullJid};
use kameo::actor::ActorId;
use tracing::warn;
use waddle_xmpp::muc::{durable::OccupancyLeaveCause, room_actor::LeaveSessionSelector};

const BACKOFF_BASE: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const STUCK_ATTEMPTS: u32 = 10;
const MAX_FULL_JID_SWEEPS: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalDepartureItem {
    FullJidSweep {
        jid: FullJid,
    },
    RoomDeparture {
        room: BareJid,
        jid: FullJid,
        cause: OccupancyLeaveCause,
        selector: LeaveSessionSelector,
    },
    ConfirmRetired {
        room: BareJid,
        jid: FullJid,
        actor: ActorId,
        cause: OccupancyLeaveCause,
        selector: LeaveSessionSelector,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum LocalDepartureKey {
    FullJidSweep(FullJid),
    RoomScoped(BareJid, FullJid, u8),
}

impl LocalDepartureItem {
    fn key(&self) -> LocalDepartureKey {
        match self {
            Self::FullJidSweep { jid } => LocalDepartureKey::FullJidSweep(jid.clone()),
            Self::RoomDeparture {
                room, jid, cause, ..
            }
            | Self::ConfirmRetired {
                room, jid, cause, ..
            } => LocalDepartureKey::RoomScoped(room.clone(), jid.clone(), cause_key(*cause)),
        }
    }

    fn merge_with_existing(self, existing: &PendingLocalDeparture) -> Self {
        let merged_selector = merge_selectors(existing.item.selector(), self.selector());
        match (existing.item.clone(), self) {
            (
                LocalDepartureItem::RoomDeparture {
                    room, jid, cause, ..
                },
                LocalDepartureItem::ConfirmRetired { actor, .. },
            ) => LocalDepartureItem::ConfirmRetired {
                room,
                jid,
                actor,
                cause,
                selector: merged_selector.unwrap_or(LeaveSessionSelector::Any),
            },
            (
                LocalDepartureItem::RoomDeparture {
                    room, jid, cause, ..
                },
                _,
            ) => LocalDepartureItem::RoomDeparture {
                room,
                jid,
                cause,
                selector: merged_selector.unwrap_or(LeaveSessionSelector::Any),
            },
            (
                LocalDepartureItem::ConfirmRetired {
                    room,
                    jid,
                    actor,
                    cause,
                    ..
                },
                _,
            ) => LocalDepartureItem::ConfirmRetired {
                room,
                jid,
                actor,
                cause,
                selector: merged_selector.unwrap_or(LeaveSessionSelector::Any),
            },
            (existing, _) => existing,
        }
    }

    fn selector(&self) -> Option<LeaveSessionSelector> {
        match self {
            Self::FullJidSweep { .. } => None,
            Self::RoomDeparture { selector, .. } | Self::ConfirmRetired { selector, .. } => {
                Some(*selector)
            }
        }
    }
}

/// A re-recorded departure widens responsibility: `Any` dominates, otherwise
/// the NEWEST watermark wins (a later disconnect of a re-joined session must
/// not be judged `Superseded` by an older attempt's watermark).
fn merge_selectors(
    existing: Option<LeaveSessionSelector>,
    incoming: Option<LeaveSessionSelector>,
) -> Option<LeaveSessionSelector> {
    match (existing, incoming) {
        (None, other) | (other, None) => other,
        (Some(LeaveSessionSelector::Any), _) | (_, Some(LeaveSessionSelector::Any)) => {
            Some(LeaveSessionSelector::Any)
        }
        (
            Some(LeaveSessionSelector::JoinedAtOrBefore(left)),
            Some(LeaveSessionSelector::JoinedAtOrBefore(right)),
        ) => Some(LeaveSessionSelector::JoinedAtOrBefore(left.max(right))),
    }
}

const fn cause_key(cause: OccupancyLeaveCause) -> u8 {
    match cause {
        OccupancyLeaveCause::Explicit => 0,
        OccupancyLeaveCause::Disconnect => 1,
        OccupancyLeaveCause::Administrative => 2,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLocalDeparture {
    pub item: LocalDepartureItem,
    pub attempts: u32,
    pub not_before: Instant,
}

#[derive(Debug)]
pub struct PendingLocalMucDepartures {
    entries: Mutex<HashMap<LocalDepartureKey, PendingLocalDeparture>>,
    /// Cap on retained `FullJidSweep` items: sweeps are minted on room
    /// enumeration failure for every disconnecting JID (false positives
    /// included), so under a prolonged registry outage they grow with
    /// connection churn. Room-scoped items are bounded by observed local
    /// occupancies and carry no cap.
    full_jid_sweep_cap: usize,
}

impl Default for PendingLocalMucDepartures {
    fn default() -> Self {
        Self::with_full_jid_sweep_cap(MAX_FULL_JID_SWEEPS)
    }
}

impl PendingLocalMucDepartures {
    pub fn record(&self, item: LocalDepartureItem) {
        self.record_at(item, Instant::now());
    }

    pub(crate) fn with_full_jid_sweep_cap(full_jid_sweep_cap: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            full_jid_sweep_cap,
        }
    }

    fn record_at(&self, item: LocalDepartureItem, now: Instant) {
        let key = item.key();
        let mut entries = self.entries.lock().expect("local departure inventory lock");
        if let Some(existing) = entries.get_mut(&key) {
            existing.item = item.merge_with_existing(existing);
            record_pending_gauges(&entries);
            return;
        }
        if matches!(item, LocalDepartureItem::FullJidSweep { .. }) {
            evict_oldest_sweep_if_at_cap(&mut entries, self.full_jid_sweep_cap);
        }
        entries.insert(
            key,
            PendingLocalDeparture {
                item,
                attempts: 0,
                not_before: now,
            },
        );
        record_pending_gauges(&entries);
    }

    pub fn take_due(&self, now: Instant) -> Vec<PendingLocalDeparture> {
        let mut entries = self.entries.lock().expect("local departure inventory lock");
        let mut keys = entries
            .iter()
            .filter(|(_, entry)| entry.not_before <= now)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.sort_unstable();
        let mut due = keys
            .into_iter()
            .filter_map(|key| entries.remove(&key))
            .collect::<Vec<_>>();
        due.sort_by(|left, right| {
            left.not_before
                .cmp(&right.not_before)
                .then_with(|| left.item.key().cmp(&right.item.key()))
        });
        record_pending_gauges(&entries);
        due
    }

    pub fn requeue_with_backoff(&self, mut entry: PendingLocalDeparture) {
        entry.attempts = entry.attempts.saturating_add(1);
        entry.not_before = Instant::now() + backoff(entry.attempts);
        if entry.attempts > STUCK_ATTEMPTS {
            warn!(?entry.item, attempts = entry.attempts, "local MUC departure remains pending");
            crate::metrics::record_local_departure_retry("stuck");
        }
        let mut entries = self.entries.lock().expect("local departure inventory lock");
        entries.insert(entry.item.key(), entry);
        record_pending_gauges(&entries);
    }

    #[cfg(test)]
    #[cfg(all(test, feature = "clustering"))]
    pub(crate) fn record_pending_for_test(&self, entry: PendingLocalDeparture) {
        let mut entries = self.entries.lock().expect("local departure inventory lock");
        entries.insert(entry.item.key(), entry);
        record_pending_gauges(&entries);
    }

    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("local departure inventory lock")
            .len()
    }
}

fn backoff(attempts: u32) -> Duration {
    let multiplier = 1_u64 << attempts.saturating_sub(1).min(31);
    Duration::from_secs(
        BACKOFF_BASE
            .as_secs()
            .saturating_mul(multiplier)
            .min(BACKOFF_MAX.as_secs()),
    )
}

/// Drop the oldest retained sweep once the cap is reached. Only the overflow
/// path pays the linear scan; ordinary inserts stay O(1).
fn evict_oldest_sweep_if_at_cap(
    entries: &mut HashMap<LocalDepartureKey, PendingLocalDeparture>,
    cap: usize,
) {
    if entries.len() < cap {
        return;
    }
    let sweeps = entries
        .iter()
        .filter(|(key, _)| matches!(key, LocalDepartureKey::FullJidSweep(_)));
    if sweeps.clone().count() < cap {
        return;
    }
    let Some(oldest) = sweeps
        .min_by(|(left_key, left), (right_key, right)| {
            left.not_before
                .cmp(&right.not_before)
                .then_with(|| left_key.cmp(right_key))
        })
        .map(|(key, _)| key.clone())
    else {
        return;
    };
    entries.remove(&oldest);
    warn!("local MUC departure sweep inventory overflow; dropped oldest sweep");
    crate::metrics::record_local_departure_retry("overflow");
}

fn record_pending_gauges(entries: &HashMap<LocalDepartureKey, PendingLocalDeparture>) {
    let mut counts = [0_i64; 3];
    for entry in entries.values() {
        let slot = match entry.item {
            LocalDepartureItem::FullJidSweep { .. } => 0,
            LocalDepartureItem::RoomDeparture { .. } => 1,
            LocalDepartureItem::ConfirmRetired { .. } => 2,
        };
        counts[slot] += 1;
    }
    for (kind, value) in ["full_jid_sweep", "room_departure", "confirm_retired"]
        .into_iter()
        .zip(counts)
    {
        crate::metrics::record_local_departure_pending(kind, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::muc::room_actor::OccupancyWatermark;

    fn jid(value: &str) -> FullJid {
        value.parse().expect("jid")
    }
    fn room(value: &str) -> BareJid {
        format!("{value}@muc.example.com").parse().expect("room")
    }

    #[test]
    fn record_merges_by_key_keeping_older_selector_and_attempts() {
        let inventory = PendingLocalMucDepartures::default();
        let now = Instant::now();
        let room = room("group");
        let jid = jid("alice@example.com/web");
        inventory.record_at(
            LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: jid.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
            },
            now,
        );
        let first = inventory.take_due(now).pop().expect("first");
        inventory.requeue_with_backoff(first);
        inventory.record(LocalDepartureItem::ConfirmRetired {
            room,
            jid,
            actor: ActorId::new(7_u64),
            cause: OccupancyLeaveCause::Disconnect,
            selector: LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(9)),
        });
        let entry = inventory
            .entries
            .lock()
            .expect("lock")
            .values()
            .next()
            .cloned()
            .expect("entry");
        assert_eq!(entry.attempts, 1);
        assert!(matches!(
            entry.item,
            LocalDepartureItem::ConfirmRetired {
                selector: LeaveSessionSelector::Any,
                ..
            }
        ));
    }

    #[test]
    fn record_merge_keeps_newest_watermark_and_any_dominates() {
        let inventory = PendingLocalMucDepartures::default();
        let now = Instant::now();
        let room = room("merge");
        let jid = jid("alice@example.com/web");
        let departure = |selector| LocalDepartureItem::RoomDeparture {
            room: room.clone(),
            jid: jid.clone(),
            cause: OccupancyLeaveCause::Disconnect,
            selector,
        };

        inventory.record_at(
            departure(LeaveSessionSelector::JoinedAtOrBefore(
                OccupancyWatermark::from_revision(3),
            )),
            now,
        );
        inventory.record_at(
            departure(LeaveSessionSelector::JoinedAtOrBefore(
                OccupancyWatermark::from_revision(7),
            )),
            now,
        );
        let merged = inventory.take_due(now).pop().expect("merged departure");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::JoinedAtOrBefore(watermark),
                ..
            } if watermark == OccupancyWatermark::from_revision(7)
        ));

        inventory.record_at(
            departure(LeaveSessionSelector::JoinedAtOrBefore(
                OccupancyWatermark::from_revision(3),
            )),
            now,
        );
        inventory.record_at(departure(LeaveSessionSelector::Any), now);
        inventory.record_at(
            departure(LeaveSessionSelector::JoinedAtOrBefore(
                OccupancyWatermark::from_revision(7),
            )),
            now,
        );
        let merged = inventory.take_due(now).pop().expect("any departure");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::Any,
                ..
            }
        ));

        inventory.record_at(departure(LeaveSessionSelector::Any), now);
        inventory.record_at(
            LocalDepartureItem::ConfirmRetired {
                room: room.clone(),
                jid: jid.clone(),
                actor: ActorId::new(9_u64),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::JoinedAtOrBefore(
                    OccupancyWatermark::from_revision(11),
                ),
            },
            now,
        );
        let merged = inventory.take_due(now).pop().expect("confirm retired");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::ConfirmRetired {
                room: merged_room,
                jid: merged_jid,
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
                ..
            } if merged_room == room && merged_jid == jid
        ));
    }

    #[test]
    fn full_jid_sweep_overflow_drops_oldest_with_metric() {
        const CAP: usize = 8;
        let inventory = PendingLocalMucDepartures::with_full_jid_sweep_cap(CAP);
        let now = Instant::now();
        for index in 0..=CAP {
            inventory.record_at(
                LocalDepartureItem::FullJidSweep {
                    jid: jid(&format!("u{index}@example.com/r")),
                },
                now + Duration::from_secs(index as u64),
            );
        }
        assert_eq!(inventory.len(), CAP);
        let retained = inventory.entries.lock().expect("lock");
        assert!(
            !retained.contains_key(&LocalDepartureKey::FullJidSweep(jid("u0@example.com/r"))),
            "the oldest sweep is the one dropped"
        );
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(2), Duration::from_secs(4));
        assert_eq!(backoff(6), Duration::from_secs(60));
        assert_eq!(backoff(99), Duration::from_secs(60));
    }

    #[test]
    fn take_due_orders_by_not_before() {
        let inventory = PendingLocalMucDepartures::default();
        let now = Instant::now();
        inventory.record_at(
            LocalDepartureItem::FullJidSweep {
                jid: jid("b@example.com/web"),
            },
            now + Duration::from_secs(2),
        );
        inventory.record_at(
            LocalDepartureItem::FullJidSweep {
                jid: jid("a@example.com/web"),
            },
            now + Duration::from_secs(1),
        );
        let due = inventory.take_due(now + Duration::from_secs(2));
        assert_eq!(due.len(), 2);
        assert!(
            matches!(&due[0].item, LocalDepartureItem::FullJidSweep { jid } if jid.as_str() == "a@example.com/web")
        );
    }
}
