//! XEP-0397 Instant Stream Resumption (ISR) via a SASL2 (XEP-0388)
//! `<authenticate/>` (ADR-0017 Phase 3 Slice 8).
//!
//! This module owns the **inline** ISR resume flow: a fresh, not-yet
//! -authenticated connection sends `<authenticate xmlns='urn:xmpp:sasl:2'
//! mechanism='PLAIN'>` with a base64 SASL PLAIN `<initial-response>` whose
//! "password" field is the ISR token, plus an inline `<inst-resume
//! with-isr-token='true'>` wrapping a XEP-0198 `<resume/>`
//! ([`waddle_xmpp::protocol::frame::parse_frame`] parses this shape into
//! [`waddle_xmpp::protocol::InboundFrame::IsrResumeAuthenticate`] before
//! this handler ever runs).
//!
//! **Locked consume spec** (ADR-0017 Phase 3 Slice 8, element 10): identity
//! binding (the same [`waddle_xmpp::ownership::resume::verify_resume_identity`]
//! check as ordinary resume, element 8) gates the token compare — a
//! mismatch here never reaches the token store and never destroys
//! anything. Only once identity matches does
//! [`waddle_xmpp::isr::IsrTokenStore::consume`] run its own epoch-fenced,
//! constant-time compare/rotate/destroy transaction.
//!
//! **Two XEP-0397 failure paths, both produced here**:
//! - Failed-token auth on a valid SM-ID → bare SASL2 `<failure/>` **and**
//!   the claimed session state is destroyed
//!   ([`InMemorySmSessionRegistry::complete_claim`]) — the XEP's
//!   anti-brute-force MUST.
//! - Authenticated (token matched) but the underlying XEP-0198 resume is
//!   impossible (handled-count too high, or the replay window no longer
//!   covers it) → `<success/>` wrapping `<inst-resume-failed/>` wrapping a
//!   XEP-0198 `<failed/>`; the claim is released (not destroyed) and the
//!   client MAY continue with normal session establishment, exactly as the
//!   XEP allows.
//!
//! **Deviations from a full XEP-0397/XEP-0388 implementation** (see the
//! phase plan's Slice 8 section for the full rationale — this module
//! implements the plan's deliberately narrowed scope, not an oversight):
//! - Only the `PLAIN`-encoded, `with-isr-token='true'` shape is supported.
//!   General SASL2 authentication (no inline ISR request) is not
//!   implemented by this codebase at all — [`waddle_xmpp::protocol::frame`]
//!   rejects any other `<authenticate/>` shape before this handler is ever
//!   reached.
//!
//! **Council-adjudicated FIX 2 (cross-node ISR resume is now wired)**: a
//! local-claim miss (`claim_session` returns `Ok(None)`) no longer falls
//! straight through to the failed-token failure path. It instead attempts
//! the SAME cancellation-safe cross-node claim-steal machinery
//! [`super::stream_management::handle_sm_resume`] uses (Slice 6), via
//! [`super::stream_management::attempt_cross_node_resume_raced`] — reused,
//! not reinvented. `claimed_identity` (the SASL-authenticated bare JID from
//! this `<authenticate/>`'s PLAIN credentials) stands in for the
//! ordinary-resume path's already-bound `bare_jid`, checked against the
//! cross-node snapshot's owner by `prepare_cross_node_resume` itself before
//! any write (element 8's "identity check before any write" rule) — this
//! module's own [`verify_resume_identity`] check further down still runs
//! afterward too, exactly as it does for a local-claim hit, so both resume
//! paths get the identical double-check. A resume attempt for an SM-ID with
//! neither a local nor a cross-node claim/snapshot still collapses to the
//! same `not-authorized` failure as a wrong token — deliberately, to avoid
//! leaking which SM-IDs exist.

use super::stream_management::SmCtx;
use super::telemetry_privacy::sasl_mechanism_category;
use super::transport_xml::element_to_xml;
use super::*;
use waddle_xmpp::auth::{SaslInitialResponse, SaslMechanism};
use waddle_xmpp::isr::{
    inst_resume_failed_element, inst_resumed_element, IsrConsumeOutcome, ISR_PINNED_MECHANISM,
};
use waddle_xmpp::ownership::{resume::verify_resume_identity, Entity, EntityType};
use waddle_xmpp::stream_management::{stamp_replay_delay, SmFailed, SmResumed};

/// Build a bare SASL2 `<failure/>` carrying a single standard SASL
/// condition child (`urn:ietf:params:xml:ns:xmpp-sasl`, per XEP-0388's own
/// `<failure/>` example — the condition child itself stays in the SASL1
/// condition namespace even though the envelope is SASL2).
fn sasl2_failure(condition: &str) -> String {
    element_to_xml(
        Element::builder("failure", waddle_xmpp::ns::SASL2)
            .append(Element::builder(condition, waddle_xmpp::ns::SASL).build())
            .build(),
    )
}

