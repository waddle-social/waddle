use super::*;
use super::{
    interpret_loop::build_interpret_deps, state::WsConnState,
    stream_management::is_countable_stanza, transport_xml::stanza_to_xml,
};
use waddle_xmpp::telemetry::attributes::SmEvictionPath;

/// How a detach drain handles an outbound item that already belongs to a
/// durable pending-delivery row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingRowDrainPolicy {
    /// Bind the row to the detached SM sequence for resumable replay.
    PreserveForReplay,
    /// Return the row to normal pending-delivery redelivery instead of
    /// appending it to a terminal promotion queue.
    ReleaseForTerminalRecovery,
}

pub(super) struct TerminalDrainContext<'a> {
    pub(super) session: &'a waddle_xmpp::stream_management::DetachedSession,
    pub(super) blocklist: Option<&'a waddle_xmpp::protocol::session_state::Blocklist>,
    pub(super) recent_tombstones: &'a [waddle_xmpp::stream_management::RecentTombstoneRecord],
    pub(super) promote_incrementally: bool,
}

pub(super) struct TerminalDrainOutcome {
    pub(super) retained_overflow: Vec<waddle_xmpp::stream_management::DetachedUnackedStanza>,
    pub(super) released_pending_rows: bool,
    /// Incremental overflow promotion inserted fresh `pending_delivery`
    /// rows; the caller must recheck live routing afterwards in case a
    /// replacement bound mid-drain with its offline flush already spent.
    pub(super) queued_pending_rows: bool,
    /// A row-backed channel entry's `release_row` failed: its replay copy
    /// was dropped (promoting it would duplicate the still-claimed row) and
    /// the row is only freed later by the settled promotion's
    /// `release_claim` — cleanup must re-drive it after that.
    pub(super) release_failed_pending_rows: bool,
    /// An inline re-drive during the drain aborted: later items were
    /// retained, and the FINAL promotion must defer behind the stuck row.
    pub(super) redrive_aborted: bool,
}

struct TerminalDrainedFrame {
    xml: String,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
    pending_row_id: Option<waddle_xmpp::pending_delivery::PendingRowId>,
}

struct TerminalDrainedRecordResult {
    retained: Option<waddle_xmpp::stream_management::DetachedUnackedStanza>,
    released_pending_row: bool,
    queued_pending_row: bool,
    release_failed_pending_row: bool,
}

