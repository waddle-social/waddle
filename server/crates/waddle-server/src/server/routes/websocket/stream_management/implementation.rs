//! Implementation behind the parent module facade.

use super::observe;
use super::websocket_stream_close_element;
#[cfg(test)]
use super::Arc;
use super::DbExecute;
use super::IngressUowError;
use super::PrincipalResolution;
use super::SmInboundCompletionTracker;
use super::{apply_sm_ack_observation, observe_sm_ack, SmAckObservation};
use super::{
    debug, info, warn, BareJid, ConnectionPhase, Element, FromStr, Session, SmEnable, SmResume,
    SmStanza, StreamManagementState, WebSocketState,
};
use super::{ResponseFrame, StreamErrorFrame};

/// Operation-owned resume claim.  Dropping an uncommitted guard returns the
/// frozen snapshot to the registry without creating server-local claim state.
pub(in crate::server::routes::websocket) struct SmResumeClaimGuard {
    pub(in crate::server::routes::websocket::stream_management) registry:
        std::sync::Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
    pub(in crate::server::routes::websocket::stream_management) stream_id: String,
    pub(in crate::server::routes::websocket::stream_management) armed: bool,
}

impl SmResumeClaimGuard {
    pub(in crate::server::routes::websocket::stream_management) fn new(
        registry: std::sync::Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
        stream_id: String,
    ) -> Self {
        Self {
            registry,
            stream_id,
            armed: true,
        }
    }

    pub(in crate::server::routes::websocket) fn commit(mut self) {
        self.armed = false;
    }

    pub(in crate::server::routes::websocket::stream_management) async fn release(mut self) {
        if self.registry.release_claim(&self.stream_id).await.is_ok() {
            self.armed = false;
        }
    }
}

impl Drop for SmResumeClaimGuard {
    fn drop(&mut self) {
        if self.armed && !self.registry.defer_claimed_resume_release(&self.stream_id) {
            warn!(stream_id = %self.stream_id, "cancelled SM resume could not retain exact claim cleanup responsibility");
        }
    }
}

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
    pub(in crate::server::routes::websocket) phase: &'a mut ConnectionPhase,
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
    pub(in crate::server::routes::websocket) pending_resume_claim:
        &'a mut Option<SmResumeClaimGuard>,
    #[cfg(test)]
    pub(in crate::server::routes::websocket) pre_final_principal_recheck_test_hook:
        &'a mut Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    /// Set by `handle_sm_resume` so the main loop skips SM recording for
    /// the responses it returns — those are replay stanzas already tracked
    /// in the unacked queue.
    pub(in crate::server::routes::websocket) suppress_sm_record_next_batch: &'a mut bool,
    pub(in crate::server::routes::websocket) roster_interested: &'a mut bool,
    pub(in crate::server::routes::websocket) blocklist_interested: &'a mut bool,
    pub(in crate::server::routes::websocket) pending_sm_enable_commit:
        &'a mut Option<SmEnableCommit>,
}

/// Dispatch an XEP-0198 control nonza. Isolated helper so the main frame
/// dispatcher stays flat.
pub(in crate::server::routes::websocket) async fn handle_sm_stanza(
    sm: SmStanza,
    state: &WebSocketState,
    ctx: SmCtx<'_>,
    finalized_resume_outcome: &mut Option<waddle_xmpp::telemetry::attributes::SmResumeOutcome>,
) -> Vec<ResponseFrame> {
    use waddle_xmpp::stream_management::SmAck;

    match sm {
        SmStanza::Enable(enable) => {
            handle_sm_enable(
                enable,
                state,
                ctx.sm_state,
                ctx.phase,
                ctx.pending_sm_enable_commit,
            )
            .await
        }
        SmStanza::Request => {
            if flush_ingress_checkpoint(state, ctx.sm_state, ctx.sm_inbound_completion)
                .await
                .is_err()
            {
                *ctx.phase = ConnectionPhase::closing(ctx.phase.bound_jid().cloned());
                return vec![];
            }
            vec![ResponseFrame::from_serialized_xml(
                SmAck::new(ctx.sm_state.inbound_count()).to_xml(),
            )]
        }
        SmStanza::Ack(ack) => apply_sm_ack(state, ctx.sm_state, ctx.phase, ack.h).await,
        SmStanza::Resume(resume) => {
            handle_sm_resume(resume, state, ctx, finalized_resume_outcome).await
        }
        // Server-origin nonzas should never arrive from a client. Ignore.
        SmStanza::Enabled(_) | SmStanza::Resumed(_) | SmStanza::Failed(_) => vec![],
    }
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
pub(in crate::server::routes::websocket) async fn apply_sm_ack(
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    phase: &mut ConnectionPhase,
    h: u32,
) -> Vec<ResponseFrame> {
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
            settle_pending_ack_window(state, sm_state, h, h).await;
            vec![]
        }
        SmAckObservation::Advanced {
            acked_from_exclusive,
            ..
        } => {
            settle_pending_ack_window(state, sm_state, acked_from_exclusive, h).await;
            vec![]
        }
    }
}

