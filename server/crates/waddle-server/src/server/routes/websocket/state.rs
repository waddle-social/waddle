use super::*;

#[derive(Debug, Clone)]
pub struct ActiveCallThread {
    pub anchor_origin_id: String,
    pub initiator: BareJid,
    pub media: waddle_xmpp::xep::CallThreadMedia,
    pub started: chrono::DateTime<chrono::Utc>,
    /// The anchor message's `urn:waddle:threads:0` thread id (the
    /// `<thread/>` value). Correlates the ended fastening back to the
    /// inbox/threads rows so [`InboxStorage::mark_call_thread_ended`]
    /// can stamp the ended timestamp + duration onto every
    /// subscriber's projection of `(room, thread_id)`.
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DmCallThreadKey {
    pub low_peer: BareJid,
    pub high_peer: BareJid,
    pub sid: xmpp_parsers::jingle::SessionId,
}

impl DmCallThreadKey {
    pub fn new(a: BareJid, b: BareJid, sid: xmpp_parsers::jingle::SessionId) -> Self {
        let (low_peer, high_peer) = if a.as_str() <= b.as_str() {
            (a, b)
        } else {
            (b, a)
        };
        Self {
            low_peer,
            high_peer,
            sid,
        }
    }
}

const MAX_RESOLVER_AFFILIATION_SYNC_WORKERS: usize = 128;
const MAX_RECENT_RESOLVER_AFFILIATION_SYNC_COMPLETIONS: usize = 128;

/// One resolver verdict captured from a rejected join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolverAffiliationSyncWork {
    pub affiliation: waddle_xmpp::Affiliation,
    pub expected_admission_revision: u64,
}

/// Logical worker identity. Every verdict for this actor incarnation and
/// member is serialized through one latest-wins worker.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResolverAffiliationSyncWorkerKey {
    room_jid: BareJid,
    jid: BareJid,
    actor_id: kameo::actor::ActorId,
}

#[derive(Debug, Clone, Copy)]
struct ResolverAffiliationSyncSlot {
    latest: ResolverAffiliationSyncWork,
    version: u64,
}

#[derive(Debug, Clone)]
struct ResolverAffiliationSyncCompletion {
    key: ResolverAffiliationSyncWorkerKey,
    source_admission_revision: u64,
    resulting_admission_revision: u64,
}

#[derive(Debug)]
struct ResolverAffiliationSyncState {
    workers:
        std::collections::HashMap<ResolverAffiliationSyncWorkerKey, ResolverAffiliationSyncSlot>,
    recent_completions: std::collections::VecDeque<ResolverAffiliationSyncCompletion>,
    max_workers: usize,
}

fn take_completion_revision(
    state: &mut ResolverAffiliationSyncState,
    key: &ResolverAffiliationSyncWorkerKey,
    source_admission_revision: u64,
) -> Option<u64> {
    let position = state
        .recent_completions
        .iter()
        .position(|completion| completion.key == *key)?;
    let completion = &state.recent_completions[position];
    if completion.source_admission_revision > source_admission_revision {
        return None;
    }
    let completion = state
        .recent_completions
        .remove(position)
        .expect("resolver sync completion position");
    (completion.source_admission_revision == source_admission_revision)
        .then_some(completion.resulting_admission_revision)
}

fn has_newer_completion(
    state: &ResolverAffiliationSyncState,
    key: &ResolverAffiliationSyncWorkerKey,
    source_admission_revision: u64,
) -> bool {
    state.recent_completions.iter().any(|completion| {
        completion.key == *key && completion.source_admission_revision > source_admission_revision
    })
}

fn clear_completion_through(
    state: &mut ResolverAffiliationSyncState,
    key: &ResolverAffiliationSyncWorkerKey,
    source_admission_revision: u64,
) {
    if let Some(position) = state.recent_completions.iter().position(|completion| {
        completion.key == *key && completion.source_admission_revision <= source_admission_revision
    }) {
        state.recent_completions.remove(position);
    }
}

fn retain_completion(
    state: &mut ResolverAffiliationSyncState,
    key: &ResolverAffiliationSyncWorkerKey,
    source_admission_revision: u64,
    resulting_admission_revision: u64,
) {
    if let Some(position) = state
        .recent_completions
        .iter()
        .position(|completion| completion.key == *key)
    {
        if state.recent_completions[position].source_admission_revision > source_admission_revision
        {
            return;
        }
        state.recent_completions.remove(position);
    }
    if state.recent_completions.len() >= MAX_RECENT_RESOLVER_AFFILIATION_SYNC_COMPLETIONS {
        state.recent_completions.pop_front();
    }
    state
        .recent_completions
        .push_back(ResolverAffiliationSyncCompletion {
            key: key.clone(),
            source_admission_revision,
            resulting_admission_revision,
        });
}

/// Result of offering a resolver verdict to the bounded scheduler.
pub enum ResolverAffiliationSyncSchedule {
    /// A new logical worker owns this key and must be spawned by the caller.
    Started(Box<ResolverAffiliationSyncWorker>),
    /// An existing worker atomically retained this newer verdict.
    Updated,
    /// The existing worker already owns an identical latest verdict.
    Coalesced,
    /// A newer source snapshot is already retained for this worker.
    Stale,
    /// All global worker slots are occupied by other logical keys.
    AtCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverAffiliationSyncTerminalDisposition {
    /// No actor admission state changed, so pending same-source work retains
    /// the effective revision already authorized for this worker.
    NonMutatingExhaustion,
    /// The actor outcome invalidated the current chain; pending work must
    /// establish its own source revision or consume another retained chain.
    InvalidatingOutcome,
}

/// Bounds detached resolver-affiliation repair to one latest-wins worker per
/// `(actor incarnation, room, member)`. The worker table and close/update
/// handshake share one mutex, so a verdict cannot land between a worker's
/// final empty check and removal.
#[derive(Debug)]
pub struct ResolverAffiliationSyncScheduler {
    state: std::sync::Mutex<ResolverAffiliationSyncState>,
}

pub struct ResolverAffiliationSyncWorker {
    scheduler: Arc<ResolverAffiliationSyncScheduler>,
    key: ResolverAffiliationSyncWorkerKey,
    current: ResolverAffiliationSyncWork,
    effective_admission_revision: u64,
    version: u64,
    closed: bool,
}

impl Default for ResolverAffiliationSyncScheduler {
    fn default() -> Self {
        Self::with_capacity(MAX_RESOLVER_AFFILIATION_SYNC_WORKERS)
    }
}

impl ResolverAffiliationSyncScheduler {
    pub(crate) fn with_capacity(max_workers: usize) -> Self {
        Self {
            state: std::sync::Mutex::new(ResolverAffiliationSyncState {
                workers: std::collections::HashMap::new(),
                recent_completions: std::collections::VecDeque::new(),
                max_workers,
            }),
        }
    }

    pub fn schedule(
        self: &Arc<Self>,
        room_jid: &BareJid,
        jid: &BareJid,
        actor_id: kameo::actor::ActorId,
        work: ResolverAffiliationSyncWork,
    ) -> ResolverAffiliationSyncSchedule {
        let key = ResolverAffiliationSyncWorkerKey {
            room_jid: room_jid.clone(),
            jid: jid.clone(),
            actor_id,
        };
        let mut state = self.state.lock().expect("resolver sync scheduler lock");
        if let Some(slot) = state.workers.get_mut(&key) {
            if work.expected_admission_revision < slot.latest.expected_admission_revision {
                return ResolverAffiliationSyncSchedule::Stale;
            }
            if slot.latest == work {
                return ResolverAffiliationSyncSchedule::Coalesced;
            }
            slot.latest = work;
            slot.version = slot
                .version
                .checked_add(1)
                .expect("resolver sync worker version overflow");
            return ResolverAffiliationSyncSchedule::Updated;
        }
        if has_newer_completion(&state, &key, work.expected_admission_revision) {
            return ResolverAffiliationSyncSchedule::Stale;
        }
        if state.workers.len() >= state.max_workers {
            waddle_xmpp::telemetry::reliability::increment_resolver_affiliation_sync_capacity_drop(
            );
            return ResolverAffiliationSyncSchedule::AtCapacity;
        }
        let effective_admission_revision =
            take_completion_revision(&mut state, &key, work.expected_admission_revision)
                .unwrap_or(work.expected_admission_revision);
        let version = 0;
        state.workers.insert(
            key.clone(),
            ResolverAffiliationSyncSlot {
                latest: work,
                version,
            },
        );
        ResolverAffiliationSyncSchedule::Started(Box::new(ResolverAffiliationSyncWorker {
            scheduler: Arc::clone(self),
            key,
            current: work,
            effective_admission_revision,
            version,
            closed: false,
        }))
    }
}

impl ResolverAffiliationSyncWorker {
    pub fn current(&self) -> ResolverAffiliationSyncWork {
        self.current
    }

    pub fn effective_admission_revision(&self) -> u64 {
        self.effective_admission_revision
    }

    /// Close all repair state for an actor incarnation that can no longer
    /// accept mutations. Pending verdicts and retained revision chains are
    /// obsolete once the actor is sealed, so release global capacity before
    /// any potentially slow registry cleanup.
    pub fn close_actor_terminal(&mut self) {
        let mut state = self
            .scheduler
            .state
            .lock()
            .expect("resolver sync scheduler lock");
        state.workers.remove(&self.key);
        state
            .recent_completions
            .retain(|completion| completion.key != self.key);
        self.closed = true;
    }