/// Drain `outbound_rx` of all immediately-available
/// [`OutboundStanza`] values and record them into the per-connection
/// XEP-0198 unacked queue (and, when a `detached_stream_id` is
/// supplied, into the detached SM session's stored replay buffer).
///
/// Dispatches on [`DeliveryKind`] so the recipient-pass contract
/// PR11 introduced is preserved through the detach path. Without
/// this dispatch, queued `PeerStanza` values would be serialized
/// raw and replayed bytes would be missing the recipient-side
/// `<stanza-id>` stamp / archive write that the recipient pipeline
/// produces — exactly the bug Qodo flagged on PR269.
///
/// `state_machine` borrows the per-connection SM mutably so it can
/// feed `InboundEvent::StanzaFromPeer` for queued PeerStanza values.
/// When `state_machine` is `None` (pre-bind queue, never reached in
/// practice for a detach drain) PeerStanza values are dropped with
/// a WARN log.
pub(super) async fn drain_outbound_into_replay(
    state: &WebSocketState,
    state_machine: Option<&mut XmppStateMachine>,
    sm_state: &mut StreamManagementState,
    authenticated_session: Option<&crate::auth::Session>,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    detached_stream_id: Option<&str>,
    pending_row_policy: PendingRowDrainPolicy,
) {
    let principal = authenticated_session.map(super::ResolvedPrincipal::from_authenticated_session);
    let deps = build_interpret_deps(state, principal);
    let mut sm_borrow: Option<&mut XmppStateMachine> = state_machine;
    while let Ok(outbound_stanza) = outbound_rx.try_recv() {
        // Codex P2 review on PR #361: when this is a pending_delivery
        // flush replay, preserve the row's original_receipt_at instead
        // of stamping `Utc::now()` at drain time. Otherwise a flush
        // queued just before the WebSocket dropped would replay later
        // (after Q6 promotion re-creates the pending row) with a
        // wrong XEP-0203 `<delay/>` time.
        let receipt_at = outbound_stanza
            .pending_row_original_receipt_at
            .unwrap_or_else(chrono::Utc::now);
        let pending_row_id = outbound_stanza.pending_row_id.clone();
        match outbound_stanza.kind {
            DeliveryKind::DirectFrame => {
                let xml = stanza_to_xml(&outbound_stanza.stanza);
                record_drained_xml(
                    state,
                    sm_state,
                    detached_stream_id,
                    xml,
                    receipt_at,
                    pending_row_id,
                    pending_row_policy,
                )
                .await;
            }
            DeliveryKind::PeerStanza => {
                let Some(sm) = sm_borrow.as_deref_mut() else {
                    warn!(
                        "PeerStanza encountered in detach drain without an SM; \
                         dropping. Resumed connection will not see this stanza."
                    );
                    continue;
                };
                let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(
                    outbound_stanza.stanza,
                )));
                // Detach drain: there is no live socket, so `close`,
                // keepalive probes, and timer commands are all moot —
                // only the frames matter, recorded for resume replay.
                let drive = drive_interpret_loop(events, sm, &deps).await;
                let mut row_id_for_first = pending_row_id.clone();
                for xml in drive.frames {
                    let row_for_this = row_id_for_first.take();
                    record_drained_xml(
                        state,
                        sm_state,
                        detached_stream_id,
                        xml,
                        receipt_at,
                        row_for_this,
                        pending_row_policy,
                    )
                    .await;
                }
            }
        }
    }
}