pub(in crate::server::routes::websocket::stream_management) async fn settle_pending_ack_window(
    state: &WebSocketState,
    sm_state: &StreamManagementState,
    acked_from_exclusive: u32,
    h: u32,
) -> bool {
    if let Some(stream_id) = sm_state.stream_id.clone() {
        let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(stream_id);
        match state
            .deps
            .protocol
            .pending_delivery_storage
            .delete_acked_in_window(&session_id, acked_from_exclusive, h)
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
            Err(_) => {
                warn!(
                    session = %session_id,
                    h,
                    failure = "storage",
                    "pending_delivery ACK deletion retained; claim release must settle it"
                );
                return false;
            }
        }
    }
    true
}

pub(in crate::server::routes::websocket::stream_management) async fn handle_sm_enable(
    enable: SmEnable,
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    phase: &ConnectionPhase,
    pending_commit: &mut Option<SmEnableCommit>,
) -> Vec<ResponseFrame> {
    use waddle_xmpp::stream_management::{SmEnabled, SmFailed};

    if !phase.allows_stream_management_enable() {
        return vec![ResponseFrame::from(
            SmFailed::with_condition("unexpected-request").to_element(),
        )];
    }
    if sm_state.enabled || pending_commit.is_some() {
        return vec![ResponseFrame::from(
            SmFailed::with_condition("unexpected-request").to_element(),
        )];
    }

    let stream_id = uuid::Uuid::new_v4().to_string();
    let max_resume_secs = max_resume_secs_from_env();
    let max = match enable.max {
        Some(m) if m > max_resume_secs => {
            waddle_xmpp::telemetry::reliability::increment_sm_resume_window_clamped();
            max_resume_secs
        }
        Some(m) => m,
        None => max_resume_secs,
    };
    // ADR-0017 Phase 3 Slice 6, element 8: for a resumable enable, ensure
    // this node's `ClaimStore` claim at `<enable/>` time — not only at detach
    // time (Slice 5's `acquire_claim_store_entry_for_detach`). Without a
    // claim row for a still-live session, a cross-node resume attempt
    // would have nothing to discover: it needs the `clustering_claims` row
    // to exist to know this entity is "live, owned by this node" at all.
    // `ensure_claimed`'s self-idempotence (see the method's own doc
    // comment) is exactly what keeps this call from spuriously conflicting
    // with the detach-time call for the same stream id later in this
    // session's lifetime. No-op in practice for single-node deployments
    // (`InProcessClaimStore`'s bookkeeping, never a foreign conflict).
    // A non-resumable stream has no `previd`, is never detached, and cannot
    // be discovered or resumed cross-node. Giving it a durable ownership
    // claim would retain an entity with no terminal close path, so it does
    // not participate in clustered SM ownership at all.
    let claim_publication = if enable.resume {
        let Some(publication) = state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim(&stream_id)
            .await
        else {
            warn!(
                stream_id = %stream_id,
                "SM enable rejected because exact claim admission did not complete"
            );
            return vec![ResponseFrame::from(
                SmFailed::with_condition("resource-constraint").to_element(),
            )];
        };
        Some(publication)
    } else {
        None
    };
    let fence = state
        .deps
        .protocol
        .sm_session_registry
        .current_sm_claim_fence(&stream_id);
    let claim_guard = claim_publication.map(|publication| {
        SmEnableClaimGuard::new(
            state.deps.protocol.sm_session_registry.clone(),
            waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.clone()),
            publication,
        )
    });
    if enable.resume
        && state
            .deps
            .protocol
            .ingress
            .enroll_stream(&waddle_xmpp::pending_delivery::SmSessionId::new(
                stream_id.clone(),
            ))
            .await
            .is_err()
    {
        return vec![ResponseFrame::from(
            SmFailed::with_condition("resource-constraint").to_element(),
        )];
    }
    // The pending commit retains both exact cleanup responsibility and the
    // identity-publication guard through the `<enabled/>` transport write.
    // Identity rotation therefore cannot demote the durable claim in the gap
    // between admission and the state the peer has observed.
    let enabled = if enable.resume {
        SmEnabled::with_resume(stream_id.clone(), max)
    } else {
        SmEnabled::new(stream_id.clone())
    };
    *pending_commit = Some(SmEnableCommit::new(
        claim_guard,
        fence,
        waddle_xmpp::pending_delivery::SmSessionId::new(stream_id),
        enable.resume,
        max,
    ));
    vec![ResponseFrame::from(enabled.to_element())]
}