    /// Adopt the most recent verdict when one arrived while the current
    /// verdict was running. Leaves the worker registered when unchanged.
    pub fn take_update(&mut self) -> Option<ResolverAffiliationSyncWork> {
        let mut state = self
            .scheduler
            .state
            .lock()
            .expect("resolver sync scheduler lock");
        let slot = *state
            .workers
            .get(&self.key)
            .expect("live resolver sync worker slot");
        if slot.version == self.version {
            return None;
        }
        let previous_source_admission_revision = self.current.expected_admission_revision;
        self.version = slot.version;
        self.current = slot.latest;
        if self.current.expected_admission_revision != previous_source_admission_revision {
            self.effective_admission_revision = take_completion_revision(
                &mut state,
                &self.key,
                self.current.expected_admission_revision,
            )
            .unwrap_or(self.current.expected_admission_revision);
        }
        Some(self.current)
    }

    /// Atomically adopt an update or retain this applied repair's exact
    /// revision chain for one later worker with the same source snapshot.
    /// A concurrent `schedule` call must run before or after this lock, so it
    /// can never publish into a slot after the worker decided to exit.
    pub fn finish_applied_or_take_update(
        &mut self,
        resulting_admission_revision: u64,
    ) -> Option<ResolverAffiliationSyncWork> {
        let mut state = self
            .scheduler
            .state
            .lock()
            .expect("resolver sync scheduler lock");
        let slot = *state
            .workers
            .get(&self.key)
            .expect("live resolver sync worker slot");
        if slot.version != self.version {
            let completed_source_admission_revision = self.current.expected_admission_revision;
            self.version = slot.version;
            self.current = slot.latest;
            self.effective_admission_revision = if self.current.expected_admission_revision
                == completed_source_admission_revision
            {
                resulting_admission_revision
            } else {
                take_completion_revision(
                    &mut state,
                    &self.key,
                    self.current.expected_admission_revision,
                )
                .unwrap_or(self.current.expected_admission_revision)
            };
            return Some(self.current);
        }
        state.workers.remove(&self.key);
        retain_completion(
            &mut state,
            &self.key,
            self.current.expected_admission_revision,
            resulting_admission_revision,
        );
        self.closed = true;
        None
    }

    /// Atomically adopt a pending update or close with disposition-specific
    /// revision handling. Non-mutating exhaustion retains only the exact
    /// revision already authorized for this worker; invalidating outcomes
    /// clear that chain.
    pub fn finish_terminal_or_take_update(
        &mut self,
        disposition: ResolverAffiliationSyncTerminalDisposition,
    ) -> Option<ResolverAffiliationSyncWork> {
        let mut state = self
            .scheduler
            .state
            .lock()
            .expect("resolver sync scheduler lock");
        let slot = *state
            .workers
            .get(&self.key)
            .expect("live resolver sync worker slot");
        if slot.version != self.version {
            let completed_source_admission_revision = self.current.expected_admission_revision;
            let completed_effective_admission_revision = self.effective_admission_revision;
            self.version = slot.version;
            self.current = slot.latest;
            self.effective_admission_revision = if disposition
                == ResolverAffiliationSyncTerminalDisposition::NonMutatingExhaustion
                && self.current.expected_admission_revision == completed_source_admission_revision
            {
                completed_effective_admission_revision
            } else {
                take_completion_revision(
                    &mut state,
                    &self.key,
                    self.current.expected_admission_revision,
                )
                .unwrap_or(self.current.expected_admission_revision)
            };
            return Some(self.current);
        }
        state.workers.remove(&self.key);
        match disposition {
            ResolverAffiliationSyncTerminalDisposition::NonMutatingExhaustion => {
                retain_completion(
                    &mut state,
                    &self.key,
                    self.current.expected_admission_revision,
                    self.effective_admission_revision,
                );
            }
            ResolverAffiliationSyncTerminalDisposition::InvalidatingOutcome => {
                clear_completion_through(
                    &mut state,
                    &self.key,
                    self.current.expected_admission_revision,
                );
            }
        }
        self.closed = true;
        None
    }
}

impl Drop for ResolverAffiliationSyncWorker {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let mut state = self
            .scheduler
            .state
            .lock()
            .expect("resolver sync scheduler lock");
        state.workers.remove(&self.key);
        clear_completion_through(
            &mut state,
            &self.key,
            self.current.expected_admission_revision,
        );
    }
}

#[cfg(test)]
mod resolver_affiliation_sync_scheduler_tests {
    use super::*;