/// Terminal-recovery variant of [`drain_outbound_into_replay`]. Records only
/// into [`WsConnState::terminal_sm_recovery`], enforcing that queue's hard cap
/// in-place so terminal cleanup never evicts its recorded prefix while
/// draining already accepted outbound work.
pub(super) async fn drain_outbound_into_terminal_recovery(
    state: &WebSocketState,
    conn: &mut WsConnState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    pending_row_policy: PendingRowDrainPolicy,
    terminal: TerminalDrainContext<'_>,
) -> TerminalDrainOutcome {
    let principal_session = conn.authenticated_session.clone();
    let principal = principal_session
        .as_ref()
        .map(super::ResolvedPrincipal::from_authenticated_session);
    let deps = build_interpret_deps(state, principal);
    let mut retained_overflow = Vec::new();
    let mut retain_overflow_for_retry = false;
    let mut released_pending_rows = false;
    let mut queued_pending_rows = false;
    let mut release_failed_pending_rows = false;
    let mut redrive_aborted = false;
    while let Ok(outbound_stanza) = outbound_rx.try_recv() {
        let receipt_at = outbound_stanza
            .pending_row_original_receipt_at
            .unwrap_or_else(chrono::Utc::now);
        let pending_row_id = outbound_stanza.pending_row_id.clone();
        match outbound_stanza.kind {
            DeliveryKind::DirectFrame => {
                let xml = stanza_to_xml(&outbound_stanza.stanza);
                let result = record_drained_terminal_xml(
                    state,
                    conn,
                    TerminalDrainedFrame {
                        xml,
                        original_receipt_at: receipt_at,
                        pending_row_id,
                    },
                    pending_row_policy,
                    &terminal,
                    retain_overflow_for_retry,
                )
                .await;
                released_pending_rows |= result.released_pending_row;
                if result.release_failed_pending_row {
                    release_failed_pending_rows = true;
                    // The failed row stays claimed until promotion's
                    // release_claim; promoting later tail items live would
                    // overtake it, so the remainder is retained.
                    retain_overflow_for_retry = true;
                }
                if result.queued_pending_row || result.released_pending_row {
                    queued_pending_rows |= result.queued_pending_row;
                    // Re-drive BEFORE the next frame: each single-item
                    // promotion takes a fresh online-resource snapshot, so a
                    // later frame could otherwise deliver live ahead of this
                    // just-queued (or just-released) row and invert the
                    // stream's FIFO order. An aborted re-drive stops direct
                    // promotion of the remaining tail — it is retained for
                    // the janitor behind the stuck row instead.
                    if super::cleanup::redrive_terminal_pending_rows_to_live_resource(
                        state,
                        &terminal.session.jid.to_bare(),
                    )
                    .await
                        == super::cleanup::TerminalRedriveOutcome::Aborted
                    {
                        retain_overflow_for_retry = true;
                        redrive_aborted = true;
                    }
                }
                if let Some(entry) = result.retained {
                    retain_overflow_for_retry = true;
                    retained_overflow.push(entry);
                }
            }
            DeliveryKind::PeerStanza => {
                let Some(sm) = conn.state_machine.as_mut() else {
                    warn!(
                        "PeerStanza encountered in terminal drain without an SM; \
                         dropping. Fresh bind will not replay this stanza."
                    );
                    continue;
                };
                let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(
                    outbound_stanza.stanza,
                )));
                let drive = drive_interpret_loop(events, sm, &deps).await;
                let mut row_id_for_first = pending_row_id.clone();
                for xml in drive.frames {
                    let row_for_this = row_id_for_first.take();
                    let result = record_drained_terminal_xml(
                        state,
                        conn,
                        TerminalDrainedFrame {
                            xml,
                            original_receipt_at: receipt_at,
                            pending_row_id: row_for_this,
                        },
                        pending_row_policy,
                        &terminal,
                        retain_overflow_for_retry,
                    )
                    .await;
                    released_pending_rows |= result.released_pending_row;
                    if result.release_failed_pending_row {
                        release_failed_pending_rows = true;
                        retain_overflow_for_retry = true;
                    }
                    if result.queued_pending_row || result.released_pending_row {
                        queued_pending_rows |= result.queued_pending_row;
                        if super::cleanup::redrive_terminal_pending_rows_to_live_resource(
                            state,
                            &terminal.session.jid.to_bare(),
                        )
                        .await
                            == super::cleanup::TerminalRedriveOutcome::Aborted
                        {
                            retain_overflow_for_retry = true;
                            redrive_aborted = true;
                        }
                    }
                    if let Some(entry) = result.retained {
                        retain_overflow_for_retry = true;
                        retained_overflow.push(entry);
                    }
                }
            }
        }
    }
    conn.warn_terminal_recovery_drops_once();
    TerminalDrainOutcome {
        retained_overflow,
        released_pending_rows,
        queued_pending_rows,
        release_failed_pending_rows,
        redrive_aborted,
    }
}