/// Outcome of racing [`waddle_xmpp::stream_management::InMemorySmSessionRegistry::prepare_cross_node_resume`]
/// — the cancellable, read-only half of a cross-node resume attempt — against
/// this node's graceful-shutdown token (council-adjudicated FIX 3, corrected
/// by FIX A/deviation 47's rewrite: only the read-only `prepare` half is ever
/// raced; the write half,
/// [`waddle_xmpp::stream_management::InMemorySmSessionRegistry::finish_cross_node_steal`],
/// always runs to completion once reached — see `cross_node_resume.rs`'s
/// module doc "Cancellation boundary" section for why racing the whole
/// attempt was unsound). It remains a `waddle-server`-only concern, not part
/// of `waddle-xmpp`'s registry API.
pub(in crate::server::routes::websocket) enum CrossNodeAttemptOutcome {
    /// The attempt reached a terminal outcome (successfully or with an
    /// error) before shutdown fired, OR `finish_cross_node_steal` ran (it
    /// is never raced, so it always reaches this variant once started).
    Completed(
        Result<
            waddle_xmpp::stream_management::CrossNodeResumeOutcome,
            waddle_xmpp::stream_management::SmRegistryError,
        >,
    ),
    /// This node's graceful-shutdown token fired before `prepare_cross_node_resume`
    /// produced even a [`waddle_xmpp::stream_management::CrossNodeResumeStage::ReadyToSteal`]
    /// ticket — abandoned cleanly, never retried, and (unlike the pre-FIX-A
    /// design) provably no write was ever issued: `prepare_cross_node_resume`
    /// performs none.
    ShutdownAbandoned,
}

/// Outcome of racing only `prepare_cross_node_resume` (FIX A). Distinct from
/// [`CrossNodeAttemptOutcome`] because a `ReadyToSteal` ticket is not itself
/// a terminal state — it still needs `finish_cross_node_steal`, called
/// un-raced, immediately after.
pub(in crate::server::routes::websocket::stream_management) enum PrepareRaceOutcome {
    ShutdownAbandoned,
    Terminal(
        Result<
            waddle_xmpp::stream_management::CrossNodeResumeOutcome,
            waddle_xmpp::stream_management::SmRegistryError,
        >,
    ),
    ReadyToSteal(waddle_xmpp::stream_management::StealTicket),
}

