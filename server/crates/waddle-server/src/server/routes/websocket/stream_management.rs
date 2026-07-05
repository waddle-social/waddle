use super::transport_xml::{build_handled_count_too_high_stream_error, websocket_stream_close_xml};
use super::*;

mod registration;

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
    pub(super) pending_resume_stream_id: &'a mut Option<String>,
    pub(super) pending_resume_h: &'a mut Option<u32>,
    /// Set by `handle_sm_resume` so the main loop skips SM recording for
    /// the responses it returns — those are replay stanzas already tracked
    /// in the unacked queue.
    pub(super) suppress_sm_record_next_batch: &'a mut bool,
    pub(super) roster_interested: &'a mut bool,
    pub(super) blocklist_interested: &'a mut bool,
}

/// Dispatch an XEP-0198 control nonza. Isolated helper so the main frame
/// dispatcher stays flat.
pub(super) async fn handle_sm_stanza(
    sm: SmStanza,
    state: &WebSocketState,
    ctx: SmCtx<'_>,
) -> Vec<String> {
    use waddle_xmpp::stream_management::SmAck;

    match sm {
        SmStanza::Enable(enable) => handle_sm_enable(enable, state, ctx.sm_state, ctx.phase),
        SmStanza::Request => vec![SmAck::new(ctx.sm_state.get_inbound_count()).to_xml()],
        SmStanza::Ack(ack) => {
            apply_sm_ack(state, ctx.sm_state, ack.h).await;
            vec![]
        }
        SmStanza::Resume(resume) => handle_sm_resume(resume, state, ctx).await,
        // Server-origin nonzas should never arrive from a client. Ignore.
        SmStanza::Enabled(_) | SmStanza::Resumed(_) | SmStanza::Failed(_) => vec![],
    }
}

/// Apply a client `<a h='N'/>` ack: advance the SM counters, drop the
/// acked prefix of the unacked queue, and range-delete every
/// `pending_delivery` row this XEP-0198 session claimed whose
/// recorded outbound counter is <= `h`.
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
/// Spawning would let a quick ack arrive and run delete_acked_through
/// against a row whose outbound_sequence is still NULL (because the
/// record_pushed_at task hadn't completed), silently skipping the
/// delete.
///
/// Shared by the `<a/>` frame handler and the mid-batch drain in
/// [`super::batch_write`] (issue #1089) so both paths honour the same
/// ack lifecycle.
pub(super) async fn apply_sm_ack(
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    h: u32,
) {
    sm_state.acknowledge(h);
    if let Some(stream_id) = sm_state.stream_id.clone() {
        let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(stream_id);
        match state
            .deps
            .protocol
            .pending_delivery_storage
            .delete_acked_through(&session_id, h)
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
                    error = %error,
                    "pending_delivery delete_acked_through failed; rows \
                     will be retried on next session via release_claim"
                );
            }
        }
    }
}

fn handle_sm_enable(
    enable: SmEnable,
    state: &WebSocketState,
    sm_state: &mut StreamManagementState,
    phase: &ConnectionPhase,
) -> Vec<String> {
    use waddle_xmpp::stream_management::{SmEnabled, SmFailed};

    if !phase.allows_stream_management_enable() {
        return vec![SmFailed::with_condition("unexpected-request").to_xml()];
    }
    if sm_state.enabled {
        return vec![SmFailed::with_condition("unexpected-request").to_xml()];
    }

    let stream_id = uuid::Uuid::new_v4().to_string();
    let max_resume_secs = max_resume_secs_from_env();
    let max = match enable.max {
        Some(m) if m > max_resume_secs => {
            waddle_xmpp::prometheus::increment_sm_resume_window_clamped();
            max_resume_secs
        }
        Some(m) => m,
        None => max_resume_secs,
    };
    sm_state.enable(stream_id.clone(), enable.resume, Some(max));

    // Publish the stream id onto the registry's ConnectionEntry so
    // the offline-flush path can claim `pending_delivery` rows under
    // a session id that's unique to this XEP-0198 session (not just
    // the resource JID — distinct SM sessions on the same resource
    // would otherwise share the same key, causing cross-session row
    // deletion). Locked Q7b SM-ack lifecycle (issue #209).
    if let Some(jid) = phase.bound_jid() {
        if let Some(entry) = state.deps.protocol.connection_registry.get_entry(jid) {
            entry.set_sm_stream_id(Some(waddle_xmpp::pending_delivery::SmSessionId::new(
                stream_id.clone(),
            )));
        }
    }

    info!(stream_id = %stream_id, resume = enable.resume, max = max, "SM enabled");
    if enable.resume {
        vec![SmEnabled::with_resume(stream_id, max).to_xml()]
    } else {
        vec![SmEnabled::new(stream_id).to_xml()]
    }
}

