//! Implementation behind the parent module facade.

use super::observe;
use super::websocket_stream_close_element;
use super::IngressUowError;
use super::SmInboundCompletionTracker;
use super::{apply_sm_ack_observation, observe_sm_ack, SmAckObservation};
use super::{
    debug, info, warn, ConnectionPhase, Element, FromStr, Session, SmEnable, SmResume, SmStanza,
    StreamManagementState, WebSocketState,
};
use super::{ResponseFrame, StreamErrorFrame};

pub(in crate::server::routes::websocket::stream_management) struct SmEnableClaimGuard {
    pub(in crate::server::routes::websocket::stream_management) registry:
        std::sync::Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
    pub(in crate::server::routes::websocket::stream_management) stream_id:
        waddle_xmpp::pending_delivery::SmSessionId,
    pub(in crate::server::routes::websocket::stream_management) _claim_publication:
        waddle_xmpp::ownership::CurrentNodeIdentityGuard,
    pub(in crate::server::routes::websocket::stream_management) armed: bool,
}

/// Typed post-write effect for `<enable/>`. The resumable claim is acquired
/// before the response is built, but neither local SM state nor connection-
/// registry publication occurs until the transport confirms that the
/// `<enabled/>` frame was written.
pub(in crate::server::routes::websocket) struct SmEnableCommit {
    pub(in crate::server::routes::websocket::stream_management) claim_guard:
        Option<SmEnableClaimGuard>,
    pub(in crate::server::routes::websocket::stream_management) fence:
        Option<waddle_xmpp::stream_management::persistence::SmClaimFence>,
    pub(in crate::server::routes::websocket::stream_management) stream_id:
        waddle_xmpp::pending_delivery::SmSessionId,
    pub(in crate::server::routes::websocket::stream_management) resume: bool,
    pub(in crate::server::routes::websocket::stream_management) max: u32,
}

impl SmEnableCommit {
    pub(in crate::server::routes::websocket::stream_management) fn new(
        claim_guard: Option<SmEnableClaimGuard>,
        fence: Option<waddle_xmpp::stream_management::persistence::SmClaimFence>,
        stream_id: waddle_xmpp::pending_delivery::SmSessionId,
        resume: bool,
        max: u32,
    ) -> Self {
        Self {
            claim_guard,
            fence,
            stream_id,
            resume,
            max,
        }
    }

    #[cfg(feature = "test-support")]
    pub(in crate::server::routes::websocket) fn stream_id(
        &self,
    ) -> &waddle_xmpp::pending_delivery::SmSessionId {
        &self.stream_id
    }

    pub(in crate::server::routes::websocket) fn publish(
        mut self,
        state: &WebSocketState,
        sm_state: &mut StreamManagementState,
        sm_ingress_fence: &mut Option<waddle_xmpp::stream_management::persistence::SmClaimFence>,
        bound_jid: Option<&jid::FullJid>,
        registry_owner: Option<&waddle_xmpp::registry::LocalConnectionBindingId>,
    ) {
        let registry_published = match (bound_jid, registry_owner) {
            (Some(jid), Some(owner)) => state
                .deps
                .protocol
                .connection_registry
                .set_sm_stream_id_if_owner(jid, owner, Some(self.stream_id.clone())),
            _ => false,
        };

        // The transport has already written `<enabled/>`; XEP-0198 makes
        // that positive reply the SM-session commit point. A concurrent
        // same-JID replacement may reject only the shared registry alias,
        // never roll back the state the peer has already observed.
        *sm_ingress_fence = self.fence.take();
        sm_state.enable(self.stream_id.to_string(), self.resume, Some(self.max));
        if let Some(guard) = self.claim_guard.take() {
            guard.commit();
        }
        // Only a RESUMABLE enable mints a claim fence (`ensure_session_claim`
        // is skipped for resume='false'), so only a resumable publication can
        // have raced a demotion — a fenceless non-resumable stream is the
        // normal state, not a kill signal.
        if registry_published && self.resume {
            if let (Some(jid), Some(owner)) = (bound_jid, registry_owner) {
                terminate_if_demoted_during_publication(state, jid, owner, Some(&self.stream_id));
            }
        }
        if !registry_published {
            debug!(
                stream_id = %self.stream_id,
                "SM enabled after transport commit without publishing a stale registry alias"
            );
        }
        info!(stream_id = %self.stream_id, resume = self.resume, max = self.max, "SM enabled");
    }
}

