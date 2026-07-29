use super::*;
use waddle_xmpp_client::messaging::{
    build_finish, build_finish_migrated, build_muji_session_initiate, build_muji_session_terminate,
    build_proceed, build_propose, build_reject, build_reject_with_options, build_retract,
    build_retract_with_options, build_ringing, build_session_accept, build_session_initiate,
    build_session_terminate, jingle_reason_from_wire_name, wrap_jmi_message, CallMedia,
    JingleReason, SessionId,
};

#[cfg(test)]
const NS_JINGLE: &str = "urn:xmpp:jingle:1";
#[cfg(test)]
const NS_JINGLE_MESSAGE: &str = "urn:xmpp:jingle-message:0";
#[cfg(test)]
const NS_MUJI: &str = "urn:xmpp:jingle:muji:0";
const NS_JABBER_CLIENT: &str = "jabber:client";

/// Compute the SFU mixer JID for a server domain. Matches the
/// server-side helper at
/// `waddle_xmpp::protocol::handlers::jingle::calls_mixer_jid`.
fn calls_mixer_jid(server_domain: &str) -> String {
    format!("calls.{server_domain}")
}

// Inbound Muji session-accept stanzas (XEP-0166 §6.3 separate
// IQ-set) are parsed by `waddle_xmpp_client::messaging::parse_jingle_iq`
// via the `events.rs::UnhandledStanza` → `on_call` dispatcher and
// surfaced to the chat-side as a typed `CallEventKind::SessionAccept`.
// There is no per-wasm Muji-specific accept parser; the LiveKit join
// extraction is centralised in `waddle-xmpp-client/src/messaging/call.rs`.

/// Wrap a JMI body in the XEP-0353 §3-conformant `<message type='chat'>`
/// envelope with a XEP-0334 `<store/>` hint. Routes through
/// [`waddle_xmpp_client::messaging::wrap_jmi_message`] so the wasm and
/// native clients ship byte-identical envelopes; the only wasm-local
/// concern is parsing the JS `String` `to` into a typed
/// [`jid::BareJid`] / [`jid::FullJid`] at the boundary.
fn message_with_jmi(to: jid::Jid, jmi: Element) -> Element {
    wrap_jmi_message(&to, jmi)
}

fn message_with_jmi_to_bare(to: &str, jmi: Element) -> Result<Element, JsValue> {
    let bare: jid::BareJid = to
        .parse()
        .map_err(|_| js_error(format!("invalid bare JID for JMI envelope: {to}")))?;
    Ok(message_with_jmi(bare.into(), jmi))
}

fn message_with_jmi_to_full(to: &str, jmi: Element) -> Result<Element, JsValue> {
    let full = parse_full_jid(to)?;
    Ok(message_with_jmi(full.into(), jmi))
}

