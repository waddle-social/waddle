//! XEP-0045 ghost users: a stalled `route_muc` row evicts the occupants pinning it.
use super::*;
use waddle_xmpp::{
    muc::room_registry_actor::GetRoom, registry::OutboundStanza,
    stream_management::InMemorySmSessionRegistry,
};

const EVICTED: &str = "muc.ghost_occupants.evicted";
const STALLED: &str = "ingress.maintenance.unrecoverable_obligations";
const STALLED_LABELS: [(&str, &str); 2] =
    [("kind", "route_muc"), ("reason", "no_durable_progress")];

/// Why one frozen occupant copy can never be taken, and whether the occupancy
/// that holds it is provably abandoned.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GhostCase {
    /// No socket and no resumable session anywhere: an evictable ghost.
    Evictable,
    /// A resumable session lives in the shared durable store, so another node
    /// may still resume this occupancy.
    Resumable,
    /// A connection-registry entry for the exact full JID still exists.
    Connected,
    /// The durable resumable probe cannot answer: absence is unproven.
    ProbeFailed,
}

struct GhostFixture {
    f: IngressFixture,
    state: Arc<WebSocketState>,
    env: Arc<dyn RecoveryEnvironment>,
    cursor: MaintenanceCursor,
    key: MessageKey,
    receipt: crate::ingress::EffectReceiptKey,
    ghost: jid::FullJid,
    watcher: jid::FullJid,
    watcher_rx: tokio::sync::mpsc::Receiver<OutboundStanza>,
    /// Kept alive so the ghost's registry entry is not reaped as closed.
    _ghost_rx: Option<tokio::sync::mpsc::Receiver<OutboundStanza>>,
    /// Kept alive so the durable row written for [`GhostCase::Resumable`] stays.
    _durable_writer: Option<Arc<InMemorySmSessionRegistry>>,
}

const ROOM: &str = "recovery@muc.example.com";

async fn ghost_fixture(f: IngressFixture, case: GhostCase) -> GhostFixture {
    let sm = persistent_sm(&f).await;
    let state = state_for(&f, sm.clone()).await;
    let ghost: jid::FullJid = "alice@example.com/phone".parse().expect("ghost");
    let watcher: jid::FullJid = "ben@example.com/phone".parse().expect("watcher");
    let submission = planned_room(&f, &state, Case::Lost, &[ghost.clone(), watcher.clone()]).await;
    let muc = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .cloned()
        .expect("room fanout intent");
    let receipt = receipt_key(&muc).expect("MUC receipt");
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("room commit");
    let key = decision.message_key.expect("key");
    super::departed::settle_non_delivery_effects(&f, &state, &decision).await;
    // The watcher owns a live socket: its copy lands on the first pass, so the
    // stall streak that follows is caused by the ghost alone.
    let (watcher_tx, watcher_rx) = tokio::sync::mpsc::channel(16);
    socket_tests::register_test_connection(&state, &watcher, watcher_tx).await;
    let mut ghost_rx = None;
    let mut durable_writer = None;
    match case {
        GhostCase::Evictable | GhostCase::ProbeFailed => {}
        GhostCase::Resumable => {
            // A second registry over the SAME durable store models a session
            // resume-stolen by another node: nothing in this node's memory,
            // but a durable row that proves the occupancy is resumable.
            let writer = persistent_sm(&f).await;
            store_detached(&writer, &ghost).await;
            durable_writer = Some(writer);
        }
        GhostCase::Connected => {
            let (tx, rx) = tokio::sync::mpsc::channel(1);
            socket_tests::register_test_connection(&state, &ghost, tx).await;
            // Saturate the one-slot channel so every delivery is dropped: the
            // registry entry survives, which is what the guard must observe.
            assert_eq!(
                state.deps.protocol.connection_registry.try_send_to(
                    &ghost,
                    waddle_xmpp::Stanza::Presence(xmpp_parsers::presence::Presence::new(
                        xmpp_parsers::presence::Type::None
                    ))
                ),
                waddle_xmpp::registry::BroadcastOutcome::Delivered
            );
            ghost_rx = Some(rx);
        }
    }
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    GhostFixture {
        f,
        state,
        env,
        cursor: MaintenanceCursor::default(),
        key,
        receipt,
        ghost,
        watcher,
        watcher_rx,
        _ghost_rx: ghost_rx,
        _durable_writer: durable_writer,
    }
}

