use super::*;

pub(super) fn events_contain_iq_error(events: &[waddle_xmpp::protocol::OutboundEvent]) -> bool {
    use waddle_xmpp::protocol::OutboundEvent;
    use waddle_xmpp::Stanza;
    events.iter().any(|event| {
        let OutboundEvent::SendStanza(stanza) = event else {
            return false;
        };
        matches!(
            stanza.as_ref(),
            Stanza::Iq(iq) if matches!(iq.as_ref(), xmpp_parsers::iq::Iq::Error { .. })
        )
    })
}

/// Dispatch an IQ whose payload namespace has a registered handler in
/// the protocol dispatcher.
///
/// Returns `Some(frames)` when a registered handler owned the IQ —
/// even when `frames` is empty. An empty `Some` is a legitimate,
/// terminal outcome: a handler may forward the stanza to a peer
/// (`OutboundEvent::RouteToConnection`) and produce no synchronous
/// frame for the sender (e.g. XEP-0166 Jingle 1:1 `session-initiate`,
/// which the peer's client answers). The caller MUST treat `Some(_)`
/// as final and NOT fall through to the unhandled-IQ branch, otherwise
/// a successfully-forwarded call IQ is wrongly answered with
/// `feature-not-implemented`.
///
/// Returns `None` only when no registered handler claims the
/// namespace, so the caller can continue to the remaining (disco,
/// misc, MUC, pubsub) branches.
pub(super) async fn handle_sans_io_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
    conn_state: &mut IqConnState<'_>,
) -> Option<Vec<String>> {
    let iq = ctx.iq;
    let id = ctx.id;
    let payload_ns = ctx.payload_ns;
    let domain = ctx.domain;
    let response_from = ctx.response_from;
    let response_to = ctx.response_to;

    // Sans-I/O dispatch: if the IQ namespace has a registered handler in
    // the protocol dispatcher, route through it and translate the emitted
    // OutboundEvents into outbound XML frames via `interpret()`.
    //
    // Handlers that still need async I/O (for example MAM, Jingle, disco,
    // and any other namespaces not yet registered with the dispatcher)
    // continue to fall through to the legacy string-matching branches
    // below until the two-phase async callback machinery lands.
    let carbons_toggle = match iq {
        xmpp_parsers::iq::Iq::Set { payload: e, .. }
            if e.ns() == CARBONS_NS && (e.name() == "enable" || e.name() == "disable") =>
        {
            Some(e.name() == "enable")
        }
        _ => None,
    };
    if state.deps.protocol.dispatcher.has_iq_handler(payload_ns) {
        if payload_ns == waddle_xmpp::xep::NS_VERSION && !is_version_query(iq) {
            return Some(vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )]);
        }
        if payload_ns == waddle_xmpp::xep::NS_VERSION
            && iq
                .to()
                .is_some_and(|target| target.to_bare().as_str() != domain)
        {
            return Some(vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                service_unavailable_iq_error("Service unavailable at this address."),
            )]);
        }
        if payload_ns == waddle_xmpp::xep::NS_TIME {
            if !is_time_query(iq) {
                return Some(vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    bad_request_iq_error("Malformed IQ payload."),
                )]);
            }
            if iq
                .to()
                .is_some_and(|target| target.to_bare().as_str() != domain)
            {
                return Some(vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    service_unavailable_iq_error("Service unavailable at this address."),
                )]);
            }
        }
        let Some(full_jid) = phase.bound_jid() else {
            return Some(vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )]);
        };
        if payload_ns == waddle_xmpp::xep::xep0166::NS_JINGLE {
            if let Some(reply) = peer_jingle_blocklist_reply(state, full_jid, iq, domain).await {
                return Some(vec![match reply {
                    PeerJingleBlocklistReply::Bounce(stanza) => stanza_to_xml(&stanza),
                    PeerJingleBlocklistReply::Error(error) => {
                        build_iq_error_xml_typed(id, response_from, response_to, error)
                    }
                }]);
            }
            if let Some(rate_limit_error) = pre_dispatch_muji_rate_limit_error(state, full_jid, iq)
            {
                return Some(vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    rate_limit_error,
                )]);
            }
        }
        if let Some(enabled) = carbons_toggle {
            *conn_state.carbons_enabled = enabled;
            let _ = state
                .deps
                .protocol
                .connection_registry
                .set_carbons_enabled(full_jid, enabled);
            if let Some(owner) = conn_state.registry_owner {
                mirror_remote_carbons_update(state, full_jid, owner, enabled).await;
            }
        }
        // XMPP-native MUC-call membership gate (XEP-0272 Muji): a
        // session may only mint a LiveKit JWT for a room it has
        // actually joined through the XEP-0045 path. The pure
        // dispatcher in `waddle-xmpp` has no handle to the room
        // registry, so the check has to happen here before
        // dispatch. The gate triggers on the Jingle namespace and
        // only when the embedded `<jingle/>` carries a `<muji/>`
        // child — 1:1 Jingle traffic passes through untouched.
        let mut media_capabilities = None;
        if payload_ns == waddle_xmpp::xep::xep0166::NS_JINGLE {
            match super::jingle_muji_gate::verify_muji_jingle_request(
                state,
                full_jid,
                iq,
                super::jingle_muji_gate::GateInvocation::ClientOrigin,
            )
            .await
            {
                super::jingle_muji_gate::GateOutcome::Allow {
                    media_capabilities: gate_capabilities,
                } => {
                    // Authorization produced the grant: the Jingle
                    // handler's Muji mint consumes exactly the
                    // capabilities the gate derived from the sender's
                    // current XEP-0045 role.
                    media_capabilities = gate_capabilities;
                }
                super::jingle_muji_gate::GateOutcome::Deny(stanza_error) => {
                    return Some(vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        *stanza_error,
                    )]);
                }
                super::jingle_muji_gate::GateOutcome::RoomNotLocal { room_jid } => {
                    // #1445: no room actor in this process. On a
                    // clustered deployment the room may be alive on the
                    // claim-owning node — relay the Muji IQ there
                    // instead of answering for a room that exists
                    // elsewhere.
                    let reply = IqReplyAddressing {
                        id,
                        response_from,
                        response_to,
                    };
                    match relay_muji_to_room_owner(
                        state, conn_state, full_jid, iq, &room_jid, reply,
                    )
                    .await
                    {
                        MujiRelayOutcome::Frames(frames) => return Some(frames),
                        // Terminate could not be relayed. Fall through
                        // to local dispatch, which is what this path
                        // did before the relay existed: unregistering
                        // is idempotent, so a local no-op is strictly
                        // better than failing the client's hangup.
                        MujiRelayOutcome::ProcessLocally {
                            enqueue_owner_cleanup,
                        } => {
                            if enqueue_owner_cleanup {
                                enqueue_muji_relay_teardown_fallback(state, &room_jid, full_jid)
                                    .await;
                            }
                        }
                    }
                }
            }
        }
        let ctx = ProtocolStanzaContext {
            domain,
            full_jid,
            media_capabilities,
        };
        let muji_terminate_room = super::jingle_muji_gate::muji_session_terminate_room(iq);
        let events = state.deps.protocol.dispatcher.dispatch_iq(iq, &ctx);
        let muji_clear_after = muji_terminate_room.filter(|_| !events_contain_iq_error(&events));
        let deps = crate::server::routes::interpret::Deps {
            connection_registry: &state.deps.protocol.connection_registry,
            user_registry: Some(&state.deps.protocol.user_registry),
            sm_session_registry: Some(&state.deps.protocol.sm_session_registry),
            mam_storage: Some(&state.deps.protocol.mam_storage),
            inbox_storage: Some(&state.deps.protocol.inbox_storage),
            extension_manager: Some(&state.deps.protocol.extension_manager),
            room_registry: Some(&state.deps.protocol.room_registry),
            web_socket_state: Some(state),
            authenticated_session: authenticated_session.as_ref(),
            local_domain: state.deps.auth_state.xmpp_domain.as_str(),
            blocking_storage: Some(&state.deps.protocol.blocking_storage),
            message_dispatcher: Some(&state.deps.protocol.dispatcher),
            pending_delivery_storage: Some(&state.deps.protocol.pending_delivery_storage),
            ordered_relay_origin: conn_state.ordered_relay_origin.clone(),
            sfu: state.deps.protocol.sfu.as_deref(),
        };
        let outcome = crate::server::routes::interpret::interpret(events, &deps).await;
        if let Some(room_jid) = muji_clear_after {
            crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
                state, &room_jid, full_jid, None,
            )
            .await;
        }
        if outcome.close {
            warn!(
                ns = %payload_ns,
                "Sans-I/O handler requested transport close; \
                 WebSocket adapter cannot honour CloseTransport yet"
            );
        }
        // A registered handler owned this IQ. Empty frames are valid
        // and terminal (e.g. a Jingle 1:1 stanza forwarded to the peer
        // with no synchronous reply for the sender) — return `Some` so
        // the caller does NOT fall through to the unhandled-IQ error.
        return Some(outcome.frames);
    }
    None
}

