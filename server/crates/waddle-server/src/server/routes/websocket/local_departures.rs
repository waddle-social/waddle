//! Retained local responsibility for MUC departures that could not be projected.

use std::collections::HashSet;
use std::sync::Arc;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use jid::{BareJid, FullJid};
use kameo::actor::ActorId;
use tracing::warn;
use waddle_xmpp::muc::{
    durable::OccupancyLeaveCause,
    room_actor::{LeaveAttemptId, LeaveSessionSelector},
};

const BACKOFF_BASE: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const STUCK_ATTEMPTS: u32 = 10;
/// How long a write-ahead [`LocalDepartureItem::InFlight`] entry may go
/// without a lease renewal before the janitor assumes its live task died.
/// The lease renews every [`IN_FLIGHT_RENEWAL`], so a live task — however long
/// its fan-out — never lets the deadline lapse; a dead one is replayed within
/// this delay.
pub(crate) const IN_FLIGHT_REPLAY_DELAY: Duration = Duration::from_secs(30);
const IN_FLIGHT_RENEWAL: Duration = Duration::from_secs(10);
/// Sweeps an owed acknowledgement outlives a room the registry cannot find:
/// a live-roster handoff (demote, then publish the successor holding the
/// transferred receipt) briefly answers "no room" while the receipt is in
/// flight; a destroyed room never comes back, so the ack is dropped after
/// this many absent-room sweeps.
pub(crate) const ACK_ABSENT_ROOM_RETRIES: u32 = 3;
const MAX_FULL_JID_SWEEPS: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalDepartureItem {
    FullJidSweep {
        jid: FullJid,
        /// The occupancy-order ceiling for the whole redrive, minted when the
        /// ORIGINAL disconnect cleanup started: a session that (re)joined
        /// after this attempt is not the sweep's target, so the actor's
        /// order fence classifies it `Superseded` instead of the sweep
        /// evicting a live replacement (#1647, codex round 23).
        attempt: LeaveAttemptId,
    },
    RoomDeparture {
        room: BareJid,
        jid: FullJid,
        cause: OccupancyLeaveCause,
        selector: LeaveSessionSelector,
        /// Idempotency key replayed on retry so a departure the actor already
        /// completed (reply lost) yields its retained outcome, not `NotOccupant`.
        attempt: LeaveAttemptId,
        /// Remaining occupants that already received this departure's fan-out
        /// (carried over from a died task's write-ahead entry): a resumed
        /// replay skips them instead of announcing the departure twice.
        notified: HashSet<FullJid>,
    },
    ConfirmRetired {
        room: BareJid,
        jid: FullJid,
        actor: ActorId,
        cause: OccupancyLeaveCause,
        selector: LeaveSessionSelector,
        attempt: LeaveAttemptId,
        /// Fan-out progress carried through the retirement watch so the
        /// successor's retry resumes where the dead task stopped.
        notified: HashSet<FullJid>,
    },
    /// A departure whose reply WAS delivered and whose effects ran, but whose
    /// receipt acknowledgement could not be handed to the actor in time: the
    /// receipt must still be dropped, or a later gone-JID leave of the same
    /// cause would replay the already-emitted effects.
    /// Write-ahead retention for a departure a live task is asking for and
    /// effecting right now. Keyed per attempt, apart from `RoomDeparture`, so
    /// it never merges with (or completes away) any other responsibility.
    /// The live task holds an [`InFlightLease`] that keeps renewing the
    /// deadline while it runs; if the task dies (lease dropped) the janitor
    /// converts the entry into a retained retry once [`IN_FLIGHT_REPLAY_DELAY`]
    /// lapses without renewal and replays the receipt.
    InFlight {
        room: BareJid,
        jid: FullJid,
        cause: OccupancyLeaveCause,
        attempt: LeaveAttemptId,
        /// Per-recipient fan-out progress (see `RoomDeparture::notified`).
        notified: HashSet<FullJid>,
    },
    /// A guarded empty-room destroy whose bounded registry ask failed: the
    /// destroy is owed until the registry answers definitively (destroyed,
    /// absent, or refused because a newer join bumped the revision).
    EvictEmptyRoom {
        room: BareJid,
        occupancy_revision: u64,
    },
    AckReceipt {
        room: BareJid,
        jid: FullJid,
        attempt: LeaveAttemptId,
        /// Consecutive sweeps that found no registered room for this ack (a
        /// live-roster handoff window, or a room gone for good). Separate
        /// from the generic `attempts` so ask timeouts and `NotAuthoritative`
        /// answers never consume the absent-room budget.
        absent_sweeps: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum LocalDepartureKey {
    FullJidSweep(FullJid),
    RoomScoped(BareJid, FullJid, u8),
    Ack(BareJid, FullJid, LeaveAttemptId),
    Evict(BareJid),
    InFlight(BareJid, FullJid, u8, LeaveAttemptId),
}

impl LocalDepartureItem {
    fn key(&self) -> LocalDepartureKey {
        match self {
            Self::FullJidSweep { jid, .. } => LocalDepartureKey::FullJidSweep(jid.clone()),
            Self::RoomDeparture {
                room, jid, cause, ..
            }
            | Self::ConfirmRetired {
                room, jid, cause, ..
            } => LocalDepartureKey::RoomScoped(room.clone(), jid.clone(), cause_key(*cause)),
            Self::AckReceipt {
                room, jid, attempt, ..
            } => LocalDepartureKey::Ack(room.clone(), jid.clone(), *attempt),
            Self::EvictEmptyRoom { room, .. } => LocalDepartureKey::Evict(room.clone()),
            Self::InFlight {
                room,
                jid,
                cause,
                attempt,
                ..
            } => {
                LocalDepartureKey::InFlight(room.clone(), jid.clone(), cause_key(*cause), *attempt)
            }
        }
    }

    fn merge_with_existing(self, existing: &PendingLocalDeparture) -> Self {
        let merged_selector = merge_selectors(existing.item.selector(), self.selector());
        // The NEWEST attempt always wins: the actor refuses an attempt minted
        // before the live session joined (`Superseded`), so a merged item that
        // widens to `Any` must carry an attempt at least as new as the session
        // it now targets. An older completed attempt whose reply was lost is
        // still recoverable because the actor replays the full JID's
        // unacknowledged receipt of the same cause when the session is gone.
        let merged_attempt = match (existing.item.attempt(), self.attempt()) {
            (Some(existing_attempt), Some(incoming_attempt)) => {
                Some(existing_attempt.max(incoming_attempt))
            }
            (existing_attempt, incoming_attempt) => existing_attempt.or(incoming_attempt),
        };
        let merged_selector = merged_selector.unwrap_or(LeaveSessionSelector::Any);
        let merged_attempt = merged_attempt.unwrap_or_else(LeaveAttemptId::generate);
        // Fan-out progress belongs to ONE attempt: when a newer attempt
        // supersedes, the older attempt's recipients are not "already
        // notified" of the new departure (they may have seen a re-join in
        // between), so only the winning attempt's progress survives.
        let merged_notified = match (existing.item.attempt(), self.attempt()) {
            (Some(existing_attempt), Some(incoming_attempt))
                if existing_attempt == incoming_attempt =>
            {
                let mut notified = existing.item.notified().cloned().unwrap_or_default();
                notified.extend(self.notified().into_iter().flatten().cloned());
                notified
            }
            (Some(existing_attempt), Some(incoming_attempt))
                if incoming_attempt > existing_attempt =>
            {
                self.notified().cloned().unwrap_or_default()
            }
            _ => existing.item.notified().cloned().unwrap_or_default(),
        };
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
                selector: merged_selector,
                attempt: merged_attempt,
                notified: merged_notified.clone(),
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
                selector: merged_selector,
                attempt: merged_attempt,
                notified: merged_notified,
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
                selector: merged_selector,
                attempt: merged_attempt,
                notified: merged_notified.clone(),
            },
            // A newer disconnect's ceiling covers the older sweep's rooms
            // too (their sessions joined even earlier), so the newest
            // attempt is the right merged fence.
            (LocalDepartureItem::FullJidSweep { jid, .. }, _) => LocalDepartureItem::FullJidSweep {
                jid,
                attempt: merged_attempt,
            },
            (
                LocalDepartureItem::EvictEmptyRoom {
                    room,
                    occupancy_revision: existing_revision,
                },
                LocalDepartureItem::EvictEmptyRoom {
                    occupancy_revision: incoming_revision,
                    ..
                },
            ) => LocalDepartureItem::EvictEmptyRoom {
                room,
                // The newest leave's revision is the guard that must hold.
                occupancy_revision: existing_revision.max(incoming_revision),
            },
            (existing, _) => existing,
        }
    }

    fn attempt(&self) -> Option<LeaveAttemptId> {
        match self {
            Self::AckReceipt { .. } | Self::EvictEmptyRoom { .. } => None,
            Self::FullJidSweep { attempt, .. }
            | Self::RoomDeparture { attempt, .. }
            | Self::ConfirmRetired { attempt, .. }
            | Self::InFlight { attempt, .. } => Some(*attempt),
        }
    }

    fn notified(&self) -> Option<&HashSet<FullJid>> {
        match self {
            Self::RoomDeparture { notified, .. }
            | Self::InFlight { notified, .. }
            | Self::ConfirmRetired { notified, .. } => Some(notified),
            _ => None,
        }
    }

    fn selector(&self) -> Option<LeaveSessionSelector> {
        match self {
            Self::FullJidSweep { .. }
            | Self::AckReceipt { .. }
            | Self::InFlight { .. }
            | Self::EvictEmptyRoom { .. } => None,
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
    counts: [i64; 6],
}

const fn kind_slot(item: &LocalDepartureItem) -> usize {
    match item {
        LocalDepartureItem::FullJidSweep { .. } => 0,
        LocalDepartureItem::RoomDeparture { .. } => 1,
        LocalDepartureItem::ConfirmRetired { .. } => 2,
        LocalDepartureItem::AckReceipt { .. } => 3,
        LocalDepartureItem::InFlight { .. } => 4,
        LocalDepartureItem::EvictEmptyRoom { .. } => 5,
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

    fn merge_or_insert(&mut self, entry: PendingLocalDeparture) {
        if !self.merge_into_existing(entry.clone()) {
            self.insert(entry);
        }
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
        for (kind, value) in [
            "full_jid_sweep",
            "room_departure",
            "confirm_retired",
            "ack_receipt",
            "in_flight",
            "evict_empty_room",
        ]
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

    #[cfg(test)]
    pub(crate) fn record_pending_for_test_any(&self, entry: PendingLocalDeparture) {
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        inventory.insert(entry);
        inventory.record_gauges();
    }

    #[cfg(test)]
    pub(crate) fn take_for_test(&self, item: &LocalDepartureItem) -> Option<PendingLocalDeparture> {
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        let taken = inventory.remove(&item.key());
        inventory.record_gauges();
        taken
    }

    #[cfg(test)]
    pub(crate) fn contains_for_test(&self, item: &LocalDepartureItem) -> bool {
        self.entries
            .lock()
            .expect("local departure inventory lock")
            .entries
            .get(&item.key())
            .is_some_and(|pending| &pending.item == item)
    }

    /// Write-ahead retention for a departure a live task is about to ask for
    /// and then effect (see [`LocalDepartureItem::InFlight`]).
    pub fn record_in_flight(&self, item: LocalDepartureItem) {
        debug_assert!(matches!(item, LocalDepartureItem::InFlight { .. }));
        self.record_at(item, Instant::now() + IN_FLIGHT_REPLAY_DELAY);
    }

    /// The live task is done with this attempt (effects ran and were handed
    /// over, or the ask never produced a departure): drop its entry.
    pub fn complete_in_flight(&self, item: &LocalDepartureItem) {
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        if inventory.remove(&item.key()).is_some() {
            inventory.record_gauges();
        }
    }

    /// One remaining occupant received this departure's fan-out: record it on
    /// the entry so a resumed replay (after the task died mid fan-out) skips
    /// the recipients already notified. No-op when the entry is not in the
    /// inventory (the janitor works on drained items: its own fan-out is not
    /// resumable, which only costs a repeated `unavailable` if the janitor
    /// process itself dies mid fan-out).
    pub fn note_notified(&self, item: &LocalDepartureItem, recipient: &FullJid) {
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        let Some(pending) = inventory.entries.get_mut(&item.key()) else {
            return;
        };
        // Progress is only valid for the attempt it was made under: a newer
        // attempt merged under the same key must not inherit it.
        if pending.item.attempt() != item.attempt() {
            return;
        }
        if let LocalDepartureItem::InFlight { notified, .. }
        | LocalDepartureItem::RoomDeparture { notified, .. }
        | LocalDepartureItem::ConfirmRetired { notified, .. } = &mut pending.item
        {
            notified.insert(recipient.clone());
        }
    }

    #[cfg(test)]
    pub(crate) fn not_before_for_test(&self, item: &LocalDepartureItem) -> Option<Instant> {
        self.entries
            .lock()
            .expect("local departure inventory lock")
            .entries
            .get(&item.key())
            .map(|pending| pending.not_before)
    }

    /// Push the write-ahead deadline out again: the live task is still
    /// running (see [`InFlightLease`]).
    pub fn renew_in_flight(&self, item: &LocalDepartureItem) {
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        if let Some(pending) = inventory.entries.get_mut(&item.key()) {
            pending.not_before = Instant::now() + IN_FLIGHT_REPLAY_DELAY;
        }
    }

    /// The effects ran: atomically turn the write-ahead entry into the owed
    /// acknowledgement BEFORE the acknowledgement is awaited, so a task
    /// cancelled between its effects and the actor's answer leaves exactly
    /// one responsibility (deliver the ack), never a replayable departure.
    pub fn convert_in_flight_to_ack(&self, item: &LocalDepartureItem, acknowledge: LeaveAttemptId) {
        let (room, jid) = match item {
            LocalDepartureItem::InFlight { room, jid, .. } => (room.clone(), jid.clone()),
            _ => return,
        };
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        inventory.remove(&item.key());
        inventory.merge_or_insert(PendingLocalDeparture {
            item: LocalDepartureItem::AckReceipt {
                room,
                jid,
                attempt: acknowledge,
                absent_sweeps: 0,
            },
            attempts: 0,
            // Not due before the live task's own acknowledgement attempt
            // has had its full bound; the janitor picks it up only if that
            // attempt failed or never finished.
            not_before: Instant::now() + super::LEAVE_ASK_TIMEOUT + Duration::from_secs(2),
        });
        inventory.record_gauges();
    }

    /// The acknowledgement still owed for this full JID in this room, if any.
    /// A retained departure retry of the same JID must deliver it FIRST: the
    /// receipt it names was already effected, and a retry's JID fallback
    /// would otherwise consume and replay it.
    pub fn pending_ack_for(&self, room: &BareJid, jid: &FullJid) -> Option<LeaveAttemptId> {
        self.entries
            .lock()
            .expect("local departure inventory lock")
            .entries
            .keys()
            .find_map(|key| match key {
                LocalDepartureKey::Ack(ack_room, ack_jid, attempt)
                    if ack_room == room && ack_jid == jid =>
                {
                    Some(*attempt)
                }
                _ => None,
            })
    }

    /// Drop a retained acknowledgement once the actor accepted it.
    pub fn complete_ack(&self, room: &BareJid, jid: &FullJid, attempt: LeaveAttemptId) {
        let mut inventory = self.entries.lock().expect("local departure inventory lock");
        inventory.remove(&LocalDepartureKey::Ack(room.clone(), jid.clone(), attempt));
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

/// Liveness of the task that owns a write-ahead [`LocalDepartureItem::InFlight`]
/// entry: a background heartbeat renews the entry's deadline while the lease
/// is held, and stops the moment the lease is dropped — including when the
/// owning future is cancelled — so the janitor replays only departures whose
/// task actually died, however long a live fan-out takes.
pub struct InFlightLease {
    heartbeat: tokio::task::JoinHandle<()>,
}

const _: () = assert!(
    IN_FLIGHT_RENEWAL.as_millis() * 2 <= IN_FLIGHT_REPLAY_DELAY.as_millis(),
    "a live task must renew well inside the replay delay"
);

impl InFlightLease {
    pub fn hold(pending: Arc<PendingLocalMucDepartures>, item: LocalDepartureItem) -> Self {
        Self::hold_every(pending, item, IN_FLIGHT_RENEWAL)
    }

    pub(crate) fn hold_every(
        pending: Arc<PendingLocalMucDepartures>,
        item: LocalDepartureItem,
        renewal: Duration,
    ) -> Self {
        let heartbeat = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(renewal);
            ticks.tick().await;
            loop {
                ticks.tick().await;
                pending.renew_in_flight(&item);
            }
        });
        Self { heartbeat }
    }
}

impl Drop for InFlightLease {
    fn drop(&mut self) {
        self.heartbeat.abort();
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
                attempt: LeaveAttemptId::generate(),
                notified: HashSet::new(),
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
            attempt: LeaveAttemptId::generate(),
            notified: HashSet::new(),
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
        let departure = |selector, attempt| LocalDepartureItem::RoomDeparture {
            room: room.clone(),
            jid: jid.clone(),
            cause: OccupancyLeaveCause::Disconnect,
            selector,
            attempt,
            notified: HashSet::new(),
        };
        let attempt_a = LeaveAttemptId::generate();
        let attempt_b = LeaveAttemptId::generate();
        let attempt_c = LeaveAttemptId::generate();

        inventory.record_at(
            departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(3)),
                attempt_a,
            ),
            now,
        );
        inventory.record_at(
            departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(7)),
                attempt_b,
            ),
            now,
        );
        let merged = inventory.take_due(now).pop().expect("merged departure");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::JoinedAtOrBefore(watermark),
                attempt,
                ..
            } if watermark == OccupancyWatermark::from_revision(7) && attempt == attempt_b
        ));

        inventory.record_at(
            departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(3)),
                attempt_a,
            ),
            now,
        );
        inventory.record_at(
            departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(2)),
                attempt_c,
            ),
            now,
        );
        let merged = inventory
            .take_due(now)
            .pop()
            .expect("older watermark keeps existing selector, newest attempt");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::JoinedAtOrBefore(watermark),
                attempt,
                ..
            } if watermark == OccupancyWatermark::from_revision(3) && attempt == attempt_c
        ));

        inventory.record_at(
            departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(3)),
                attempt_a,
            ),
            now,
        );
        inventory.record_at(departure(LeaveSessionSelector::Any, attempt_b), now);
        let merged = inventory.take_due(now).pop().expect("any dominates");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::Any,
                attempt,
                ..
            } if attempt == attempt_b
        ));

        inventory.record_at(departure(LeaveSessionSelector::Any, attempt_a), now);
        inventory.record_at(
            departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(7)),
                attempt_b,
            ),
            now,
        );
        let merged = inventory
            .take_due(now)
            .pop()
            .expect("existing any keeps selector, newest attempt");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::Any,
                attempt,
                ..
            } if attempt == attempt_b
        ));

        inventory.record_at(departure(LeaveSessionSelector::Any, attempt_a), now);
        inventory.record_at(
            LocalDepartureItem::ConfirmRetired {
                room: room.clone(),
                jid: jid.clone(),
                actor: ActorId::new(9_u64),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::JoinedAtOrBefore(
                    OccupancyWatermark::from_revision(11),
                ),
                attempt: attempt_b,
                notified: HashSet::new(),
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
                attempt,
                ..
            } if merged_room == room && merged_jid == jid && attempt == attempt_b
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
            attempt: LeaveAttemptId::generate(),
            notified: HashSet::new(),
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
                attempt: LeaveAttemptId::generate(),
                notified: HashSet::new(),
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
        let after_crossing =
            _metrics_lock.counter_sum("waddle.muc.local_departure_retry", &[("outcome", "stuck")]);
        assert!(
            after_crossing.is_some_and(|count| count >= 1),
            "crossing the stuck threshold must record at least one increment"
        );

        inventory.take_due(Instant::now() + Duration::from_secs(61));
        inventory.requeue_with_backoff(entry(11));
        let after_requeue =
            _metrics_lock.counter_sum("waddle.muc.local_departure_retry", &[("outcome", "stuck")]);
        assert_eq!(
            after_requeue, after_crossing,
            "requeueing past the threshold must not emit another stuck increment"
        );
    }

    #[test]
    fn record_merge_widening_to_any_adopts_the_newest_attempt() {
        // Regression: an older `Any` entry merged with a newer deferred
        // disconnect must not keep the older attempt — the actor would refuse
        // it as `Superseded` against the session that joined in between and
        // the janitor would drop the only responsibility for that session.
        let room = room("merge-any-newest");
        let jid = jid("alice@example.com/web");
        let now = Instant::now();
        let make_departure = |selector, attempt| LocalDepartureItem::RoomDeparture {
            room: room.clone(),
            jid: jid.clone(),
            cause: OccupancyLeaveCause::Disconnect,
            selector,
            attempt,
            notified: HashSet::new(),
        };
        let inventory = PendingLocalMucDepartures::default();
        let older = LeaveAttemptId::generate();
        let newer = LeaveAttemptId::generate();
        inventory.record_at(make_departure(LeaveSessionSelector::Any, older), now);
        inventory.record_at(
            make_departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(5)),
                newer,
            ),
            now,
        );
        let merged = inventory.take_due(now).pop().expect("merged");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::Any,
                attempt,
                ..
            } if attempt == newer
        ));
    }

    #[test]
    fn record_merge_keeps_newest_attempt_for_every_selector_outcome() {
        let room = room("merge-attempt");
        let jid = jid("alice@example.com/web");
        let now = Instant::now();
        let make_departure = |selector, attempt| LocalDepartureItem::RoomDeparture {
            room: room.clone(),
            jid: jid.clone(),
            cause: OccupancyLeaveCause::Disconnect,
            selector,
            attempt,
            notified: HashSet::new(),
        };

        let inventory = PendingLocalMucDepartures::default();
        let attempt_a = LeaveAttemptId::generate();
        let attempt_b = LeaveAttemptId::generate();
        inventory.record_at(
            make_departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(3)),
                attempt_a,
            ),
            now,
        );
        inventory.record_at(
            make_departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(7)),
                attempt_b,
            ),
            now,
        );
        let merged = inventory
            .take_due(now)
            .pop()
            .expect("merged newer watermark");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::JoinedAtOrBefore(watermark),
                attempt,
                ..
            } if watermark == OccupancyWatermark::from_revision(7) && attempt == attempt_b
        ));

        let inventory = PendingLocalMucDepartures::default();
        let attempt_c = LeaveAttemptId::generate();
        inventory.record_at(
            make_departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(3)),
                attempt_a,
            ),
            now,
        );
        inventory.record_at(
            make_departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(2)),
                attempt_c,
            ),
            now,
        );
        let merged = inventory
            .take_due(now)
            .pop()
            .expect("merged older watermark");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::JoinedAtOrBefore(watermark),
                attempt,
                ..
            } if watermark == OccupancyWatermark::from_revision(3) && attempt == attempt_c
        ));

        let inventory = PendingLocalMucDepartures::default();
        let any_existing_attempt = LeaveAttemptId::generate();
        let any_incoming_attempt = LeaveAttemptId::generate();
        inventory.record_at(
            make_departure(LeaveSessionSelector::Any, any_existing_attempt),
            now,
        );
        inventory.record_at(
            make_departure(LeaveSessionSelector::Any, any_incoming_attempt),
            now,
        );
        let merged = inventory
            .take_due(now)
            .pop()
            .expect("merged equal any selectors");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::Any,
                attempt,
                ..
            } if attempt == any_incoming_attempt
        ));

        let inventory = PendingLocalMucDepartures::default();
        let attempt_any = LeaveAttemptId::generate();
        inventory.record_at(
            make_departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(3)),
                attempt_a,
            ),
            now,
        );
        inventory.record_at(make_departure(LeaveSessionSelector::Any, attempt_any), now);
        let merged = inventory.take_due(now).pop().expect("merged any selector");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::Any,
                attempt,
                ..
            } if attempt == attempt_any
        ));

        let inventory = PendingLocalMucDepartures::default();
        let existing_any_attempt = LeaveAttemptId::generate();
        inventory.record_at(
            make_departure(LeaveSessionSelector::Any, existing_any_attempt),
            now,
        );
        let incoming_deferred_attempt = LeaveAttemptId::generate();
        inventory.record_at(
            make_departure(
                LeaveSessionSelector::JoinedAtOrBefore(OccupancyWatermark::from_revision(9)),
                incoming_deferred_attempt,
            ),
            now,
        );
        let merged = inventory
            .take_due(now)
            .pop()
            .expect("merged existing any selector");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::Any,
                attempt,
                ..
            } if attempt == incoming_deferred_attempt
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
                    attempt: LeaveAttemptId::generate(),
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
    fn ack_receipts_key_separately_from_departures_and_from_each_other() {
        let pending = PendingLocalMucDepartures::default();
        let room: BareJid = "room@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        let first = LeaveAttemptId::generate();
        let second = LeaveAttemptId::generate();
        pending.record(LocalDepartureItem::RoomDeparture {
            room: room.clone(),
            jid: jid.clone(),
            cause: OccupancyLeaveCause::Explicit,
            selector: LeaveSessionSelector::Any,
            attempt: first,
            notified: HashSet::new(),
        });
        pending.record(LocalDepartureItem::AckReceipt {
            room: room.clone(),
            jid: jid.clone(),
            attempt: first,
            absent_sweeps: 0,
        });
        pending.record(LocalDepartureItem::AckReceipt {
            room: room.clone(),
            jid: jid.clone(),
            attempt: second,
            absent_sweeps: 0,
        });
        pending.record(LocalDepartureItem::AckReceipt {
            room,
            jid,
            attempt: second,
            absent_sweeps: 0,
        });
        assert_eq!(
            pending.len(),
            3,
            "a departure and two distinct acknowledgements; the repeat coalesces"
        );
    }

    #[test]
    fn in_flight_entries_are_per_attempt_and_renewable() {
        let pending = PendingLocalMucDepartures::default();
        let room: BareJid = "room@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        let first = LeaveAttemptId::generate();
        let second = LeaveAttemptId::generate();
        let in_flight = |attempt| LocalDepartureItem::InFlight {
            room: room.clone(),
            jid: jid.clone(),
            cause: OccupancyLeaveCause::Disconnect,
            attempt,
            notified: HashSet::new(),
        };
        pending.record_in_flight(in_flight(first));
        pending.record_in_flight(in_flight(second));
        assert_eq!(pending.len(), 2, "two live tasks hold two entries");
        // Completing one task's entry never touches the other's.
        pending.complete_in_flight(&in_flight(second));
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_for_test(&in_flight(first)));
        // A renewal pushes the deadline out; the entry is never due while the
        // lease keeps renewing it.
        pending.renew_in_flight(&in_flight(first));
        assert!(pending
            .take_due(Instant::now() + IN_FLIGHT_REPLAY_DELAY / 2)
            .is_empty());
        assert_eq!(
            pending
                .take_due(Instant::now() + IN_FLIGHT_REPLAY_DELAY + Duration::from_secs(1))
                .len(),
            1,
            "without renewal the entry goes due after the replay delay"
        );
    }

    #[test]
    fn converting_in_flight_to_ack_leaves_exactly_the_owed_acknowledgement() {
        let pending = PendingLocalMucDepartures::default();
        let room: BareJid = "room@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        let attempt = LeaveAttemptId::generate();
        let acknowledge = LeaveAttemptId::generate();
        let in_flight = LocalDepartureItem::InFlight {
            room: room.clone(),
            jid: jid.clone(),
            cause: OccupancyLeaveCause::Explicit,
            attempt,
            notified: HashSet::new(),
        };
        pending.record_in_flight(in_flight.clone());
        pending.convert_in_flight_to_ack(&in_flight, acknowledge);
        assert_eq!(pending.len(), 1);
        assert!(!pending.contains_for_test(&in_flight));
        assert_eq!(pending.pending_ack_for(&room, &jid), Some(acknowledge));
        assert!(
            pending.take_due(Instant::now()).is_empty(),
            "the owed ack is not due before the live ack attempt's bound"
        );
        pending.complete_ack(&room, &jid, acknowledge);
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn fan_out_progress_is_scoped_to_its_attempt() {
        let pending = PendingLocalMucDepartures::default();
        let room: BareJid = "room@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        let bob: FullJid = "bob@example.com/phone".parse().expect("bob");
        let older = LeaveAttemptId::generate();
        let newer = LeaveAttemptId::generate();
        let departure = |attempt, notified| LocalDepartureItem::RoomDeparture {
            room: room.clone(),
            jid: jid.clone(),
            cause: OccupancyLeaveCause::Disconnect,
            selector: LeaveSessionSelector::Any,
            attempt,
            notified,
        };
        let carol: FullJid = "carol@example.com/tablet".parse().expect("carol");
        pending.record(departure(older, HashSet::from([bob.clone()])));
        // Progress recorded for a different attempt than the stored one is
        // ignored — even for a recipient not yet in the stored set.
        pending.note_notified(&departure(newer, HashSet::new()), &carol);
        assert!(pending.contains_for_test(&departure(older, HashSet::from([bob.clone()]))));
        // A newer attempt merged under the same key starts with ITS progress,
        // not the older attempt's.
        pending.record(departure(newer, HashSet::new()));
        assert!(pending.contains_for_test(&departure(newer, HashSet::new())));
        // Same attempt re-recorded: progress unions.
        pending.record(departure(newer, HashSet::from([bob.clone()])));
        assert!(pending.contains_for_test(&departure(newer, HashSet::from([bob.clone()]))));
        // The retirement watch carries progress: a ConfirmRetired merged onto
        // the same attempt keeps the recipients, and converting back does too.
        pending.record(LocalDepartureItem::ConfirmRetired {
            room: room.clone(),
            jid: jid.clone(),
            actor: kameo::actor::ActorId::generate(),
            cause: OccupancyLeaveCause::Disconnect,
            selector: LeaveSessionSelector::Any,
            attempt: newer,
            notified: HashSet::new(),
        });
        let retained = pending
            .take_for_test(&departure(newer, HashSet::new()))
            .expect("merged retirement watch");
        assert!(matches!(
            retained.item,
            LocalDepartureItem::ConfirmRetired { ref notified, .. }
                if notified == &HashSet::from([bob.clone()])
        ));
        // Reverse direction: an existing ConfirmRetired's progress must be
        // READ when something merges onto it (pins the accessor arm).
        pending.record_pending_for_test_any(retained);
        pending.record(departure(newer, HashSet::new()));
        let merged = pending
            .take_for_test(&departure(newer, HashSet::new()))
            .expect("re-merged retirement watch");
        assert!(matches!(
            merged.item,
            LocalDepartureItem::ConfirmRetired { ref notified, .. }
                if notified == &HashSet::from([bob])
        ));
    }

    #[tokio::test]
    async fn lease_keeps_renewing_until_dropped() {
        let pending = Arc::new(PendingLocalMucDepartures::default());
        let room: BareJid = "room@muc.example.com".parse().expect("room");
        let jid: FullJid = "alice@example.com/web".parse().expect("jid");
        let item = LocalDepartureItem::InFlight {
            room,
            jid,
            cause: OccupancyLeaveCause::Explicit,
            attempt: LeaveAttemptId::generate(),
            notified: HashSet::new(),
        };
        pending.record_in_flight(item.clone());
        let before = pending.not_before_for_test(&item).expect("entry");
        let lease = InFlightLease::hold_every(
            Arc::clone(&pending),
            item.clone(),
            Duration::from_millis(10),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if pending.not_before_for_test(&item).expect("entry") > before {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "a held lease renews the deadline"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(lease);
        tokio::time::sleep(Duration::from_millis(30)).await;
        let after_drop = pending.not_before_for_test(&item).expect("entry");
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(
            pending.not_before_for_test(&item).expect("entry"),
            after_drop,
            "a dropped lease stops renewing"
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
                attempt: LeaveAttemptId::generate(),
            },
            now + Duration::from_secs(2),
        );
        inventory.record_at(
            LocalDepartureItem::FullJidSweep {
                jid: jid("a@example.com/web"),
                attempt: LeaveAttemptId::generate(),
            },
            now + Duration::from_secs(1),
        );
        let due = inventory.take_due(now + Duration::from_secs(2));
        assert_eq!(due.len(), 2);
        assert!(
            matches!(&due[0].item, LocalDepartureItem::FullJidSweep { jid, .. } if jid.as_str() == "a@example.com/web")
        );
    }
}