    #[test]
    fn scheduler_keeps_one_worker_and_replaces_its_pending_verdict() {
        let scheduler = Arc::new(ResolverAffiliationSyncScheduler::default());
        let room: BareJid = "room@muc.example.com".parse().expect("room JID");
        let alice: BareJid = "alice@example.com".parse().expect("member JID");
        let bob: BareJid = "bob@example.com".parse().expect("member JID");
        let actor_a = kameo::actor::ActorId::new(1);
        let actor_b = kameo::actor::ActorId::new(2);
        let initial = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::None,
            expected_admission_revision: 7,
        };
        let newer = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::Outcast,
            expected_admission_revision: 7,
        };

        let ResolverAffiliationSyncSchedule::Started(mut alice_worker) =
            scheduler.schedule(&room, &alice, actor_a, initial)
        else {
            panic!("first repair starts a worker");
        };
        assert!(
            matches!(
                scheduler.schedule(&room, &alice, actor_a, initial),
                ResolverAffiliationSyncSchedule::Coalesced
            ),
            "identical latest work must coalesce"
        );
        assert!(
            matches!(
                scheduler.schedule(&room, &alice, actor_a, newer),
                ResolverAffiliationSyncSchedule::Updated
            ),
            "a conflicting verdict updates the existing worker"
        );
        assert_eq!(alice_worker.take_update(), Some(newer));

        assert!(matches!(
            scheduler.schedule(&room, &alice, actor_b, initial),
            ResolverAffiliationSyncSchedule::Started(_)
        ));
        assert!(matches!(
            scheduler.schedule(&room, &bob, actor_a, initial),
            ResolverAffiliationSyncSchedule::Started(_)
        ));
    }

    #[test]
    fn scheduler_rejects_a_stale_update_when_a_newer_revision_is_queued() {
        let scheduler = Arc::new(ResolverAffiliationSyncScheduler::default());
        let room: BareJid = "room@muc.example.com".parse().expect("room JID");
        let member: BareJid = "member@example.com".parse().expect("member JID");
        let actor_id = kameo::actor::ActorId::new(1);
        let initial = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::None,
            expected_admission_revision: 0,
        };
        let newer = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::Member,
            expected_admission_revision: 2,
        };
        let stale = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::Outcast,
            expected_admission_revision: 1,
        };

        let ResolverAffiliationSyncSchedule::Started(mut worker) =
            scheduler.schedule(&room, &member, actor_id, initial)
        else {
            panic!("first repair starts a worker");
        };
        assert!(matches!(
            scheduler.schedule(&room, &member, actor_id, newer),
            ResolverAffiliationSyncSchedule::Updated
        ));
        assert!(matches!(
            scheduler.schedule(&room, &member, actor_id, stale),
            ResolverAffiliationSyncSchedule::Stale
        ));
        assert_eq!(
            worker.take_update(),
            Some(newer),
            "out-of-order stale completion must not replace the newer queued verdict"
        );
    }

    #[test]
    fn scheduler_close_atomically_adopts_an_update_or_releases_capacity() {
        let scheduler = Arc::new(ResolverAffiliationSyncScheduler::with_capacity(1));
        let room: BareJid = "room@muc.example.com".parse().expect("room JID");
        let alice: BareJid = "alice@example.com".parse().expect("member JID");
        let bob: BareJid = "bob@example.com".parse().expect("member JID");
        let actor_id = kameo::actor::ActorId::new(1);
        let initial = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::None,
            expected_admission_revision: 0,
        };
        let newer = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::Outcast,
            expected_admission_revision: 0,
        };

        let ResolverAffiliationSyncSchedule::Started(mut worker) =
            scheduler.schedule(&room, &alice, actor_id, initial)
        else {
            panic!("first repair starts a worker");
        };
        assert!(matches!(
            scheduler.schedule(&room, &bob, actor_id, initial),
            ResolverAffiliationSyncSchedule::AtCapacity
        ));
        assert!(matches!(
            scheduler.schedule(&room, &alice, actor_id, newer),
            ResolverAffiliationSyncSchedule::Updated
        ));
        assert_eq!(worker.finish_applied_or_take_update(1), Some(newer));
        assert_eq!(worker.finish_applied_or_take_update(2), None);

        let ResolverAffiliationSyncSchedule::Started(chained_worker) =
            scheduler.schedule(&room, &alice, actor_id, initial)
        else {
            panic!("a later same-snapshot repair starts a worker");
        };
        assert_eq!(
            chained_worker.effective_admission_revision(),
            2,
            "a completed repair carries its exact resulting revision across worker closure"
        );
        drop(chained_worker);

        assert!(
            matches!(
                scheduler.schedule(&room, &bob, actor_id, initial),
                ResolverAffiliationSyncSchedule::Started(_)
            ),
            "atomic close must release the global worker slot"
        );
    }

    #[test]
    fn scheduler_actor_terminal_close_discards_updates_and_releases_capacity() {
        let scheduler = Arc::new(ResolverAffiliationSyncScheduler::with_capacity(1));
        let room: BareJid = "room@muc.example.com".parse().expect("room JID");
        let alice: BareJid = "alice@example.com".parse().expect("member JID");
        let bob: BareJid = "bob@example.com".parse().expect("member JID");
        let actor_id = kameo::actor::ActorId::new(1);
        let initial = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::None,
            expected_admission_revision: 0,
        };
        let queued = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::Outcast,
            expected_admission_revision: 1,
        };

        let ResolverAffiliationSyncSchedule::Started(mut worker) =
            scheduler.schedule(&room, &alice, actor_id, initial)
        else {
            panic!("first repair starts a worker");
        };
        assert!(matches!(
            scheduler.schedule(&room, &alice, actor_id, queued),
            ResolverAffiliationSyncSchedule::Updated
        ));

        worker.close_actor_terminal();

        let ResolverAffiliationSyncSchedule::Started(_bob_worker) =
            scheduler.schedule(&room, &bob, actor_id, initial)
        else {
            panic!("actor-terminal close releases capacity for Bob");
        };
        let state = scheduler
            .state
            .lock()
            .expect("resolver sync scheduler lock");
        assert_eq!(state.workers.len(), 1, "only Bob's worker remains");
        assert!(
            state.workers.keys().all(|key| key.jid == bob),
            "queued work for the sealed actor/member was discarded"
        );
        assert!(state.recent_completions.is_empty());
    }

    #[test]
    fn scheduler_bounds_recent_completion_chains_independently() {
        let scheduler = Arc::new(ResolverAffiliationSyncScheduler::with_capacity(1));
        let room: BareJid = "room@muc.example.com".parse().expect("room JID");
        let actor_id = kameo::actor::ActorId::new(1);
        let work = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::Member,
            expected_admission_revision: 0,
        };

        for index in 0..=MAX_RECENT_RESOLVER_AFFILIATION_SYNC_COMPLETIONS {
            let member: BareJid = format!("member-{index}@example.com")
                .parse()
                .expect("member JID");
            let ResolverAffiliationSyncSchedule::Started(mut worker) =
                scheduler.schedule(&room, &member, actor_id, work)
            else {
                panic!("active capacity is reusable while completions accumulate");
            };
            assert_eq!(worker.finish_applied_or_take_update(1), None);
        }

        let state = scheduler
            .state
            .lock()
            .expect("resolver sync scheduler lock");
        assert!(state.workers.is_empty());
        assert_eq!(
            state.recent_completions.len(),
            MAX_RECENT_RESOLVER_AFFILIATION_SYNC_COMPLETIONS,
        );
        assert!(state
            .recent_completions
            .iter()
            .all(|completion| { completion.key.jid.as_str() != "member-0@example.com" }));
    }

    #[test]
    fn scheduler_rejects_stale_work_without_consuming_a_newer_completion() {
        let scheduler = Arc::new(ResolverAffiliationSyncScheduler::with_capacity(1));
        let room: BareJid = "room@muc.example.com".parse().expect("room JID");
        let member: BareJid = "member@example.com".parse().expect("member JID");
        let actor_id = kameo::actor::ActorId::new(1);
        let newer = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::None,
            expected_admission_revision: 1,
        };
        let stale = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::Member,
            expected_admission_revision: 0,
        };

        let ResolverAffiliationSyncSchedule::Started(mut newer_worker) =
            scheduler.schedule(&room, &member, actor_id, newer)
        else {
            panic!("newer repair starts a worker");
        };
        assert_eq!(newer_worker.finish_applied_or_take_update(2), None);

        assert!(matches!(
            scheduler.schedule(&room, &member, actor_id, stale),
            ResolverAffiliationSyncSchedule::Stale
        ));

        let ResolverAffiliationSyncSchedule::Started(chained_worker) =
            scheduler.schedule(&room, &member, actor_id, newer)
        else {
            panic!("matching newer repair starts after the stale worker drops");
        };
        assert_eq!(
            chained_worker.effective_admission_revision(),
            2,
            "rejecting stale work must preserve the newer source revision chain"
        );
    }

    #[tokio::test]
    async fn scheduler_capacity_drop_is_exported_as_a_counter() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let scheduler = Arc::new(ResolverAffiliationSyncScheduler::with_capacity(1));
        let room: BareJid = "room@muc.example.com".parse().expect("room JID");
        let alice: BareJid = "alice@example.com".parse().expect("member JID");
        let bob: BareJid = "bob@example.com".parse().expect("member JID");
        let actor_id = kameo::actor::ActorId::new(1);
        let work = ResolverAffiliationSyncWork {
            affiliation: waddle_xmpp::Affiliation::None,
            expected_admission_revision: 0,
        };

        let ResolverAffiliationSyncSchedule::Started(_worker) =
            scheduler.schedule(&room, &alice, actor_id, work)
        else {
            panic!("first repair occupies the only worker slot");
        };
        assert!(matches!(
            scheduler.schedule(&room, &bob, actor_id, work),
            ResolverAffiliationSyncSchedule::AtCapacity
        ));

        assert_eq!(
            metrics.counter_sum("xmpp.resolver.affiliation_sync_capacity_drop", &[]),
            Some(1),
            "capacity rejection must increment the exported counter"
        );
    }
}

#[derive(Debug, Clone)]
pub struct PendingDmCallOffer {
    pub media: waddle_xmpp::xep::CallThreadMedia,
    pub initiator: BareJid,
    pub started: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Default)]
pub struct RemoteMucMemberships {
    entries: dashmap::DashMap<(FullJid, BareJid), RemoteMucMembershipEntry>,
    locks: dashmap::DashMap<(FullJid, BareJid), std::sync::Arc<tokio::sync::Mutex<()>>>,
    next_generation: std::sync::atomic::AtomicU64,
}

pub struct RemoteMucMembershipLockGuard {
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

#[derive(Debug, Clone)]
enum RemoteMucMembershipEntry {
    Active(RemoteMucMembership),
    Tombstone { generation: u64 },
}

#[derive(Debug, Clone)]
struct RemoteMucMembership {
    nick: String,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMucMembershipSnapshot {
    occupant: FullJid,
    room: BareJid,
    nick: String,
    generation: u64,
}

impl RemoteMucMembershipSnapshot {
    pub fn room(&self) -> &BareJid {
        &self.room
    }

    pub fn nick(&self) -> &str {
        &self.nick
    }
}

impl RemoteMucMemberships {
    pub async fn lock_membership(
        &self,
        occupant: &FullJid,
        room: &BareJid,
    ) -> RemoteMucMembershipLockGuard {
        let lock = self
            .locks
            .entry((occupant.clone(), room.clone()))
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        RemoteMucMembershipLockGuard {
            _guard: lock.lock_owned().await,
        }
    }

    pub async fn lock_snapshot(
        &self,
        snapshot: &RemoteMucMembershipSnapshot,
    ) -> RemoteMucMembershipLockGuard {
        self.lock_membership(&snapshot.occupant, &snapshot.room)
            .await
    }

    pub fn record_join(&self, occupant: &FullJid, room: &BareJid, nick: &str) {
        let generation = self.next_generation();
        self.entries.insert(
            (occupant.clone(), room.clone()),
            RemoteMucMembershipEntry::Active(RemoteMucMembership {
                nick: nick.to_string(),
                generation,
            }),
        );
    }

    pub fn record_leave(&self, occupant: &FullJid, room: &BareJid) {
        let generation = self.next_generation();
        self.entries.insert(
            (occupant.clone(), room.clone()),
            RemoteMucMembershipEntry::Tombstone { generation },
        );
    }

    pub fn contains(&self, occupant: &FullJid, room: &BareJid) -> bool {
        self.entries
            .get(&(occupant.clone(), room.clone()))
            .is_some_and(|entry| matches!(entry.value(), RemoteMucMembershipEntry::Active(_)))
    }

    pub fn nick_for(&self, occupant: &FullJid, room: &BareJid) -> Option<String> {
        self.entries
            .get(&(occupant.clone(), room.clone()))
            .and_then(|entry| match entry.value() {
                RemoteMucMembershipEntry::Active(membership) => Some(membership.nick.clone()),
                RemoteMucMembershipEntry::Tombstone { .. } => None,
            })
    }

    /// Distinct occupant full JIDs that still hold at least one ACTIVE
    /// remote-room membership (#1249). The reconciliation janitor scans
    /// these to re-drive unavailable relays whose first attempt failed
    /// (remote node unreachable, claim held elsewhere) — without this,
    /// a failed cleanup ghosted the occupant on the remote node until
    /// the same full JID happened to disconnect again.
    pub fn occupants_with_active_memberships(&self) -> Vec<FullJid> {
        let mut occupants: Vec<FullJid> = self
            .entries
            .iter()
            .filter_map(|entry| match entry.value() {
                RemoteMucMembershipEntry::Active(_) => Some(entry.key().0.clone()),
                RemoteMucMembershipEntry::Tombstone { .. } => None,
            })
            .collect();
        occupants.sort_unstable_by(|a, b| a.as_str().cmp(b.as_str()));
        occupants.dedup();
        occupants
    }