/// Enforce the target account's XEP-0191 blocklist before a direct-call
/// negotiation reaches the Jingle handler. That handler mints credentials and
/// registers both identities, so filtering only at the later routing boundary
/// is too late even though it prevents delivery to the blocked peer.
///
/// Muji is membership-gated separately, and extdisco uses a different
/// namespace, so neither surface enters this check.
enum PeerJingleBlocklistReply {
    Bounce(Stanza),
    Error(xmpp_parsers::stanza_error::StanzaError),
}

async fn peer_jingle_blocklist_reply(
    state: &WebSocketState,
    sender: &FullJid,
    iq: &xmpp_parsers::iq::Iq,
    local_domain: &str,
) -> Option<PeerJingleBlocklistReply> {
    let xmpp_parsers::iq::Iq::Set { payload, to, .. } = iq else {
        return None;
    };
    if payload.ns() != waddle_xmpp::xep::xep0166::NS_JINGLE
        || payload.name() != "jingle"
        || waddle_xmpp::xep::xep0272::find_muji(payload).is_some()
    {
        return None;
    }
    let action = xmpp_parsers::jingle::Jingle::try_from(payload.clone())
        .ok()
        .map(|jingle| jingle.action);
    if !matches!(
        action,
        Some(
            xmpp_parsers::jingle::Action::SessionInitiate
                | xmpp_parsers::jingle::Action::SessionAccept
        )
    ) {
        return None;
    }
    let target = to
        .as_ref()
        .filter(|target| target.resource().is_some() && target.domain().as_str() == local_domain)?;

    let blocking = DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
    match blocking
        .is_blocked_jid(&target.to_bare(), &Jid::from(sender.clone()))
        .await
    {
        Ok(true) => crate::server::routes::interpret::undeliverable_iq_reply(&Stanza::Iq(
            Box::new(iq.clone()),
        ))
        .map(PeerJingleBlocklistReply::Bounce),
        Ok(false) => None,
        Err(error) => {
            warn!(
                error = %error,
                target = %target,
                sender = %sender,
                "Failed to check blocklist before dispatching direct Jingle IQ"
            );
            Some(PeerJingleBlocklistReply::Error(
                internal_server_error_iq_error("Internal server error."),
            ))
        }
    }
}

/// Pre-dispatch limiter for the Muji actions that can be expensive
/// before the sans-I/O Jingle handler sees them. These buckets are
/// intentionally separate from the handler's own limiters: the
/// websocket path must charge before room-locality checks, membership
/// asks, or cross-node relays do any work, while the handler still
/// defends non-websocket dispatch.
fn pre_dispatch_muji_rate_limit_error(
    state: &WebSocketState,
    sender: &FullJid,
    iq: &xmpp_parsers::iq::Iq,
) -> Option<xmpp_parsers::stanza_error::StanzaError> {
    let xmpp_parsers::iq::Iq::Set { payload, .. } = iq else {
        return None;
    };
    if payload.ns() != waddle_xmpp::xep::xep0166::NS_JINGLE
        || payload.name() != "jingle"
        || waddle_xmpp::xep::xep0272::find_muji(payload).is_none()
    {
        return None;
    }
    let action = xmpp_parsers::jingle::Jingle::try_from(payload.clone())
        .ok()
        .map(|jingle| jingle.action)?;
    let sender_bare = sender.to_bare();
    let rate_limited = match action {
        xmpp_parsers::jingle::Action::SessionInitiate => return None,
        xmpp_parsers::jingle::Action::SessionTerminate => state
            .deps
            .protocol
            .muji_pre_dispatch_terminate_rate_limit
            .check_and_record(&sender_bare)
            .err()
            .map(|exceeded| {
                tracing::warn!(
                    jid = %sender_bare,
                    %exceeded,
                    "rate-limit dropped Muji session-terminate before membership or relay checks"
                );
                waddle_xmpp::telemetry::call::increment_call_control_rate_limited(
                    waddle_xmpp::telemetry::attributes::CallControlRateLimitedSurface::Terminate,
                );
                "session-terminate rate limit exceeded"
            }),
        _ => state
            .deps
            .protocol
            .muji_pre_dispatch_action_rate_limit
            .check_and_record(&sender_bare)
            .err()
            .map(|exceeded| {
                tracing::warn!(
                    jid = %sender_bare,
                    %exceeded,
                    "rate-limit dropped Muji non-initiate action before membership or relay checks"
                );
                waddle_xmpp::telemetry::call::increment_call_control_rate_limited(
                    waddle_xmpp::telemetry::attributes::CallControlRateLimitedSurface::MujiAction,
                );
                "Muji action rate limit exceeded"
            }),
    }?;
    Some(xmpp_parsers::stanza_error::StanzaError::new(
        xmpp_parsers::stanza_error::ErrorType::Cancel,
        xmpp_parsers::stanza_error::DefinedCondition::PolicyViolation,
        "en",
        rate_limited,
    ))
}