fn iq_set(to: &str, payload: Element) -> Element {
    Element::builder("iq", NS_JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            uuid::Uuid::new_v4().to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .append(payload)
        .build()
}

fn media_from_flags(audio: bool, video: bool) -> CallMedia {
    CallMedia { audio, video }
}

fn parse_full_jid(s: &str) -> Result<jid::FullJid, JsValue> {
    s.parse()
        .map_err(|_| js_error(format!("invalid full JID: {s}")))
}

/// Wrap a `String` from the JS boundary in the typed `SessionId`.
/// xmpp-parsers' `SessionId` is just a `String` newtype — the
/// conversion is infallible — but threading the typed value
/// through the builders is what the typed-payloads rule asks for.
fn sid(value: String) -> SessionId {
    SessionId(value)
}

fn stored_full_jid(
    inner: &std::rc::Rc<std::cell::RefCell<WaddleClientInner>>,
) -> Result<jid::FullJid, JsValue> {
    let stored = inner.borrow().config.clone();
    format!("{}/{}", stored.jid, stored.resource)
        .parse()
        .map_err(|_| js_error("authenticated JID/resource do not form a full JID"))
}

#[wasm_bindgen]
impl WaddleClient {
    /// Send a JMI `<propose/>` to the peer's bare JID (XEP-0353
    /// §5.1.1). The bare JID lets the responder's server ring every
    /// connected resource until one of them proceeds/rejects.
    pub fn send_call_propose(
        &self,
        peer_bare_jid: String,
        sid_str: String,
        audio: bool,
        video: bool,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi_to_bare(
                &peer_bare_jid,
                build_propose(&sid(sid_str), media_from_flags(audio, video)),
            )?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_proceed(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi_to_full(&peer_full_jid, build_proceed(&sid(sid_str)))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Send a JMI `<ringing/>` to the caller's bare JID (XEP-0353
    /// §3.2). The bare JID lets the initiator's server fan out the
    /// responder's device-ring state to every caller resource.
    pub fn send_call_ringing(&self, peer_bare_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi_to_bare(&peer_bare_jid, build_ringing(&sid(sid_str)))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_reject(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi_to_full(&peer_full_jid, build_reject(&sid(sid_str)))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_reject_tie_break(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi_to_full(
                &peer_full_jid,
                build_reject_with_options(&sid(sid_str), Some(JingleReason::Expired), true),
            )?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Send a JMI `<retract/>` to the peer's bare JID (XEP-0353 §3).
    /// Like `<propose/>`, retract is addressed to the BARE JID so the
    /// responder's server can fan the disavowal out to every resource
    /// that may have seen the original ring. Typing the parameter as a
    /// bare JID (and routing through `message_with_jmi_to_bare`, which
    /// rejects a resource-bearing JID) enforces that on the wire — the
    /// resource-targeted variant is `send_call_retract_tie_break`.
    pub fn send_call_retract(&self, peer_bare_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi_to_bare(&peer_bare_jid, build_retract(&sid(sid_str)))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_retract_tie_break(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi_to_full(
                &peer_full_jid,
                build_retract_with_options(&sid(sid_str), Some(JingleReason::Expired), true),
            )?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_finish(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi_to_full(&peer_full_jid, build_finish(&sid(sid_str)))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_finish_migrated(
        &self,
        peer_full_jid: String,
        old_sid_str: String,
        new_sid_str: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let new_sid = sid(new_sid_str);
            let stanza = message_with_jmi_to_full(
                &peer_full_jid,
                build_finish_migrated(&sid(old_sid_str), JingleReason::Expired, &new_sid),
            )?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Send a Jingle `session-initiate` IQ to the peer's full JID
    /// (XEP-0166 §6.4). The `initiator` attribute names the call
    /// originator; the server's Jingle handler additionally
    /// validates the authenticated session matches it.
    pub fn send_call_session_initiate(
        &self,
        peer_full_jid: String,
        initiator_full_jid: String,
        sid_str: String,
        audio: bool,
        video: bool,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let initiator = parse_full_jid(&initiator_full_jid)?;
            // Validate peer_full_jid at the wasm boundary so a bare or
            // malformed JID surfaces as a clear JsError instead of
            // silently shipping an invalid stanza that the server's
            // Jingle handler then rejects with a generic bad-request.
            let _ = parse_full_jid(&peer_full_jid)?;
            let stanza = iq_set(
                &peer_full_jid,
                build_session_initiate(&sid(sid_str), &initiator, media_from_flags(audio, video)),
            );
            send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Send a Jingle `session-accept` IQ in response to a received
    /// session-initiate. `responder` is validated as a full JID at
    /// the wasm boundary so a malformed JID surfaces
    /// as a clear `JsError` rather than a wire-rejected stanza.
    pub fn send_call_session_accept(
        &self,
        peer_full_jid: String,
        responder_full_jid: String,
        sid_str: String,
        audio: bool,
        video: bool,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let responder = parse_full_jid(&responder_full_jid)?;
            let _ = parse_full_jid(&peer_full_jid)?;
            let stanza = iq_set(
                &peer_full_jid,
                build_session_accept(&sid(sid_str), &responder, media_from_flags(audio, video)),
            );
            send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Send a Jingle `session-terminate` IQ to hang up. `reason` is
    /// one of the XEP-0166 §7.4 condition names ("success",
    /// "decline", "cancel", "busy", "gone", …) parsed against
    /// `xmpp_parsers::jingle::Reason`; unknown values are rejected
    /// at the wasm boundary so a typo can't ship a malformed
    /// condition over the wire.
    /// Send a XEP-0272 Muji-bearing Jingle `session-initiate` IQ to
    /// the SFU mixer (`calls.<server-domain>`) to join the room's
    /// group call, and return the issued LiveKit credentials as a
    /// typed `{ url, room, identity, token }` object.
    ///
    /// Wire shape (XEP-0166 + XEP-0272 §Joining):
    ///
    /// ```xml
    /// <iq type='set' to='calls.<domain>' id='…'>
    ///   <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate'
    ///           sid='ATTEMPT_ID'>
    ///     <muji xmlns='urn:xmpp:jingle:muji:0' room='ROOM_JID'/>
    ///     <content creator='initiator' name='audio' senders='both'>
    ///       <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
    ///       <transport xmlns='urn:waddle:transports:livekit:0'/>
    ///     </content>
    ///     [<content creator='initiator' name='video'>…</content>]
    ///   </jingle>
    /// </iq>
    /// ```
    ///
    /// Convention: `sid` is a per-attempt correlation id while
    /// `<muji room='…'/>` remains the stable SFU room identity.
    /// This lets the chat UI ignore a stale accept from a cancelled
    /// same-room retry without changing XEP-0272 room semantics.
    ///
    /// `video` opt-in mirrors the call store's `media.video`
    /// flag — audio is always advertised (the call wouldn't be
    /// useful otherwise); video is included only when the user
    /// asked for it. LiveKit handles the actual codec selection
    /// once connected, so the descriptions are minimal.
    pub fn send_muji_session_initiate(
        &self,
        room_jid: String,
        sid_str: String,
        video: bool,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let initiator = stored_full_jid(&inner)?;
            let mixer = calls_mixer_jid(initiator.domain().as_str());

            let sid = sid(sid_str);
            let jingle = build_muji_session_initiate(&sid, &initiator, &room_jid, video);
            let stanza = Element::builder("iq", NS_JABBER_CLIENT)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    uuid::Uuid::new_v4().to_string(),
                )
                .attr(minidom::rxml::xml_ncname!("to").to_owned(), mixer.as_str())
                .append(jingle)
                .build();
            // XEP-0166 §6.3 conformance: the server replies with an
            // EMPTY IQ-result ack. The actual session-accept
            // arrives as a SEPARATE server-initiated
            // `<iq type='set'>` and surfaces to the chat-side via
            // the `on_call` callback as a typed
            // `CallEventKind::SessionAccept`. The chat layer
            // matches it to the pending session-initiate by the
            // per-attempt `sid` and resolves its own join Promise.
            send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// XEP-0215 §3.2: fetch the external services (TURN/STUN) the user's own
    /// server advertises, resolving to a typed array the chat maps to
    /// `RTCIceServer[]` for LiveKit's `rtcConfig` at connect time. The query is
    /// addressed to the authenticated user's server domain; an empty
    /// `<services/>` requests every advertised service type.
    pub fn fetch_external_services(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let server_domain = stored_full_jid(&inner)?.domain().to_string();
            let request_id = uuid::Uuid::new_v4().to_string();
            let iq = waddle_xmpp_client::xep::xep0215::build_extdisco_services_iq(
                &server_domain,
                &request_id,
            );
            let result = send_iq_command(inner, iq).await?;
            // Defense-in-depth (RFC 6120 §8.1.2.1): if the reply stamps a
            // `from`, it MUST be our own server. An absent `from` is acceptable
            // for a reply from one's own bare server — the IQ id correlation in
            // `send_iq_command` already pins the response.
            if let Some(from) = result.attr("from") {
                if !from.eq_ignore_ascii_case(&server_domain) {
                    return Err(js_error(format!(
                        "extdisco services reply `from` {from:?} does not match queried server {server_domain:?}"
                    )));
                }
            }
            let services: Vec<WaddleExternalService> =
                waddle_xmpp_client::xep::xep0215::parse_external_services(&result)
                    .into_iter()
                    .map(WaddleExternalService::from)
                    .collect();
            to_js_value(&services)
        })
    }

    /// Send a XEP-0272 Muji-bearing Jingle `session-terminate` IQ
    /// to the SFU mixer (`calls.<server-domain>`). Server
    /// unregisters the participant + revokes every jti it minted
    /// for `(room, identity)`. Resolves with no payload.
    ///
    /// Wire shape (XEP-0166 §6.7):
    ///
    /// ```xml
    /// <iq type='set' to='calls.<domain>' id='…'>
    ///   <jingle xmlns='urn:xmpp:jingle:1' action='session-terminate'
    ///           sid='PER_ATTEMPT_SID'>
    ///     <muji xmlns='urn:xmpp:jingle:muji:0' room='ROOM_JID'/>
    ///     <reason><success/></reason>
    ///   </jingle>
    /// </iq>
    /// ```
    /// `iq_id` is caller-supplied (and validated against the
    /// [`waddle_xmpp_client::request::StanzaId`] invariant) so the JS
    /// side can cancel the still-pending or deferred IQ via
    /// [`WaddleClient::cancel_raw_iq`] when its own teardown deadline
    /// expires — a stale room-scoped terminate must not linger in the
    /// driver after the room has been re-occupied (#1606 review).
    pub fn send_muji_session_terminate(
        &self,
        room_jid: String,
        sid_str: String,
        iq_id: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq_id = waddle_xmpp_client::request::StanzaId::new(iq_id)
                .map_err(|err| js_error(err.to_string()))?;
            let mixer = calls_mixer_jid(stored_full_jid(&inner)?.domain().as_str());

            let jingle = build_muji_session_terminate(&room_jid, &sid(sid_str));
            let stanza = Element::builder("iq", NS_JABBER_CLIENT)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    iq_id.as_str().to_string(),
                )
                .attr(minidom::rxml::xml_ncname!("to").to_owned(), mixer.as_str())
                .append(jingle)
                .build();
            send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_session_terminate(
        &self,
        peer_full_jid: String,
        sid_str: String,
        reason: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let typed_reason = match reason {
                Some(name) => Some(
                    jingle_reason_from_wire_name(&name)
                        .ok_or_else(|| js_error(format!("unknown jingle reason: {name}")))?,
                ),
                None => None,
            };
            let _ = parse_full_jid(&peer_full_jid)?;
            let stanza = iq_set(
                &peer_full_jid,
                build_session_terminate(&sid(sid_str), typed_reason),
            );
            send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_session_terminate_with_outcome(
        &self,
        peer_full_jid: String,
        sid_str: String,
        reason: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let typed_reason = match reason {
                Some(name) => Some(
                    jingle_reason_from_wire_name(&name)
                        .ok_or_else(|| js_error(format!("unknown jingle reason: {name}")))?,
                ),
                None => None,
            };
            let _ = parse_full_jid(&peer_full_jid)?;
            let stanza = iq_set(
                &peer_full_jid,
                build_session_terminate(&sid(sid_str), typed_reason),
            );
            let outcome = match send_iq_command_stanza_aware(inner, stanza).await? {
                Ok(_) => WaddleCallSessionTerminateOutcome::Ok,
                Err(err) if is_orphaned_dm_call_terminate(&err) => {
                    WaddleCallSessionTerminateOutcome::Orphaned
                }
                Err(_) => WaddleCallSessionTerminateOutcome::Error,
            };
            to_js_value(&outcome)
        })
    }
}

fn is_orphaned_dm_call_terminate(err: &waddle_xmpp_client::StanzaError) -> bool {
    err.condition == "forbidden"
        && err.text.as_deref() == Some("Jingle terminator is not a participant in this call")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::FullJid;
    use std::str::FromStr;

    // Muji session-accept parsing is exercised by the
    // `waddle-xmpp-client` crate's `messaging::call::parse_jingle_iq`
    // tests — the canonical parser the events.rs dispatcher uses.
    // The wasm-side per-Muji parser was removed once the wire
    // shape moved to the XEP-0166 §6.3 separate-IQ pattern.

    #[test]
    fn calls_mixer_jid_appends_localpart_to_domain() {
        assert_eq!(calls_mixer_jid("waddle.test"), "calls.waddle.test");
        assert_eq!(calls_mixer_jid("example.com"), "calls.example.com");
    }

    #[test]
    fn classifies_only_orphaned_dm_terminate_stanza_error() {
        let orphaned = waddle_xmpp_client::StanzaError {
            error_type: waddle_xmpp_client::StanzaErrorType::Auth,
            condition: "forbidden".to_string(),
            text: Some("Jingle terminator is not a participant in this call".to_string()),
            application_condition: None,
        };
        assert!(is_orphaned_dm_call_terminate(&orphaned));

        let generic_forbidden = waddle_xmpp_client::StanzaError {
            error_type: waddle_xmpp_client::StanzaErrorType::Auth,
            condition: "forbidden".to_string(),
            text: Some("Jingle responder was not invited to this call party".to_string()),
            application_condition: None,
        };
        assert!(!is_orphaned_dm_call_terminate(&generic_forbidden));

        let transportish = waddle_xmpp_client::StanzaError {
            error_type: waddle_xmpp_client::StanzaErrorType::Cancel,
            condition: "remote-server-timeout".to_string(),
            text: None,
            application_condition: None,
        };
        assert!(!is_orphaned_dm_call_terminate(&transportish));
    }

    /// `parse_full_jid` itself returns `Result<FullJid, JsValue>`,
    /// and `wasm_bindgen::JsValue` panics under `cargo test` on
    /// native targets (it's only safe on `wasm32`). The validation
    /// is a thin `s.parse::<FullJid>()` call though, so we test the
    /// underlying parse directly — same coverage, no JsValue
    /// instantiation on native.
    #[test]
    fn full_jid_parse_accepts_full_jid() {
        assert!(FullJid::from_str("alice@waddle.test/desktop").is_ok());
    }

    #[test]
    fn full_jid_parse_rejects_bare_jid() {
        // `send_call_session_initiate / accept / terminate` all
        // validate `peer_full_jid` via this parse at the wasm
        // boundary; a bare JID must be rejected before we ship the
        // stanza.
        assert!(FullJid::from_str("alice@waddle.test").is_err());
    }

    #[test]
    fn full_jid_parse_rejects_garbage() {
        assert!(FullJid::from_str("not-a-jid").is_err());
        assert!(FullJid::from_str("@domain/res").is_err());
        assert!(FullJid::from_str("").is_err());
    }

    /// XEP-0353 §3: every JMI envelope MUST be `type='chat'` and MUST
    /// contain a XEP-0334 `<store/>` hint. Tests against the shared
    /// helper which both the native and wasm clients route through —
    /// keeps the wire shape locked from the wasm boundary down.
    #[test]
    fn message_with_jmi_stamps_chat_type_and_store_hint() {
        use waddle_xmpp_client::messaging::build_propose;
        let body = build_propose(
            &waddle_xmpp_client::messaging::SessionId("c1".into()),
            waddle_xmpp_client::messaging::CallMedia::audio_only(),
        );
        let stanza =
            super::message_with_jmi_to_bare("bob@waddle.test", body).expect("valid JID accepted");
        assert_eq!(stanza.name(), "message");
        assert_eq!(stanza.attr("type"), Some("chat"));
        assert_eq!(stanza.attr("to"), Some("bob@waddle.test"));
        assert!(
            stanza
                .children()
                .any(|c| c.name() == "store" && c.ns() == "urn:xmpp:hints"),
            "XEP-0334 <store/> hint required by XEP-0353 §3"
        );
        assert!(
            stanza
                .children()
                .any(|c| c.name() == "propose" && c.ns() == "urn:xmpp:jingle-message:0"),
            "JMI body preserved"
        );
    }

    #[test]
    fn jmi_response_envelope_requires_full_jid() {
        use waddle_xmpp_client::messaging::build_proceed;
        let body = build_proceed(&waddle_xmpp_client::messaging::SessionId("c1".into()));
        let stanza = super::message_with_jmi_to_full("bob@waddle.test/phone", body)
            .expect("full JID accepted");
        assert_eq!(stanza.attr("to"), Some("bob@waddle.test/phone"));
        assert!(FullJid::from_str("bob@waddle.test").is_err());
    }

    #[test]
    fn jmi_ringing_targets_bare_jid_and_rejects_full_jid() {
        use waddle_xmpp_client::messaging::{build_ringing, SessionId};

        let stanza = super::message_with_jmi_to_bare(
            "bob@waddle.test",
            build_ringing(&SessionId("c1".into())),
        )
        .expect("bare JID accepted for ringing");
        assert_eq!(stanza.attr("to"), Some("bob@waddle.test"));
        assert!(
            stanza
                .children()
                .any(|child| child.name() == "ringing" && child.ns() == NS_JINGLE_MESSAGE),
            "JMI ringing child preserved"
        );
        assert!("bob@waddle.test/phone".parse::<jid::BareJid>().is_err());
    }

    #[test]
    fn jmi_tie_break_response_targets_full_jid_and_carries_expired_reason() {
        use waddle_xmpp_client::messaging::{build_retract_with_options, JingleReason, SessionId};

        let body =
            build_retract_with_options(&SessionId("c1".into()), Some(JingleReason::Expired), true);
        let stanza = super::message_with_jmi_to_full("bob@waddle.test/phone", body)
            .expect("full JID accepted");
        let retract = stanza
            .children()
            .find(|child| child.name() == "retract" && child.ns() == NS_JINGLE_MESSAGE)
            .expect("JMI retract child is present");
        assert_eq!(stanza.attr("to"), Some("bob@waddle.test/phone"));
        assert!(retract
            .children()
            .any(|child| child.name() == "tie-break" && child.ns() == NS_JINGLE_MESSAGE));
        assert!(retract.children().any(|child| {
            child.name() == "reason"
                && child.ns() == NS_JINGLE
                && child
                    .children()
                    .any(|condition| condition.name() == "expired" && condition.ns() == NS_JINGLE)
        }));
    }

    /// XEP-0353 §3: a non-tie-break `<retract/>` is addressed to the
    /// responder's BARE JID (like `<propose/>`), so the disavowal fans
    /// out to every resource that may have seen the ring. The bare-JID
    /// helper backing `send_call_retract` therefore MUST reject a
    /// resource-bearing JID — only `send_call_retract_tie_break` (which
    /// routes through the full-JID helper) targets a single resource.
    #[test]
    fn jmi_retract_targets_bare_jid_and_rejects_full_jid() {
        use waddle_xmpp_client::messaging::{build_retract, SessionId};

        let stanza = super::message_with_jmi_to_bare(
            "bob@waddle.test",
            build_retract(&SessionId("c1".into())),
        )
        .expect("bare JID accepted for retract");
        assert_eq!(stanza.attr("to"), Some("bob@waddle.test"));
        assert!(
            stanza
                .children()
                .any(|child| child.name() == "retract" && child.ns() == NS_JINGLE_MESSAGE),
            "JMI retract child preserved"
        );

        // A resource-bearing JID must be refused so a plain retract can
        // never accidentally target a single resource. Assert at the
        // JID-parse layer that `message_with_jmi_to_bare` routes
        // through (a full JID is not a valid `BareJid`) — the helper's
        // own error arm builds a `JsValue`, which aborts on the
        // non-wasm host test target, so we check the parse directly the
        // same way `jmi_response_envelope_requires_full_jid` does.
        assert!("bob@waddle.test/phone".parse::<jid::BareJid>().is_err());
    }

    #[test]
    fn muji_session_terminate_uses_session_sid_and_keeps_room_metadata() {
        let jingle = super::build_muji_session_terminate(
            "chan@muc.waddle.test",
            &SessionId("attempt-123".to_string()),
        );

        assert_eq!(jingle.name(), "jingle");
        assert_eq!(jingle.ns(), NS_JINGLE);
        assert_eq!(jingle.attr("action"), Some("session-terminate"));
        assert_eq!(jingle.attr("sid"), Some("attempt-123"));
        assert!(jingle.children().any(|child| {
            child.name() == "muji"
                && child.ns() == NS_MUJI
                && child.attr("room") == Some("chan@muc.waddle.test")
        }));
        assert!(jingle.children().any(|child| {
            child.name() == "reason"
                && child.ns() == NS_JINGLE
                && child
                    .children()
                    .any(|condition| condition.name() == "success" && condition.ns() == NS_JINGLE)
        }));
    }

    /// `message_with_jmi` returns `Result<_, JsValue>` on the error
    /// path, and `JsValue::from_str` panics under `cargo test` on
    /// native (it's only safe on wasm32). The JID parse itself is a
    /// thin `s.parse::<jid::Jid>()` call though, so we test the
    /// underlying parse directly — same coverage, no JsValue
    /// instantiation on native.
    #[test]
    fn jmi_envelope_jid_parse_rejects_garbage() {
        assert!("@bogus".parse::<jid::Jid>().is_err());
        assert!("".parse::<jid::Jid>().is_err());
    }
}
