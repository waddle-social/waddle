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

    /// Whether the remaining occupants were told the ghost left, per §7.14.
    fn watcher_saw_unavailable(&mut self) -> bool {
        let nick = self
            .ghost
            .node()
            .expect("ghost node")
            .to_string()
            .to_string();
        let from: jid::Jid = format!("{ROOM}/{nick}").parse().expect("room nick");
        std::iter::from_fn(|| self.watcher_rx.try_recv().ok()).any(|outbound| {
            matches!(&outbound.stanza, waddle_xmpp::Stanza::Presence(presence)
                if presence.from.as_ref() == Some(&from)
                    && presence.type_ == xmpp_parsers::presence::Type::Unavailable)
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
        f.watcher_saw_unavailable(),
        evictable,
        "XEP-0045 §7.14: remaining occupants learn the ghost left"
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