impl GhostFixture {
    async fn pass(&self) {
        assert_eq!(
            pass(&self.f, &self.env, &self.cursor).await,
            MaintenanceOutcome::Complete
        );
        self.cursor.wait_for_recovery_accounting().await;
    }

    async fn occupies(&self, occupant: &jid::FullJid) -> bool {
        let actor = self
            .state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: ROOM.parse().expect("room"),
            })
            .await
            .expect("registry lookup")
            .expect("local room actor");
        actor
            .ask(waddle_xmpp::muc::room_actor::GetOccupantByJid {
                jid: occupant.clone(),
            })
            .await
            .expect("occupancy probe")
            .is_some()
    }

    async fn progress(&self) -> Vec<jid::FullJid> {
        let mut tx = self.f.uow.begin().await.expect("inspect progress");
        let mut progress = DeliveryProgressRepository::load(&mut tx, self.key, &self.receipt)
            .await
            .expect("delivery progress");
        tx.commit().await.expect("read commit");
        progress.sort();
        progress
    }

    async fn terminal(&self) -> bool {
        let mut tx = self.f.uow.begin().await.expect("inspect terminal");
        let receipted = EffectReceiptRepository::contains(
            &mut tx,
            self.key,
            self.receipt.kind,
            &self.receipt.semantic_identity_hash,
        )
        .await
        .expect("aggregate receipt");
        let terminal = CanonicalMessageRepository::is_terminal(&mut tx, self.key)
            .await
            .expect("terminal");
        tx.commit().await.expect("read commit");
        assert_eq!(receipted, terminal, "the row terminalizes with its receipt");
        terminal
    }

    /// Each case must exercise the guard it names, not merely fail to evict:
    /// pin the state that guard reads before the passes run.
    async fn assert_guard_premise(&self, case: GhostCase) {
        let connected = self
            .state
            .deps
            .protocol
            .connection_registry
            .is_connected(&self.ghost);
        assert_eq!(connected, case == GhostCase::Connected);
        if case == GhostCase::ProbeFailed {
            // The durable store is still readable here; the pass that follows
            // hides it, and only then is the probe expected to fail.
            return;
        }
        let probe = self
            .state
            .deps
            .protocol
            .sm_session_registry
            .probe_resumable_session_for_full_jid(&self.ghost)
            .await;
        let expected = if case == GhostCase::Resumable {
            waddle_xmpp::stream_management::ResumableSessionProbe::Present
        } else {
            waddle_xmpp::stream_management::ResumableSessionProbe::Absent
        };
        assert!(
            matches!(probe, ref actual if std::mem::discriminant(actual) == std::mem::discriminant(&expected)),
            "resumable probe premise"
        );
    }

    /// The XEP-0045 status codes the remaining occupant's §7.14 unavailable
    /// presence for the ghost carried, or `None` if no such presence arrived.
    fn watcher_leave_status_codes(&mut self) -> Option<Vec<String>> {
        let nick = self
            .ghost
            .node()
            .expect("ghost node")
            .to_string()
            .to_string();
        let from: jid::Jid = format!("{ROOM}/{nick}").parse().expect("room nick");
        std::iter::from_fn(|| self.watcher_rx.try_recv().ok()).find_map(|outbound| {
            let waddle_xmpp::Stanza::Presence(presence) = &outbound.stanza else {
                return None;
            };
            if presence.from.as_ref() != Some(&from)
                || presence.type_ != xmpp_parsers::presence::Type::Unavailable
            {
                return None;
            }
            let x = presence
                .payloads
                .iter()
                .find(|payload| payload.is("x", waddle_xmpp::muc::presence::NS_MUC_USER))?;
            Some(
                x.children()
                    .filter(|child| child.is("status", waddle_xmpp::muc::presence::NS_MUC_USER))
                    .filter_map(|child| child.attr("code").map(str::to_owned))
                    .collect(),
            )
        })
    }
}