    pub fn take_for_occupant(&self, occupant: &FullJid) -> Vec<RemoteMucMembershipSnapshot> {
        let snapshots: Vec<RemoteMucMembershipSnapshot> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let (entry_occupant, room) = entry.key();
                if entry_occupant != occupant {
                    return None;
                }
                let RemoteMucMembershipEntry::Active(membership) = entry.value() else {
                    return None;
                };
                Some(RemoteMucMembershipSnapshot {
                    occupant: entry_occupant.clone(),
                    room: room.clone(),
                    nick: membership.nick.clone(),
                    generation: membership.generation,
                })
            })
            .collect();
        snapshots
            .into_iter()
            .filter_map(|snapshot| self.mark_snapshot_taken(snapshot))
            .collect()
    }

    pub fn restore_snapshot_if_current(&self, snapshot: &RemoteMucMembershipSnapshot) {
        let key = (snapshot.occupant.clone(), snapshot.room.clone());
        if let dashmap::mapref::entry::Entry::Occupied(mut entry) = self.entries.entry(key) {
            if matches!(
                entry.get(),
                RemoteMucMembershipEntry::Tombstone { generation }
                    if *generation == snapshot.generation
            ) {
                entry.insert(RemoteMucMembershipEntry::Active(RemoteMucMembership {
                    nick: snapshot.nick.clone(),
                    generation: snapshot.generation,
                }));
            }
        }
    }

    pub fn forget_snapshot_if_current(&self, snapshot: &RemoteMucMembershipSnapshot) {
        let key = (snapshot.occupant.clone(), snapshot.room.clone());
        self.entries.remove_if(&key, |_, current| {
            matches!(
                current,
                RemoteMucMembershipEntry::Tombstone { generation }
                    if *generation == snapshot.generation
            )
        });
    }

    pub fn snapshot_is_current_tombstone(&self, snapshot: &RemoteMucMembershipSnapshot) -> bool {
        let key = (snapshot.occupant.clone(), snapshot.room.clone());
        self.entries.get(&key).is_some_and(|entry| {
            matches!(
                entry.value(),
                RemoteMucMembershipEntry::Tombstone { generation }
                    if *generation == snapshot.generation
            )
        })
    }

    fn mark_snapshot_taken(
        &self,
        snapshot: RemoteMucMembershipSnapshot,
    ) -> Option<RemoteMucMembershipSnapshot> {
        let key = (snapshot.occupant.clone(), snapshot.room.clone());
        let mut entry = self.entries.get_mut(&key)?;
        match entry.value() {
            RemoteMucMembershipEntry::Active(membership)
                if membership.generation == snapshot.generation =>
            {
                *entry = RemoteMucMembershipEntry::Tombstone {
                    generation: snapshot.generation,
                };
                Some(snapshot)
            }
            RemoteMucMembershipEntry::Active(_) | RemoteMucMembershipEntry::Tombstone { .. } => {
                None
            }
        }
    }

