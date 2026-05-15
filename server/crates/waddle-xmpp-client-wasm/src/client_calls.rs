use super::*;
use std::str::FromStr;
use waddle_xmpp_client::messaging::{
    build_finish, build_proceed, build_propose, build_reject, build_retract, build_session_accept,
    build_session_initiate, build_session_terminate, CallMedia, JingleReason, SessionId,
};

const NS_JABBER_CLIENT: &str = "jabber:client";

fn message_with_jmi(to: &str, jmi: Element) -> Element {
    Element::builder("message", NS_JABBER_CLIENT)
        .attr("to", to)
        .append(jmi)
        .build()
}

fn iq_set(to: &str, payload: Element) -> Element {
    Element::builder("iq", NS_JABBER_CLIENT)
        .attr("type", "set")
        .attr("id", uuid::Uuid::new_v4().to_string())
        .attr("to", to)
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
            let stanza = message_with_jmi(
                &peer_bare_jid,
                build_propose(&sid(sid_str), media_from_flags(audio, video)),
            );
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_proceed(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_full_jid, build_proceed(&sid(sid_str)));
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_reject(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_full_jid, build_reject(&sid(sid_str)));
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_retract(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_full_jid, build_retract(&sid(sid_str)));
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_finish(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_full_jid, build_finish(&sid(sid_str)));
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
            let stanza = iq_set(
                &peer_full_jid,
                build_session_initiate(&sid(sid_str), &initiator, media_from_flags(audio, video)),
            );
            send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Send a Jingle `session-accept` IQ in response to a received
    /// session-initiate. `initiator` and `responder` are validated
    /// as full JIDs at the wasm boundary so a malformed JID surfaces
    /// as a clear `JsError` rather than a wire-rejected stanza.
    pub fn send_call_session_accept(
        &self,
        peer_full_jid: String,
        initiator_full_jid: String,
        responder_full_jid: String,
        sid_str: String,
        audio: bool,
        video: bool,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let initiator = parse_full_jid(&initiator_full_jid)?;
            let responder = parse_full_jid(&responder_full_jid)?;
            let stanza = iq_set(
                &peer_full_jid,
                build_session_accept(
                    &sid(sid_str),
                    &initiator,
                    &responder,
                    media_from_flags(audio, video),
                ),
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
                    JingleReason::from_str(&name)
                        .map_err(|_| js_error(format!("unknown jingle reason: {name}")))?,
                ),
                None => None,
            };
            let stanza = iq_set(
                &peer_full_jid,
                build_session_terminate(&sid(sid_str), typed_reason),
            );
            send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }
}
