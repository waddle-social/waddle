use super::*;
use super::{
    frame::{ResponseFrame, StreamErrorFrame},
    transport_xml::websocket_stream_close_element,
};
mod observe;
use observe::{apply_sm_ack_observation, observe_sm_ack, SmAckObservation};

/// Operation-owned resume claim.  Dropping an uncommitted guard returns the
/// frozen snapshot to the registry without creating server-local claim state.
pub(super) struct SmResumeClaimGuard {
    registry: std::sync::Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
    stream_id: String,
    armed: bool,
}

impl SmResumeClaimGuard {
    fn new(
        registry: std::sync::Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
        stream_id: String,
    ) -> Self {
        Self {
            registry,
            stream_id,
            armed: true,
        }
    }

    pub(super) fn commit(mut self) {
        self.armed = false;
    }

    async fn release(mut self) {
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

struct SmEnableClaimGuard {
    registry: std::sync::Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
    stream_id: waddle_xmpp::pending_delivery::SmSessionId,
    _claim_publication: waddle_xmpp::ownership::CurrentNodeIdentityGuard,
    armed: bool,
}

/// Typed post-write effect for `<enable/>`. The resumable claim is acquired
/// before the response is built, but neither local SM state nor connection-
/// registry publication occurs until the transport confirms that the
/// `<enabled/>` frame was written.
pub(super) struct SmEnableCommit {
    claim_guard: Option<SmEnableClaimGuard>,
    stream_id: waddle_xmpp::pending_delivery::SmSessionId,
    resume: bool,
    max: u32,
}

fn try_enqueue_shadow_stream_enrollment(
    ingress_shadow: &crate::ingress_shadow::IngressShadowHandle,
    stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
) {
    try_enqueue_shadow_stream_enrollment_with(stream_id, |stream_id| {
        ingress_shadow.ensure_stream_enrollment(stream_id)
    });
}

fn try_enqueue_shadow_stream_enrollment_with(
    stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    mut ensure_enrollment: impl FnMut(
        waddle_xmpp::pending_delivery::SmSessionId,
    ) -> crate::ingress_shadow::IngressShadowDisposition,
) {
    let _ = ensure_enrollment(stream_id.clone());
}

impl SmEnableCommit {
    fn new(
        claim_guard: Option<SmEnableClaimGuard>,
        stream_id: waddle_xmpp::pending_delivery::SmSessionId,
        resume: bool,
        max: u32,
    ) -> Self {
        Self {
            claim_guard,
            stream_id,
            resume,
            max,
        }
    }

    pub(super) fn publish(
        mut self,
        state: &WebSocketState,
        sm_state: &mut StreamManagementState,
        bound_jid: Option<&jid::FullJid>,
        registry_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
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
        sm_state.enable(self.stream_id.to_string(), self.resume, Some(self.max));
        if let Some(guard) = self.claim_guard.take() {
            guard.commit();
        }
        try_enqueue_shadow_stream_enrollment(&state.deps.protocol.ingress_shadow, &self.stream_id);
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
    fn new(
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

    fn commit(mut self) {
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

pub(super) fn defer_superseded_sm_claim(state: &WebSocketState, sm_state: &StreamManagementState) {
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

#[cfg(test)]
mod enable_claim_guard_tests {
    use super::SmEnableClaimGuard;
    use std::sync::Arc;
    use waddle_xmpp::ownership::{ClaimStore as _, Entity, EntityType};
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

    #[tokio::test]
    async fn dropped_prepublication_guard_defers_and_releases_the_exact_claim() {
        let store = Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
        let identity = waddle_xmpp::ownership::NodeIdentity::new("sm-node", "incarnation");
        let shared_identity = waddle_xmpp::ownership::SharedNodeIdentity::new(identity);
        let registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_claim_store(store.clone(), shared_identity.clone()),
        );
        let stream_id = "cancelled-before-enabled-publication";
        let claim_publication = registry
            .ensure_session_claim(stream_id)
            .await
            .expect("claim admission");

        let guard = SmEnableClaimGuard::new(
            registry.clone(),
            waddle_xmpp::pending_delivery::SmSessionId::new(stream_id),
            claim_publication,
        );
        drop(guard);

        assert_eq!(registry.pending_claim_release_count(), 1);
        assert_eq!(registry.retry_pending_claim_releases(1).await, 1);
        assert_eq!(registry.pending_claim_release_count(), 0);
        assert!(store
            .current_claim(&Entity::new(EntityType::SmSession, stream_id))
            .await
            .expect("claim lookup")
            .is_none());
    }

    #[tokio::test]
    async fn prepublication_guard_blocks_self_fence_demotion_until_commit() {
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let stream_id = "enable-publication-vs-self-fence";
        let claim_publication = registry
            .ensure_session_claim(stream_id)
            .await
            .expect("claim admission");
        let guard = SmEnableClaimGuard::new(
            registry.clone(),
            waddle_xmpp::pending_delivery::SmSessionId::new(stream_id),
            claim_publication,
        );
        let demotion = tokio::spawn({
            let registry = registry.clone();
            async move { registry.forget_claim_locally(stream_id).await }
        });
        tokio::task::yield_now().await;
        assert!(
            !demotion.is_finished(),
            "pending transport publication must block self-fence demotion"
        );

        guard.commit();
        demotion.await.expect("self-fence demotion");
        assert!(registry
            .locally_owned_claim_ids()
            .expect("owned inventory")
            .is_empty());
    }
}

#[cfg(test)]
mod shadow_enrollment_tests {
    use super::try_enqueue_shadow_stream_enrollment_with;
    use crate::ingress_shadow::IngressShadowDisposition;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn every_fresh_sm_enable_attempts_shadow_enrollment_once() {
        let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new("sm-enable-stream");
        let attempts = Arc::new(AtomicUsize::new(0));

        try_enqueue_shadow_stream_enrollment_with(&stream_id, {
            let attempts = attempts.clone();
            move |_stream_id| {
                attempts.fetch_add(1, Ordering::SeqCst);
                IngressShadowDisposition::Enqueued
            }
        });

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shadow_enrollment_result_never_changes_enable_commit() {
        let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new("shadow-result-stream");
        for disposition in [
            IngressShadowDisposition::Disabled,
            IngressShadowDisposition::QueueFull,
            IngressShadowDisposition::Closed,
            IngressShadowDisposition::Enqueued,
        ] {
            let attempts = Arc::new(AtomicUsize::new(0));
            try_enqueue_shadow_stream_enrollment_with(&stream_id, {
                let attempts = attempts.clone();
                move |_stream_id| {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    disposition
                }
            });
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
        }
    }
}

mod registration;

pub(super) use observe::observe_sm_resume_finalized;
pub(super) use registration::{
    finalize_sm_after_registry_registration, SmRegistrationFinalization,
};

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
pub(super) fn is_countable_stanza(frame: &str) -> bool {
    let Ok(element) = Element::from_str(frame.trim_start()) else {
        return false;
    };
    matches!(element.name(), "iq" | "message" | "presence")
}

pub(super) fn sm_show_from_name(value: &str) -> Option<xmpp_parsers::presence::Show> {
    match value {
        "away" => Some(xmpp_parsers::presence::Show::Away),
        "chat" => Some(xmpp_parsers::presence::Show::Chat),
        "dnd" => Some(xmpp_parsers::presence::Show::Dnd),
        "xa" => Some(xmpp_parsers::presence::Show::Xa),
        _ => None,
    }
}

pub(super) fn sm_show_name(show: &xmpp_parsers::presence::Show) -> &'static str {
    match show {
        xmpp_parsers::presence::Show::Away => "away",
        xmpp_parsers::presence::Show::Chat => "chat",
        xmpp_parsers::presence::Show::Dnd => "dnd",
        xmpp_parsers::presence::Show::Xa => "xa",
    }
}

fn max_resume_secs_from_env() -> u32 {
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
pub(super) struct SmCtx<'a> {
    pub(super) phase: &'a mut ConnectionPhase,
    pub(super) sm_state: &'a mut StreamManagementState,
    pub(super) authenticated_session: &'a mut Option<Session>,
    pub(super) carbons_enabled: &'a mut bool,
    pub(super) presence_available: &'a mut bool,
    pub(super) presence_show: &'a mut Option<xmpp_parsers::presence::Show>,
    pub(super) presence_status: &'a mut Option<String>,
    pub(super) presence_priority: &'a mut i8,
    pub(super) presence_payloads: &'a mut Vec<minidom::Element>,
    pub(super) pending_subscribes_flushed: &'a mut bool,
    pub(super) pending_resume_stream_id: &'a mut Option<String>,
    pub(super) pending_resume_h: &'a mut Option<u32>,
    pub(super) pending_resume_claim: &'a mut Option<SmResumeClaimGuard>,
    #[cfg(test)]
    pub(super) pre_final_principal_recheck_test_hook:
        &'a mut Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    /// Set by `handle_sm_resume` so the main loop skips SM recording for
    /// the responses it returns — those are replay stanzas already tracked
    /// in the unacked queue.
    pub(super) suppress_sm_record_next_batch: &'a mut bool,
    pub(super) roster_interested: &'a mut bool,
    pub(super) blocklist_interested: &'a mut bool,
    pub(super) pending_sm_enable_commit: &'a mut Option<SmEnableCommit>,
}

/// Dispatch an XEP-0198 control nonza. Isolated helper so the main frame
/// dispatcher stays flat.
pub(super) async fn handle_sm_stanza(
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
        SmStanza::Request => vec![ResponseFrame::from_serialized_xml(
            SmAck::new(ctx.sm_state.get_inbound_count()).to_xml(),
        )],
        SmStanza::Ack(ack) => apply_sm_ack(state, ctx.sm_state, ctx.phase, ack.h).await,
        SmStanza::Resume(resume) => {
            handle_sm_resume(resume, state, ctx, finalized_resume_outcome).await
        }
        // Server-origin nonzas should never arrive from a client. Ignore.
        SmStanza::Enabled(_) | SmStanza::Resumed(_) | SmStanza::Failed(_) => vec![],
    }
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
/// [`super::batch_write`] (issue #1089) so both paths honour the same
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
pub(super) async fn apply_sm_ack(
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    phase: &mut ConnectionPhase,
    h: u32,
) -> Vec<ResponseFrame> {
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
        SmAckObservation::Duplicate { .. } => vec![],
        SmAckObservation::Advanced {
            acked_from_exclusive,
            ..
        } => {
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
                            "pending_delivery delete_acked_in_window failed; rows \
                             will be retried on next session via release_claim"
                        );
                    }
                }
            }
            vec![]
        }
    }
}

async fn handle_sm_enable(
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
    let claim_guard = claim_publication.map(|publication| {
        SmEnableClaimGuard::new(
            state.deps.protocol.sm_session_registry.clone(),
            waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.clone()),
            publication,
        )
    });
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
pub(super) enum CrossNodeAttemptOutcome {
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
enum PrepareRaceOutcome {
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
pub(super) async fn attempt_cross_node_resume_raced(
    state: &WebSocketState,
    stream_id: &str,
    bare_jid: &BareJid,
) -> CrossNodeAttemptOutcome {
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

async fn handle_sm_resume(
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

async fn handle_sm_resume_terminal(
    resume: SmResume,
    state: &WebSocketState,
    ctx: SmCtx<'_>,
) -> observe::SmResumeTerminal {
    use observe::{SmResumeOutcome, SmResumeTerminal};
    use waddle_xmpp::stream_management::{stamp_replay_delay, CrossNodeResumeOutcome};

    let SmCtx {
        phase,
        sm_state,
        authenticated_session,
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
    } = ctx;

    // Stream resumption is only legal before this transport has established a
    // fresh SASL/bind lifecycle of its own.
    if !phase.allows_stream_management_resume() {
        return SmResumeTerminal::failed(
            waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
            SmResumeOutcome::UnexpectedRequest,
        );
    }

    let detached = match state
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
                Some(attempt_cross_node_resume_raced(state, &resume.previd, bare_jid).await)
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
                Some(CrossNodeAttemptOutcome::Completed(Ok(CrossNodeResumeOutcome::NotFound)))
                | None => {
                    return SmResumeTerminal::failed_cross_node(
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

    let early_resolution = state
        .deps
        .auth_state
        .session_manager
        .resolve_principal(&principal)
        .await;
    match early_resolution {
        crate::auth::session::PrincipalResolution::Resolved(_) => {}
        crate::auth::session::PrincipalResolution::Missing => {
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::PrincipalUnavailable,
            );
        }
        // A durable row exists but its auth context diverged (context
        // version, auth epoch, or JID): an identity mismatch, not an
        // unavailable principal. Same `not-authorized` wire condition.
        crate::auth::session::PrincipalResolution::Mismatched => {
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
        crate::auth::session::PrincipalResolution::Missing => {
            claim_guard.release().await;
            return SmResumeTerminal::failed(
                waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
                SmResumeOutcome::PrincipalUnavailable,
            );
        }
        // Mirror of the early resolution: diverged durable principal =
        // identity mismatch (same wire condition).
        crate::auth::session::PrincipalResolution::Mismatched => {
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

    // Commit the staged snapshot only after the durable recheck succeeds.
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
                    .ask(crate::db::actor::DbExecute {
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

    // Issue #1178: stamp each replayed stanza with a XEP-0203 <delay/>
    // carrying its original receipt time, so clients sort it at its true
    // timeline position instead of the drain time (XEP-0198 Acks-section
    // redelivery stamping, applied to the <resumed/> replay by analogy).
    let server_domain = state.deps.auth_state.xmpp_domain.as_str();
    let replay: Vec<ResponseFrame> = sm_state
        .get_stanzas_to_resend(resume.h)
        .into_iter()
        .map(|entry| {
            ResponseFrame::from_serialized_xml(stamp_replay_delay(
                &entry.stanza_xml,
                server_domain,
                entry.original_receipt_at,
            ))
        })
        .collect();
    SmResumeTerminal::resumed(
        waddle_xmpp::pending_delivery::SmSessionId::new(resume.previd),
        sm_state.get_inbound_count(),
        detached.jid,
        replay,
    )
}
