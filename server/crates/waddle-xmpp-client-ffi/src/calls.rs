use minidom::Element;
use waddle_xmpp_client::messaging::{
    build_finish, build_finish_migrated, build_finish_with_reason, build_proceed, build_propose,
    build_reject, build_reject_with_options, build_retract, build_retract_with_options,
    build_ringing, build_session_accept, build_session_initiate, build_session_terminate,
    CallMedia, JingleReason, SessionId,
};
use waddle_xmpp_client::xep::xep0215::{
    build_extdisco_services_iq, parse_external_services, ExternalService, ExternalServiceTransport,
    ExternalServiceType,
};
use waddle_xmpp_client::{ClientError, StanzaError};

use crate::boundary_convert::jid_domain;
use crate::convert::jingle_reason_from_ffi;
use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::stanza::{iq_set, message_with_jmi};
use crate::{
    WaddleCallSessionTerminateOutcome, WaddleClient, WaddleError, WaddleExternalService,
    WaddleExternalServiceTransport, WaddleExternalServiceType, WaddleJingleReason,
};

// ── Pure factories and classifiers ───────────────────────────────────────────
//
// The exported verbs below delegate their wire shapes to these pure
// functions so the dedicated XEP test suite (`calls_tests.rs`) can pin
// the exact stanzas the FFI emits without a live connection — same
// pattern as `messaging_verbs.rs`.

/// Build the XEP-0166 §7.4 session-terminate `<iq type='set'>` shared
/// by `send_call_session_terminate` and its outcome-reporting twin, so
/// the two can never drift apart on the wire.
pub(crate) fn session_terminate_iq(
    peer: &jid::FullJid,
    sid: &SessionId,
    reason: Option<JingleReason>,
) -> Element {
    iq_set(&peer.clone().into(), build_session_terminate(sid, reason))
}

/// The Waddle server's Jingle handler answers a `session-terminate`
/// for a call it no longer tracks with `forbidden` plus this exact
/// text (see `waddle_xmpp::protocol::handlers::jingle`). Ported from
/// the wasm client's `is_orphaned_dm_call_terminate` so recovered /
/// stale-call cleanup can still send the JMI `<finish/>` bookend.
pub(crate) fn is_orphaned_dm_call_terminate(err: &StanzaError) -> bool {
    err.condition == "forbidden"
        && err.text.as_deref() == Some("Jingle terminator is not a participant in this call")
}

/// Collapse a terminate IQ round-trip into the typed three-way
/// outcome. Only the classified orphaned stanza error maps to
/// `Orphaned`; every other failure is `Error`.
pub(crate) fn classify_terminate_result(
    result: &Result<Element, ClientError>,
) -> WaddleCallSessionTerminateOutcome {
    match result {
        Ok(_) => WaddleCallSessionTerminateOutcome::Ok,
        Err(ClientError::StanzaError(err)) if is_orphaned_dm_call_terminate(err) => {
            WaddleCallSessionTerminateOutcome::Orphaned
        }
        Err(_) => WaddleCallSessionTerminateOutcome::Error,
    }
}

/// Defense-in-depth for the XEP-0215 fetch (RFC 6120 §8.1.2.1): if the
/// reply stamps a `from`, it MUST be the queried server domain. An
/// absent `from` is acceptable for a reply from one's own server — the
/// IQ id correlation already pins the response. Mirrors the wasm
/// client's guard byte for byte.
pub(crate) fn extdisco_reply_from_own_server(reply: &Element, server_domain: &str) -> bool {
    match reply.attr("from") {
        Some(from) => from.eq_ignore_ascii_case(server_domain),
        None => true,
    }
}