/// Attempt cross-node XEP-0198 resume for `stream_id`, verifying `bare_jid`
/// (the caller's already-established identity) against whatever cross-node
/// claim/persisted snapshot is found. Races only the cancellable
/// `prepare_cross_node_resume` half against this node's graceful-shutdown
/// token (FIX A/deviation 47's rewrite) — `finish_cross_node_steal` always
/// runs to completion once reached; see `cross_node_resume.rs`'s module doc,
/// "Cancellation boundary" section, for why racing the whole attempt is
/// unsound.
///
/// This is the single XEP-0198 resume path and the only place its
/// cancellation-safe cross-node machinery races against shutdown.
pub(in crate::server::routes::websocket) async fn attempt_cross_node_resume_raced(
    state: &WebSocketState,
    stream_id: &str,
    bare_jid: &BareJid,
) -> super::CrossNodeAttemptOutcome {
    let handshake_budget = state
        .deps
        .app_state
        .clustering_claims
        .resume_handshake_timeout()
        // Unreachable in practice: a `None` budget only occurs when
        // clustering is disabled/not compiled in, in which case
        // `prepare_cross_node_resume` itself short-circuits to `NotFound`
        // before ever consulting this budget.
        .unwrap_or(std::time::Duration::ZERO);
    let shutdown_token = state.deps.shutdown.stop_token();
    let prepared = tokio::select! {
        biased;
        _ = shutdown_token.cancelled() => PrepareRaceOutcome::ShutdownAbandoned,
        result = state
            .deps
            .protocol
            .sm_session_registry
            .prepare_cross_node_resume(stream_id, bare_jid, handshake_budget) => {
            match result {
                Ok(waddle_xmpp::stream_management::CrossNodeResumeStage::Terminal(outcome)) => {
                    PrepareRaceOutcome::Terminal(Ok(outcome))
                }
                Ok(waddle_xmpp::stream_management::CrossNodeResumeStage::ReadyToSteal(ticket)) => {
                    PrepareRaceOutcome::ReadyToSteal(ticket)
                }
                Err(error) => PrepareRaceOutcome::Terminal(Err(error)),
            }
        }
    };
    match prepared {
        PrepareRaceOutcome::ShutdownAbandoned => CrossNodeAttemptOutcome::ShutdownAbandoned,
        PrepareRaceOutcome::Terminal(result) => CrossNodeAttemptOutcome::Completed(result),
        PrepareRaceOutcome::ReadyToSteal(ticket) => CrossNodeAttemptOutcome::Completed(
            state
                .deps
                .protocol
                .sm_session_registry
                .finish_cross_node_steal(ticket)
                .await,
        ),
    }
}

pub(in crate::server::routes::websocket::stream_management) async fn handle_sm_resume(
    resume: SmResume,
    state: &WebSocketState,
    ctx: SmCtx<'_>,
    finalized_resume_outcome: &mut Option<waddle_xmpp::telemetry::attributes::SmResumeOutcome>,
) -> Vec<ResponseFrame> {
    let terminal = handle_sm_resume_terminal(resume, state, ctx).await;
    observe::observe_sm_resume(&terminal);
    // Terminal results are recorded only once the frame carrying them is
    // written (the main loop consumes this slot after the batch write).
    // Two exceptions record immediately here:
    // - `ShutdownAbandoned` deliberately produces no frames — the attempt
    //   is terminal server-side with nothing to write-gate on;
    // - a preliminary `Resumed` stays un-staged: claim finalization decides
    //   the real terminal and stages it (`registration.rs`).
    match terminal.outcome() {
        observe::SmResumeOutcome::Resumed => {}
        observe::SmResumeOutcome::ShutdownAbandoned => {
            observe::observe_sm_resume_finalized(observe::SmResumeOutcome::ShutdownAbandoned, None);
        }
        outcome => *finalized_resume_outcome = Some(outcome),
    }
    terminal.into_frames()
}