impl SmEnableClaimGuard {
    pub(in crate::server::routes::websocket::stream_management) fn new(
        registry: std::sync::Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
        stream_id: waddle_xmpp::pending_delivery::SmSessionId,
        claim_publication: waddle_xmpp::ownership::CurrentNodeIdentityGuard,
    ) -> Self {
        Self {
            registry,
            stream_id,
            _claim_publication: claim_publication,
            armed: true,
        }
    }

    pub(in crate::server::routes::websocket::stream_management) fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for SmEnableClaimGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if !self
            .registry
            .defer_unpublished_enabled_claim_release(self.stream_id.as_str())
        {
            warn!(
                stream_id = %self.stream_id,
                "SM enable cancelled before publication but exact claim cleanup could not be inventoried"
            );
        }
    }
}

/// Event-driven closure of the demotion-vs-publication race: an SM claim
/// demotion that runs between fence recording and stream-owner-index
/// publication cannot find the connection to signal (its poller is
/// best-effort and any finite budget can be outlived by a stalled response
/// write). The PUBLISHER holds the binding at exactly the moment it lands,
/// so it verifies post-publication that the fence survived and self-sends
/// the force-detach the demotion could not deliver — the prioritized
/// force-detach arm then routes the connection through the fenceless
/// terminal-promotion teardown. The ack receiver is dropped on purpose:
/// nobody waits on a self-signal.
pub(in crate::server::routes::websocket) fn terminate_if_demoted_during_publication(
    state: &WebSocketState,
    jid: &jid::FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    stream_id: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
) {
    let Some(stream_id) = stream_id.map(waddle_xmpp::pending_delivery::SmSessionId::as_str) else {
        return;
    };
    if state
        .deps
        .protocol
        .sm_session_registry
        .current_sm_claim_fence(stream_id)
        .is_some()
    {
        return;
    }
    let Some(entry) = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(jid, owner)
    else {
        return;
    };
    warn!(
        stream_id,
        jid = %jid,
        "SM claim was demoted during stream-id publication; terminating the connection"
    );
    let (ack, _ack_rx) = tokio::sync::oneshot::channel();
    let request = waddle_xmpp::registry::ForceDetachRequest {
        origin: waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
        target_binding: entry.binding_ref().cloned(),
        requester_bare_jid: jid.to_bare(),
        ack,
    };
    if entry.force_detach_sender().try_send(request).is_err() {
        // Channel full or closed: the connection is already being torn
        // down or has queued kills ahead of this one — either path ends
        // in the same fenceless terminal teardown.
        debug!(
            stream_id,
            "self force-detach after demoted publication not enqueued"
        );
    }
}

pub(in crate::server::routes::websocket) fn defer_superseded_sm_claim(
    state: &WebSocketState,
    sm_state: &StreamManagementState,
) {
    if !sm_state.is_resumable() {
        return;
    }
    let Some(stream_id) = sm_state.stream_id.as_deref() else {
        return;
    };
    if !state
        .deps
        .protocol
        .sm_session_registry
        .defer_superseded_enabled_claim_release(stream_id)
    {
        warn!(
            stream_id,
            "Superseded SM connection could not inventory its exact terminal claim"
        );
    }
}

/// Returns true if the frame is an XMPP stanza that counts toward XEP-0198
/// handled/sent counters. Only `<iq>`, `<message>`, `<presence>` qualify;
/// stream headers, SASL frames, and SM control nonzas do not.
///
/// Frames at this layer sit past the serialization boundary (they are
/// the exact bytes about to hit — or replay onto — the wire), so the
/// XEP-0198 decision re-enters the typed domain here: the frame is
/// parsed into a [`minidom::Element`] and classified on the resolved
/// element name, never on string prefixes (a substring match like
/// `starts_with("<message")` would also accept nonzas such as
/// `<messages>`). Anything that does not parse is by definition not a
/// stanza this server produced and does not count.
pub(in crate::server::routes::websocket) fn is_countable_stanza(frame: &str) -> bool {
    let Ok(element) = Element::from_str(frame.trim_start()) else {
        return false;
    };
    matches!(element.name(), "iq" | "message" | "presence")
}