/// Four passes: the first lands the watcher's copy, the next three stall with
/// unchanged evidence and reach `no_durable_progress` classification.
async fn ghost_recovery(fixture: IngressFixture, case: GhostCase) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let before_evicted = metrics.counter_sum(EVICTED, &[]).unwrap_or(0);
    let before_stalled = metrics.counter_sum(STALLED, &STALLED_LABELS).unwrap_or(0);
    let mut f = ghost_fixture(fixture, case).await;
    f.assert_guard_premise(case).await;
    f.pass().await;
    assert_eq!(
        f.progress().await,
        vec![f.watcher.clone()],
        "the watcher's copy lands on the first pass"
    );
    if case == GhostCase::ProbeFailed {
        f.f.execute("ALTER TABLE sm_sessions RENAME TO sm_sessions_hidden", ())
            .await;
        assert!(
            matches!(
                f.state
                    .deps
                    .protocol
                    .sm_session_registry
                    .probe_resumable_session_for_full_jid(&f.ghost)
                    .await,
                waddle_xmpp::stream_management::ResumableSessionProbe::Failed
            ),
            "the durable resumable probe must be unable to answer"
        );
    }
    for _ in 0..3 {
        f.pass().await;
    }
    let evictable = case == GhostCase::Evictable;
    assert_eq!(
        f.occupies(&f.ghost).await,
        !evictable,
        "only a provably abandoned occupancy is evicted"
    );
    assert_eq!(
        f.progress().await,
        if evictable {
            let mut both = vec![f.ghost.clone(), f.watcher.clone()];
            both.sort();
            both
        } else {
            vec![f.watcher.clone()]
        }
    );
    assert_eq!(f.terminal().await, evictable);
    assert_eq!(
        f.watcher_leave_status_codes(),
        // XEP-0045 §7.14: remaining occupants learn the ghost left, and
        // §`#service-error-kick`: a SERVICE-side removal because of a
        // technical problem carries status 333 (and never 307, which the
        // same section calls "generally not advisable" here).
        evictable.then(|| vec!["333".to_owned()]),
        "XEP-0045 §7.14 + #service-error-kick: the remaining occupants learn          the ghost was removed by the service"
    );
    assert_eq!(
        metrics.counter_sum(EVICTED, &[]).unwrap_or(0),
        before_evicted + u64::from(evictable)
    );
    assert_eq!(
        metrics.counter_sum(STALLED, &STALLED_LABELS).unwrap_or(0),
        before_stalled + u64::from(!evictable),
        "a repaired row is never classified no_durable_progress"
    );
    assert_eq!(super::super::super::attempt_count(f.key), 4);
    if case == GhostCase::ProbeFailed {
        f.f.execute("ALTER TABLE sm_sessions_hidden RENAME TO sm_sessions", ())
            .await;
    }
    f.f.close().await;
}

macro_rules! paired_ghost {
    ($sqlite:ident, $postgres:ident, $case:expr) => {
        #[tokio::test]
        async fn $sqlite() {
            ghost_recovery(IngressFixture::sqlite().await, $case).await;
        }
        #[tokio::test]
        async fn $postgres() {
            if let Some(f) = IngressFixture::postgres(stringify!($postgres)).await {
                ghost_recovery(f, $case).await;
            }
        }
    };
}

