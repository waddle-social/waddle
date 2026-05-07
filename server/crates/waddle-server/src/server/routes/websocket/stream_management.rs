use super::transport_xml::build_handled_count_too_high_stream_error;
use super::*;

/// Returns true if the frame is an XMPP stanza that counts toward XEP-0198
/// handled/sent counters. Only `<iq>`, `<message>`, `<presence>` qualify;
/// stream headers, SASL frames, and SM control nonzas do not.
///
/// Matches on the element name rather than a string-prefix: a substring
/// match like `starts_with("<message")` would also accept future nonzas
/// such as `<messages>` or `<presences>`.
pub(super) fn is_countable_stanza(frame: &str) -> bool {
    let trimmed = frame.trim_start();
    let Some(after_lt) = trimmed.strip_prefix('<') else {
        return false;
    };
    let name_end = after_lt
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(after_lt.len());
    matches!(&after_lt[..name_end], "iq" | "message" | "presence")
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
            ctx.sm_state.acknowledge(ack.h);
            // Locked Q7b SM-ack lifecycle (issue #209): range-delete
            // every `pending_delivery` row claimed by this XEP-0198
            // session whose recorded outbound counter is <= `ack.h`.
            // This is what actually frees the row from the durable
            // queue — the flush path no longer deletes on push.
            //
            // Session id is the XEP-0198 stream_id (NOT the resource
            // JID — Qodo review on PR #358: distinct SM sessions on
            // the same resource share the same JID, so keying by JID
            // would let one session's ack delete another's claimed
            // rows). The flush function reads the same stream_id from
            // the connection's `ConnectionEntry` so claim and delete
            // agree on the key.
            // Greptile review on PR #358: this MUST run inline so it
            // executes after any preceding `record_pushed_at` for the
            // same connection. Spawning would let a quick ack arrive
            // and run delete_acked_through against a row whose
            // outbound_sequence is still NULL (because the
            // record_pushed_at task hadn't completed), silently
            // skipping the delete.
            if let Some(stream_id) = ctx.sm_state.stream_id.clone() {
                let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(stream_id);
                let h = ack.h;
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
            vec![]
        }
        SmStanza::Resume(resume) => handle_sm_resume(resume, state, ctx).await,
        // Server-origin nonzas should never arrive from a client. Ignore.
        SmStanza::Enabled(_) | SmStanza::Resumed(_) | SmStanza::Failed(_) => vec![],
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
    use waddle_xmpp::stream_management::{SmFailed, SmResumed};

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
        return vec![build_handled_count_too_high_stream_error(
            resume.h,
            detached.outbound_count,
        )];
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

    let replay: Vec<String> = sm_state
        .get_stanzas_to_resend(resume.h)
        .into_iter()
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
