use super::*;
use waddle_xmpp_client::messaging::{
    build_finish, build_proceed, build_propose, build_reject, build_retract, build_session_accept,
    build_session_initiate, build_session_terminate, CallMedia,
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

#[wasm_bindgen]
impl WaddleClient {
    /// Send a JMI `<propose/>` to the peer's bare JID (XEP-0353
    /// §5.1.1). The bare JID lets the responder's server ring every
    /// connected resource until one of them proceeds/rejects.
    pub fn send_call_propose(
        &self,
        peer_bare_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(
                &peer_bare_jid,
                build_propose(&sid, media_from_flags(audio, video)),
            );
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_proceed(&self, peer_bare_jid: String, sid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_bare_jid, build_proceed(&sid));
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_reject(&self, peer_bare_jid: String, sid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_bare_jid, build_reject(&sid));
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_retract(&self, peer_bare_jid: String, sid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_bare_jid, build_retract(&sid));
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_finish(&self, peer_bare_jid: String, sid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_bare_jid, build_finish(&sid));
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Send a Jingle `session-initiate` IQ to the peer's full JID
    /// (XEP-0166 §6.4). The `initiator` attribute is omitted; the
    /// server stamps the authenticated session as the call originator
    /// and rewrites the empty Waddle LiveKit transport with a join
    /// token before forwarding to the responder.
    pub fn send_call_session_initiate(
        &self,
        peer_full_jid: String,
        initiator_full_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = iq_set(
                &peer_full_jid,
                build_session_initiate(&sid, &initiator_full_jid, media_from_flags(audio, video)),
            );
            send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Send a Jingle `session-accept` IQ in response to a received
    /// session-initiate. The `initiator` attribute MUST match the
    /// original caller's bare JID so the server can re-derive the
    /// call scope.
    pub fn send_call_session_accept(
        &self,
        peer_full_jid: String,
        initiator_full_jid: String,
        responder_full_jid: String,
        sid: String,
        audio: bool,
        video: bool,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = iq_set(
                &peer_full_jid,
                build_session_accept(
                    &sid,
                    &initiator_full_jid,
                    &responder_full_jid,
                    media_from_flags(audio, video),
                ),
            );
            send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Send a Jingle `session-terminate` IQ to hang up. `reason` is
    /// one of the XEP-0166 §7.4 condition names ("success",
    /// "decline", "cancel", "busy", "gone", …) or `None` for no
    /// explicit reason.
    pub fn send_call_session_terminate(
        &self,
        peer_full_jid: String,
        sid: String,
        reason: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = iq_set(
                &peer_full_jid,
                build_session_terminate(&sid, reason.as_deref()),
            );
            send_iq_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }
}