/// What the caller should do after a relay attempt.
#[cfg(feature = "clustering")]
enum MujiRelayOutcome {
    /// Terminal: send these frames to the client.
    Frames(Vec<String>),
    /// Non-terminal: handle the IQ on this node after all. Only ever
    /// returned for `session-terminate`, whose local execution is an
    /// idempotent no-op and therefore a better answer than an error.
    ProcessLocally { enqueue_owner_cleanup: bool },
}

/// Resolve a Muji IQ whose room has no local actor (#1445): relay it
/// to the room's claim-owning node over the ordered MUC proxy — the
/// owner runs the gate, mint, and registry mutation where the
/// occupancy lives, and its replies (for an initiate: the IQ ack and
/// the server-initiated `session-accept` carrying the LiveKit token)
/// ride back on the relay ACK to be written to this socket.
///
/// `session-terminate` is relayed too, and must be: since an initiate
/// registers the participant on the OWNER, a terminate executed here
/// would clear nothing there, leaving a phantom in-call participant
/// that also suppresses `DeleteRoom` for everyone else in the room.
/// When a terminate cannot be relayed it degrades to local execution
/// rather than an error.
///
/// Outcome mapping for an initiate:
/// - `Delivered` with frames → the owner's reply frames, verbatim.
/// - `RoomUnclaimed` and `LocalRoom` → terminal `room_not_found`
///   denial. `LocalRoom` belongs here, not in the retry bucket: it
///   means the claim store says THIS node owns the room while the gate
///   found no local actor, so the room has no occupants and no retry
///   can change that — the same conclusion the owner-side executor
///   reaches. Retrying would loop forever against a stale claim row.
/// - everything else (claim unavailable, origin unavailable, relay
///   dropped/maybe-committed, or a `Delivered` carrying no frames) → a
///   `type='wait'` internal-server-error so the client retries; a
///   duplicate mint on retry is harmless (set-insert registration,
///   superseding token).
///
/// Every terminal arm records call-setup telemetry — `deny` through
/// [`deny_room_not_found`], `retry_later` explicitly — because a relay
/// failure is a complete, client-visible call-setup attempt. Leaving
/// it uncounted would hide exactly the cross-node failure class #1445
/// exists to fix (#1452).
#[cfg(feature = "clustering")]
async fn relay_muji_to_room_owner(
    state: &WebSocketState,
    conn_state: &IqConnState<'_>,
    full_jid: &FullJid,
    iq: &xmpp_parsers::iq::Iq,
    room_jid: &jid::BareJid,
    reply: IqReplyAddressing<'_>,
) -> MujiRelayOutcome {
    use crate::clustering::ordered_relay::OrderedRelayMucProxyKind;
    use crate::clustering::route_bridge::{MucProxyRouteDecision, OrderedRelayMucProxyOutcome};

    // A terminate that cannot be relayed falls back to this node
    // instead of erroring; an initiate that cannot be relayed is a
    // real failure the client must see.
    //
    // The error builders are passed as closures and invoked ONLY on
    // the initiate branch. They have telemetry side effects (a denial
    // counter, a call-setup failure, a WARN naming the room and user),
    // and a hangup is neither a token denial nor a call-setup attempt
    // — evaluating them eagerly would fabricate `room_not_found`
    // denials on every cross-node teardown.
    let is_terminate = super::jingle_muji_gate::muji_session_terminate_room(iq).is_some();
    let is_session_initiate = matches!(
        iq,
        xmpp_parsers::iq::Iq::Set { payload, .. }
            if payload.ns() == waddle_xmpp::xep::xep0166::NS_JINGLE
                && payload.name() == "jingle"
                && payload.attr("action") == Some("session-initiate")
    );
    let unrelayable = |enqueue_owner_cleanup: bool, frames: &dyn Fn() -> Vec<String>| {
        if is_terminate {
            MujiRelayOutcome::ProcessLocally {
                enqueue_owner_cleanup,
            }
        } else {
            MujiRelayOutcome::Frames(frames())
        }
    };
    // A terminate that falls back to local execution because the relay
    // was *unavailable* is NOT the benign no-op the unclaimed-room case
    // is, and must not be silent. Locally there is nothing registered
    // to clear (the initiate registered on the owner), yet
    // `unregister_call_participant` still fires `RemoveParticipant`, so
    // the user's media does stop — while the OWNER keeps the registry
    // entry and never runs the Muji-presence clear. That phantom
    // suppresses `DeleteRoom` and keeps the room's "in call" state lit
    // for every other occupant until the reconcile sweep catches it.
    // The telemetry-bearing error builders are deliberately skipped for
    // a terminate, so without this line the whole situation is
    // invisible. Convergence is still owed to the durable control plane
    // in #1449.
    let unrelayable_after_relay_failure = |reason: &'static str| {
        if is_terminate {
            tracing::warn!(
                room = %room_jid,
                user = %full_jid.to_bare(),
                reason,
                "Muji session-terminate could not be relayed to the room owner; \
                 executing locally — the owner may hold a phantom call participant \
                 until reconciliation"
            );
        }
    };

    let deny = || {
        vec![build_iq_error_xml_typed(
            reply.id,
            reply.response_from,
            reply.response_to,
            *if is_session_initiate {
                super::jingle_muji_gate::deny_room_not_found(room_jid, &full_jid.to_bare())
            } else {
                super::jingle_muji_gate::deny_room_not_found_without_setup_telemetry()
            },
        )]
    };
    let relay_error_frames = || {
        vec![build_iq_error_xml_typed(
            reply.id,
            reply.response_from,
            reply.response_to,
            internal_server_error_iq_error(
                "the requested room is owned by another node and could not be reached; \
                 please retry",
            ),
        )]
    };
    // The owner was definitely never reached, so this attempt ended
    // here and nowhere else: record it, once, as a relay failure.
    //
    // NOT as an SFU token denial — the membership gate did not reject
    // this request, it never ran. Routing relay outages into
    // `sfu_token.denied` / `membership_check_failed` would make a
    // clustering incident read as a permissions problem.
    let relay_failed = || {
        if is_session_initiate {
            waddle_xmpp::telemetry::call::record_call_setup_rejected(
                waddle_xmpp::telemetry::attributes::CallSetupFailureReason::OwnerUnreachable,
            );
        }
        relay_error_frames()
    };
    // The owner MAY have executed this already and recorded its own
    // terminal outcome (`setup.ok`, or a failure of its own). Counting
    // a failure here too would let one client attempt appear as both
    // succeeded and failed, breaking the exactly-one-terminal-outcome
    // property `setup.ok / setup.attempted` depends on. An uncounted
    // ambiguous attempt is the honest treatment of "we don't know";
    // the log carries the diagnosis.
    let relay_uncertain = |reason: &'static str| {
        tracing::warn!(
            room = %room_jid,
            user = %full_jid.to_bare(),
            reason,
            "Muji relay outcome is uncertain; the room owner may or may not have \
             executed this request, so no call-setup outcome is recorded for it"
        );
        relay_error_frames()
    };

    // The relay's envelope validation requires a bare `to` (and binds
    // the `<muji room>` payload to the channel's room); a non-bare
    // `to` is NACKed as a parse failure by the receiver, which
    // diverts the Muji signaling lane (#1597) and would silently
    // break this sender's later Muji IQs for the room. The exact
    // mixer JID is enforced only here at ingress — a client controls
    // `to`, so reject the shape here rather than letting a malformed
    // (or hostile) one reach the relay at all.
    // Same for a room outside this server's MUC domain: it can have no
    // local claim, so relaying it only buys a claim-store lookup.
    let addressed_to_mixer = iq.to().is_some_and(|to| {
        to.resource().is_none()
            && to.to_bare()
                == waddle_xmpp::protocol::handlers::jingle::calls_mixer_jid(
                    state.deps.auth_state.xmpp_domain.as_str(),
                )
    });
    let room_is_local = waddle_xmpp::protocol::handlers::jingle::room_is_on_local_muc_service(
        room_jid,
        state.deps.auth_state.xmpp_domain.as_str(),
    );
    if !addressed_to_mixer || !room_is_local {
        return unrelayable(false, &deny);
    }

    let bridge = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref();
    let (Some(bridge), Some(origin)) = (bridge, conn_state.ordered_relay_origin.as_ref()) else {
        // No relay substrate (clustering disabled at runtime): local
        // absence is definitive, exactly the pre-#1445 semantics.
        return unrelayable(false, &deny);
    };
    // Stamp the authenticated full JID as `from` before relaying:
    // clients legitimately omit `from` (the server derives the sender
    // from the bound session), but the envelope's sender-claim
    // validation and the owner-side executor both read the stanza's
    // `from`. Unconditional overwrite — a client-supplied `from` is
    // never trusted.
    let mut relayed = iq.clone();
    let stamped_from = Some(jid::Jid::from(full_jid.clone()));
    match &mut relayed {
        xmpp_parsers::iq::Iq::Get { from, .. }
        | xmpp_parsers::iq::Iq::Set { from, .. }
        | xmpp_parsers::iq::Iq::Result { from, .. }
        | xmpp_parsers::iq::Iq::Error { from, .. } => *from = stamped_from,
    }
    let stanza = waddle_xmpp::Stanza::Iq(Box::new(relayed));
    match bridge
        .try_proxy_muc_remote_decision(
            room_jid,
            &stanza,
            OrderedRelayMucProxyKind::MujiJingleIq,
            origin,
        )
        .await
    {
        MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Delivered(replies))
            if !replies.is_empty() =>
        {
            MujiRelayOutcome::Frames(
                replies
                    .iter()
                    .map(crate::server::routes::websocket::transport_xml::stanza_to_xml)
                    .collect(),
            )
        }
        // A `Delivered` carrying no frames (e.g. `QueuedDetached`)
        // would otherwise return `Some(vec![])`, which the caller
        // treats as terminal — the client's IQ would get no result and
        // no error, and the call would silently never start.
        // `Delivered` with no frames (`QueuedDetached`) means the owner
        // took it — outcome unknown to us, and its own.
        MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Delivered(_)) => {
            unrelayable_after_relay_failure("delivered_without_replies");
            unrelayable(true, &|| relay_uncertain("delivered_without_replies"))
        }
        // No node owns the room (or this one does, with no actor): the
        // local fallback for a terminate genuinely IS a no-op, because
        // there is no owner holding a registration to strand.
        MucProxyRouteDecision::RoomUnclaimed | MucProxyRouteDecision::LocalRoom => {
            unrelayable(false, &deny)
        }
        // Definitely not delivered: the attempt ended here.
        MucProxyRouteDecision::Attempted(
            OrderedRelayMucProxyOutcome::Unavailable | OrderedRelayMucProxyOutcome::Dropped,
        ) => {
            unrelayable_after_relay_failure("relay_delivery_failed");
            unrelayable(true, &relay_failed)
        }
        // Ambiguous by construction — the owner may have committed it.
        MucProxyRouteDecision::Attempted(
            OrderedRelayMucProxyOutcome::MaybeCommitted
            | OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
        ) => {
            unrelayable_after_relay_failure("relay_maybe_committed");
            unrelayable(true, &|| relay_uncertain("relay_maybe_committed"))
        }
        MucProxyRouteDecision::RoomClaimUnavailable => {
            unrelayable_after_relay_failure("room_claim_unavailable");
            unrelayable(true, &relay_failed)
        }
        MucProxyRouteDecision::OriginUnavailable => {
            unrelayable_after_relay_failure("origin_unavailable");
            unrelayable(true, &relay_failed)
        }
    }
}