async fn handle_sm_resume(resume: SmResume, state: &WebSocketState, ctx: SmCtx<'_>) -> Vec<String> {
    use waddle_xmpp::stream_management::{stamp_replay_delay, SmFailed, SmResumed};

    let SmCtx {
        phase,
        sm_state,
        authenticated_session,
        carbons_enabled,
        presence_available,
        presence_show,
        presence_status,
        presence_priority,
        pending_resume_stream_id,
        pending_resume_h,
        suppress_sm_record_next_batch,
        roster_interested,
        blocklist_interested,
    } = ctx;

    // Stream resumption is only legal before this transport has established a
    // fresh SASL/bind lifecycle of its own.
    if !phase.allows_stream_management_resume() {
        return vec![SmFailed::with_condition("unexpected-request").to_xml()];
    }

    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .claim_session(&resume.previd)
        .await
    {
        Ok(Some(session)) => session,
        Ok(None) => {
            info!(stream_id = %resume.previd, "SM resume rejected: session not found or expired");
            return vec![SmFailed::with_condition("item-not-found").to_xml()];
        }
        Err(e) => {
            warn!(stream_id = %resume.previd, error = %e, "SM resume failed: registry error");
            return vec![SmFailed::with_condition("internal-server-error").to_xml()];
        }
    };

    if let ConnectionPhase::Authenticated { bare_jid } = phase {
        if detached.jid.to_bare() != *bare_jid {
            warn!(
                current_jid = %bare_jid,
                resumed_jid = %detached.jid,
                "SM resume rejected due to authenticated identity mismatch"
            );
            if let Err(error) = state
                .deps
                .protocol
                .sm_session_registry
                .release_claim(&resume.previd)
                .await
            {
                warn!(stream_id = %resume.previd, error = %error, "Failed to release rejected SM resume claim");
            }
            return vec![SmFailed::with_condition("not-authorized").to_xml()];
        }
    }

    let preserve_authenticated_session = matches!(phase, ConnectionPhase::Authenticated { .. });

    if resume.h > detached.outbound_count {
        if let Err(error) = state
            .deps
            .protocol
            .sm_session_registry
            .release_claim(&resume.previd)
            .await
        {
            warn!(stream_id = %resume.previd, error = %error, "Failed to release invalid SM resume claim");
        }
        *phase = ConnectionPhase::closing(None);
        info!(
            stream_id = %resume.previd,
            client_h = resume.h,
            send_count = detached.outbound_count,
            "SM resume rejected: handled count too high"
        );
        return vec![
            build_handled_count_too_high_stream_error(resume.h, detached.outbound_count),
            websocket_stream_close_xml(),
        ];
    }

    if !detached.can_resume_from(resume.h) {
        warn!(
            stream_id = %resume.previd,
            jid = %detached.jid,
            client_h = resume.h,
            replay_gap_through = ?detached.replay_gap_through,
            "SM resume rejected: replay window no longer contains every stanza required by client h"
        );
        if let Err(error) = state
            .deps
            .protocol
            .sm_session_registry
            .release_claim(&resume.previd)
            .await
        {
            warn!(stream_id = %resume.previd, error = %error, "Failed to release truncated SM resume claim");
        }
        return vec![
            SmFailed::resume_failed("resource-constraint", detached.inbound_count).to_xml(),
        ];
    }

    // Restore SM counters + the unacked queue.
    sm_state.restore_from_session(&detached);
    // The client tells us how many of OUR outbound stanzas they've actually
    // handled. Acknowledge up to that point so the replay set is minimal.
    sm_state.acknowledge(resume.h);

    // Restore authentication identity. If the detached sidecar has no
    // matching Session (TTL expired / crash), the authenticated resume keeps
    // the fresh transport's current Session context.
    let restored_session = state
        .deps
        .protocol
        .resumable_sessions
        .get(&resume.previd)
        .map(|s| s.clone());
    if restored_session.is_none() && preserve_authenticated_session {
        warn!(
            stream_id = %resume.previd,
            jid = %detached.jid,
            "SM resumed without cached detached Session; preserving current authenticated Session"
        );
    }

    let resumed_session = restored_session.or_else(|| {
        if preserve_authenticated_session {
            authenticated_session.clone()
        } else {
            None
        }
    });

    *authenticated_session = resumed_session;
    *carbons_enabled = detached.carbons_enabled;
    *roster_interested = detached.roster_interested;
    *blocklist_interested = detached.blocklist_interested;
    *presence_available = detached.presence_available;
    *presence_show = detached.presence_show.clone();
    *presence_status = detached.presence_status.clone();
    *presence_priority = detached.presence_priority;
    *pending_resume_stream_id = Some(resume.previd.clone());
    *pending_resume_h = Some(resume.h);
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
    let replay: Vec<String> = sm_state
        .get_stanzas_to_resend(resume.h)
        .into_iter()
        .map(|entry| {
            stamp_replay_delay(&entry.stanza_xml, server_domain, entry.original_receipt_at)
        })
        .collect();
    info!(
        stream_id = %resume.previd,
        jid = %detached.jid,
        replay = replay.len(),
        "SM resumed"
    );

    let mut responses = Vec::with_capacity(replay.len() + 1);
    responses.push(SmResumed::new(resume.previd, sm_state.get_inbound_count()).to_xml());
    responses.extend(replay);
    responses
}