/// Map a typed core [`ExternalService`] to the FFI record. `expires`
/// crosses the boundary as an RFC 3339 string, the same transit shape
/// the wasm client hands to JavaScript.
pub(crate) fn external_service_to_ffi(service: ExternalService) -> WaddleExternalService {
    WaddleExternalService {
        service_type: match service.service_type {
            ExternalServiceType::Stun => WaddleExternalServiceType::Stun,
            ExternalServiceType::Turn => WaddleExternalServiceType::Turn,
            ExternalServiceType::Turns => WaddleExternalServiceType::Turns,
        },
        host: service.host,
        port: service.port,
        transport: service.transport.map(|transport| match transport {
            ExternalServiceTransport::Udp => WaddleExternalServiceTransport::Udp,
            ExternalServiceTransport::Tcp => WaddleExternalServiceTransport::Tcp,
        }),
        username: service.username,
        password: service.password,
        expires: service.expires.map(|expires| expires.to_rfc3339()),
        restricted: service.restricted,
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// Send a XEP-0353 §5.1.1 `<propose/>` to the peer's bare JID.
    /// The bare JID lets the responder's server ring every connected
    /// resource until one of them proceeds or rejects.
    pub async fn send_call_propose(
        &self,
        peer_bare_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> bool {
        let Some(peer) = self.parse_bare_jid(&peer_bare_jid, "send_call_propose") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_propose") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_propose(&sid, CallMedia { audio, video }),
        );
        self.send_stanza_or_error(stanza, "send_call_propose").await
    }

    /// Send a XEP-0353 §5.1.2 `<proceed/>` to the *full* JID of the
    /// originator (preserved from the propose `from` per §0.6).
    pub async fn send_call_proceed(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_proceed") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_proceed") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_proceed(&sid));
        self.send_stanza_or_error(stanza, "send_call_proceed").await
    }

    /// Send a XEP-0353 `<ringing/>` to the caller's *bare* JID so the
    /// initiator's server fans the responder's device-ring state out
    /// to every caller resource (XEP-0353 §3.2, "Intermediate State:
    /// Device Rings"). Emitted when an inbound `<propose/>` lands and
    /// the ringing UI starts.
    pub async fn send_call_ringing(&self, peer_bare_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_bare_jid(&peer_bare_jid, "send_call_ringing") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_ringing") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_ringing(&sid));
        self.send_stanza_or_error(stanza, "send_call_ringing").await
    }

    /// Send a XEP-0353 §5.1.3 `<reject/>` to the originator's full JID.
    pub async fn send_call_reject(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_reject") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_reject") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_reject(&sid));
        self.send_stanza_or_error(stanza, "send_call_reject").await
    }

    /// Send a XEP-0353 tie-break `<reject/>` carrying
    /// `<reason><expired/></reason>` plus `<tie-break/>`.
    pub async fn send_call_reject_tie_break(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_reject_tie_break") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_reject_tie_break") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_reject_with_options(&sid, Some(JingleReason::Expired), true),
        );
        self.send_stanza_or_error(stanza, "send_call_reject_tie_break")
            .await
    }

    /// Send a XEP-0353 §5.1.4 `<retract/>` to cancel a ringing call
    /// before the peer answers. Addressed to the responder's *bare*
    /// JID so every resource that may have been ringing receives the
    /// cancellation (XEP-0353 §5.1.4: a retract is addressed to the
    /// callee's bare JID, exactly like the originating propose).
    pub async fn send_call_retract(&self, peer_bare_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_bare_jid(&peer_bare_jid, "send_call_retract") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_retract") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_retract(&sid));
        self.send_stanza_or_error(stanza, "send_call_retract").await
    }

    /// Send a XEP-0353 tie-break `<retract/>` carrying
    /// `<reason><expired/></reason>` plus `<tie-break/>`.
    pub async fn send_call_retract_tie_break(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_retract_tie_break") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_retract_tie_break") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_retract_with_options(&sid, Some(JingleReason::Expired), true),
        );
        self.send_stanza_or_error(stanza, "send_call_retract_tie_break")
            .await
    }

    /// Send a `<finish/>` Waddle JMI extension signaling clean
    /// teardown after a call ended. Addressed to the peer's full JID
    /// so the originating resource sees the finish notice.
    pub async fn send_call_finish(&self, peer_full_jid: String, sid: String) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_finish") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_finish") else {
            return false;
        };
        let stanza = message_with_jmi(&peer.into(), build_finish(&sid));
        self.send_stanza_or_error(stanza, "send_call_finish").await
    }

    /// Send a `<finish/>` carrying an explicit `<reason/>` — the
    /// XEP-0353-conformant way for a responder to abandon a session it
    /// already answered with `<proceed/>` (a `<reject/>` after proceed
    /// is a contradictory double answer; cf. the tie-break-2 example,
    /// where an accepted-but-incomplete session finishes with
    /// `<expired/>`). Addressed to the peer's full JID.
    pub async fn send_call_finish_with_reason(
        &self,
        peer_full_jid: String,
        sid: String,
        reason: WaddleJingleReason,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_finish_with_reason") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_finish_with_reason") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_finish_with_reason(&sid, Some(jingle_reason_from_ffi(reason))),
        );
        self.send_stanza_or_error(stanza, "send_call_finish_with_reason")
            .await
    }

    /// Send Waddle's XEP-0353-compatible migration marker:
    /// `<finish/>` with `<reason><expired/></reason>` and
    /// `<migrated to='new-sid'/>`.
    pub async fn send_call_finish_migrated(
        &self,
        peer_full_jid: String,
        old_sid: String,
        new_sid: String,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_finish_migrated") else {
            return false;
        };
        let Some(old_sid) = self.parse_session_id(old_sid, "send_call_finish_migrated") else {
            return false;
        };
        let Some(new_sid) = self.parse_session_id(new_sid, "send_call_finish_migrated") else {
            return false;
        };
        let stanza = message_with_jmi(
            &peer.into(),
            build_finish_migrated(&old_sid, JingleReason::Expired, &new_sid),
        );
        self.send_stanza_or_error(stanza, "send_call_finish_migrated")
            .await
    }

    /// Send a XEP-0166 §6.4 `session-initiate` IQ to the peer's full
    /// JID. `initiator_full_jid` names the call originator per §7.1.
    pub async fn send_call_session_initiate(
        &self,
        peer_full_jid: String,
        initiator_full_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_session_initiate") else {
            return false;
        };
        let Some(initiator) =
            self.parse_full_jid(&initiator_full_jid, "send_call_session_initiate")
        else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_session_initiate") else {
            return false;
        };
        let payload = build_session_initiate(&sid, &initiator, CallMedia { audio, video });
        let iq = iq_set(&peer.into(), payload);
        self.send_iq_or_error(iq, "send_call_session_initiate")
            .await
    }

    /// Send a XEP-0166 §7.2 `session-accept` IQ.
    pub async fn send_call_session_accept(
        &self,
        peer_full_jid: String,
        responder_full_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_session_accept") else {
            return false;
        };
        let Some(responder) = self.parse_full_jid(&responder_full_jid, "send_call_session_accept")
        else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_session_accept") else {
            return false;
        };
        let payload = build_session_accept(&sid, &responder, CallMedia { audio, video });
        let iq = iq_set(&peer.into(), payload);
        self.send_iq_or_error(iq, "send_call_session_accept").await
    }

    /// Send a XEP-0166 §7.4 `session-terminate` IQ.
    pub async fn send_call_session_terminate(
        &self,
        peer_full_jid: String,
        sid: String,
        reason: Option<WaddleJingleReason>,
    ) -> bool {
        let Some(peer) = self.parse_full_jid(&peer_full_jid, "send_call_session_terminate") else {
            return false;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_session_terminate") else {
            return false;
        };
        let iq = session_terminate_iq(&peer, &sid, reason.map(jingle_reason_from_ffi));
        self.send_iq_or_error(iq, "send_call_session_terminate")
            .await
    }

    /// Send a XEP-0166 §7.4 `session-terminate` IQ and report the
    /// typed outcome instead of a bare bool. `Orphaned` classifies the
    /// Waddle server's `forbidden` + "Jingle terminator is not a
    /// participant in this call" stanza error — the call registry
    /// entry is already gone, so the caller should still send the
    /// XEP-0353 `<finish/>` bookend that keeps both MAM archives
    /// consistent (wasm `send_call_session_terminate_with_outcome`
    /// parity).
    pub async fn send_call_session_terminate_with_outcome(
        &self,
        peer_full_jid: String,
        sid: String,
        reason: Option<WaddleJingleReason>,
    ) -> WaddleCallSessionTerminateOutcome {
        let Some(peer) =
            self.parse_full_jid(&peer_full_jid, "send_call_session_terminate_with_outcome")
        else {
            return WaddleCallSessionTerminateOutcome::Error;
        };
        let Some(sid) = self.parse_session_id(sid, "send_call_session_terminate_with_outcome")
        else {
            return WaddleCallSessionTerminateOutcome::Error;
        };
        let iq = session_terminate_iq(&peer, &sid, reason.map(jingle_reason_from_ffi));
        let Some(handle) = self.clone_handle().await else {
            return WaddleCallSessionTerminateOutcome::Error;
        };
        let result = send_iq_with_timeout(&handle, iq).await;
        let outcome = classify_terminate_result(&result);
        if outcome == WaddleCallSessionTerminateOutcome::Error {
            if let Err(e) = result {
                self.emit_error(format!(
                    "send_call_session_terminate_with_outcome failed: {e}"
                ));
            }
        }
        outcome
    }

    /// XEP-0215 §3.2: fetch the external services (TURN/STUN) the
    /// user's own server advertises, as typed entries the app maps to
    /// WebRTC ICE servers for LiveKit's RTC configuration at connect
    /// time. The query is addressed to the authenticated user's server
    /// domain; an empty `<services/>` requests every advertised
    /// service type.
    pub async fn fetch_external_services(&self) -> Result<Vec<WaddleExternalService>, WaddleError> {
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let server_domain = jid_domain(&self.config.jid).to_string();
        let iq = build_extdisco_services_iq(&server_domain, &uuid::Uuid::new_v4().to_string());
        match send_iq_with_timeout(&handle, iq).await {
            Ok(reply) => {
                if !extdisco_reply_from_own_server(&reply, &server_domain) {
                    return Err(WaddleError::UntrustedReply);
                }
                Ok(parse_external_services(&reply)
                    .into_iter()
                    .map(external_service_to_ffi)
                    .collect())
            }
            Err(e) => Err(client_error_to_waddle(&e)),
        }
    }
}