pub(in crate::server::routes::websocket) fn sm_show_from_name(
    value: &str,
) -> Option<xmpp_parsers::presence::Show> {
    match value {
        "away" => Some(xmpp_parsers::presence::Show::Away),
        "chat" => Some(xmpp_parsers::presence::Show::Chat),
        "dnd" => Some(xmpp_parsers::presence::Show::Dnd),
        "xa" => Some(xmpp_parsers::presence::Show::Xa),
        _ => None,
    }
}

pub(in crate::server::routes::websocket) fn sm_show_name(
    show: &xmpp_parsers::presence::Show,
) -> &'static str {
    match show {
        xmpp_parsers::presence::Show::Away => "away",
        xmpp_parsers::presence::Show::Chat => "chat",
        xmpp_parsers::presence::Show::Dnd => "dnd",
        xmpp_parsers::presence::Show::Xa => "xa",
    }
}

pub(in crate::server::routes::websocket::stream_management) fn max_resume_secs_from_env() -> u32 {
    const DEFAULT_MAX_RESUME_SECS: u32 = 300;
    const MIN_MAX_RESUME_SECS: u32 = 60;
    const MAX_MAX_RESUME_SECS: u32 = 86_400;
    match std::env::var("WADDLE_SM_MAX_RESUME_SECS") {
        Ok(raw) => match raw.parse::<u32>() {
            Ok(secs) => secs.clamp(MIN_MAX_RESUME_SECS, MAX_MAX_RESUME_SECS),
            Err(_) => {
                warn!(
                    raw = %raw,
                    "WADDLE_SM_MAX_RESUME_SECS not parseable; using default {DEFAULT_MAX_RESUME_SECS}s"
                );
                DEFAULT_MAX_RESUME_SECS
            }
        },
        Err(_) => DEFAULT_MAX_RESUME_SECS,
    }
}

/// Bundle the session-level borrows that XEP-0198 control handlers mutate.
/// Passed through `handle_sm_stanza` and its helpers so each signature stays
/// below the clippy too-many-arguments threshold.
pub(in crate::server::routes::websocket) struct SmCtx<'a> {
    pub(in crate::server::routes::websocket) binding_ref:
        &'a mut Option<waddle_xmpp::ownership::BindingRef>,
    pub(in crate::server::routes::websocket) bindings:
        &'a mut crate::server::routes::websocket::connection::BindingLedger,
    #[cfg(test)]
    pub(in crate::server::routes::websocket) resume_binding_attempts:
        &'a mut Vec<waddle_xmpp::ownership::BindingRef>,
    pub(in crate::server::routes::websocket) phase: &'a mut ConnectionPhase,
    /// A failed resume left accepted work on this socket's old outbound
    /// queue, so the socket cannot publish another binding.
    pub(in crate::server::routes::websocket) resume_endpoint_handoff_blocked: bool,
    pub(in crate::server::routes::websocket) sm_state: &'a mut StreamManagementState,
    pub(in crate::server::routes::websocket) sm_ingress_fence:
        &'a mut Option<waddle_xmpp::stream_management::persistence::SmClaimFence>,
    pub(in crate::server::routes::websocket) sm_inbound_completion:
        &'a mut SmInboundCompletionTracker,
    pub(in crate::server::routes::websocket) authenticated_session: &'a mut Option<Session>,
    pub(in crate::server::routes::websocket) occupancy_session:
        &'a mut waddle_xmpp_core::OccupancySessionGeneration,
    pub(in crate::server::routes::websocket) carbons_enabled: &'a mut bool,
    pub(in crate::server::routes::websocket) presence_available: &'a mut bool,
    pub(in crate::server::routes::websocket) presence_show:
        &'a mut Option<xmpp_parsers::presence::Show>,
    pub(in crate::server::routes::websocket) presence_status: &'a mut Option<String>,
    pub(in crate::server::routes::websocket) presence_priority: &'a mut i8,
    pub(in crate::server::routes::websocket) presence_payloads: &'a mut Vec<minidom::Element>,
    pub(in crate::server::routes::websocket) pending_subscribes_flushed: &'a mut bool,
    pub(in crate::server::routes::websocket) pending_resume_stream_id: &'a mut Option<String>,
    pub(in crate::server::routes::websocket) pending_resume_h: &'a mut Option<u32>,
    /// Set by `handle_sm_resume` so the main loop skips SM recording for
    /// the responses it returns — those are replay stanzas already tracked
    /// in the unacked queue.
    pub(in crate::server::routes::websocket) suppress_sm_record_next_batch: &'a mut bool,
    pub(in crate::server::routes::websocket) roster_interested: &'a mut bool,
    pub(in crate::server::routes::websocket) blocklist_interested: &'a mut bool,
    pub(in crate::server::routes::websocket) pending_sm_enable_commit:
        &'a mut Option<SmEnableCommit>,
    /// The connection's handle on the durable session authority.
    pub(in crate::server::routes::websocket) durable_sm: &'a mut super::durable::DurableSmLink,
}