    fn next_generation(&self) -> u64 {
        self.next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod remote_muc_membership_tests {
    use super::*;

    fn full_jid(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn room_bare_jid(local: &str) -> BareJid {
        format!("{local}@muc.example.com")
            .parse()
            .expect("bare jid")
    }

    /// #1249: the janitor enumeration surfaces occupants with ACTIVE
    /// memberships only — tombstones (cleanup in flight) are invisible,
    /// and restoring a snapshot makes the occupant visible again so the
    /// re-drive loop converges.
    #[test]
    fn occupants_with_active_memberships_tracks_restore_and_forget() {
        let memberships = RemoteMucMemberships::default();
        let occupant = full_jid("carol@example.com/web");
        let room = room_bare_jid("reconcile");

        assert!(memberships.occupants_with_active_memberships().is_empty());
        memberships.record_join(&occupant, &room, "carol");
        assert_eq!(
            memberships.occupants_with_active_memberships(),
            vec![occupant.clone()]
        );

        let snapshots = memberships.take_for_occupant(&occupant);
        assert_eq!(snapshots.len(), 1);
        assert!(
            memberships.occupants_with_active_memberships().is_empty(),
            "a tombstoned membership must not be re-driven concurrently"
        );

        memberships.restore_snapshot_if_current(&snapshots[0]);
        assert_eq!(
            memberships.occupants_with_active_memberships(),
            vec![occupant.clone()],
            "a restored (failed-relay) membership re-enters the janitor scan"
        );

        let snapshots = memberships.take_for_occupant(&occupant);
        memberships.forget_snapshot_if_current(&snapshots[0]);
        assert!(memberships.occupants_with_active_memberships().is_empty());
    }

    #[test]
    fn stale_snapshot_does_not_remove_newer_same_nick_join() {
        let memberships = RemoteMucMemberships::default();
        let occupant = full_jid("alice@example.com/web");
        let room = room_bare_jid("race");

        memberships.record_join(&occupant, &room, "alice");
        let stale_snapshots = memberships.take_for_occupant(&occupant);
        assert_eq!(stale_snapshots.len(), 1);
        assert!(memberships.snapshot_is_current_tombstone(&stale_snapshots[0]));

        memberships.record_join(&occupant, &room, "alice");
        assert!(!memberships.snapshot_is_current_tombstone(&stale_snapshots[0]));
        memberships.restore_snapshot_if_current(&stale_snapshots[0]);

        assert_eq!(
            memberships.nick_for(&occupant, &room).as_deref(),
            Some("alice")
        );
    }

    #[test]
    fn stale_snapshot_does_not_resurrect_after_newer_join_leaves() {
        let memberships = RemoteMucMemberships::default();
        let occupant = full_jid("alice@example.com/web");
        let room = room_bare_jid("race-left");

        memberships.record_join(&occupant, &room, "alice");
        let stale_snapshots = memberships.take_for_occupant(&occupant);
        assert_eq!(stale_snapshots.len(), 1);
        assert!(memberships.snapshot_is_current_tombstone(&stale_snapshots[0]));

        memberships.record_join(&occupant, &room, "alice");
        memberships.record_leave(&occupant, &room);
        assert!(!memberships.snapshot_is_current_tombstone(&stale_snapshots[0]));
        memberships.restore_snapshot_if_current(&stale_snapshots[0]);

        assert_eq!(memberships.nick_for(&occupant, &room), None);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DmPairKey {
    pub low_peer: BareJid,
    pub high_peer: BareJid,
}

impl DmPairKey {
    pub fn new(a: BareJid, b: BareJid) -> Self {
        let (low_peer, high_peer) = if a.as_str() <= b.as_str() {
            (a, b)
        } else {
            (b, a)
        };
        Self {
            low_peer,
            high_peer,
        }
    }

    pub fn contains(&self, jid: &BareJid) -> bool {
        self.low_peer == *jid || self.high_peer == *jid
    }
}

#[derive(Debug, Default)]
pub struct DmPinStore {
    entries: dashmap::DashMap<DmPairKey, Vec<waddle_xmpp::muc::PinnedEntry>>,
}

impl DmPinStore {
    pub fn apply_pin(&self, key: DmPairKey, entry: waddle_xmpp::muc::PinnedEntry) {
        let target_stanza_id = entry.target_stanza_id.clone();
        let mut entries = self.entries.entry(key).or_default();
        entries.retain(|existing| existing.target_stanza_id.id != target_stanza_id.id);
        entries.insert(0, entry);
        if entries.len() > waddle_xmpp::muc::pin::MAX_PINNED_ENTRIES {
            entries.pop();
        }
    }

    pub fn list(&self, key: &DmPairKey) -> Vec<waddle_xmpp::muc::PinnedEntry> {
        self.entries
            .get(key)
            .map(|entries| entries.clone())
            .unwrap_or_default()
    }

    pub fn unpin(
        &self,
        key: &DmPairKey,
        target_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    ) -> bool {
        let Some(mut entries) = self.entries.get_mut(key) else {
            return false;
        };
        let before = entries.len();
        entries.retain(|entry| entry.target_stanza_id.id != target_stanza_id.id);
        entries.len() != before
    }

    pub fn contains(
        &self,
        key: &DmPairKey,
        target_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    ) -> bool {
        self.entries
            .get(key)
            .map(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.target_stanza_id.id == target_stanza_id.id)
            })
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct XmppServiceDomains {
    pub muc: String,
    pub spaces: String,
    pub upload: String,
    pub extensions: String,
    pub push: String,
    /// Hosts community-wide pubsub features that are NOT space-bookmark
    /// nodes (XEP-0472 social feed, XEP-0501 stories). Kept distinct
    /// from `spaces` so the spaces disco#items enumeration only
    /// surfaces actual community spaces.
    pub community: String,
}

impl XmppServiceDomains {
    /// `muc` and `spaces` are the deployment-authoritative service domains —
    /// overridable via `WADDLE_MUC_DOMAIN` / `WADDLE_SPACES_JID` — and MUST be
    /// the exact values the MUC room registry and spaces component are built
    /// from. disco#info routing compares targets against `muc` (#757); a
    /// re-derived `muc.<domain>` would diverge from the registry under an
    /// override and silently break room disco#info. The remaining components
    /// have no override and are derived from `xmpp_domain`.
    pub fn new(xmpp_domain: &str, muc: &str, spaces: &str) -> Self {
        Self {
            muc: muc.to_string(),
            spaces: spaces.to_string(),
            upload: format!("upload.{xmpp_domain}"),
            extensions: format!("extensions.{xmpp_domain}"),
            push: format!("push.{xmpp_domain}"),
            community: format!("community.{xmpp_domain}"),
        }
    }
}

/// WebSocket route dependencies kept narrower than the full server graph.
pub struct WebSocketState {
    pub deps: WebSocketDeps,
}

pub struct WebSocketDeps {
    /// Core app state for accessing the global and per-waddle databases.
    pub app_state: Arc<AppState>,
    /// Authentication state for session validation.
    pub auth_state: Arc<AuthState>,
    /// Authoritative XMPP component/service JIDs used by this deployment.
    pub service_domains: XmppServiceDomains,
    /// Protocol/runtime services used by the WebSocket C2S path.
    pub protocol: ProtocolServices,
    /// Per-deployment XEP-0421 occupant-id HMAC key. Cheap-clone via the
    /// inner `Arc<[u8]>`; shared with `RoomRegistryActor` so every
    /// stamping site uses the same key.
    pub occupant_id_secret: OccupantIdSecret,
    /// Operator controls for server-side link preview enrichment.
    pub link_preview: crate::config::LinkPreviewConfig,
    /// Snapshot of `WADDLE_PROVIDER_*_WEBHOOK_*` env vars, built once at
    /// startup; the extension webhook handler reads from this instead of
    /// re-parsing env on every request.
    pub provider_ingress: Arc<crate::server::routes::extension_webhooks::ProviderIngressRegistry>,
    /// Tracker for spawned provider webhook dispatch tasks, drained on
    /// graceful shutdown so in-flight deliveries can finish updating the
    /// ledger before runtime teardown.
    pub provider_dispatch_tasks: crate::server::routes::extension_webhooks::ProviderDispatchTracker,
    /// RFC 7395 §3.8 keepalive knobs (issue #1090), parsed and
    /// validated at startup from `WADDLE_WS_KEEPALIVE_*` env vars.
    pub ws_keepalive: waddle_xmpp::protocol::KeepaliveConfig,
    /// Ecdysis graceful-shutdown view (issue #1091): the accept path
    /// mints a [`waddle_ecdysis::ConnectionGuard`] per connection so
    /// `drain()` tracks real connections, and the per-connection loop
    /// observes the stop token to close live sessions with
    /// `<stream:error><system-shutdown/>`.
    pub shutdown: waddle_ecdysis::ShutdownHandle,
}

pub struct ProtocolServices {
    /// Registry for tracking active connections by JID.
    pub connection_registry: Arc<ConnectionRegistry>,
    /// Actor-backed per-user registry (ADR-0017 Phase 1). Populated in
    /// lock-step with `connection_registry` on the live register/unregister
    /// path. Bare-JID routing selection (`route_to_connection`) reads the
    /// candidate set + RFC priority ranking from this actor (Slice 1,
    /// intersected with DashMap liveness, provably equal to the legacy
    /// selection), and 1:1/DM delivery routes through the actor's `TrySend*`
    /// (Slice 2, `deliver_*_to_full`) with no DashMap send fallback. Empty
    /// actors left by delivery-path eviction are reaped by
    /// `spawn_user_actor_reaper`.
    pub user_registry: ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    /// Actor-backed registry for MUC rooms.
    pub room_registry: ActorRef<RoomRegistryActor>,
    /// Shared XMPP MAM storage for archived message history.
    pub mam_storage: Arc<dyn MamStorage>,
    /// Shared Waddle inbox projection storage.
    pub inbox_storage: Arc<dyn InboxStorage>,
    /// Per-thread cross-channel view (urn:waddle:threads:0). Reads
    /// from the same backend as the inbox projection.
    pub threads_storage: Arc<dyn crate::threads::storage::ThreadsStorage>,
    /// Shared XEP-0191 blocking-list storage. Used by the headless
    /// offline-recipient pass (#229 PR15) to seed a transient
    /// recipient state machine's blocklist when delivering to a
    /// local-domain bare JID with no available resources.
    pub blocking_storage: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage>,
    /// XEP-0160 offline-message storage. Used by the
    /// [`crate::server::routes::interpret::interpret`] arm for
    /// `OutboundEvent::QueueOfflineDelivery` to persist DM stanzas
    /// during the headless recipient pass (issue #209).
    pub pending_delivery_storage:
        Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage>,
    /// Registry for ad-hoc commands exposed over the WebSocket transport.
    pub command_registry: Arc<CommandRegistry>,
    /// Runtime extension manager for message embeds + feature advertisements.
    pub extension_manager: Arc<ExtensionManager>,
    /// Sans-I/O stanza dispatcher. Handlers migrated so far (ping, session,
    /// carbons) are routed through this before falling back to the
    /// legacy string-matching code paths below.
    pub dispatcher: Arc<StanzaDispatcher>,
    /// WebSocket-layer pre-dispatch limiter for Muji `session-terminate`.
    /// This is intentionally a separate bucket set from the handler's
    /// own defense-in-depth limiter: the websocket path must charge
    /// before room-locality checks or cross-node relays fan out work.
    pub muji_pre_dispatch_terminate_rate_limit:
        Arc<waddle_xmpp::protocol::handlers::session_initiate_rate_limit::TerminateRateLimit>,
    /// WebSocket-layer pre-dispatch limiter for Muji non-initiate actions.
    /// Kept separate from the handler-side limiter so the effective
    /// budget remains unchanged while foreign-room and membership-gated
    /// requests are metered before they trigger expensive checks.
    pub muji_pre_dispatch_action_rate_limit:
        Arc<waddle_xmpp::protocol::handlers::session_initiate_rate_limit::MujiActionRateLimit>,
    /// Shared PubSub/PEP storage (XEP-0060/XEP-0163).
    pub pubsub_storage: Arc<dyn PubSubStorage>,
    /// XEP-0357 push subscription storage.
    pub push_store: Arc<dyn waddle_xmpp::push::PushSubscriptionStore>,
    /// First-party XMPP Push Service state and fake provider dispatch.
    pub push_service: Arc<crate::push_service::DatabasePushServiceStore>,
    /// User-server notification candidate/outbox state. This coalesces
    /// committed XMPP state into durable XEP-0357 PubSub publish jobs while
    /// leaving canonical registration state in `push_store`.
    pub notification_outbox: Arc<crate::notification_outbox::NotificationOutboxStore>,
    /// Durable convergence queue for failed LiveKit admin teardown and
    /// cross-node Muji-presence cleanup effects.
    pub call_teardown_outbox: Arc<crate::call_teardown_outbox::CallTeardownOutboxStore>,
    /// Single supervised producer retry path used when a direct atomic Muji
    /// teardown insertion encounters a transient database failure.
    pub(crate) call_teardown_persistence:
        crate::call_teardown_outbox::CallTeardownPersistenceSupervisor,
    /// Durable, per-room FIFO mutation-effect queue.  Producers own enqueueing
    /// in their mutation transaction; this shared handle is exclusively the
    /// consumer/janitor and arm-supervisor boundary.
    pub room_effect_outbox: Arc<crate::room_effect_outbox::RoomEffectOutboxStore>,
    /// Origin-owned retry path for committed staged config effects.  It has
    /// only the truthful Arm disposition; rollback/supersession deletes rows
    /// transactionally elsewhere.
    pub room_effect_arm_supervisor: crate::room_effect_outbox::RoomEffectArmSupervisor,
    /// Async LiveKit admin view of the concrete SFU, retained separately
    /// from the synchronous protocol service for the teardown janitor.
    pub call_teardown_executor: Option<waddle_sfu::LiveKitTeardownExecutor>,
    /// Durable derived projection of XEP-0492 notification settings from
    /// canonical XMPP state.
    pub notification_settings_projection:
        Arc<crate::notification_settings_projection::NotificationSettingsProjectionStore>,
    /// Durable derived projection of the user's `urn:waddle:dnd:0` PEP
    /// item (#367). Lookup keys: `owner_bare_jid` → typed
    /// [`waddle_xmpp::xep::xep_waddle_dnd::WaddleDnd`]. Consulted at
    /// T1 push dispatch via [`crate::dnd_reader::PepDndReader`].
    pub dnd_projection: Arc<crate::dnd_projection::DndProjectionStore>,
    /// PEP-backed DND reader plumbed into the T1 push gate
    /// (`notification_outbox`). Reads [`dnd_projection`] + a
    /// `chrono::Utc::now()` clock and resolves the typed
    /// [`crate::notification_outbox::DndState`] used to suppress
    /// candidates with [`crate::notification_outbox::SuppressedReason::WaddleDnd`].
    pub dnd_reader: Arc<crate::dnd_reader::PepDndReader>,
    /// Durable per-(user, conversation) activity projection backing
    /// the XEP-0513 `<active/>` push filter. Ingested from typed
    /// XEP-0085 chat-state changes, XEP-0490 read-marker advances,
    /// outbound message commits, and XEP-0045 presence events; read
    /// by the T1 push-gate evaluator (`notification_outbox`).
    pub notification_activity: Arc<crate::notification_activity::NotificationActivityStore>,
    /// XEP-0198 detached-session registry — holds state for clients whose
    /// WebSocket has closed but may still resume within the session timeout.
    pub sm_session_registry: Arc<InMemorySmSessionRegistry>,
    /// Non-blocking shadow executor for durable SM handled-frontier writes.
    pub ingress_shadow: crate::ingress_shadow::IngressShadowHandle,
    /// Bounded admission for deferred link-preview resolver fetches
    /// (#1470). Serial frame dispatch used to throttle lookups to one in
    /// flight per connection as a side effect of blocking; once resolution
    /// moved off the dispatch path the bound must be explicit so a lookup
    /// burst cannot fan out into an unbounded outbound-fetch storm.
    /// Admission is `try_acquire` at IQ dispatch — a saturated resolver
    /// answers `failed` immediately (previews fail open, #822) instead of
    /// queueing tasks whose replies would outlive the client's IQ budget.
    /// Build with [`default_link_preview_resolve_permits`].
    pub link_preview_resolves: Arc<tokio::sync::Semaphore>,
    /// XEP-0115 entity-capabilities resolver. Maintains the
    /// process-wide hash-keyed `CapsCache` plus per-resource caps
    /// mappings used by XEP-0163 §3 fan-out filtering.
    pub caps_resolver: Arc<crate::server::caps_resolution::CapsResolver>,
    /// Per-(BareJid) mutex set guarding the `user_avatar_source`
    /// read-then-publish critical section. Both the OIDC publish
    /// chain (`profile::publish::ensure_pep_profile_published`) and
    /// the wire avatar-publish hook (`pubsub_dispatch` calling
    /// `record_self_published`) acquire the same mutex by
    /// `BareJid`, closing the TOCTOU race where OIDC could read
    /// `'oidc'` between the user's wire publish and the user's
    /// provenance flip and then wipe the just-set avatar.
    ///
    /// Entries are evicted by `AvatarLockGuard::drop` when no
    /// acquirer is in flight, so the map stays bounded by current
    /// contention rather than growing to the lifetime user set.
    pub avatar_source_locks: Arc<crate::profile::AvatarLockMap>,
    /// Tracker for the OIDC → PEP profile-publish background tasks.
    /// The auth callback registers each `tokio::spawn` here so the
    /// graceful-shutdown drain can `wait()` on in-flight publishes
    /// before tearing down the runtime — preventing the split-state
    /// where the empty `<metadata/>` is published but vcard-temp
    /// PHOTO is still set.
    pub profile_publish_tracker: tokio_util::task::TaskTracker,
    /// PEP → community feed bridge. Observes successful PEP publishes
    /// (mood / activity / tune / avatar / vCard4) and shadow-publishes
    /// a typed feed entry on `community.<domain>` so the community
    /// Feed pane surfaces user activity automatically. Holds per-
    /// (user, kind) throttle state.
    pub pep_feed_bridge: Arc<crate::pep_feed_bridge::PepFeedBridge>,
    /// Active MUC call-thread anchors keyed by room bare JID. The room
    /// anchor message records a XEP-0359 origin-id so the SFU webhook path
    /// can later fasten a historical `<call-thread-ended/>` record to that
    /// exact anchor with XEP-0422 `<apply-to/>`.
    pub call_threads: Arc<dashmap::DashMap<BareJid, ActiveCallThread>>,
    /// Per-room serialization of the call-thread "ended" completion so a
    /// webhook delivery and a MUC presence clear racing on the same room
    /// produce exactly one ended broadcast. Explicit state (not a module
    /// static) so the dependency is visible and per-instance in tests.
    pub call_thread_end_locks: Arc<dashmap::DashMap<BareJid, Arc<tokio::sync::Mutex<()>>>>,
    /// Remote-owned MUC rooms this node admitted a local connection into.
    /// Used by unclean disconnect cleanup to send the XEP-0045 unavailable
    /// presence to the authoritative remote RoomActor even though no local
    /// RoomActor exists to discover by registry scan.
    pub remote_muc_memberships: Arc<RemoteMucMemberships>,
    /// At most one detached resolver-affiliation repair per room/member pair.
    /// Rejected joins coalesce here instead of spawning unbounded retry tasks.
    pub resolver_affiliation_syncs: Arc<ResolverAffiliationSyncScheduler>,
    /// Active 1:1 call-thread anchors keyed by the two bare peers plus
    /// JMI/Jingle sid. `proceed` creates the anchor; `finish` later
    /// consumes the entry to stamp the ended summary.
    pub dm_call_threads: Arc<dashmap::DashMap<DmCallThreadKey, ActiveCallThread>>,
    /// Shared pair-scoped 1:1 pinned-message projection. The pair key
    /// deliberately ignores direction so both participants fetch the
    /// same list using their peer's bare JID.
    pub dm_pin_store: Arc<DmPinStore>,
    /// Per-owner projection guard for 1:1 call-thread anchors. `ArchiveDirect`
    /// runs once per user archive row; this lets each owner receive its own
    /// MAM id exactly once while duplicate/replayed proceeds remain inert.
    pub dm_call_thread_projections: Arc<dashmap::DashSet<(BareJid, DmCallThreadKey)>>,
    /// JMI proposals keyed by the two bare peers plus sid so the later
    /// bodyless `<proceed/>` can author a typed call-thread anchor with
    /// the originally offered media.
    pub pending_dm_call_offers: Arc<dashmap::DashMap<DmCallThreadKey, PendingDmCallOffer>>,
    /// LiveKit SFU bridge — `Some` iff `LIVEKIT_*` env vars are set
    /// and `register_call_handlers` was invoked at startup. The
    /// WebSocket cleanup paths call
    /// [`waddle_sfu::SfuService::unregister_call_participant`] when a
    /// session leaves a MUC so a participant's SFU presence doesn't
    /// outlive their XMPP presence (graceful unavailable, tab close,
    /// SM-expiry — all go through `cleanup_muc_presence`).
    pub sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
}

/// Per-node cap for [`ProtocolServices::link_preview_resolves`]. Deliberately
/// generous: the old inline path allowed one concurrent resolve per
/// connection, so node-wide concurrency scaled with connection count.
pub const MAX_CONCURRENT_LINK_PREVIEW_RESOLVES: usize = 32;

/// Fresh admission semaphore for [`ProtocolServices::link_preview_resolves`].
pub fn default_link_preview_resolve_permits() -> Arc<tokio::sync::Semaphore> {
    Arc::new(tokio::sync::Semaphore::new(
        MAX_CONCURRENT_LINK_PREVIEW_RESOLVES,
    ))
}

/// Per-connection mutable state for a single WebSocket XMPP transport.
///
/// Bundles the typed lifecycle phase and the remaining transport/session
/// adjuncts (SM state, carbons flag, suppress-SM-record flag) into a single
/// value threaded through the frame dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InboundFrameTerminal {
    AuthorityRevoked,
}

/// Transport certainty for the server's RFC 7395 `<open/>` response.
///
/// The frame dispatcher handles a client `<open/>` before the batch writer
/// reaches the socket, so lifecycle logic must distinguish that semantic fact
/// from the conservative fact that the entire authority-aware response batch
/// reached its wire commit point.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum StreamOpenWireState {
    #[default]
    NotCommitted,
    PendingResponse,
    Committed,
}

/// A borrowed capability proving that a session came from this WebSocket
/// connection's authenticated state.
///
/// The wrapped [`Session`] remains owned by [`WsConnState`].  Protocol
/// dispatch can borrow it, but cannot manufacture this capability from a
/// standalone session value.  `WsConnState::authenticated_session` is only
/// populated at the successful SASL and durable XEP-0198 resume boundaries.
#[derive(Clone, Copy)]
pub(crate) struct ResolvedPrincipal<'a>(&'a Session);

impl<'a> ResolvedPrincipal<'a> {
    /// Convert an identity already accepted by an authenticated transport
    /// boundary into the dispatch capability. This is crate-visible for the
    /// non-WebSocket interpreter adapters that share the same dispatch path.
    pub(crate) fn from_authenticated_session(session: &'a Session) -> Self {
        Self(session)
    }

    pub(crate) fn session(self) -> &'a Session {
        self.0
    }
}

impl std::ops::Deref for ResolvedPrincipal<'_> {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

pub(super) struct WsConnState {
    pub(super) phase: ConnectionPhase,
    /// Semantic state: the frame dispatcher has handled the client's current
    /// RFC 7395 `<open/>`. This resets on an actual XMPP stream close or
    /// restart, independently from lifecycle revocation.
    pub(super) stream_open_handled: bool,
    /// Conservative transport state for the current server `<open/>` response.
    /// `Committed` is reached only after its entire authority-aware batch
    /// returns `Continue`; lifecycle close uses this state to decide whether a
    /// stream error is legal to send.
    pub(super) stream_open_wire_state: StreamOpenWireState,
    /// The authenticated backend Session for this connection, if any.
    /// Populated on SASL success and used for SM resume/detach.
    pub(super) authenticated_session: Option<Session>,
    /// XEP-0198 state for this WebSocket. Counts stanzas in both directions
    /// once enabled and holds the unacked queue used for resumption.
    pub(super) sm_state: StreamManagementState,
    pub(super) sm_inbound_completion: crate::server::routes::interpret::SmInboundCompletionTracker,
    /// Set only after a selected stanza's reserved SM slot has been settled
    /// as unhandled because its serving generation was revoked.
    pub(super) inbound_frame_terminal: Option<InboundFrameTerminal>,
    pub(super) ordered_relay_handoff_tx: Option<
        tokio::sync::mpsc::UnboundedSender<
            crate::server::routes::interpret::OrderedRelayHandoffCompletion,
        >,
    >,
    /// Per-connection XEP-0280 opt-in state. Updated when this resource
    /// sends `<enable/>` / `<disable/>` and restored from detached SM state
    /// on resume so re-registration preserves carbons behavior.
    pub(super) carbons_enabled: bool,
    /// Per-stream RFC 6121 roster-interest state restored only on true SM resume.
    pub(super) roster_interested: bool,
    /// Per-stream XEP-0191 blocklist-interest state restored only on true SM resume.
    pub(super) blocklist_interested: bool,
    /// Presence availability restored from XEP-0198 detached state.
    pub(super) presence_available: bool,
    pub(super) presence_show: Option<xmpp_parsers::presence::Show>,
    pub(super) presence_status: Option<String>,
    pub(super) presence_priority: i8,
    /// Presence extension payloads (XEP-0115 caps, XEP-0319 idle, ...)
    /// restored from XEP-0198 detached state so re-registration
    /// republishes the full last presence per RFC 6121 §4.3.2 (#1103).
    pub(super) presence_payloads: Vec<minidom::Element>,
    /// Whether the once-per-session pending-subscribe flush (RFC 6121
    /// §3.1.3, issue #1104) was consumed by the resumed session before
    /// it detached. Restored only on true SM resume; drives the
    /// pre-claim in `registration.rs` so a resumed session that went
    /// available → unavailable → detached does not re-prompt.
    pub(super) pending_subscribes_flushed: bool,
    /// SM stream id restored by `<resume/>` but not yet removed from the
    /// detached registry. The main loop clears it after live routing is
    /// registered, closing the take-before-register fanout gap.
    pub(super) pending_resume_stream_id: Option<String>,
    pub(super) pending_resume_h: Option<u32>,
    /// Claim ownership retained from `<resume/>` through registration.
    pub(super) pending_resume_claim: Option<super::stream_management::SmResumeClaimGuard>,
    #[cfg(test)]
    pub(super) pre_final_principal_recheck_test_hook:
        Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    #[cfg(test)]
    pub(super) post_sm_finalization_test_hook:
        Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    /// Ownership handle for the current connection-registry entry.
    pub(super) registry_owner: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// One-shot flag: when set, the main loop must NOT push the current
    /// frame's responses into `sm_state.record_outbound`. The flag is
    /// raised by `handle_sm_resume` because the responses it returns are
    /// replayed stanzas that were already pushed into the unacked queue
    /// before detach; re-recording them would double-count `outbound_count`
    /// and duplicate queue entries. The main loop resets the flag to
    /// `false` after skipping the record step.
    pub(super) suppress_sm_record_next_batch: bool,
    /// Post-write `<enable/>` publication. While present, a resumable claim
    /// remains guarded and SM stays locally disabled; dropping the
    /// connection or failing the write terminalizes the exact claim.
    pub(super) pending_sm_enable_commit: Option<super::stream_management::SmEnableCommit>,
    /// Inbound text frames pulled off the socket by the mid-batch ack
    /// drain (issue #1089) that were NOT `<a/>` acks. The batch writer
    /// may only consume acks out of order; everything else must reach
    /// the main frame dispatcher in arrival order, so the connection
    /// loop processes this queue before polling the socket again.
    pub(super) deferred_inbound: std::collections::VecDeque<axum::extract::ws::Utf8Bytes>,
    /// The send-window pause ran out of reserved inbound headroom before it
    /// could read a recovering XEP-0198 ack.  Cleanup must promote the
    /// already-recorded queue rather than detach a resumable snapshot: the
    /// unwritten batch suffix was never accepted into SM ownership.
    pub(super) sm_recovery_required: bool,
    /// Terminal XEP-0198 recovery queue. Once the bounded live queue has
    /// exhausted its deferred-inbound headroom, replies from already accepted
    /// frames are recorded here instead of risking an eviction from
    /// `sm_state`. This queue is also bounded at
    /// [`TERMINAL_RECOVERY_QUEUE_CAP`]: once full, later countable replies
    /// from the same terminal session are dropped instead of evicting the
    /// recorded prefix. Partial recording is acceptable here because cleanup
    /// promotes what was retained, rejects XEP-0198 resume, and regenerable
    /// presence fan-out is replayed by a fresh bind/rejoin.
    pub(super) terminal_sm_recovery: StreamManagementState,
    /// Countable stanzas dropped because terminal recovery hit
    /// [`TERMINAL_RECOVERY_QUEUE_CAP`]. Logged at most once per connection.
    pub(super) terminal_sm_recovery_dropped: usize,
    /// Whether the terminal recovery cap warning was already emitted.
    pub(super) terminal_sm_recovery_drop_warned: bool,
    /// Loop-level XEP-0198 send-window pause deadline (issue #1219).
    /// `Some(deadline)` while the connection loop is holding off draining
    /// the outbound mpsc because `sm_state.needs_send_pause()` latched:
    /// set on the rising edge (alongside a forced `<r/>`), cleared once the
    /// window recovers, and fired as a select arm that closes the
    /// connection into detach-for-resume if a dead peer never acks it down.
    /// Uses `tokio::time::Instant` so the arm can `sleep_until` it.
    pub(super) send_window_pause_deadline: Option<tokio::time::Instant>,
    /// `sm_state.last_acked` at the moment the loop last emitted a
    /// send-window `<r/>` (issue #1219 review). The wasm client acks only
    /// when prompted, so while paused the loop re-requests each time the
    /// client has acked since its last prompt but not yet recovered the
    /// window — otherwise a partial ack (XEP-0198 §5 `h` = stanzas *handled*
    /// ≤ received) would leave the stream stalled in the hysteresis band
    /// until the pause deadline, disconnecting a merely-slow client.
    pub(super) send_window_last_request_acked: u32,
    /// Per-connection sans-I/O state machine.
    ///
    /// Initialized in the `Unauthenticated` phase at WS upgrade by
    /// [`Self::init_prebind_state_machine`] so the RFC 7395 §3.8
    /// keepalive policy (issue #1090) covers the connection from its
    /// very first instant — a client that wedges before authenticating
    /// is reaped by the same liveness clock. Replaced at bind /
    /// SM-resume by [`Self::ensure_state_machine`] with a `Ready`
    /// machine carrying the bound user's session snapshot.
    ///
    /// The per-connection main loop dispatches [`OutboundStanza`]
    /// entries on their [`DeliveryKind`]: `PeerStanza` values feed
    /// [`InboundEvent::StanzaFromPeer`] into this state machine and
    /// the resulting outbound events are resolved via
    /// [`crate::server::routes::interpret::interpret`] before any wire
    /// write (an `Unauthenticated` machine drops them with a WARN, as
    /// the previous `None` guard did). `DirectFrame` values bypass the
    /// state machine entirely and write straight to the wire.
    ///
    /// [`InboundEvent::StanzaFromPeer`]: waddle_xmpp::protocol::InboundEvent::StanzaFromPeer
    pub(super) state_machine: Option<XmppStateMachine>,
    /// Deployment keepalive knobs, retained so the bind-time machine
    /// replacement in [`Self::ensure_state_machine`] re-seeds the
    /// policy with the same configuration the pre-bind machine used.
    pub(super) keepalive_config: waddle_xmpp::protocol::KeepaliveConfig,
}

impl WsConnState {
    pub(super) fn new() -> Self {
        Self {
            phase: ConnectionPhase::new(),
            stream_open_handled: false,
            stream_open_wire_state: StreamOpenWireState::NotCommitted,
            authenticated_session: None,
            sm_state: StreamManagementState::new(),
            sm_inbound_completion:
                crate::server::routes::interpret::SmInboundCompletionTracker::default(),
            inbound_frame_terminal: None,
            ordered_relay_handoff_tx: None,
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
            pending_resume_stream_id: None,
            pending_resume_h: None,
            pending_resume_claim: None,
            #[cfg(test)]
            pre_final_principal_recheck_test_hook: None,
            #[cfg(test)]
            post_sm_finalization_test_hook: None,
            registry_owner: None,
            suppress_sm_record_next_batch: false,
            pending_sm_enable_commit: None,
            deferred_inbound: std::collections::VecDeque::new(),
            sm_recovery_required: false,
            terminal_sm_recovery: StreamManagementState::new(),
            terminal_sm_recovery_dropped: 0,
            terminal_sm_recovery_drop_warned: false,
            send_window_pause_deadline: None,
            send_window_last_request_acked: 0,
            state_machine: None,
            keepalive_config: waddle_xmpp::protocol::KeepaliveConfig::default(),
        }
    }

