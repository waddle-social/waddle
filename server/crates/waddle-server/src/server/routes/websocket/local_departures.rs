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
    entries: Mutex<Inventory>,
    /// Cap on retained `FullJidSweep` items: sweeps are minted on room
    /// enumeration failure for every disconnecting JID (false positives
    /// included), so under a prolonged registry outage they grow with
    /// connection churn. Room-scoped items are bounded by observed local
    /// occupancies and carry no cap.
    full_jid_sweep_cap: usize,
}

/// The keyed inventory plus per-kind counts maintained incrementally so the
/// pending gauges never rescan the map under the lock.
#[derive(Debug, Default)]
struct Inventory {
    entries: HashMap<LocalDepartureKey, PendingLocalDeparture>,
    counts: [i64; 3],
}

const fn kind_slot(item: &LocalDepartureItem) -> usize {
    match item {
        LocalDepartureItem::FullJidSweep { .. } => 0,
        LocalDepartureItem::RoomDeparture { .. } => 1,
        LocalDepartureItem::ConfirmRetired { .. } => 2,
    }
}

impl Inventory {
    fn insert(&mut self, entry: PendingLocalDeparture) {
        let key = entry.item.key();
        self.counts[kind_slot(&entry.item)] += 1;
        if let Some(previous) = self.entries.insert(key, entry) {
            self.counts[kind_slot(&previous.item)] -= 1;
        }
    }

    fn remove(&mut self, key: &LocalDepartureKey) -> Option<PendingLocalDeparture> {
        let removed = self.entries.remove(key)?;
        self.counts[kind_slot(&removed.item)] -= 1;
        Some(removed)
    }

    /// Merge an incoming item into the entry already held under its key.
    fn merge_into_existing(&mut self, entry: PendingLocalDeparture) -> bool {
        let key = entry.item.key();
        let Some(existing) = self.entries.get_mut(&key) else {
            return false;
        };
        let previous_slot = kind_slot(&existing.item);
        existing.item = entry.item.merge_with_existing(existing);
        existing.not_before = existing.not_before.min(entry.not_before);
        existing.attempts = existing.attempts.max(entry.attempts);
        let next_slot = kind_slot(&existing.item);
        if previous_slot != next_slot {
            self.counts[previous_slot] -= 1;
            self.counts[next_slot] += 1;
        }
        true
    }

    fn record_gauges(&self) {
        for (kind, value) in ["full_jid_sweep", "room_departure", "confirm_retired"]
            .into_iter()
            .zip(self.counts)
        {
            crate::metrics::record_local_departure_pending(kind, value);
        }
    }
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
            entries: Mutex::new(Inventory::default()),
            full_jid_sweep_cap,
        }
    }

    fn record_at(&self, item: LocalDepartureItem, now: Instant) {
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        let entry = PendingLocalDeparture {
            item,
            attempts: 0,
            not_before: now,
        };
        if inventory.merge_into_existing(entry.clone()) {
            inventory.record_gauges();
            return;
        }
        if matches!(entry.item, LocalDepartureItem::FullJidSweep { .. }) {
            evict_oldest_sweep_if_at_cap(&mut inventory, self.full_jid_sweep_cap);
        }
        inventory.insert(entry);
        inventory.record_gauges();
    }

    /// Every due item, oldest deadline first (tests and small sweeps).
    pub fn take_due(&self, now: Instant) -> Vec<PendingLocalDeparture> {
        self.take_due_bounded(now, usize::MAX)
    }

    /// At most `budget` due items, oldest deadline first. The janitor drains
    /// a backlog in bounded passes so a recovery after an outage cannot pin
    /// the sweep task on one enormous serial pass; the rest stay due for the
    /// next tick.
    pub fn take_due_bounded(&self, now: Instant, budget: usize) -> Vec<PendingLocalDeparture> {
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        let mut due_keys = inventory
            .entries
            .iter()
            .filter(|(_, entry)| entry.not_before <= now)
            .map(|(key, entry)| (entry.not_before, key.clone()))
            .collect::<Vec<_>>();
        due_keys.sort();
        let due = due_keys
            .into_iter()
            .take(budget)
            .filter_map(|(_, key)| inventory.remove(&key))
            .collect::<Vec<_>>();
        inventory.record_gauges();
        due
    }

    /// Re-arm an item the sweep took out with `take_due`. Merges with any
    /// entry recorded for the same key in the meantime (a newer disconnect
    /// must never be overwritten by an older attempt): selectors merge by
    /// breadth, the earliest deadline and the larger attempt count win.
    pub fn requeue_with_backoff(&self, mut entry: PendingLocalDeparture) {
        entry.attempts = entry.attempts.saturating_add(1);
        entry.not_before = Instant::now() + jittered(backoff(entry.attempts));
        if entry.attempts == STUCK_ATTEMPTS + 1 {
            // Warn once when the threshold is crossed; the pending gauge keeps
            // reporting the backlog without a per-retry warning storm.
            warn!(?entry.item, attempts = entry.attempts, "local MUC departure remains pending");
            crate::metrics::record_local_departure_retry("stuck");
        }
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        if !inventory.merge_into_existing(entry.clone()) {
            inventory.insert(entry);
        }
        inventory.record_gauges();
    }

    #[cfg(all(test, feature = "clustering"))]
    pub(crate) fn record_pending_for_test(&self, entry: PendingLocalDeparture) {
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        inventory.insert(entry);
        inventory.record_gauges();
    }

    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("local departure inventory lock")
            .entries
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
fn evict_oldest_sweep_if_at_cap(inventory: &mut Inventory, cap: usize) {
    if inventory.counts[0] < cap as i64 {
        return;
    }
    let Some(oldest) = inventory
        .entries
        .iter()
        .filter(|(key, _)| matches!(key, LocalDepartureKey::FullJidSweep(_)))
        .min_by(|(left_key, left), (right_key, right)| {
            left.not_before
                .cmp(&right.not_before)
                .then_with(|| left_key.cmp(right_key))
        })
        .map(|(key, _)| key.clone())
    else {
        return;
    };
    inventory.remove(&oldest);
    warn!("local MUC departure sweep inventory overflow; dropped oldest sweep");
    crate::metrics::record_local_departure_retry("overflow");
}