/// Dispatch an XEP-0198 control nonza. Isolated helper so the main frame
/// dispatcher stays flat.
pub(in crate::server::routes::websocket) async fn handle_sm_stanza(
    sm: SmStanza,
    state: &WebSocketState,
    ctx: SmCtx<'_>,
    finalized_resume_outcome: &mut Option<waddle_xmpp::telemetry::attributes::SmResumeOutcome>,
) -> Vec<ResponseFrame> {
    match sm {
        SmStanza::Enable(enable) => {
            handle_sm_enable(
                enable,
                state,
                ctx.sm_state,
                ctx.phase,
                ctx.pending_sm_enable_commit,
                SmEnableOwner {
                    durable_sm: ctx.durable_sm,
                    binding: ctx.binding_ref.as_ref(),
                    session: ctx.authenticated_session.as_ref(),
                    occupancy_session: *ctx.occupancy_session,
                },
            )
            .await
        }
        SmStanza::Request => handle_sm_request(
            state,
            ctx.phase,
            ctx.sm_state,
            ctx.sm_inbound_completion,
            ctx.durable_sm,
        )
        .await
        .map(|ack| ResponseFrame::from_serialized_xml(ack.to_xml()))
        .into_iter()
        .collect(),
        SmStanza::Ack(ack) => {
            apply_sm_ack_with_fence(
                state,
                ctx.sm_state,
                ctx.phase,
                ack.h,
                ctx.sm_ingress_fence.as_ref(),
                ctx.durable_sm,
            )
            .await
        }
        SmStanza::Resume(resume) => {
            handle_sm_resume(resume, state, ctx, finalized_resume_outcome).await
        }
        // Server-origin nonzas should never arrive from a client. Ignore.
        SmStanza::Enabled(_) | SmStanza::Resumed(_) | SmStanza::Failed(_) => vec![],
    }
}

/// Flush the handled prefix before returning the one possible SM response.
pub(in crate::server::routes::websocket) async fn handle_sm_request(
    state: &WebSocketState,
    phase: &mut ConnectionPhase,
    sm_state: &mut StreamManagementState,
    completion: &mut SmInboundCompletionTracker,
    durable_sm: &mut super::durable::DurableSmLink,
) -> Option<waddle_xmpp::stream_management::SmAck> {
    if flush_ingress_checkpoint(state, sm_state, completion)
        .await
        .is_err()
    {
        *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
        return None;
    }
    durable_sm
        .report_inbound_handled(sm_state.inbound_count())
        .await;
    if durable_sm.is_superseded() {
        // Another generation owns the session; this binding answers nothing
        // more for it and retires.
        return None;
    }
    Some(waddle_xmpp::stream_management::SmAck::new(
        sm_state.inbound_count(),
    ))
}