paired_ghost!(
    sqlite_stalled_ghost_occupant_is_evicted_and_settled,
    postgres_ghost_evicted,
    GhostCase::Evictable
);
paired_ghost!(
    sqlite_stalled_ghost_with_durable_resumable_session_stays,
    postgres_ghost_resumable,
    GhostCase::Resumable
);
paired_ghost!(
    sqlite_stalled_ghost_with_live_registry_entry_stays,
    postgres_ghost_connected,
    GhostCase::Connected
);
paired_ghost!(
    sqlite_stalled_ghost_with_failed_resumable_probe_stays,
    postgres_ghost_probe_failed,
    GhostCase::ProbeFailed
);

/// Before the stall threshold an unreachable occupant is merely pending: the
/// eviction is the last resort of a parked row, not a delivery fallback.
#[tokio::test]
async fn sqlite_ghost_occupant_survives_below_the_stall_threshold() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let before_evicted = metrics.counter_sum(EVICTED, &[]).unwrap_or(0);
    let f = ghost_fixture(IngressFixture::sqlite().await, GhostCase::Evictable).await;
    for _ in 0..3 {
        f.pass().await;
    }
    assert!(
        f.occupies(&f.ghost).await,
        "two stalled samples are not a stall"
    );
    assert_eq!(f.progress().await, vec![f.watcher.clone()]);
    assert!(!f.terminal().await);
    assert_eq!(
        metrics.counter_sum(EVICTED, &[]).unwrap_or(0),
        before_evicted
    );
    f.f.close().await;
}