    /// Enter terminal non-resumable recovery without ever appending another
    /// frame to the capped live SM queue. This queue exists only until
    /// cleanup's XEP-0198 §5 promotion completes.
    pub(super) fn begin_terminal_sm_recovery(&mut self) {
        if self.sm_recovery_required {
            return;
        }
        self.sm_recovery_required = true;
        let Some(stream_id) = self.sm_state.stream_id.clone() else {
            return;
        };
        self.terminal_sm_recovery_dropped = 0;
        self.terminal_sm_recovery_drop_warned = false;
        let mut recovery = StreamManagementState::with_config(usize::MAX, u32::MAX);
        recovery.enable(
            stream_id,
            self.sm_state.resumable,
            self.sm_state.max_resume_time,
        );
        self.terminal_sm_recovery = recovery;
    }

    pub(super) fn record_terminal_recovery_outbound(&mut self, stanza_xml: String) {
        self.record_terminal_recovery_outbound_with_receipt_at(stanza_xml, chrono::Utc::now());
    }

    pub(super) fn record_terminal_recovery_outbound_with_receipt_at(
        &mut self,
        stanza_xml: String,
        original_receipt_at: chrono::DateTime<chrono::Utc>,
    ) {
        if self.terminal_sm_recovery.queue_len() >= TERMINAL_RECOVERY_QUEUE_CAP {
            self.terminal_sm_recovery_dropped = self.terminal_sm_recovery_dropped.saturating_add(1);
            return;
        }
        let _ = self.terminal_sm_recovery.record_outbound_with_receipt_at(
            stanza_xml,
            original_receipt_at,
            waddle_xmpp::telemetry::attributes::SmEvictionPath::ReplayTail,
        );
    }