pub(in crate::server::routes::websocket::stream_management) async fn complete_acknowledged_ingress_custody(
    state: &WebSocketState,
    sm_state: &StreamManagementState,
    h: u32,
) -> bool {
    let Some(stream) = sm_state.stream_id.as_deref() else {
        return true;
    };
    match state
        .deps
        .protocol
        .sm_session_registry
        .complete_ingress_appends_through(
            &waddle_xmpp::pending_delivery::SmSessionId::new(stream),
            sm_state.last_acked,
            h,
        )
        .await
    {
        Ok(()) => true,
        Err(error) => {
            warn!(%error, stream, "cannot durably acknowledge ingress custody");
            false
        }
    }
}

pub(in crate::server::routes::websocket) async fn complete_acknowledged_ingress_receipts(
    state: &WebSocketState,
    sm_state: &StreamManagementState,
    h: u32,
) -> bool {
    super::super::batch_write::complete_replayed_ingress_receipts(
        state,
        &sm_state.ingress_receipts_through(h),
    )
    .await
}

/// Apply a client `<a h='N'/>` ack: advance the SM counters, drop the
/// acked prefix of the unacked queue, and range-delete every
/// `pending_delivery` row this XEP-0198 session claimed whose
/// recorded outbound counter lies in the newly-acknowledged mod-2^32
/// window `(last_acked, h]`.
///
/// Locked Q7b SM-ack lifecycle (issue #209): the range-delete is what
/// actually frees rows from the durable queue — the flush path no
/// longer deletes on push.
///
/// Session id is the XEP-0198 stream_id (NOT the resource JID — Qodo
/// review on PR #358: distinct SM sessions on the same resource share
/// the same JID, so keying by JID would let one session's ack delete
/// another's claimed rows). The flush function reads the same
/// stream_id from the connection's `ConnectionEntry` so claim and
/// delete agree on the key.
///
/// Greptile review on PR #358: this MUST run inline so it executes
/// after any preceding `record_pushed_at` for the same connection.
/// Spawning would let a quick ack arrive and run delete_acked_in_window
/// against a row whose outbound_sequence is still NULL (because the
/// record_pushed_at task hadn't completed), silently skipping the
/// delete.
///
/// Shared by the `<a/>` frame handler and the mid-batch drain in
/// [`super::super::batch_write`] (issue #1089) so both paths honour the same
/// ack lifecycle.
///
/// Issue #1099 / XEP-0198 §4: `h` is validated wrap-aware (mod 2^32)
/// against the live `outbound_count` BEFORE anything is acknowledged.
/// A bogus-high `h` previously purged the whole replay queue and
/// range-deleted the session's claimed `pending_delivery` rows,
/// silently destroying undelivered messages. On violation nothing is
/// purged; the returned frames are the `<handled-count-too-high/>`
/// undefined-condition stream error plus the RFC 7395 close frame
/// (mirroring the resume path), and the connection phase is set to
/// Closing so the loop terminates the stream.
#[cfg(test)]
pub(in crate::server::routes::websocket) async fn apply_sm_ack(
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    phase: &mut ConnectionPhase,
    h: u32,
) -> Vec<ResponseFrame> {
    apply_sm_ack_with_fence(
        state,
        sm_state,
        phase,
        h,
        None,
        &mut super::durable::DurableSmLink::default(),
    )
    .await
}