/// Helper: record a single drained XML frame into the per-connection
/// SM unacked queue and, when applicable, into the detached SM
/// session's stored replay buffer. Pulled out so both the
/// `DirectFrame` and per-frame `PeerStanza` arms in
/// [`drain_outbound_into_replay`] can share the same recording
/// contract.
async fn record_drained_xml(
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    detached_stream_id: Option<&str>,
    xml: String,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
    pending_row_id: Option<waddle_xmpp::pending_delivery::PendingRowId>,
    pending_row_policy: PendingRowDrainPolicy,
) {
    if matches!(
        pending_row_policy,
        PendingRowDrainPolicy::ReleaseForTerminalRecovery
    ) {
        if let Some(row_id) = pending_row_id {
            // Ownership-conditional when the dead stream is known: a claim
            // released mid-cleanup and re-claimed by a replacement must not
            // be stripped from that replacement (see the terminal drain).
            let result = match sm_state.stream_id.as_deref() {
                Some(stream_id) => {
                    state
                        .deps
                        .protocol
                        .pending_delivery_storage
                        .release_row_if_session(
                            &row_id,
                            &waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.to_string()),
                        )
                        .await
                }
                None => {
                    state
                        .deps
                        .protocol
                        .pending_delivery_storage
                        .release_row(&row_id)
                        .await
                }
            };
            if let Err(error) = result {
                warn!(
                    row_id = %row_id,
                    %error,
                    "pending_delivery release_row (terminal recovery drain) failed; \
                     claim-expiry janitor will recover the row"
                );
            }
            return;
        }
    }
    if !sm_state.enabled || !is_countable_stanza(&xml) {
        if let Some(row_id) = pending_row_id {
            if let Err(error) = state
                .deps
                .protocol
                .pending_delivery_storage
                .release_row(&row_id)
                .await
            {
                warn!(
                    row_id = %row_id,
                    %error,
                    "pending_delivery release_row (drained non-countable or SM-disabled) failed"
                );
            }
        }
        return;
    }
    // Drain path: we're recording into the unacked queue for replay
    // on the next resume, NOT writing to a live wire. The SM cadence
    // signal is moot — there is no socket to follow up with `<r/>`.
    //
    // No MAM replay exemption here (issue #1089 review): nothing on
    // this path was ever written to a wire, so the client's `h` can
    // never include it — counting without queueing would permanently
    // desync `outbound_count` from `h`. Recording everything keeps
    // the counters convergent (the resume delivers the stanza, the
    // client counts it). Server-generated MAM responses also cannot
    // reach this path: they are produced synchronously on the
    // requester's own connection, never routed via the registry.
    // This record runs while a live transport is detaching; it keeps the
    // connection-local replay queue aligned with the detached-session drain.
    let _ = sm_state.record_outbound_with_receipt_at(
        xml.clone(),
        original_receipt_at,
        SmEvictionPath::DetachDrain,
    );
    let sequence = sm_state.outbound_count;
    if let Some(row_id) = pending_row_id.as_ref() {
        if let Err(error) = state
            .deps
            .protocol
            .pending_delivery_storage
            .record_pushed_at(row_id, sequence)
            .await
        {
            warn!(
                row_id = %row_id,
                sequence,
                %error,
                "pending_delivery record_pushed_at (drain path) failed; deleting row \
                 because SM unacked queue owns recovery"
            );
            if let Err(delete_error) = state
                .deps
                .protocol
                .pending_delivery_storage
                .delete_row(row_id)
                .await
            {
                warn!(
                    row_id = %row_id,
                    error = %delete_error,
                    "pending_delivery delete_row (drain record_pushed_at fallback) failed"
                );
            }
        }
    }
    if let Some(stream_id) = detached_stream_id {
        if let Err(error) = state
            .deps
            .protocol
            .sm_session_registry
            .record_outbound_for_detached_stream_at(stream_id, sequence, xml, original_receipt_at)
            .await
        {
            warn!(stream_id = %stream_id, %error, "Failed to record drained outbound for detached SM session");
        }
    }
}