/// The repair settles a chunk of copies and then sweeps the ghosts one by one
/// under a bounded budget. A cancellation between two sweeps must not leave a
/// settled ghost seated with nobody owing its removal — the row may already be
/// terminal, so recovery never revisits it. Every settled ghost therefore gets
/// a retained janitor sweep BEFORE its inline sweep starts; the window the
/// inline sweep opens observes it.
#[tokio::test]
async fn sqlite_settled_ghost_has_a_retained_sweep_before_its_inline_sweep() {
    use crate::ingress::recovery_ghosts::{hook_ghost_eviction_window, GhostEvictionWindow};
    use crate::server::routes::websocket::LocalDepartureItem;

    /// Records whether a full-JID sweep for the ghost was already retained
    /// when the inline sweep's window opened.
    struct ObserveRetained {
        state: std::sync::Arc<crate::server::routes::websocket::WebSocketState>,
        retained: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl GhostEvictionWindow for ObserveRetained {
        async fn enter(&self, occupant: &jid::FullJid) {
            let due = self
                .state
                .deps
                .protocol
                .pending_local_muc_departures
                .take_due(std::time::Instant::now() + std::time::Duration::from_secs(3600));
            let found = due.iter().any(|entry| {
                matches!(
                    &entry.item,
                    LocalDepartureItem::FullJidSweep { jid, .. } if jid == occupant
                )
            });
            self.retained
                .store(found, std::sync::atomic::Ordering::SeqCst);
        }
    }

    let f = ghost_fixture(IngressFixture::sqlite().await, GhostCase::Evictable).await;
    for _ in 0..3 {
        f.pass().await;
    }
    let retained = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    hook_ghost_eviction_window(
        f.key,
        std::sync::Arc::new(ObserveRetained {
            state: std::sync::Arc::clone(&f.state),
            retained: std::sync::Arc::clone(&retained),
        }) as std::sync::Arc<dyn GhostEvictionWindow>,
    );
    f.pass().await;

    assert!(
        !f.occupies(&f.ghost).await,
        "the inline sweep still unseats the ghost"
    );
    assert!(
        retained.load(std::sync::atomic::Ordering::SeqCst),
        "a settled ghost's removal is owed by the janitor before the inline sweep runs"
    );
    f.f.close().await;
}

/// The eviction can take away the very authority the settlement it enables
/// needs. The leave sweep that removes the ghost runs the empty-room path for
/// the room it just emptied (`maybe_evict_empty_room`), and that destroy —
/// immediately, or on the local-departure janitor's next tick once the
/// departure receipt is acknowledged — removes the registry entry AND
/// releases the durable room claim. A settlement that re-derives authority
/// afterwards then finds no room to settle against, and since no node hosts
/// the room any more, no later pass can either: the row is pinned forever,
/// which is the bug this whole path exists to fix.
///
/// The copy is therefore settled under the authority the ghost was proven
/// with, BEFORE the sweep runs. The hook models the destroy landing at the
/// worst possible moment — the instant the eviction is confirmed.
#[tokio::test]
async fn sqlite_ghost_copy_settles_although_the_eviction_destroys_the_room() {
    use crate::ingress::recovery_ghosts::{hook_ghost_eviction_window, GhostEvictionWindow};
    use waddle_xmpp::muc::room_registry_actor::{DestroyRoom, DestroyRoomReason};

    /// Destroys the room exactly once, from inside the window the eviction
    /// opens: from here the registry no longer hosts it and its claim is gone.
    struct DestroyOnce {
        registry: kameo::actor::ActorRef<waddle_xmpp::muc::room_registry_actor::RoomRegistryActor>,
        done: std::sync::atomic::AtomicBool,
        ran: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl GhostEvictionWindow for DestroyOnce {
        async fn enter(&self, _occupant: &jid::FullJid) {
            if self.done.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
            self.registry
                .ask(DestroyRoom {
                    room_jid: ROOM.parse().expect("room"),
                    reason: DestroyRoomReason::Destroy,
                })
                .await
                .expect("the empty-room destroy removes the room");
        }
    }

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let before_evicted = metrics.counter_sum(EVICTED, &[]).unwrap_or(0);
    let f = ghost_fixture(IngressFixture::sqlite().await, GhostCase::Evictable).await;
    for _ in 0..3 {
        f.pass().await;
    }
    let hook_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    hook_ghost_eviction_window(
        f.key,
        std::sync::Arc::new(DestroyOnce {
            registry: f.state.deps.protocol.room_registry.clone(),
            done: std::sync::atomic::AtomicBool::new(false),
            ran: std::sync::Arc::clone(&hook_ran),
        }) as std::sync::Arc<dyn GhostEvictionWindow>,
    );
    f.pass().await;

    assert!(
        hook_ran.load(std::sync::atomic::Ordering::SeqCst),
        "the eviction-window hook must have run"
    );
    assert!(
        !f.state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: ROOM.parse().expect("room"),
            })
            .await
            .expect("registry lookup")
            .is_some(),
        "the room is gone, so nothing can re-derive its authority"
    );
    assert_eq!(
        f.progress().await,
        {
            let mut both = vec![f.ghost.clone(), f.watcher.clone()];
            both.sort();
            both
        },
        "the ghost's copy was settled under the authority it was proven with"
    );
    assert!(f.terminal().await, "the repaired row terminalizes");
    assert_eq!(
        metrics.counter_sum(EVICTED, &[]).unwrap_or(0),
        before_evicted + 1
    );
    f.f.close().await;
}