    pub(super) fn warn_terminal_recovery_drops_once(&mut self) {
        if self.terminal_sm_recovery_drop_warned || self.terminal_sm_recovery_dropped == 0 {
            return;
        }
        warn!(
            stream_id = self
                .terminal_sm_recovery
                .stream_id
                .as_deref()
                .or(self.sm_state.stream_id.as_deref())
                .unwrap_or("<unset>"),
            cap = TERMINAL_RECOVERY_QUEUE_CAP,
            dropped = self.terminal_sm_recovery_dropped,
            "Terminal SM recovery queue hit cap; keeping recorded prefix and dropping replayable tail until fresh bind"
        );
        self.terminal_sm_recovery_drop_warned = true;
    }

    /// Covers tests and exceptional cleanup paths that established the
    /// terminal flag before this buffer existed.
    pub(super) fn ensure_terminal_sm_recovery(&mut self) {
        if !self.sm_recovery_required || self.terminal_sm_recovery.enabled {
            return;
        }
        self.sm_recovery_required = false;
        self.begin_terminal_sm_recovery();
    }

    /// Start the semantic handling of a typed client `<open/>` frame.
    ///
    /// The server response has not passed through the authority-aware writer
    /// yet, so this deliberately removes the previous stream's wire proof.
    pub(super) fn begin_server_stream_open_response(&mut self) {
        self.stream_open_handled = true;
        self.stream_open_wire_state = StreamOpenWireState::PendingResponse;
    }