/// Spread retries that were minted together (a burst of disconnects during
/// an outage) by up to 25% so they do not re-arrive in lock-step.
fn jittered(base: Duration) -> Duration {
    use rand::RngExt as _;
    let spread_ms = base.as_millis() as u64 / 4;
    if spread_ms == 0 {
        return base;
    }
    base + Duration::from_millis(rand::rng().random_range(0..=spread_ms))
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
            .entries
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
    fn requeue_merges_with_concurrently_recorded_newer_disconnect() {
        let inventory = PendingLocalMucDepartures::default();
        let room = room("race");
        let jid = jid("alice@example.com/web");
        let original_due = Instant::now() - Duration::from_secs(10);
        let concurrent_not_before = Instant::now() - Duration::from_secs(1);
        let departure = |selector| LocalDepartureItem::RoomDeparture {
            room: room.clone(),
            jid: jid.clone(),
            cause: OccupancyLeaveCause::Disconnect,
            selector,
        };

        inventory.record_at(
            departure(LeaveSessionSelector::JoinedAtOrBefore(
                OccupancyWatermark::from_revision(1),
            )),
            original_due,
        );
        let mut taken = inventory
            .take_due(Instant::now())
            .pop()
            .expect("taken entry");
        taken.attempts = 3;
        inventory.record_at(
            departure(LeaveSessionSelector::JoinedAtOrBefore(
                OccupancyWatermark::from_revision(2),
            )),
            concurrent_not_before,
        );

        inventory.requeue_with_backoff(taken);

        let entry = inventory
            .entries
            .lock()
            .expect("lock")
            .entries
            .values()
            .next()
            .cloned()
            .expect("entry");
        assert_eq!(entry.not_before, concurrent_not_before);
        assert_eq!(entry.attempts, 4);
        assert!(matches!(
            entry.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::JoinedAtOrBefore(watermark),
                ..
            } if watermark == OccupancyWatermark::from_revision(2)
        ));
    }

    #[tokio::test]
    async fn stuck_is_recorded_once_when_crossing_the_threshold() {
        let _metrics_lock = waddle_xmpp::telemetry::test_support::acquire().await;
        let inventory = PendingLocalMucDepartures::default();
        let room = room("stuck");
        let jid = jid("alice@example.com/web");
        let due = Instant::now() - Duration::from_secs(1);
        let entry = |attempts| PendingLocalDeparture {
            item: LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: jid.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
            },
            attempts,
            not_before: due,
        };

        inventory.requeue_with_backoff(entry(9));
        assert_eq!(
            _metrics_lock.counter_sum("waddle.muc.local_departure_retry", &[("outcome", "stuck")]),
            None
        );

        inventory.take_due(Instant::now() + Duration::from_secs(61));
        inventory.requeue_with_backoff(entry(10));
        assert_eq!(
            _metrics_lock.counter_sum("waddle.muc.local_departure_retry", &[("outcome", "stuck")]),
            Some(1)
        );

        inventory.take_due(Instant::now() + Duration::from_secs(61));
        inventory.requeue_with_backoff(entry(11));
        assert_eq!(
            _metrics_lock.counter_sum("waddle.muc.local_departure_retry", &[("outcome", "stuck")]),
            Some(1)
        );
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
            !retained
                .entries
                .contains_key(&LocalDepartureKey::FullJidSweep(jid("u0@example.com/r"))),
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