pub(in crate::server::routes::websocket::stream_management) async fn handle_sm_resume_terminal(
    resume: SmResume,
    state: &WebSocketState,
    ctx: SmCtx<'_>,
) -> observe::SmResumeTerminal {
    use observe::{SmResumeOutcome, SmResumeTerminal};
    use waddle_xmpp::stream_management::CrossNodeResumeOutcome;

    let SmCtx {
        phase,
        sm_state,
        sm_ingress_fence,
        authenticated_session,
        occupancy_session,
        carbons_enabled,
        presence_available,
        presence_show,
        presence_status,
        presence_priority,
        presence_payloads,
        pending_subscribes_flushed,
        pending_resume_stream_id,
        pending_resume_h,
        pending_resume_claim,
        #[cfg(test)]
        pre_final_principal_recheck_test_hook,
        suppress_sm_record_next_batch,
        roster_interested,
        blocklist_interested,
        pending_sm_enable_commit: _,
        sm_inbound_completion: _,
    } = ctx;

    // Stream resumption is only legal before this transport has established a
    // fresh SASL/bind lifecycle of its own.
    if !phase.allows_stream_management_resume() {
        return SmResumeTerminal::failed(
            waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
            SmResumeOutcome::UnexpectedRequest,
        );
    }

    // Capture and retire the old transport before claiming its snapshot. The
    // connection-owned handshake retains counters, replay and occupancy and
    // acknowledges only after cleanup. Never re-resolve/retry the live owner:
    // another resume can publish the same SM ID on a replacement meanwhile.
    if let ConnectionPhase::Authenticated { bare_jid } = &*phase {
        use crate::clustering::resume_bridge::{
            request_validated_local_resume, LocalForcedDetachOutcome, LocalResumeRejected,
        };
        use waddle_xmpp::stream_management::SmResumeRejection;
        let shutdown = state.deps.shutdown.stop_token();
        let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new(&resume.previd);
        let outcome = tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                return SmResumeTerminal::failed(
                    waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                    SmResumeOutcome::ShutdownAbandoned,
                );
            }
            outcome = request_validated_local_resume(
                &state.deps.protocol.connection_registry,
                &stream_id,
                bare_jid,
                resume.h,
            ) => outcome,
        };
        match outcome {
            Ok(LocalForcedDetachOutcome::IdentityMismatch) => {
                return SmResumeTerminal::failed(stream_id, SmResumeOutcome::IdentityMismatch);
            }
            Err(LocalResumeRejected {
                reason: SmResumeRejection::NotResumable,
                ..
            }) => {
                return SmResumeTerminal::failed(stream_id, SmResumeOutcome::NotFound);
            }
            Err(LocalResumeRejected {
                jid,
                reason:
                    SmResumeRejection::ReplayGap {
                        inbound_count,
                        replay_gap_through,
                    },
            }) => {
                return SmResumeTerminal::replay_gap(
                    stream_id,
                    inbound_count,
                    jid,
                    resume.h,
                    replay_gap_through,
                );
            }
            Err(LocalResumeRejected {
                reason: SmResumeRejection::HandledTooHigh { outbound_count },
                ..
            }) => {
                *phase = ConnectionPhase::closing(None);
                return SmResumeTerminal::handled_too_high(stream_id, resume.h, outbound_count);
            }
            Ok(LocalForcedDetachOutcome::Detached | LocalForcedDetachOutcome::NotLiveLocally) => {}
        }
        // NotLiveLocally is not proof of cleanup. A snapshot must still be
        // claimable below; prepare_socket_bind separately waits for the exact
        // incumbent to stop before <resumed/>, replay or route publication.
    }

    let mut detached = match state
        .deps
        .protocol
        .sm_session_registry
        .claim_session(&resume.previd)
        .await
    {
        Ok(Some(session)) => session,
        // ADR-0017 Phase 3 Slice 6, element 8: this node has no local
        // record of the session at all — before falling back to today's
        // plain item-not-found, try the cross-node claim-steal resume path.
        // Single-node/non-clustering behavior is byte-identical: with no
        // cluster claim store wired (or no foreign claim on this entity),
        // `attempt_cross_node_resume` itself returns `NotFound` and this
        // collapses to exactly the pre-Slice-6 outcome. Only attempted when
        // this connection already has a SASL-authenticated bare JID to
        // check identity against — a resume attempt with no established
        // identity has nothing for `verify_resume_identity` to compare, so
        // it falls straight through to `item-not-found` exactly as before.
        Ok(None) => {
            // Council-adjudicated FIX 3, corrected by FIX A (deviation 47's
            // rewrite): race ONLY the cancellable, read-only
            // `prepare_cross_node_resume` half (including its whole
            // held-response retry loop) against this node's
            // graceful-shutdown token, so a `<resume/>` held for up to the
            // resume-handshake budget (FIX 1: now itself bounded) can never
            // delay this connection's own graceful-shutdown handling — the
            // outer connection loop's `shutdown_token.cancelled()` arm is
            // otherwise starved for as long as this `.await` runs (see
            // `connection.rs`'s select loop: once an arm's body starts
            // executing, the loop cannot observe any other arm, including
            // shutdown, until that body's own awaits resolve).
            //
            // The write half, `finish_cross_node_steal`, is deliberately
            // called OUTSIDE this `tokio::select!` once `prepare_cross_node_resume`
            // hands back a `ReadyToSteal` ticket — never raced, never
            // wrapped in a timeout that could drop it mid-sequence. Racing
            // the whole attempt (the original FIX 3 shape) could drop the
            // future between `steal_for_resume` committing in Postgres and
            // `hydrate_reclaimed`/`claim_session` completing, stranding a
            // self-owned, un-hydrated claim under a fresh lease the orphan
            // reaper can never steal back — see
            // `waddle_xmpp::stream_management::session_registry`'s
            // `cross_node_resume.rs` module doc, "Cancellation boundary".
            let cross_node = if let ConnectionPhase::Authenticated { bare_jid } = &*phase {
                Some(super::attempt_cross_node_resume_raced(state, &resume.previd, bare_jid).await)
            } else {
                None
            };
            match cross_node {
                Some(CrossNodeAttemptOutcome::Completed(Ok(CrossNodeResumeOutcome::Claimed(
                    session,
                )))) => *session,
                Some(CrossNodeAttemptOutcome::Completed(Ok(
                    CrossNodeResumeOutcome::NotAuthorized,
                ))) => {
                    return SmResumeTerminal::failed_cross_node(
                        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                        SmResumeOutcome::IdentityMismatch,
                    );
                }
                Some(CrossNodeAttemptOutcome::Completed(Ok(
                    CrossNodeResumeOutcome::OwnerUnreachable,
                ))) => {
                    return SmResumeTerminal::failed_cross_node(
                        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                        SmResumeOutcome::OwnerUnreachable,
                    );
                }
                Some(CrossNodeAttemptOutcome::Completed(Ok(
                    CrossNodeResumeOutcome::StorageUnavailable,
                ))) => {
                    return SmResumeTerminal::failed_cross_node(
                        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                        SmResumeOutcome::Storage,
                    );
                }
                Some(CrossNodeAttemptOutcome::Completed(Ok(CrossNodeResumeOutcome::NotFound))) => {
                    return SmResumeTerminal::failed_cross_node(
                        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                        SmResumeOutcome::NotFound,
                    );
                }
                // `None` means the cross-node path was never entered (no
                // authenticated bare JID to verify against) — a LOCAL miss.
                // Marking it cross-node would miscount the clustering
                // dashboard's cross-node rejection panel.
                None => {
                    return SmResumeTerminal::failed(
                        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                        SmResumeOutcome::NotFound,
                    );
                }
                Some(CrossNodeAttemptOutcome::Completed(Err(
                    waddle_xmpp::stream_management::SmRegistryError::StorageUnavailable(_),
                ))) => {
                    return SmResumeTerminal::failed_cross_node(
                        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                        SmResumeOutcome::Storage,
                    );
                }
                Some(CrossNodeAttemptOutcome::Completed(Err(_))) => {
                    return SmResumeTerminal::failed_cross_node(
                        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                        SmResumeOutcome::Internal,
                    );
                }
                Some(CrossNodeAttemptOutcome::ShutdownAbandoned) => {
                    // FIX 3: cancellation won the race before the attempt
                    // completed — abandoned cleanly (no CAS after
                    // cancellation: `tokio::select!` drops the losing future
                    // rather than polling it to completion, so no
                    // `steal_for_resume` this call issued can have committed
                    // after this point). No response is sent for this
                    // `<resume/>` at all: the connection's own select loop
                    // observes the same, now-cancelled `shutdown_token` on
                    // its very next iteration and sends the conformant
                    // `<system-shutdown/>` stream error instead — a more
                    // accurate signal to the client than a resume-specific
                    // failure would be. A client that instead hard
                    // -disconnects mid-hold is observed at the next loop
                    // iteration (the WS read arm); the hold itself is
                    // already bounded by FIX 1's budget regardless.
                    return SmResumeTerminal::failed_cross_node(
                        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                        SmResumeOutcome::ShutdownAbandoned,
                    );
                }
            }
        }
        // Claim-store backend outages and timeouts are a storage incident,
        // not a registry logic error — keep them out of `internal`.
        Err(waddle_xmpp::stream_management::SmRegistryError::StorageUnavailable(_)) => {
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::Storage,
            );
        }
        Err(_) => {
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::Internal,
            );
        }
    };

    let claim_guard = SmResumeClaimGuard::new(
        state.deps.protocol.sm_session_registry.clone(),
        resume.previd.clone(),
    );
    let principal = match state
        .deps
        .protocol
        .sm_session_registry
        .session_principal(&resume.previd)
        .await
    {
        Ok(Some(principal)) => principal,
        Ok(None) => {
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::PrincipalUnavailable,
            );
        }
        Err(_) => {
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::Storage,
            );
        }
    };

    if restore_ingress_checkpoint(state, &mut detached)
        .await
        .is_err()
    {
        claim_guard.release().await;
        return SmResumeTerminal::failed(
            waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
            SmResumeOutcome::Storage,
        );
    }

    let early_resolution = state
        .deps
        .auth_state
        .session_manager
        .resolve_principal(&principal)
        .await;
    match early_resolution {
        crate::auth::session::PrincipalResolution::Resolved(_) => {}
        PrincipalResolution::Missing => {
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::PrincipalUnavailable,
            );
        }
        // A durable row exists but its auth context diverged (context
        // version, auth epoch, or JID): an identity mismatch, not an
        // unavailable principal. Same `not-authorized` wire condition.
        PrincipalResolution::Mismatched => {
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::IdentityMismatch,
            );
        }
        crate::auth::session::PrincipalResolution::StorageError(_) => {
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::Storage,
            );
        }
    }

    if let ConnectionPhase::Authenticated { bare_jid } = phase {
        if detached.jid.to_bare() != *bare_jid {
            claim_guard.release().await;
            return SmResumeTerminal::identity_mismatch(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                detached.jid.clone(),
            );
        }
    }

    // Ordering matters, mirroring the live ack path:
    // `handled_count_exceeds_outbound` is an exact mod-2^32 window
    // from last_acked, which classifies the regressed half-space as
    // "outside the window" too. `can_resume_from` rejects a regressed
    // `h` first, so a stale mod-behind `h` stays a failed resume
    // (`<failed/>`, client starts a fresh session) rather than being
    // reclassified as a handled-count-too-high stream error; an
    // ahead-of-window `h` passes it and hits the too-high error below.
    if !detached.can_resume_from(resume.h) {
        claim_guard.release().await;
        return SmResumeTerminal::replay_gap(
            waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
            detached.inbound_count,
            detached.jid.clone(),
            resume.h,
            detached.replay_gap_through,
        );
    }

    if detached.handled_count_exceeds_outbound(resume.h) {
        claim_guard.release().await;
        *phase = ConnectionPhase::closing(None);
        return SmResumeTerminal::handled_too_high(
            waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
            resume.h,
            detached.outbound_count,
        );
    }

    #[cfg(test)]
    if let Some((reached, release)) = pre_final_principal_recheck_test_hook.take() {
        reached.notify_one();
        release.notified().await;
    }

    // This is the resume linearization point: no SM state has been installed
    // yet, and this is the final await before Ready/publication/replay.
    let resumed_session = match state
        .deps
        .auth_state
        .session_manager
        .resolve_principal(&principal)
        .await
    {
        crate::auth::session::PrincipalResolution::Resolved(session) => session,
        PrincipalResolution::Missing => {
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::PrincipalUnavailable,
            );
        }
        // Mirror of the early resolution: diverged durable principal =
        // identity mismatch (same wire condition).
        PrincipalResolution::Mismatched => {
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::IdentityMismatch,
            );
        }
        crate::auth::session::PrincipalResolution::StorageError(_) => {
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::Storage,
            );
        }
    };

    // The snapshot and its principal are written in one atomic statement, so
    // they cannot diverge today; this gate turns that invariant into an
    // enforced precondition of the commit below rather than an assumption.
    if detached.jid.to_bare() != *principal.bare_jid() {
        claim_guard.release().await;
        return SmResumeTerminal::detached_divergence(
            waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
            detached.jid.clone(),
        );
    }

    // Retain the original window for receipt and pending-row settlement, but
    // commit its ACK to the canonical source first. A failed final resume or
    // cancellation must not restore replay entries whose pending rows vanished.
    let mut staged_sm_state = StreamManagementState::new();
    staged_sm_state.restore_from_session(&detached);
    let restored_inbound = detached.inbound_count;
    detached = match state
        .deps
        .protocol
        .sm_session_registry
        .acknowledge_claimed_resume(&detached, resume.h)
        .await
    {
        Ok(mut acknowledged) => {
            acknowledged.inbound_count = waddle_xmpp::stream_management::sequence::max_in_window(
                acknowledged.inbound_count,
                restored_inbound,
            );
            acknowledged
        }
        Err(error) => {
            warn!(%error, "SM resume ACK could not commit; retaining source and pending rows");
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::Storage,
            );
        }
    };
    if !complete_acknowledged_ingress_custody(state, &staged_sm_state, resume.h).await
        || !complete_acknowledged_ingress_receipts(state, &staged_sm_state, resume.h).await
        || !settle_pending_ack_window(
            state,
            &staged_sm_state,
            staged_sm_state.last_acked,
            resume.h,
        )
        .await
        || state
            .deps
            .protocol
            .sm_session_registry
            .settle_session_acknowledgements(
                &detached,
                state.deps.protocol.pending_delivery_storage.as_ref(),
            )
            .await
            .is_err()
    {
        claim_guard.release().await;
        return SmResumeTerminal::failed(
            waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
            SmResumeOutcome::Storage,
        );
    }
    *sm_ingress_fence = state
        .deps
        .protocol
        .sm_session_registry
        .current_sm_claim_fence(&detached.stream_id);
    sm_state.restore_from_session(&detached);
    sm_state.acknowledge(resume.h);
    // A native-SCRAM login persisted its own resume-fence row at
    // authentication; adopting the resumed lineage supersedes that fresh
    // candidate row, which nothing else will ever delete. Fire-and-forget:
    // the row is bearer-inert either way, this only reclaims storage.
    if let Some(superseded) = authenticated_session.take() {
        if superseded.native_resume_only && superseded.id != resumed_session.id {
            let actor = state.deps.auth_state.session_manager.actor_ref();
            tokio::spawn(async move {
                if actor
                    .ask(DbExecute {
                        sql: format!(
                            "DELETE FROM sessions WHERE id = ? AND token_hash LIKE '{}:%'",
                            crate::auth::session::NATIVE_RESUME_TOKEN_SENTINEL
                        ),
                        params: vec![crate::db::Value::from(superseded.id.clone())],
                    })
                    .await
                    .is_err()
                {
                    warn!(
                        failure = "storage",
                        "failed to delete superseded native-resume session row"
                    );
                }
            });
        }
    }
    *authenticated_session = Some(resumed_session);
    *occupancy_session = detached.occupancy_session;
    *carbons_enabled = detached.carbons_enabled;
    *roster_interested = detached.roster_interested;
    *blocklist_interested = detached.blocklist_interested;
    *presence_available = detached.presence_available;
    *presence_show = detached.presence_show.clone();
    *presence_status = detached.presence_status.clone();
    *presence_priority = detached.presence_priority;
    *presence_payloads = detached.presence_payloads.clone();
    *pending_subscribes_flushed = detached.pending_subscribes_flushed;
    *pending_resume_stream_id = Some(resume.previd.clone());
    *pending_resume_h = Some(resume.h);
    *pending_resume_claim = Some(claim_guard);
    *phase = ConnectionPhase::ready(detached.jid.clone(), true);
    // Responses below include replayed stanzas straight from the restored
    // unacked queue. They already carry their original sequence numbers —
    // the main loop must NOT push them through `record_outbound` again.
    *suppress_sm_record_next_batch = true;

    let replay = super::super::resume_replay::replay_frames(
        sm_state,
        resume.h,
        state.deps.auth_state.xmpp_domain.as_str(),
    );
    SmResumeTerminal::resumed(
        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
        sm_state.inbound_count(),
        detached.jid,
        replay,
    )
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

pub(in crate::server::routes::websocket::stream_management) async fn restore_ingress_checkpoint(
    state: &WebSocketState,
    detached: &mut waddle_xmpp::stream_management::DetachedSession,
) -> Result<(), IngressUowError> {
    let checkpoint = state
        .deps
        .protocol
        .ingress
        .load_resume_checkpoint(&waddle_xmpp::pending_delivery::SmSessionId::new(
            detached.stream_id.clone(),
        ))
        .await?
        .ok_or(IngressUowError::SmIngressStreamMissing)?;
    detached.inbound_count = waddle_xmpp::stream_management::sequence::max_in_window(
        detached.inbound_count,
        checkpoint.to_storage(),
    );
    Ok(())
}