/// A client that rebinds the SAME full JID and rejoins while the probes are
/// running (the web client reuses its `web-<uuid>` resource across reconnects
/// within a page). The generation is pinned before the probes, so the sweep
/// targets the session the evidence was gathered about and the new one is
/// classified `Superseded` instead of being torn out.
#[tokio::test]
async fn sqlite_rejoin_during_the_ghost_probes_keeps_its_seat() {
    use crate::ingress::recovery_ghosts::{hook_ghost_probe_window, GhostProbeWindow};

    /// Rejoins the ghost's full JID exactly once, from inside the probe
    /// window the generation pin exists to close.
    struct RejoinOnce {
        room: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
        ghost: jid::FullJid,
        done: std::sync::atomic::AtomicBool,
        ran: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl GhostProbeWindow for RejoinOnce {
        async fn enter(&self, occupant: &jid::FullJid) {
            if occupant != &self.ghost || self.done.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return;
            }
            self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
            let admission_revision = self
                .room
                .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
                .await
                .expect("room snapshot")
                .admission_revision;
            self.room
                .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
                    sender_jid: self.ghost.clone(),
                    nick: "alice".to_owned(),
                    affiliation_grant: waddle_xmpp::muc::room_actor::JoinAffiliationGrant::Resolver(
                        waddle_xmpp::Affiliation::Member,
                    ),
                    local_domain: "example.com".to_owned(),
                    admission_revision,
                    session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                })
                .await
                .expect("the client rebinds the same resource and rejoins");
        }
    }

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let before_evicted = metrics.counter_sum(EVICTED, &[]).unwrap_or(0);
    let f = ghost_fixture(IngressFixture::sqlite().await, GhostCase::Evictable).await;
    let room = f
        .state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: ROOM.parse().expect("room"),
        })
        .await
        .expect("registry lookup")
        .expect("local room actor");
    let seated = |room: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
                  ghost: jid::FullJid| async move {
        room.ask(waddle_xmpp::muc::room_actor::GetOccupantSessionGeneration { jid: ghost })
            .await
            .expect("generation probe")
    };
    let before = seated(room.clone(), f.ghost.clone())
        .await
        .expect("the ghost starts seated");

    // Three stalled passes, then the pass that would evict — with the rejoin
    // landing between the generation pin and the probes.
    for _ in 0..3 {
        f.pass().await;
    }
    let hook_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    hook_ghost_probe_window(
        f.key,
        std::sync::Arc::new(RejoinOnce {
            room: room.clone(),
            ghost: f.ghost.clone(),
            done: std::sync::atomic::AtomicBool::new(false),
            ran: std::sync::Arc::clone(&hook_ran),
        }) as std::sync::Arc<dyn GhostProbeWindow>,
    );
    f.pass().await;

    assert!(
        hook_ran.load(std::sync::atomic::Ordering::SeqCst),
        "the probe-window hook must have run"
    );
    let after = seated(room, f.ghost.clone())
        .await
        .expect("the rejoined session keeps its seat");
    assert_ne!(
        after, before,
        "the fixture must have rejoined a NEW session"
    );
    assert!(
        f.occupies(&f.ghost).await,
        "a session that joined during the probes must not be evicted"
    );
    assert_eq!(
        f.progress().await,
        vec![f.watcher.clone()],
        "no copy is settled for a seat that is still live"
    );
    assert!(!f.terminal().await);
    assert_eq!(
        metrics.counter_sum(EVICTED, &[]).unwrap_or(0),
        before_evicted,
        "a superseded sweep is not an eviction"
    );
    f.f.close().await;
}