pub(in crate::server::routes::websocket) async fn apply_sm_ack_with_fence(
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    phase: &mut ConnectionPhase,
    h: u32,
    expected_fence: Option<&waddle_xmpp::stream_management::persistence::SmClaimFence>,
    durable_sm: &mut super::durable::DurableSmLink,
) -> Vec<ResponseFrame> {
    // A durable session validates the count against its committed exposure
    // ceiling and releases the acknowledged frames before the connection's
    // projection moves.
    match durable_sm.settle_ack(h).await {
        super::durable::AckVerdict::Apply => {}
        super::durable::AckVerdict::HandledInvalid => {
            let observation = SmAckObservation::TooHigh {
                acknowledged: h,
                send_count: sm_state.outbound_count,
            };
            observe_sm_ack(observation);
            info!(
                stream_id = %sm_state.stream_id.as_deref().unwrap_or("<unset>"),
                outcome = ?observation.outcome(),
                client_h = h,
                send_count = sm_state.outbound_count,
                "SM ack rejected by the durable authority: handled count was never exposed"
            );
            *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
            return vec![
                ResponseFrame::from(StreamErrorFrame::HandledCountTooHigh {
                    acknowledged: h,
                    send_count: sm_state.outbound_count,
                }),
                ResponseFrame::from(websocket_stream_close_element()),
            ];
        }
        // The binding retires; it owes this stream no further frame.
        super::durable::AckVerdict::Superseded => return Vec::new(),
        super::durable::AckVerdict::Unavailable => {
            *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
            return vec![
                ResponseFrame::from(Element::from(xmpp_parsers::stream_error::StreamError {
                    condition: xmpp_parsers::stream_error::DefinedCondition::InternalServerError,
                    texts: Default::default(),
                    application_specific: Vec::new(),
                })),
                ResponseFrame::from(websocket_stream_close_element()),
            ];
        }
    }
    if !sm_state.ack_regresses_last_acked(h) && !sm_state.ack_exceeds_outbound(h) {
        // Keep the old acknowledgement window on a failed custody write so a
        // resume can retry it. Acknowledging only in memory would leave the
        // independent durable recovery queue replaying an already-handled stanza.
        if !complete_acknowledged_ingress_custody(state, sm_state, h).await {
            *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
            let error: Element = xmpp_parsers::stream_error::StreamError {
                condition: xmpp_parsers::stream_error::DefinedCondition::InternalServerError,
                texts: Default::default(),
                application_specific: Vec::new(),
            }
            .into();
            return vec![
                ResponseFrame::from(error),
                ResponseFrame::from(websocket_stream_close_element()),
            ];
        }
        // Move proofs to the authority's retry queue before releasing replay
        // carriers. Receipt storage must not veto a valid XEP-0198 ACK.
        super::complete_acknowledged_ingress_receipts(state, sm_state, h).await;
    }
    let observation = apply_sm_ack_observation(sm_state, h);
    observe_sm_ack(observation);
    if matches!(
        observation,
        SmAckObservation::Advanced { .. } | SmAckObservation::Duplicate { .. }
    ) {
        waddle_xmpp::telemetry::authority::record_sm_unacked_depth(
            sm_state.unacked_count() as usize
        );
    }

    match observation {
        SmAckObservation::Regressed => {
            warn!(
                stream_id = %sm_state.stream_id.as_deref().unwrap_or("<unset>"),
                outcome = ?observation.outcome(),
                client_h = h,
                last_acked = sm_state.last_acked,
                "SM ack ignored: handled count regressed behind last_acked"
            );
            vec![]
        }
        SmAckObservation::TooHigh {
            acknowledged,
            send_count,
        } => {
            info!(
                stream_id = %sm_state.stream_id.as_deref().unwrap_or("<unset>"),
                outcome = ?observation.outcome(),
                client_h = acknowledged,
                send_count,
                "SM ack rejected: handled count too high"
            );
            *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
            vec![
                ResponseFrame::from(StreamErrorFrame::HandledCountTooHigh {
                    acknowledged,
                    send_count,
                }),
                ResponseFrame::from(websocket_stream_close_element()),
            ]
        }
        SmAckObservation::Duplicate { .. } => {
            if matches!(
                settle_pending_ack_window(state, sm_state, h, h, expected_fence).await,
                Err(waddle_xmpp::pending_delivery::storage::PendingStorageError::NotOwner { .. })
            ) {
                *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
                return vec![
                    ResponseFrame::from(Element::from(xmpp_parsers::stream_error::StreamError {
                        condition:
                            xmpp_parsers::stream_error::DefinedCondition::InternalServerError,
                        texts: Default::default(),
                        application_specific: Vec::new(),
                    })),
                    ResponseFrame::from(websocket_stream_close_element()),
                ];
            }
            vec![]
        }
        SmAckObservation::Advanced {
            acked_from_exclusive,
            ..
        } => {
            if matches!(
                settle_pending_ack_window(
                    state,
                    sm_state,
                    acked_from_exclusive,
                    h,
                    expected_fence,
                )
                .await,
                Err(waddle_xmpp::pending_delivery::storage::PendingStorageError::NotOwner {
                    ..
                })
            ) {
                *phase = ConnectionPhase::closing(phase.bound_jid().cloned());
                return vec![
                    ResponseFrame::from(Element::from(xmpp_parsers::stream_error::StreamError {
                        condition:
                            xmpp_parsers::stream_error::DefinedCondition::InternalServerError,
                        texts: Default::default(),
                        application_specific: Vec::new(),
                    })),
                    ResponseFrame::from(websocket_stream_close_element()),
                ];
            }
            vec![]
        }
    }
}