    /// Mark the typed server `<open/>` response as wire-committed only after
    /// its authority-aware response batch returned `Continue`.
    pub(super) fn commit_server_stream_open_response(&mut self) {
        if matches!(
            self.stream_open_wire_state,
            StreamOpenWireState::PendingResponse
        ) {
            self.stream_open_wire_state = StreamOpenWireState::Committed;
        }
    }

    /// Clear state for an actual XMPP stream end or restart. Lifecycle
    /// revocation intentionally does not call this: an established stream
    /// remains eligible for the best-effort system-shutdown error.
    pub(super) fn reset_stream_open_for_xmpp_lifecycle(&mut self) {
        self.stream_open_handled = false;
        self.stream_open_wire_state = StreamOpenWireState::NotCommitted;
    }

    pub(super) fn has_committed_live_stream_open(&self) -> bool {
        self.stream_open_handled
            && matches!(self.stream_open_wire_state, StreamOpenWireState::Committed)
    }

    /// Apply the typed `<enable/>` effect after its `<enabled/>` frame has
    /// been written. This is synchronous by design: no cancellation point
    /// may exist between transport success, local SM enablement, registry
    /// publication, and disarming exact-claim cleanup.
    pub(super) fn publish_pending_sm_enable(&mut self, state: &WebSocketState) {
        let Some(commit) = self.pending_sm_enable_commit.take() else {
            return;
        };
        let bound_jid = self.phase.bound_jid().cloned();
        commit.publish(
            state,
            &mut self.sm_state,
            bound_jid.as_ref(),
            self.registry_owner.as_ref(),
        );
    }

    /// Initialize the per-connection [`XmppStateMachine`] in the
    /// `Unauthenticated` phase, seeding the RFC 7395 §3.8 keepalive
    /// policy (issue #1090) with the deployment config. Called at WS
    /// upgrade, before the connection loop's first `select!` (the
    /// caller then feeds `InboundEvent::TransportReady` to arm the
    /// keepalive clock), and again by the failed-SM-resume reset so
    /// the machine — and with it the keepalive tick chain — is never
    /// absent mid-connection.
    pub(super) fn init_prebind_state_machine(
        &mut self,
        domain: &str,
        dispatcher: &Arc<StanzaDispatcher>,
        keepalive: waddle_xmpp::protocol::KeepaliveConfig,
    ) {
        self.keepalive_config = keepalive;
        let mut sm = XmppStateMachine::new(domain.to_string(), (**dispatcher).clone());
        sm.set_keepalive_config(keepalive);
        self.state_machine = Some(sm);
    }

    /// Feed transport-level liveness evidence (any inbound WS frame —
    /// text, ping, pong, or binary) into the keepalive policy. The
    /// `KeepaliveAck` event never produces outbound effects.
    pub(super) fn note_transport_activity(&mut self) {
        if let Some(sm) = self.state_machine.as_mut() {
            let events = sm.handle(InboundEvent::KeepaliveAck);
            debug_assert!(events.is_empty(), "KeepaliveAck must be effect-free");
        }
    }

    /// Initialize the per-connection [`XmppStateMachine`] in
    /// [`ConnectionPhase::Ready`] for `full_jid`, cloning the
    /// process-wide handler dispatcher into a per-connection copy
    /// (cheap — handlers are `Arc`-shared).
    ///
    /// Called by the bind / SM-resume transition paths so the SM is
    /// ready to handle [`InboundEvent::StanzaFromPeer`] dispatches
    /// from the outbound channel as soon as routing peers can target
    /// this connection. Idempotent — re-initialization on resume
    /// replaces the previous machine, dropping any pending callback
    /// state from before the detach (which is correct: the SM-level
    /// `pending_ops` table belongs to the prior dispatch context and
    /// the resumed wire state is replayed via XEP-0198, not via SM
    /// callback completion).
    ///
    /// `blocklist` is the bound user's persisted XEP-0191 blocklist,
    /// loaded once from `DatabaseBlockingStorage` immediately before
    /// this call; it seeds the SM's session-state snapshot consumed
    /// by the message pipeline (#229 PR13). Per #229 Q5 the snapshot
    /// is frozen for the duration of the session — see
    /// [`XmppStateMachine::set_blocklist`].
    ///
    /// [`InboundEvent::StanzaFromPeer`]: waddle_xmpp::protocol::InboundEvent::StanzaFromPeer
    pub(super) fn ensure_state_machine(
        &mut self,
        domain: &str,
        dispatcher: &Arc<StanzaDispatcher>,
        full_jid: jid::FullJid,
        resumed: bool,
        blocklist: Blocklist,
    ) {
        let mut sm = XmppStateMachine::new(domain.to_string(), (**dispatcher).clone());
        sm.set_keepalive_config(self.keepalive_config);
        sm.transition_to_ready(full_jid, resumed);
        sm.set_blocklist(blocklist);
        self.state_machine = Some(sm);
    }

    /// Mirror the legacy [`Self::phase`] tracker's current value into
    /// the per-connection [`XmppStateMachine`]'s phase. Called after
    /// every `handle_xmpp_frame` round-trip + at start of
    /// `cleanup_connection_shutdown` so a phase transition driven by
    /// a deeply-nested helper (e.g. SASL failure or stream-error
    /// inside `handle_xmpp_frame`) doesn't leave the SM stuck in
    /// `Ready` and willing to accept late `PeerStanza` dispatches.
    ///
    /// Currently only the `Ready → Closing` direction needs explicit
    /// mirroring (the `Ready` direction is handled lazily by
    /// [`Self::ensure_state_machine`] at bind / SM-resume).
    pub(super) fn sync_state_machine_phase(&mut self) {
        if let Some(sm) = self.state_machine.as_mut() {
            if matches!(self.phase, ConnectionPhase::Closing { .. })
                && !matches!(sm.phase(), ConnectionPhase::Closing { .. })
            {
                sm.transition_to_closing();
            }
        }
    }
}
/// Terminal recovery stops accepting another deferred inbound frame once this
/// many replayable responses are retained. Further countable replies from the
/// already-terminal session are dropped in place; the recorded prefix is still
/// promoted, resume is rejected, and the sender must re-establish any
/// regenerable state such as MUC presence via a fresh bind/rejoin.
pub(super) const TERMINAL_RECOVERY_QUEUE_CAP: usize = 4096;