/// Non-clustering builds have no other replica that could own the
/// room, so local absence is definitive — the terminal denial,
/// byte-identical to the pre-#1445 wire behavior.
#[cfg(not(feature = "clustering"))]
enum MujiRelayOutcome {
    Frames(Vec<String>),
    ProcessLocally { enqueue_owner_cleanup: bool },
}

#[cfg(not(feature = "clustering"))]
async fn relay_muji_to_room_owner(
    _state: &WebSocketState,
    _conn_state: &IqConnState<'_>,
    full_jid: &FullJid,
    iq: &xmpp_parsers::iq::Iq,
    room_jid: &jid::BareJid,
    reply: IqReplyAddressing<'_>,
) -> MujiRelayOutcome {
    if super::jingle_muji_gate::muji_session_terminate_room(iq).is_some() {
        return MujiRelayOutcome::ProcessLocally {
            enqueue_owner_cleanup: false,
        };
    }
    let denial = if matches!(
        iq,
        xmpp_parsers::iq::Iq::Set { payload, .. }
            if payload.ns() == waddle_xmpp::xep::xep0166::NS_JINGLE
                && payload.name() == "jingle"
                && payload.attr("action") == Some("session-initiate")
    ) {
        super::jingle_muji_gate::deny_room_not_found(room_jid, &full_jid.to_bare())
    } else {
        super::jingle_muji_gate::deny_room_not_found_without_setup_telemetry()
    };
    MujiRelayOutcome::Frames(vec![build_iq_error_xml_typed(
        reply.id,
        reply.response_from,
        reply.response_to,
        *denial,
    )])
}

