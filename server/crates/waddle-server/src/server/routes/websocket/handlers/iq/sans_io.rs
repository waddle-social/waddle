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
                        MujiRelayOutcome::ProcessLocally => {}
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
        };
        let outcome = crate::server::routes::interpret::interpret(events, &deps).await;
        if let Some(room_jid) = muji_clear_after {
            crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
                state, &room_jid, full_jid,
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

/// What the caller should do after a relay attempt.
#[cfg(feature = "clustering")]
enum MujiRelayOutcome {
    /// Terminal: send these frames to the client.
    Frames(Vec<String>),
    /// Non-terminal: handle the IQ on this node after all. Only ever
    /// returned for `session-terminate`, whose local execution is an
    /// idempotent no-op and therefore a better answer than an error.
    ProcessLocally,
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
    let unrelayable = |frames: &dyn Fn() -> Vec<String>| {
        if is_terminate {
            MujiRelayOutcome::ProcessLocally
        } else {
            MujiRelayOutcome::Frames(frames())
        }
    };

    let deny = || {
        vec![build_iq_error_xml_typed(
            reply.id,
            reply.response_from,
            reply.response_to,
            *super::jingle_muji_gate::deny_room_not_found(room_jid, &full_jid.to_bare()),
        )]
    };
    let retry_later = || {
        // `record_sfu_token_denial` already records the call-setup
        // attempted/failed pair via `setup_failure_reason`, so this
        // must NOT also call `record_call_setup_rejected` — doing both
        // double-counted every relay failure in exactly the #1452 SLI
        // this path exists to feed.
        super::super::super::call_signaling_telemetry::record_sfu_token_denial(
            room_jid,
            &full_jid.to_bare(),
            waddle_xmpp::telemetry::attributes::SfuDenialReason::InternalError,
        );
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

    // The relay's envelope validation requires a bare `to` naming the
    // calls mixer; anything else is NACKed as a parse failure by the
    // receiver, which diverts the shared ordered channel and would
    // silently break this sender's ordinary MUC traffic for the room.
    // A client controls `to`, so reject the shape here rather than
    // letting a malformed (or hostile) one reach the relay at all.
    // Same for a room outside this server's MUC domain: it can have no
    // local claim, so relaying it only buys a claim-store lookup.
    let addressed_to_mixer = iq.to().is_some_and(|to| {
        to.resource().is_none()
            && to.to_bare()
                == waddle_xmpp::protocol::handlers::jingle::calls_mixer_jid(
                    state.deps.auth_state.xmpp_domain.as_str(),
                )
    });
    let room_is_local =
        room_jid.domain().as_str() == format!("muc.{}", state.deps.auth_state.xmpp_domain.as_str());
    if !addressed_to_mixer || !room_is_local {
        return unrelayable(&deny);
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
        return unrelayable(&deny);
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
        MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Delivered(_)) => {
            unrelayable(&retry_later)
        }
        MucProxyRouteDecision::RoomUnclaimed | MucProxyRouteDecision::LocalRoom => {
            unrelayable(&deny)
        }
        MucProxyRouteDecision::Attempted(
            OrderedRelayMucProxyOutcome::Unavailable
            | OrderedRelayMucProxyOutcome::Dropped
            | OrderedRelayMucProxyOutcome::MaybeCommitted
            | OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
        )
        | MucProxyRouteDecision::RoomClaimUnavailable
        | MucProxyRouteDecision::OriginUnavailable => unrelayable(&retry_later),
    }
}

/// Non-clustering builds have no other replica that could own the
/// room, so local absence is definitive — the terminal denial,
/// byte-identical to the pre-#1445 wire behavior.
#[cfg(not(feature = "clustering"))]
enum MujiRelayOutcome {
    Frames(Vec<String>),
    ProcessLocally,
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
        return MujiRelayOutcome::ProcessLocally;
    }
    MujiRelayOutcome::Frames(vec![build_iq_error_xml_typed(
        reply.id,
        reply.response_from,
        reply.response_to,
        *super::jingle_muji_gate::deny_room_not_found(room_jid, &full_jid.to_bare()),
    )])
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
    use crate::server::routes::websocket::tests::create_test_websocket_state_with_calls;
    use waddle_xmpp::xep::xep0167::MediaKind;
    use waddle_xmpp::xep::xep0272::{Creator, Muji, MujiContent};
    use xmpp_parsers::iq::Iq;
    use xmpp_parsers::jingle::{Action, Jingle, SessionId};

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
            matches!(outcome, super::MujiRelayOutcome::ProcessLocally),
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
}