/// Build a SASL2 `<success/>` wrapping `authorization-identifier` (XEP-0388
/// §"Success": the negotiated identity) plus one ISR result child
/// (`<inst-resumed/>` or `<inst-resume-failed/>`).
fn sasl2_success(bare_jid: &BareJid, isr_child: Element) -> String {
    element_to_xml(
        Element::builder("success", waddle_xmpp::ns::SASL2)
            .append(
                Element::builder("authorization-identifier", waddle_xmpp::ns::SASL2)
                    .append(bare_jid.to_string())
                    .build(),
            )
            .append(isr_child)
            .build(),
    )
}

pub(super) async fn handle_isr_resume_authenticate(
    mechanism: SaslMechanism,
    initial_response: SaslInitialResponse,
    resume: SmResume,
    state: &WebSocketState,
    ctx: SmCtx<'_>,
) -> Vec<String> {
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
        suppress_sm_record_next_batch,
        roster_interested,
        blocklist_interested,
    } = ctx;

    // Q8's gate: ISR is only wired when clustering.enabled && Postgres —
    // `isr_token_store()`/`claim_pair()` are `None` under the exact same
    // conditions the stream feature is unadvertised, so a client that
    // somehow pipelines this without ever seeing the advertisement (a
    // stale token from before a topology change, a misbehaving client)
    // gets a conformant failure rather than a panic on a missing handle.
    let (Some(isr_token_store), Some((claim_store, node_identity))) = (
        state.deps.app_state.clustering_claims.isr_token_store(),
        state.deps.app_state.clustering_claims.claim_pair(),
    ) else {
        return vec![sasl2_failure("invalid-mechanism")];
    };

    if mechanism != SaslMechanism::Plain {
        warn!(
            mechanism = sasl_mechanism_category(&mechanism),
            "ISR resume rejected: unsupported pinned mechanism"
        );
        return vec![sasl2_failure("invalid-mechanism")];
    }

    let credentials = match waddle_xmpp::auth::parse_plain(initial_response.as_bytes()) {
        Ok(credentials) => credentials,
        Err(_) => {
            warn!(
                category = "plain-initial-response-malformed",
                "ISR resume: failed to parse PLAIN initial-response"
            );
            return vec![sasl2_failure("not-authorized")];
        }
    };
    let claimed_identity = credentials.authcid;
    let presented_token = credentials.password;

    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .claim_session(&resume.previd)
        .await
    {
        Ok(Some(session)) => session,
        // Council-adjudicated FIX 2: no LOCAL record of this SM-ID — before
        // falling back to the failed-token outcome, try the same
        // cancellation-safe cross-node claim-steal machinery
        // `handle_sm_resume` uses (Slice 6). `claimed_identity` is already
        // the SASL-authenticated bare JID from this `<authenticate/>`'s
        // PLAIN credentials; `prepare_cross_node_resume` checks it against
        // the cross-node snapshot's owner before any write, exactly like
        // the ordinary resume path's own identity gate (element 8).
        Ok(None) => match super::stream_management::attempt_cross_node_resume_raced(
            state,
            &resume.previd,
            &claimed_identity,
        )
        .await
        {
            super::stream_management::CrossNodeAttemptOutcome::Completed(Ok(
                waddle_xmpp::stream_management::CrossNodeResumeOutcome::Claimed(session),
            )) => *session,
            super::stream_management::CrossNodeAttemptOutcome::Completed(Ok(
                waddle_xmpp::stream_management::CrossNodeResumeOutcome::NotAuthorized,
            )) => {
                warn!(
                    category = "cross-node-identity-mismatch",
                    "ISR resume rejected: cross-node identity mismatch"
                );
                return vec![sasl2_failure("not-authorized")];
            }
            super::stream_management::CrossNodeAttemptOutcome::Completed(Ok(
                waddle_xmpp::stream_management::CrossNodeResumeOutcome::OwnerUnreachable,
            )) => {
                warn!(
                    category = "cross-node-owner-unreachable",
                    "ISR resume failed: cross-node owner unreachable within the \
                     resume-handshake window"
                );
                return vec![sasl2_failure("temporary-auth-failure")];
            }
            super::stream_management::CrossNodeAttemptOutcome::Completed(Ok(
                waddle_xmpp::stream_management::CrossNodeResumeOutcome::NotFound,
            )) => {
                // Neither a local nor a cross-node record of this SM-ID.
                // Collapses to the same observable outcome as a wrong
                // token — the conservative choice from a
                // not-leaking-which-SM-IDs-exist standpoint.
                info!(
                    category = "session-not-found",
                    "ISR resume rejected: no local or cross-node session record"
                );
                return vec![sasl2_failure("not-authorized")];
            }
            super::stream_management::CrossNodeAttemptOutcome::Completed(Err(_)) => {
                warn!(
                    category = "cross-node-registry-error",
                    "ISR resume failed: cross-node registry error"
                );
                return vec![sasl2_failure("temporary-auth-failure")];
            }
            super::stream_management::CrossNodeAttemptOutcome::ShutdownAbandoned => {
                // Mirrors `handle_sm_resume`'s own shutdown-abandoned
                // handling: no write was ever issued (`tokio::select!`
                // drops the losing future rather than polling it), so no
                // response is sent — the connection's own select loop
                // observes the same cancelled shutdown token on its next
                // iteration and sends the conformant stream-error instead.
                info!(
                    category = "shutdown-abandoned",
                    "ISR resume abandoned: graceful shutdown in progress"
                );
                return vec![];
            }
        },
        Err(_) => {
            warn!(
                category = "registry-error",
                "ISR resume failed: registry error"
            );
            return vec![sasl2_failure("temporary-auth-failure")];
        }
    };

    // Identity binding (element 8's "identity check before any write"
    // rule, major fix 7): a mismatch never reaches the token compare and
    // never destroys anything — the claim goes right back to the
    // resumable pool, exactly like `handle_sm_resume`'s own identity check.
    if verify_resume_identity(&claimed_identity, &detached.jid.to_bare()).is_none() {
        warn!(
            category = "identity-mismatch",
            "ISR resume rejected: authenticated identity does not match the SM session owner"
        );
        if state
            .deps
            .protocol
            .sm_session_registry
            .release_claim(&resume.previd)
            .await
            .is_err()
        {
            warn!(
                category = "claim-release-error",
                "Failed to release rejected ISR resume claim"
            );
        }
        return vec![sasl2_failure("not-authorized")];
    }

    // `claim_session` above already established this node's Postgres claim
    // on the SM-session entity (via its own internal `ensure_claimed`);
    // this call is the same idempotent self-reacquire side channel
    // `sm_persistence_fenced::claim_epoch_for` uses to learn the epoch
    // value it just granted, not a second, independent acquire attempt.
    let entity = Entity::new(EntityType::SmSession, resume.previd.clone());
    let epoch = match claim_store
        .ensure_claimed(&entity, &node_identity.current())
        .await
    {
        Ok(epoch) => epoch,
        Err(_) => {
            warn!(
                category = "claim-epoch-error",
                "ISR resume failed: could not confirm claim epoch"
            );
            if state
                .deps
                .protocol
                .sm_session_registry
                .release_claim(&resume.previd)
                .await
                .is_err()
            {
                warn!(
                    category = "claim-release-error",
                    "Failed to release ISR resume claim after epoch failure"
                );
            }
            return vec![sasl2_failure("temporary-auth-failure")];
        }
    };

    let rotated_token = match isr_token_store
        .consume(
            &resume.previd,
            presented_token.as_bytes(),
            ISR_PINNED_MECHANISM,
            &node_identity.current(),
            epoch,
        )
        .await
    {
        Ok(IsrConsumeOutcome::Matched { rotated }) => rotated.token,
        Ok(IsrConsumeOutcome::Mismatched) => {
            // A token row genuinely EXISTED for this SM-ID and the
            // presented token did not match it — XEP-0397's anti-brute
            // -force MUST: destroy the session state the SM-ID identified.
            // `complete_claim`, not `release_claim` — this claim ends
            // here, it does not go back into the resumable pool.
            warn!(
                category = "token-mismatch",
                "ISR resume rejected: token mismatch; destroying session state"
            );
            if state
                .deps
                .protocol
                .sm_session_registry
                .complete_claim(&resume.previd)
                .await
                .is_err()
            {
                warn!(
                    category = "claim-completion-error",
                    "Failed to destroy session state after ISR token mismatch"
                );
            }
            return vec![sasl2_failure("not-authorized")];
        }
        Ok(IsrConsumeOutcome::NoSuchToken) => {
            // Council-adjudicated FIX 3: no token row existed for this
            // SM-ID at all — this session never opted into ISR (no
            // `<isr-enable/>` ever ran for it), or a previous attempt
            // already consumed/destroyed it. Narrowing the destroy blast
            // radius to genuine wrong-token attempts (the `Mismatched` arm
            // above): fail WITHOUT destroying anything, releasing the
            // claim back to the resumable pool exactly like the identity
            // -mismatch rejection above — a resumable-but-never
            // -ISR-enabled session must not be destroyed just because
            // someone attempted ISR auth against it.
            warn!(
                category = "token-not-found",
                "ISR resume rejected: no ISR token exists for this SM-ID"
            );
            if state
                .deps
                .protocol
                .sm_session_registry
                .release_claim(&resume.previd)
                .await
                .is_err()
            {
                warn!(
                    category = "claim-release-error",
                    "Failed to release ISR resume claim after no-such-token outcome"
                );
            }
            return vec![sasl2_failure("not-authorized")];
        }
        Err(_) => {
            warn!(
                category = "token-store-error",
                "ISR resume failed: token store error"
            );
            if state
                .deps
                .protocol
                .sm_session_registry
                .release_claim(&resume.previd)
                .await
                .is_err()
            {
                warn!(
                    category = "claim-release-error",
                    "Failed to release ISR resume claim after store error"
                );
            }
            return vec![sasl2_failure("temporary-auth-failure")];
        }
    };

    // Authentication succeeded (the token matched). From here on, any
    // failure is "authenticated but resume impossible" — `<success/>`
    // wrapping `<inst-resume-failed/>`, never a bare `<failure/>` — per
    // element 10's second failure path.
    let bare_jid = detached.jid.to_bare();

    // Order mirrors `handle_sm_resume` (#1099): the replay-window check runs
    // FIRST, so a stale mod-behind `h` is classified as a truncated replay
    // window (recoverable) rather than falsely tripping the too-high branch;
    // and the too-high test uses the exact mod-2^32 `handled_count_exceeds_outbound`
    // rather than the naive `h > outbound_count`, which had a half-window
    // blind spot at `h == outbound + 2^31`.
    if !detached.can_resume_from(resume.h) {
        warn!(
            category = "replay-window-truncated",
            replay_gap_through = ?detached.replay_gap_through,
            "ISR resume: resumption impossible (replay window no longer covers client h)"
        );
        if state
            .deps
            .protocol
            .sm_session_registry
            .release_claim(&resume.previd)
            .await
            .is_err()
        {
            warn!(
                category = "claim-release-error",
                "Failed to release ISR resume claim after replay-window truncation"
            );
        }
        let failed = SmFailed::resume_failed("resource-constraint", detached.inbound_count);
        return vec![sasl2_success(
            &bare_jid,
            inst_resume_failed_element(&failed),
        )];
    }

    if detached.handled_count_exceeds_outbound(resume.h) {
        warn!(
            category = "handled-count-too-high",
            client_h = resume.h,
            send_count = detached.outbound_count,
            "ISR resume: resumption impossible (handled count exceeds server's outbound count)"
        );
        if state
            .deps
            .protocol
            .sm_session_registry
            .release_claim(&resume.previd)
            .await
            .is_err()
        {
            warn!(
                category = "claim-release-error",
                "Failed to release ISR resume claim after handled-count mismatch"
            );
        }
        let failed = SmFailed::resume_failed("unexpected-request", detached.inbound_count);
        return vec![sasl2_success(
            &bare_jid,
            inst_resume_failed_element(&failed),
        )];
    }

    // Successful instant stream resumption — restore SM state exactly as
    // `handle_sm_resume`'s own tail does.
    sm_state.restore_from_session(&detached);
    sm_state.acknowledge(resume.h);

    let restored_session = state
        .deps
        .protocol
        .resumable_sessions
        .get(&resume.previd)
        .map(|s| s.clone());

    *authenticated_session = restored_session;
    *carbons_enabled = detached.carbons_enabled;
    *roster_interested = detached.roster_interested;
    *blocklist_interested = detached.blocklist_interested;
    *presence_available = detached.presence_available;
    *presence_show = detached.presence_show.clone();
    *presence_status = detached.presence_status.clone();
    *presence_priority = detached.presence_priority;
    // #1103/#1104: restore the stored extension payloads (XEP-0115 caps,
    // XEP-0319 idle, …) and the once-per-session pending-subscribe claim
    // exactly like `handle_sm_resume`'s tail — an ISR resume is the SAME
    // session, so it must not lose extension payloads or re-prompt a
    // subscribe the detached session already answered.
    *presence_payloads = detached.presence_payloads.clone();
    *pending_subscribes_flushed = detached.pending_subscribes_flushed;
    *pending_resume_stream_id = Some(resume.previd.clone());
    *pending_resume_h = Some(resume.h);
    *phase = ConnectionPhase::ready(detached.jid.clone(), true);
    *suppress_sm_record_next_batch = true;

    let server_domain = state.deps.auth_state.xmpp_domain.as_str();
    let replay: Vec<String> = sm_state
        .get_stanzas_to_resend(resume.h)
        .into_iter()
        .map(|entry| {
            stamp_replay_delay(&entry.stanza_xml, server_domain, entry.original_receipt_at)
        })
        .collect();

    info!(category = "success", replay = replay.len(), "ISR resumed");

    let resumed = SmResumed::new(resume.previd.clone(), sm_state.get_inbound_count());
    let mut responses = Vec::with_capacity(replay.len() + 1);
    responses.push(sasl2_success(
        &bare_jid,
        inst_resumed_element(&rotated_token, &resumed),
    ));
    responses.extend(replay);
    responses
}