/// Persist the owner-side convergence that a failed cross-node terminate
/// could not deliver. Enqueue errors are operationally loud but never alter
/// the client's successful hangup response.
async fn enqueue_muji_relay_teardown_fallback(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    departed: &jid::FullJid,
) {
    let call_id = match waddle_sfu::CallId::new(room_jid.to_string()) {
        Ok(call_id) => call_id,
        Err(error) => {
            tracing::warn!(
                room = %room_jid,
                %error,
                "could not model Muji relay fallback as a typed teardown intent"
            );
            return;
        }
    };
    let intents = [
        crate::call_teardown_outbox::CallTeardownIntent {
            call_id: call_id.clone(),
            target: crate::call_teardown_outbox::TeardownTarget::MujiPresenceClear {
                room_jid: room_jid.clone(),
                departed: departed.clone(),
            },
            generation: None,
            room_sid: None,
        },
        crate::call_teardown_outbox::CallTeardownIntent {
            call_id,
            target: crate::call_teardown_outbox::TeardownTarget::Participant {
                identity: departed.clone(),
                participant_sid: None,
            },
            generation: None,
            room_sid: None,
        },
    ];
    let store = &state.deps.protocol.call_teardown_outbox;
    if let Err(error) = store.enqueue_batch(&intents).await {
        tracing::warn!(
            room = %room_jid,
            departed = %departed,
            %error,
            "failed to persist Muji teardown fallback; retrying asynchronously"
        );
        state
            .deps
            .protocol
            .call_teardown_persistence
            .retry_batch(intents.to_vec());
    }
}

/// The (id, from, to) triple every IQ error reply is stamped with —
/// bundled so the relay helper's signature stays readable.
struct IqReplyAddressing<'a> {
    id: &'a str,
    response_from: Option<&'a str>,
    response_to: Option<&'a str>,
}

#[cfg(feature = "clustering")]
async fn mirror_remote_carbons_update(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    enabled: bool,
) {
    if let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    {
        bridge
            .update_remote_user_resource_if_owner(
                jid,
                owner,
                crate::clustering::route_bridge::RemoteResourceStateUpdate::Carbons { enabled },
            )
            .await;
    }
}

#[cfg(not(feature = "clustering"))]
async fn mirror_remote_carbons_update(
    _state: &WebSocketState,
    _jid: &FullJid,
    _owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    _enabled: bool,
) {
}