async fn record_drained_terminal_xml(
    state: &WebSocketState,
    conn: &mut WsConnState,
    frame: TerminalDrainedFrame,
    pending_row_policy: PendingRowDrainPolicy,
    terminal: &TerminalDrainContext<'_>,
    retain_overflow_for_retry: bool,
) -> TerminalDrainedRecordResult {
    let TerminalDrainedFrame {
        xml,
        original_receipt_at,
        pending_row_id,
    } = frame;
    if matches!(
        pending_row_policy,
        PendingRowDrainPolicy::ReleaseForTerminalRecovery
    ) {
        if let Some(row_id) = pending_row_id {
            // Ownership-conditional: long awaited work earlier in this
            // cleanup can outlive the pending-claim recency floor, letting
            // the claim janitor release the row and a replacement re-claim
            // and push it. An unconditional release would strip that
            // replacement's claim and double-deliver via the re-drive.
            let dead_session =
                waddle_xmpp::pending_delivery::SmSessionId::new(terminal.session.stream_id.clone());
            let (released_pending_row, release_failed_pending_row) = match state
                .deps
                .protocol
                .pending_delivery_storage
                .release_row_if_session(&row_id, &dead_session)
                .await
            {
                Ok(released) => (released > 0, false),
                Err(error) => {
                    warn!(
                        row_id = %row_id,
                        %error,
                        "pending_delivery release_row (terminal recovery drain) failed; \
                         the settled promotion's release_claim frees the row and cleanup \
                         re-drives it"
                    );
                    (false, true)
                }
            };
            return TerminalDrainedRecordResult {
                retained: None,
                released_pending_row,
                queued_pending_row: false,
                release_failed_pending_row,
            };
        }
    }
    if !conn.terminal_sm_recovery.enabled || !is_countable_stanza(&xml) {
        if let Some(row_id) = pending_row_id {
            if let Err(error) = state
                .deps
                .protocol
                .pending_delivery_storage
                .release_row(&row_id)
                .await
            {
                warn!(
                    row_id = %row_id,
                    %error,
                    "pending_delivery release_row (terminal drained non-countable or SM-disabled) failed"
                );
            }
        }
        return TerminalDrainedRecordResult {
            retained: None,
            released_pending_row: false,
            queued_pending_row: false,
            release_failed_pending_row: false,
        };
    }
    let entry = waddle_xmpp::stream_management::DetachedUnackedStanza {
        sequence: terminal.session.outbound_count.wrapping_add(1),
        stanza_xml: xml,
        original_receipt_at,
    };
    // The queue was snapshotted before this drain. Never put another frame
    // back into it: success promotes incrementally, while a failed/blocked
    // prefix keeps the bounded receiver tail with the synthetic session.
    if !terminal.promote_incrementally || retain_overflow_for_retry {
        return TerminalDrainedRecordResult {
            retained: Some(entry),
            released_pending_row: false,
            queued_pending_row: false,
            release_failed_pending_row: false,
        };
    }
    let Some(_blocklist) = terminal.blocklist else {
        return TerminalDrainedRecordResult {
            retained: Some(entry),
            released_pending_row: false,
            queued_pending_row: false,
            release_failed_pending_row: false,
        };
    };
    let refreshed_blocklist = match load_terminal_recovery_blocklist(
        state,
        &terminal.session.jid.to_bare(),
    )
    .await
    {
        Ok(blocklist) => blocklist,
        Err(error) => {
            warn!(
                stream_id = %terminal.session.stream_id,
                jid = %terminal.session.jid,
                %error,
                "terminal recovery could not refresh blocklist for overflow promotion; retaining terminal overflow for retry"
            );
            return TerminalDrainedRecordResult {
                retained: Some(entry),
                released_pending_row: false,
                queued_pending_row: false,
                release_failed_pending_row: false,
            };
        }
    };
    // The terminal prefix already settled, so each overflow frame is its own
    // promotion window. Re-read recent tombstones per item, then re-scrub
    // pending rows recorded during that single-item promotion span. Reusing
    // the batch-start snapshot here leaves a TOCTOU where a retraction lands
    // after the prefix promotion but before this later overflow item commits.
    let recent_tombstones = crate::sm_promotion::recent_tombstones_for_promotion(
        &state.deps.protocol.sm_session_registry,
        "terminal recovery overflow promotion",
    )
    .unwrap_or_else(|_| terminal.recent_tombstones.to_vec());
    let summary = crate::sm_promotion::promote_terminal_overflow_entry(
        terminal.session,
        entry.clone(),
        crate::sm_promotion::TerminalOverflowPromotionDeps {
            registry: &state.deps.protocol.connection_registry,
            user_registry: &state.deps.protocol.user_registry,
            pending_storage: &state.deps.protocol.pending_delivery_storage,
            blocklist: &refreshed_blocklist,
            server_domain: state.deps.auth_state.xmpp_domain.as_str(),
            recent_tombstones: &recent_tombstones,
        },
    )
    .await;
    crate::sm_promotion::scrub_pending_for_tombstones_recorded_during_promotion(
        &state.deps.protocol.sm_session_registry,
        &state.deps.protocol.pending_delivery_storage,
        &recent_tombstones,
        "terminal recovery overflow promotion",
    )
    .await;
    if summary.has_storage_failure() {
        warn!(
            stream_id = %terminal.session.stream_id,
            storage_failed = summary.storage_failed,
            "terminal recovery overflow promotion failed durable storage; retaining for retry"
        );
        return TerminalDrainedRecordResult {
            retained: Some(entry),
            released_pending_row: false,
            queued_pending_row: summary.queued > 0,
            release_failed_pending_row: false,
        };
    }
    TerminalDrainedRecordResult {
        retained: None,
        released_pending_row: false,
        queued_pending_row: summary.queued > 0,
        release_failed_pending_row: false,
    }
}