pub(in crate::server::routes::websocket::stream_management) async fn settle_pending_ack_window(
    state: &WebSocketState,
    sm_state: &StreamManagementState,
    acked_from_exclusive: u32,
    h: u32,
    expected_fence: Option<&waddle_xmpp::stream_management::persistence::SmClaimFence>,
) -> Result<(), waddle_xmpp::pending_delivery::storage::PendingStorageError> {
    if let Some(stream_id) = sm_state.stream_id.clone() {
        let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(stream_id);
        match state
            .deps
            .protocol
            .pending_delivery_storage
            .delete_acked_in_window(
                &session_id,
                acked_from_exclusive,
                h,
                match expected_fence {
                    Some(fence) => {
                        waddle_xmpp::pending_delivery::storage::PendingAckAuthority::Fenced(fence)
                    }
                    None if sm_state.enabled
                        && sm_state.stream_id.is_some()
                        && !sm_state.is_resumable() =>
                    {
                        waddle_xmpp::pending_delivery::storage::PendingAckAuthority::NonResumable
                    }
                    None => {
                        waddle_xmpp::pending_delivery::storage::PendingAckAuthority::Unavailable
                    }
                },
            )
            .await
        {
            Ok(removed) if removed > 0 => {
                debug!(
                    session = %session_id,
                    h,
                    removed,
                    "pending_delivery rows cleared by SM ack"
                );
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    session = %session_id,
                    h,
                    %error,
                    failure = "storage",
                    "pending_delivery ACK deletion retained; claim release must settle it"
                );
                return Err(error);
            }
        }
    }
    Ok(())
}

/// The binding and principal an `<enable/>` is made for, with the
/// connection's handle on the durable authority.
pub(in crate::server::routes::websocket::stream_management) struct SmEnableOwner<'a> {
    pub(in crate::server::routes::websocket::stream_management) durable_sm:
        &'a mut super::durable::DurableSmLink,
    pub(in crate::server::routes::websocket::stream_management) binding:
        Option<&'a waddle_xmpp::ownership::BindingRef>,
    pub(in crate::server::routes::websocket::stream_management) session: Option<&'a Session>,
    /// The occupancy generation the connection's resource is bound under.
    pub(in crate::server::routes::websocket::stream_management) occupancy_session:
        waddle_xmpp_core::OccupancySessionGeneration,
}