#[cfg(test)]
mod tests {
    use crate::call_teardown_outbox::TeardownTarget;
    use crate::db::actor::DbExecute;
    use crate::db::blocking::DatabaseBlockingStorage;
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state_with_calls, create_test_websocket_state_with_sfu,
        register_test_connection, RecordingSfu,
    };
    use chrono::Utc;
    use jid::{FullJid, Jid};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;
    use waddle_sfu::{
        ApiSecret, CallId, Identity, JoinToken, Jti, Jwt, MediaCapabilities, SfuError, SfuService,
        TurnCredential, TurnHost, WebsocketUrl,
    };
    use waddle_xmpp::protocol::frame::{parse_frame, InboundFrame};
    use waddle_xmpp::registry::OutboundStanza;
    use waddle_xmpp::xep::xep0167::MediaKind;
    use waddle_xmpp::xep::xep0272::{Creator, Muji, MujiContent};
    use waddle_xmpp::Stanza;
    use xmpp_parsers::iq::Iq;
    use xmpp_parsers::jingle::{Action, Jingle, SessionId};

    #[derive(Default)]
    struct RecordingCallSfu {
        issued: Mutex<Vec<(CallId, Identity, MediaCapabilities)>>,
        registered: Mutex<Vec<(CallId, Identity)>>,
    }

    impl RecordingCallSfu {
        fn issued_snapshot(&self) -> Vec<(CallId, Identity, MediaCapabilities)> {
            self.issued.lock().expect("recording lock").clone()
        }

        fn registered_snapshot(&self) -> Vec<(CallId, Identity)> {
            self.registered.lock().expect("recording lock").clone()
        }
    }

    impl SfuService for RecordingCallSfu {
        fn issue_join_token(
            &self,
            call_id: &CallId,
            identity: &Identity,
            capabilities: MediaCapabilities,
        ) -> Result<JoinToken, SfuError> {
            self.issued.lock().expect("recording lock").push((
                call_id.clone(),
                identity.clone(),
                capabilities,
            ));
            Ok(JoinToken {
                url: WebsocketUrl::new("wss://livekit.test/".parse().expect("valid url"))
                    .expect("valid ws url"),
                room: call_id.clone(),
                identity: identity.clone(),
                jwt: Jwt::from_wire("test.jwt".to_string()),
                jti: Jti::new(),
                expires_at: Utc::now(),
            })
        }

        fn issue_turn_credentials(&self, _: &Identity) -> Result<TurnCredential, SfuError> {
            unimplemented!("not exercised by these tests")
        }

        fn register_call_participant(&self, call_id: &CallId, identity: &Identity) {
            self.registered
                .lock()
                .expect("recording lock")
                .push((call_id.clone(), identity.clone()));
        }

        fn register_call_participant_observed(
            &self,
            _: &CallId,
            _: &Identity,
            _: &waddle_sfu::ObservedCallSids,
        ) -> waddle_sfu::SidObservationDisposition {
            unimplemented!("not exercised by these tests")
        }

        fn has_call_participant(&self, _: &CallId, _: &Identity) -> bool {
            false
        }

        fn revoke_issued_token(&self, _: &CallId, _: &Identity, _: &Jti) {
            unimplemented!("not exercised by these tests")
        }

        fn unregister_call_participant(
            &self,
            _: &CallId,
            _: &Identity,
            _: Option<&waddle_sfu::ObservedCallSids>,
        ) -> waddle_sfu::TeardownDisposition {
            unimplemented!("not exercised by these tests")
        }

        fn note_participant_left(
            &self,
            _: &CallId,
            _: &Identity,
            _: Option<&waddle_sfu::ObservedCallSids>,
        ) -> waddle_sfu::TeardownDisposition {
            unimplemented!("not exercised by these tests")
        }

        fn observe_call_participant_sids(
            &self,
            _: &CallId,
            _: &Identity,
            _: Option<&waddle_sfu::ObservedCallSids>,
        ) -> waddle_sfu::SidObservationDisposition {
            unimplemented!("not exercised by these tests")
        }

        fn update_participant_capabilities(&self, _: &CallId, _: &Identity, _: MediaCapabilities) {
            unimplemented!("not exercised by these tests")
        }

        fn is_revoked(&self, _: &Jti) -> bool {
            false
        }

        fn ws_url(&self) -> &WebsocketUrl {
            static URL: std::sync::OnceLock<WebsocketUrl> = std::sync::OnceLock::new();
            URL.get_or_init(|| {
                WebsocketUrl::new("wss://livekit.test/".parse().expect("valid url"))
                    .expect("valid ws url")
            })
        }

        fn turn_host(&self) -> &TurnHost {
            static HOST: std::sync::OnceLock<TurnHost> = std::sync::OnceLock::new();
            HOST.get_or_init(|| TurnHost::new("turn.test"))
        }

        fn webhook_secret(&self) -> &ApiSecret {
            static SECRET: std::sync::OnceLock<ApiSecret> = std::sync::OnceLock::new();
            SECRET.get_or_init(|| {
                ApiSecret::from_text("recording-webhook-secret-32-bytes")
                    .expect("recording webhook secret meets minimum length")
            })
        }

        fn participants_for_call(&self, _: &CallId) -> Vec<Identity> {
            Vec::new()
        }
    }

    fn ready_phase(jid: &FullJid) -> waddle_xmpp::protocol::ConnectionPhase {
        waddle_xmpp::protocol::ConnectionPhase::ready(jid.clone(), false)
    }

    fn parse_iq(xml: &str) -> Iq {
        match parse_frame(xml).expect("iq parses") {
            InboundFrame::Stanza(stanza) => match *stanza {
                Stanza::Iq(iq) => *iq,
                _ => panic!("expected iq stanza"),
            },
            _ => panic!("expected iq stanza"),
        }
    }

    fn direct_jingle_frame(id: &str, target: &str, action: &str) -> String {
        format!(
            "<iq xmlns='jabber:client' id='{id}' type='set' to='{target}'>\
               <jingle xmlns='urn:xmpp:jingle:1' action='{action}' sid='dmcall1' initiator='alice@example.com/web'>\
                 <content creator='initiator' name='audio'>\
                   <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'>\
                     <payload-type id='111' name='opus' clockrate='48000' channels='2'/>\
                     <rtcp-mux/>\
                   </description>\
                   <transport xmlns='urn:waddle:transports:livekit:0'/>\
                 </content>\
               </jingle>\
             </iq>"
        )
    }

    fn muji_jingle_frame(id: &str, target: &str, action: &str, room: &str) -> String {
        format!(
            "<iq xmlns='jabber:client' id='{id}' type='set' to='{target}'>\
               <jingle xmlns='urn:xmpp:jingle:1' action='{action}' sid='muji1' initiator='alice@example.com/web'>\
                 <muji xmlns='urn:xmpp:jingle:muji:0' room='{room}'/>\
               </jingle>\
             </iq>"
        )
    }

    fn muji_initiate_iq(room: &str) -> Iq {
        let jingle = Jingle::new(Action::SessionInitiate, SessionId("i-sid".into()));
        let mut elem: xmpp_parsers::minidom::Element = jingle.into();
        elem.append_child(
            Muji {
                room: Some(room.parse().expect("valid room jid")),
                preparing: false,
                contents: vec![MujiContent::new(
                    "audio",
                    Creator::Initiator,
                    MediaKind::Audio,
                )],
            }
            .to_element(),
        );
        Iq::Set {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some("calls.example.com".parse().expect("valid mixer jid")),
            id: "init-1".into(),
            payload: elem,
        }
    }

    fn muji_terminate_iq(room: &str) -> Iq {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("t-sid".into()));
        let mut elem: xmpp_parsers::minidom::Element = jingle.into();
        elem.append_child(
            Muji {
                room: Some(room.parse().expect("valid room jid")),
                preparing: false,
                contents: vec![MujiContent::new(
                    "audio",
                    Creator::Initiator,
                    MediaKind::Audio,
                )],
            }
            .to_element(),
        );
        Iq::Set {
            from: Some("alice@example.com/web".parse().expect("valid full jid")),
            to: Some("calls.example.com".parse().expect("valid mixer jid")),
            id: "term-1".into(),
            payload: elem,
        }
    }

    #[tokio::test]
    async fn owner_cleanup_fallback_persists_presence_and_participant_intents() {
        let state = create_test_websocket_state_with_calls().await;
        let room: jid::BareJid = "general@muc.example.com".parse().expect("room JID");
        let alice: jid::FullJid = "alice@example.com/web".parse().expect("full JID");

        super::enqueue_muji_relay_teardown_fallback(&state, &room, &alice).await;

        let jobs = state
            .deps
            .protocol
            .call_teardown_outbox
            .claim_due(8)
            .await
            .expect("claim fallback intents");
        assert_eq!(jobs.len(), 2);
        assert!(jobs
            .iter()
            .any(|job| matches!(job.intent.target, TeardownTarget::MujiPresenceClear { .. })));
        assert!(jobs
            .iter()
            .any(|job| matches!(job.intent.target, TeardownTarget::Participant { .. })));
        assert!(jobs.iter().all(|job| job.intent.generation.is_none()));
        assert!(jobs.iter().all(|job| job.intent.room_sid.is_none()));
    }

    /// #1445: a relay failure is not a membership decision. Routing it
    /// through the SFU token-denial counter classified it as
    /// `membership_check_failed`, which would make a clustering
    /// incident read as a permissions problem on the very dashboards
    /// used to diagnose it. It must land in its own bucket, and must
    /// not touch `sfu_token.denied` at all — the gate never ran.
    #[tokio::test(flavor = "current_thread")]
    async fn definite_relay_failure_is_attributed_to_the_owner_not_the_membership_gate() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state_with_calls().await;
        let alice: jid::FullJid = "alice@example.com/web".parse().unwrap();
        let room: jid::BareJid = "general@muc.example.com".parse().unwrap();
        let mut carbons = false;
        let mut roster = false;
        let mut blocklist = false;
        let conn_state = super::IqConnState {
            carbons_enabled: &mut carbons,
            roster_interested: &mut roster,
            blocklist_interested: &mut blocklist,
            registry_owner: None,
            state_machine: None,
            ordered_relay_origin: None,
        };

        // An INITIATE (not a terminate — those fall back silently) for
        // a room with no relay substrate: a definite, terminal failure.
        let outcome = super::relay_muji_to_room_owner(
            &state,
            &conn_state,
            &alice,
            &muji_initiate_iq("general@muc.example.com"),
            &room,
            super::IqReplyAddressing {
                id: "init-1",
                response_from: Some("calls.example.com"),
                response_to: Some("alice@example.com/web"),
            },
        )
        .await;
        assert!(matches!(outcome, super::MujiRelayOutcome::Frames(_)));

        assert_eq!(
            metrics
                .counter_sum(
                    "waddle.call.setup.failed",
                    &[("reason", "membership_check_failed")]
                )
                .unwrap_or(0),
            0,
            "a relay failure must not be blamed on the membership check"
        );
    }

    /// #1445: a cross-node hangup falls back to local execution when
    /// it cannot be relayed, and that fallback must be SILENT. The
    /// error builders it bypasses carry telemetry side effects — a
    /// token-denial counter, a call-setup failure, a WARN naming the
    /// room and user — and a hangup is neither a token denial nor a
    /// call-setup attempt. Evaluating them eagerly (rather than as
    /// closures behind the initiate branch) fabricated a
    /// `room_not_found` denial on every cross-node teardown, including
    /// every hangup for a non-local room on a single-node deployment.
    #[tokio::test(flavor = "current_thread")]
    async fn unrelayable_terminate_records_no_denial_telemetry() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state_with_calls().await;
        let alice: jid::FullJid = "alice@example.com/web".parse().unwrap();
        let room: jid::BareJid = "general@muc.example.com".parse().unwrap();
        let mut carbons = false;
        let mut roster = false;
        let mut blocklist = false;
        let conn_state = super::IqConnState {
            carbons_enabled: &mut carbons,
            roster_interested: &mut roster,
            blocklist_interested: &mut blocklist,
            registry_owner: None,
            state_machine: None,
            ordered_relay_origin: None,
        };

        let outcome = super::relay_muji_to_room_owner(
            &state,
            &conn_state,
            &alice,
            &muji_terminate_iq("general@muc.example.com"),
            &room,
            super::IqReplyAddressing {
                id: "term-1",
                response_from: Some("calls.example.com"),
                response_to: Some("alice@example.com/web"),
            },
        )
        .await;

        assert!(
            matches!(outcome, super::MujiRelayOutcome::ProcessLocally { .. }),
            "an unrelayable terminate must fall back to local execution"
        );
        assert_eq!(
            metrics
                .counter_sum(
                    "waddle.call.sfu_token.denied",
                    &[("reason", "room_not_found")]
                )
                .unwrap_or(0),
            0,
            "a hangup must not record a token denial"
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.attempted", &[])
                .unwrap_or(0),
            0,
            "a hangup is not a call-setup attempt"
        );
    }

    #[tokio::test]
    async fn blocked_direct_jingle_initiate_returns_service_unavailable_without_mint_or_register() {
        let sfu = Arc::new(RecordingCallSfu::default());
        let state = create_test_websocket_state_with_sfu(sfu.clone()).await;
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob: FullJid = "bob@example.com/phone".parse().expect("bob jid");
        let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone())
            .add_blocks(&bob.to_bare(), &[Jid::from(alice.clone())])
            .await
            .expect("seed blocklist");

        let responses = super::super::handle_iq(
            &direct_jingle_frame(
                "blocked-call-1",
                "bob@example.com/phone",
                "session-initiate",
            ),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&alice),
        )
        .await;

        assert_eq!(responses.len(), 1, "blocked call gets one error reply");
        let response = &responses[0];
        let expected = crate::server::routes::interpret::undeliverable_iq_reply(&Stanza::Iq(
            Box::new(parse_iq(&direct_jingle_frame(
                "blocked-call-1",
                "bob@example.com/phone",
                "session-initiate",
            ))),
        ))
        .map(|stanza| super::stanza_to_xml(&stanza))
        .expect("blocked direct Jingle must produce a bounced IQ reply");
        assert_eq!(
            response, &expected,
            "blocked direct Jingle must reuse the undeliverable reply builder exactly"
        );
        assert!(
            response.contains("<service-unavailable")
                && response.contains("<jingle xmlns='urn:xmpp:jingle:1'"),
            "blocked direct call must carry the sanitized undeliverable Jingle echo: {response}"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "blocked direct call must not reach the peer connection"
        );
        assert!(
            sfu.issued_snapshot().is_empty(),
            "blocked direct call must not mint a LiveKit token"
        );
        assert!(
            sfu.registered_snapshot().is_empty(),
            "blocked direct call must not register a participant"
        );
    }

    #[tokio::test]
    async fn blocklist_storage_failure_returns_internal_server_error_before_dispatch() {
        let sfu = Arc::new(RecordingCallSfu::default());
        let state = create_test_websocket_state_with_sfu(sfu.clone()).await;
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob: FullJid = "bob@example.com/phone".parse().expect("bob jid");
        let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbExecute {
                sql: "DROP TABLE blocking_list".to_string(),
                params: Vec::new(),
            })
            .await
            .expect("drop blocking table");

        let responses = super::super::handle_iq(
            &direct_jingle_frame(
                "blocked-call-2",
                "bob@example.com/phone",
                "session-initiate",
            ),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&alice),
        )
        .await;

        assert_eq!(responses.len(), 1, "storage failure gets one error reply");
        let response = &responses[0];
        assert!(
            response.contains("type='error'") && response.contains("<internal-server-error"),
            "blocklist failures must fail closed with internal-server-error: {response}"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "fail-closed blocklist error must not dispatch to the peer connection"
        );
        assert!(
            sfu.issued_snapshot().is_empty(),
            "fail-closed blocklist error must not mint a LiveKit token"
        );
        assert!(
            sfu.registered_snapshot().is_empty(),
            "fail-closed blocklist error must not register a participant"
        );
    }

    #[tokio::test]
    async fn peer_jingle_blocklist_helper_blocks_session_accept_only_for_local_non_muji_targets() {
        let state = create_test_websocket_state_with_sfu(Arc::new(RecordingSfu::default())).await;
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let bob: FullJid = "bob@example.com/phone".parse().expect("bob jid");
        DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone())
            .add_blocks(&bob.to_bare(), &[Jid::from(alice.clone())])
            .await
            .expect("seed blocklist");

        let blocked_accept = parse_iq(&direct_jingle_frame(
            "blocked-call-3",
            "bob@example.com/phone",
            "session-accept",
        ));
        let blocked_reply = super::peer_jingle_blocklist_reply(
            state.as_ref(),
            &alice,
            &blocked_accept,
            "example.com",
        )
        .await;
        let Some(super::PeerJingleBlocklistReply::Bounce(stanza)) = blocked_reply else {
            panic!("session-accept to a blocked local full JID must bounce via the shared helper");
        };
        let Stanza::Iq(reply) = stanza else {
            panic!("expected IQ bounce");
        };
        let xmpp_parsers::iq::Iq::Error { error, payload, .. } = reply.as_ref() else {
            panic!("expected IQ error bounce");
        };
        assert_eq!(
            error.defined_condition,
            xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable
        );
        assert!(
            payload
                .as_ref()
                .is_some_and(|payload| payload.is("jingle", waddle_xmpp::xep::xep0166::NS_JINGLE)),
            "shared bounce must echo the sanitized Jingle payload"
        );

        let muji_accept = parse_iq(&muji_jingle_frame(
            "blocked-call-4",
            "bob@example.com/phone",
            "session-accept",
            "general@muc.example.com",
        ));
        assert!(
            super::peer_jingle_blocklist_reply(state.as_ref(), &alice, &muji_accept, "example.com")
                .await
                .is_none(),
            "Muji Jingle is exempt from the direct-peer blocklist gate"
        );

        let remote_accept = parse_iq(&direct_jingle_frame(
            "blocked-call-5",
            "bob@remote.example/phone",
            "session-accept",
        ));
        assert!(
            super::peer_jingle_blocklist_reply(
                state.as_ref(),
                &alice,
                &remote_accept,
                "example.com"
            )
            .await
            .is_none(),
            "non-local targets are exempt from the local pre-dispatch gate"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn muji_terminate_rate_limit_fires_before_gate_records_leave() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state_with_calls().await;
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let max_terminates =
            waddle_xmpp::protocol::handlers::session_initiate_rate_limit::DEFAULT_MAX_TERMINATES;

        for attempt in 0..=max_terminates {
            let frame = muji_jingle_frame(
                &format!("term-pre-gate-{attempt}"),
                "calls.example.com",
                "session-terminate",
                "ghost-room@muc.example.com",
            );
            let responses = super::super::handle_iq(
                &frame,
                "example.com",
                "muc.example.com",
                state.as_ref(),
                &None,
                &ready_phase(&alice),
            )
            .await;
            if attempt == max_terminates {
                assert_eq!(responses.len(), 1);
                assert!(
                    responses[0].contains("<policy-violation"),
                    "over-budget terminate must be rejected before the gate: {}",
                    responses[0]
                );
            }
        }

        assert_eq!(
            metrics.counter_sum("waddle.call.signaling", &[("event", "muji_leave")]),
            Some(max_terminates as u64),
            "the over-budget terminate must not reach the Muji gate's leave counter"
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.control.rate_limited",
                &[("surface", "terminate")]
            ),
            Some(1),
            "the websocket pre-gate limiter must report the terminate drop"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn muji_non_initiate_rate_limit_fires_before_room_relay() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state_with_calls().await;
        let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
        let max_actions =
            waddle_xmpp::protocol::handlers::session_initiate_rate_limit::DEFAULT_MAX_MUJI_ACTIONS;

        for attempt in 0..=max_actions {
            let frame = muji_jingle_frame(
                &format!("muji-action-{attempt}"),
                "calls.example.com",
                "transport-info",
                "ghost-room@muc.example.com",
            );
            let responses = super::super::handle_iq(
                &frame,
                "example.com",
                "muc.example.com",
                state.as_ref(),
                &None,
                &ready_phase(&alice),
            )
            .await;
            assert_eq!(responses.len(), 1);
            if attempt == 0 {
                assert!(
                    responses[0].contains("<forbidden"),
                    "under-budget foreign-room Muji action should still hit the room-locality path"
                );
            }
            if attempt == max_actions {
                assert!(
                    responses[0].contains("<policy-violation"),
                    "over-budget Muji action must be rejected before room relay/membership work: {}",
                    responses[0]
                );
            }
        }

        assert_eq!(
            metrics.counter_sum(
                "waddle.call.control.rate_limited",
                &[("surface", "muji_action")]
            ),
            Some(1),
            "the websocket pre-gate limiter must report the Muji-action drop"
        );
    }
}