/// #1803, the sibling-row half of the ordering note above: ONE ghost can pin
/// SEVERAL canonical rows, and repairing the first destroys the room out from
/// under the rest. Evicting the last occupant of a non-persistent room runs
/// the empty-room destroy, which removes the registry entry AND releases the
/// durable room claim — so every OTHER row the same ghost pinned answered
/// `GetRoom -> Ok(None)` on that pass and on every pass after it. Settling
/// before the sweep keeps the repair from stranding the row it is repairing;
/// it does nothing for the siblings, which had no authoritative roster left to
/// prove anything against and stayed pending forever.
///
/// The unhosted-room proof is what reaches them: a room no node hosts has no
/// roster anywhere, so its frozen occupants are absent by definition and the
/// unchanged every-peer reachability proof decides the rest. The sibling row
/// settles on a later pass.
#[tokio::test]
async fn sqlite_sibling_row_settles_once_the_ghost_eviction_destroyed_the_room() {
    use crate::ingress::recovery_ghosts::{hook_ghost_eviction_window, GhostEvictionWindow};
    use waddle_xmpp::muc::room_registry_actor::{DestroyRoom, DestroyRoomReason};

    /// The empty-room destroy the eviction queues, landing at the worst
    /// possible moment: the instant the first row's eviction is confirmed.
    struct DestroyOnce {
        registry: kameo::actor::ActorRef<waddle_xmpp::muc::room_registry_actor::RoomRegistryActor>,
        done: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl GhostEvictionWindow for DestroyOnce {
        async fn enter(&self, _occupant: &jid::FullJid) {
            if self.done.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            self.registry
                .ask(DestroyRoom {
                    room_jid: ROOM.parse().expect("room"),
                    reason: DestroyRoomReason::Destroy,
                })
                .await
                .expect("the empty-room destroy removes the room");
        }
    }

    let f = ghost_fixture(IngressFixture::sqlite().await, GhostCase::Evictable).await;
    // The first row accumulates its stall streak alone, so the pass that
    // repairs it is the same pass the sibling row is first attempted on.
    for _ in 0..3 {
        f.pass().await;
    }
    // A SECOND obligation for the same room, frozen against the same two
    // occupants. Distinct origin id, so it is a distinct canonical row.
    let sibling = super::planned_room_with_origin(
        &f.f,
        &f.state,
        Case::Lost,
        &[f.ghost.clone(), f.watcher.clone()],
        "muc-recovery-sibling",
    )
    .await;
    let sibling_muc = sibling
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .cloned()
        .expect("sibling room fanout intent");
    let sibling_receipt = receipt_key(&sibling_muc).expect("sibling MUC receipt");
    let sibling_decision = commit_submission(&f.f.uow, &sibling, 1)
        .await
        .expect("sibling room commit");
    let sibling_key = sibling_decision.message_key.expect("sibling key");
    assert_ne!(sibling_key, f.key, "the sibling must be its own row");
    super::departed::settle_non_delivery_effects(&f.f, &f.state, &sibling_decision).await;

    hook_ghost_eviction_window(
        f.key,
        std::sync::Arc::new(DestroyOnce {
            registry: f.state.deps.protocol.room_registry.clone(),
            done: std::sync::atomic::AtomicBool::new(false),
        }) as std::sync::Arc<dyn GhostEvictionWindow>,
    );
    // The repairing pass, then one more: the sibling cannot settle before the
    // room is gone, and must settle once it is.
    f.pass().await;
    assert!(
        f.state
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: ROOM.parse().expect("room"),
            })
            .await
            .expect("registry lookup")
            .is_none(),
        "the eviction destroyed the room the sibling row still names"
    );
    f.pass().await;

    let mut tx = f.f.uow.begin().await.expect("inspect the sibling row");
    let mut progress = DeliveryProgressRepository::load(&mut tx, sibling_key, &sibling_receipt)
        .await
        .expect("sibling delivery progress");
    progress.sort();
    let mut both = vec![f.ghost.clone(), f.watcher.clone()];
    both.sort();
    assert_eq!(
        progress, both,
        "the sibling row's frozen copies are discharged: the watcher's by \
         delivery, the ghost's by the unhosted-room settlement"
    );
    assert!(
        CanonicalMessageRepository::is_terminal(&mut tx, sibling_key)
            .await
            .expect("sibling terminal"),
        "a row whose room no node hosts any more must still terminalize"
    );
    tx.commit().await.expect("read commit");
    f.f.close().await;
}

/// The OPTIONAL `#service-error-kick` code must not leak onto an ordinary
/// departure: the same room, the same watcher and the same full-JID leave
/// sweep, driven with the ordinary disconnect cause rather than the ghost
/// repair's, carries the bare XEP-0045 §7.14 shape with no status code at all.
#[tokio::test]
async fn sqlite_ordinary_disconnect_leave_carries_no_removal_status_code() {
    let mut f = ghost_fixture(IngressFixture::sqlite().await, GhostCase::Evictable).await;
    assert_eq!(
        crate::server::routes::websocket::sweep_abandoned_muc_occupancy(
            &f.state,
            &f.ghost,
            waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
            waddle_xmpp::muc::MucRemovalCause::Voluntary,
        )
        .await,
        crate::server::routes::websocket::MucCleanupOutcome::Completed
    );
    assert_eq!(
        f.watcher_leave_status_codes(),
        Some(Vec::new()),
        "a disconnect is the occupant's own departure, not a service removal"
    );
    f.f.close().await;
}