pub(in crate::server::routes::websocket::stream_management) async fn handle_sm_enable(
    enable: SmEnable,
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    phase: &ConnectionPhase,
    pending_commit: &mut Option<SmEnableCommit>,
    owner: SmEnableOwner<'_>,
) -> Vec<ResponseFrame> {
    use waddle_xmpp::stream_management::{SmEnabled, SmFailed};
    use waddle_xmpp::telemetry::attributes::SmEnableOutcome;
    use waddle_xmpp::telemetry::authority::record_sm_enable;

    if !phase.allows_stream_management_enable() {
        record_sm_enable(SmEnableOutcome::Refused);
        return vec![ResponseFrame::from(
            SmFailed::with_condition("unexpected-request").to_element(),
        )];
    }
    // An enable whose durable commit outcome is unknown holds the binding:
    // a second one could commit a second session for it.
    if sm_state.enabled || pending_commit.is_some() || !owner.durable_sm.admits_enable() {
        record_sm_enable(SmEnableOutcome::Refused);
        return vec![ResponseFrame::from(
            SmFailed::with_condition("unexpected-request").to_element(),
        )];
    }

    let max_resume_secs = max_resume_secs_from_env();
    let max = match enable.max {
        Some(m) if m > max_resume_secs => {
            waddle_xmpp::telemetry::reliability::increment_sm_resume_window_clamped();
            max_resume_secs
        }
        Some(m) => m,
        None => max_resume_secs,
    };
    if enable.resume {
        // Every resumable session belongs to the durable authority. A state
        // that never selected it offers no resumption.
        let Some(store) = super::durable::selected_store(state) else {
            record_sm_enable(SmEnableOutcome::Refused);
            return vec![ResponseFrame::from(
                SmFailed::with_condition("feature-not-implemented").to_element(),
            )];
        };
        return super::durable::enable(
            store,
            state,
            super::durable::DurableEnable {
                link: owner.durable_sm,
                binding: owner.binding,
                session: owner.session,
                full_jid: phase.bound_jid(),
                max_resume_secs: max,
                occupancy_session: owner.occupancy_session,
                pending_commit,
            },
        )
        .await;
    }
    // A non-resumable stream has no `previd`, is never detached and cannot
    // be resumed, so it takes no session claim and enrolls no ingress
    // stream. Its counters live only on this connection.
    let stream_id = uuid::Uuid::new_v4().to_string();
    *pending_commit = Some(SmEnableCommit::new(
        None,
        None,
        waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.clone()),
        false,
        max,
    ));
    record_sm_enable(SmEnableOutcome::EnabledWithoutResume);
    vec![ResponseFrame::from(SmEnabled::new(stream_id).to_element())]
}

pub(in crate::server::routes::websocket::stream_management) async fn handle_sm_resume(
    resume: SmResume,
    state: &WebSocketState,
    ctx: SmCtx<'_>,
    finalized_resume_outcome: &mut Option<waddle_xmpp::telemetry::attributes::SmResumeOutcome>,
) -> Vec<ResponseFrame> {
    // Every resumable session belongs to the durable authority, so every
    // `<resume/>` is its decision.
    if let Some(store) = super::durable::selected_store(state) {
        return match super::durable::resume(store, resume, state, ctx).await {
            // The reply is decided when the endpoint is published, or when
            // an unknown commit outcome is settled.
            super::durable::DurableResumeStep::Staged => Vec::new(),
            super::durable::DurableResumeStep::Failed(terminal) => {
                observe::observe_sm_resume(&terminal);
                match terminal.outcome() {
                    observe::SmResumeOutcome::ShutdownAbandoned => {
                        observe::observe_sm_resume_finalized(
                            observe::SmResumeOutcome::ShutdownAbandoned,
                            None,
                        );
                    }
                    outcome => *finalized_resume_outcome = Some(outcome),
                }
                terminal.into_frames()
            }
        };
    }
    // A state that never selected the durable authority holds no resumable
    // session.
    let terminal = observe::SmResumeTerminal::failed(
        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
        observe::SmResumeOutcome::NotFound,
    );
    observe::observe_sm_resume(&terminal);
    *finalized_resume_outcome = Some(terminal.outcome());
    terminal.into_frames()
}

/// Persist a newly exposed contiguous frontier before acknowledging it.
pub(in crate::server::routes::websocket) async fn flush_ingress_checkpoint(
    state: &WebSocketState,
    sm_state: &StreamManagementState,
    tracker: &mut SmInboundCompletionTracker,
) -> Result<(), IngressUowError> {
    if !tracker.checkpoint_dirty() || !sm_state.is_resumable() {
        return Ok(());
    }
    let stream_id = sm_state
        .stream_id
        .as_ref()
        .ok_or(IngressUowError::SmIngressStreamMissing)?;
    let stream = state
        .deps
        .protocol
        .ingress
        .lookup_stream(&waddle_xmpp::pending_delivery::SmSessionId::new(
            stream_id.clone(),
        ))
        .await?
        .ok_or(IngressUowError::SmIngressStreamMissing)?;
    state
        .deps
        .protocol
        .ingress
        .flush_checkpoint(
            stream,
            waddle_xmpp::ingress::WireHandledCount::from_storage(sm_state.inbound_count()),
        )
        .await?;
    tracker.checkpoint_flushed(sm_state.inbound_count());
    Ok(())
}