pub(super) async fn load_terminal_recovery_blocklist(
    state: &WebSocketState,
    recipient: &jid::BareJid,
) -> Result<
    waddle_xmpp::protocol::session_state::Blocklist,
    waddle_xmpp::xep::xep0191::BlockingStorageError,
> {
    let blocked = state
        .deps
        .protocol
        .blocking_storage
        .list_blocked_jid_entries(recipient)
        .await?;
    Ok(waddle_xmpp::protocol::session_state::Blocklist::new(
        blocked,
    ))
}

/// Accumulated result of [`drive_interpret_loop`]: everything the
/// transport adapter must act on after the state machine + interpreter
/// finished a dispatch, across all feedback rounds.
#[derive(Debug, Default)]
pub(crate) struct DriveOutcome {
    /// Serialized wire frames to write, in order.
    pub(super) frames: Vec<String>,
    /// Set when any round emitted [`OutboundEvent::CloseTransport`].
    pub(super) close: bool,
    /// RFC 7395 §3.8 liveness probes to send as WS `Ping` frames
    /// (issue #1090).
    pub(super) keepalive_probes: u32,
    /// Timer effects for the connection-local timer wheel.
    pub(super) timer_commands: Vec<crate::server::routes::interpret::TimerCommand>,
}

/// Drive the interpret-loop that resolves an initial batch of
/// [`OutboundEvent`]s and any callback-feedback rounds the dispatcher
/// chain produces (e.g. `LookupArchivedMessage` -> `ArchivedMessageLoaded`
/// -> resumed pipeline events).
///
/// Returns the accumulated [`DriveOutcome`] (frames already serialized
/// via [`crate::server::routes::interpret::interpret`]). The state
/// machine `sm` is borrowed mutably so feedback events can be re-fed
/// via `sm.handle(...)` and produce the next-round `OutboundEvent`
/// batch.
pub(crate) async fn drive_interpret_loop(
    initial_events: Vec<OutboundEvent>,
    sm: &mut XmppStateMachine,
    deps: &crate::server::routes::interpret::Deps<'_>,
) -> DriveOutcome {
    let mut drive = DriveOutcome::default();
    let mut events_to_run = initial_events;
    // Each iteration: resolve the current batch, append its frames,
    // honour `close`, and if the batch produced callback-feedback,
    // feed it back through the SM to get the next batch.
    while !events_to_run.is_empty() {
        let outcome = crate::server::routes::interpret::interpret(events_to_run, deps).await;
        drive.frames.extend(outcome.frames);
        if outcome.close {
            drive.close = true;
        }
        drive.keepalive_probes += outcome.keepalive_probes;
        drive.timer_commands.extend(outcome.timer_commands);
        if outcome.feedback.is_empty() {
            break;
        }
        let mut next_events = Vec::new();
        for fb in outcome.feedback {
            next_events.extend(sm.handle(fb));
        }
        events_to_run = next_events;
    }
    drive
}
