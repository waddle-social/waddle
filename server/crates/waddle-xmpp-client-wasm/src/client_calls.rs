use super::*;
use waddle_xmpp_client::messaging::{
    build_finish, build_proceed, build_propose, build_reject, build_retract, build_session_accept,
    build_session_initiate, build_session_terminate, jingle_reason_from_wire_name,
    wrap_jmi_message, CallMedia, SessionId,
};

const NS_WADDLE_MUC_CALL: &str = "urn:waddle:muc-call:0";
const NS_WADDLE_LIVEKIT_TRANSPORT: &str = "urn:waddle:transports:livekit:0";

const NS_JABBER_CLIENT: &str = "jabber:client";

/// Walk an `<iq type='result'>` payload from the server's
/// `<request-join xmlns='urn:waddle:muc-call:0'/>` IQ and pull out
/// the typed `WaddleLiveKitJoin`.
///
/// The wire shape (canonical, see `WaddleLiveKitTransport::to_element`
/// in `waddle-xmpp/src/xep/xep_waddle_livekit_transport.rs`) is:
///
/// ```xml
/// <iq type='result'>
///   <joined xmlns='urn:waddle:muc-call:0'>
///     <transport xmlns='urn:waddle:transports:livekit:0'
///                url='...' room='...' identity='...'>
///       <token>JWT</token>
///     </transport>
///   </joined>
/// </iq>
/// ```
///
/// `url`, `room`, `identity` are **attributes** of `<transport>`;
/// `<token>` is the sole child element. Reading any of the first
/// three via `get_child(...)` (as a pre-#495 draft of this parser
/// did) silently turns every server-issued join into a parse
/// error, which surfaces as a never-resolved call promise on the
/// chat side and a UI that does nothing when the user clicks the
/// channel call button.
fn parse_muc_call_join_result(result: &Element) -> Result<crate::types::WaddleLiveKitJoin, String> {
    let joined = result
        .get_child("joined", NS_WADDLE_MUC_CALL)
        .ok_or_else(|| "muc-call: response missing <joined/>".to_string())?;
    let transport = joined
        .get_child("transport", NS_WADDLE_LIVEKIT_TRANSPORT)
        .ok_or_else(|| "muc-call: <joined/> missing <transport/>".to_string())?;
    let attr = |name: &str| -> Result<String, String> {
        transport
            .attr(name)
            .map(str::to_owned)
            .ok_or_else(|| format!("muc-call: transport missing @{name}"))
    };
    let token = transport
        .get_child("token", NS_WADDLE_LIVEKIT_TRANSPORT)
        .map(|c| c.text())
        .ok_or_else(|| "muc-call: transport missing <token/>".to_string())?;
    Ok(crate::types::WaddleLiveKitJoin {
        url: attr("url")?,
        room: attr("room")?,
        identity: attr("identity")?,
        token,
    })
}

/// Wrap a JMI body in the XEP-0353 §3-conformant `<message type='chat'>`
/// envelope with a XEP-0334 `<store/>` hint. Routes through
/// [`waddle_xmpp_client::messaging::wrap_jmi_message`] so the wasm and
/// native clients ship byte-identical envelopes; the only wasm-local
/// concern is parsing the JS `String` `to` into a typed
/// [`jid::Jid`] at the boundary.
fn message_with_jmi(to: &str, jmi: Element) -> Result<Element, JsValue> {
    let to_jid: jid::Jid = to
        .parse()
        .map_err(|_| js_error(format!("invalid JID for JMI envelope: {to}")))?;
    Ok(wrap_jmi_message(&to_jid, jmi))
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
            )?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_proceed(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_full_jid, build_proceed(&sid(sid_str)))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_reject(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_full_jid, build_reject(&sid(sid_str)))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_retract(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_full_jid, build_retract(&sid(sid_str)))?;
            send_stanza_command(inner, stanza).await?;
            Ok(JsValue::UNDEFINED)
        })
    }

    pub fn send_call_finish(&self, peer_full_jid: String, sid_str: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let stanza = message_with_jmi(&peer_full_jid, build_finish(&sid(sid_str)))?;
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
            let _ = parse_full_jid(&peer_full_jid)?;
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
    /// Send `<request-join xmlns='urn:waddle:muc-call:0' room='ROOM_JID'/>`
    /// to the MUC room and return the issued LiveKit join credentials
    /// as a typed `{ url, room, identity, token }` object. The XML
    /// is built via `minidom::Element::builder` (XML hard rule
    /// from CLAUDE.md — no string concatenation at the wire
    /// boundary).
    pub fn send_muc_call_join(&self, room_jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            // Build the request-join IQ via the typed Element builder.
            let payload = Element::builder("request-join", NS_WADDLE_MUC_CALL)
                .attr(
                    minidom::rxml::xml_ncname!("room").to_owned(),
                    room_jid.as_str(),
                )
                .build();
            let stanza = Element::builder("iq", NS_JABBER_CLIENT)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    uuid::Uuid::new_v4().to_string(),
                )
                .attr(
                    minidom::rxml::xml_ncname!("to").to_owned(),
                    room_jid.as_str(),
                )
                .append(payload)
                .build();
            let result = send_iq_command(inner, stanza).await?;
            let join = parse_muc_call_join_result(&result).map_err(js_error)?;
            to_js_value(&join)
        })
    }

    /// Send `<request-leave xmlns='urn:waddle:muc-call:0' room='ROOM_JID'/>`
    /// to the MUC room. Server unregisters the participant + revokes
    /// every jti it minted for `(room, identity)`. Resolves with no
    /// payload.
    pub fn send_muc_call_leave(&self, room_jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let payload = Element::builder("request-leave", NS_WADDLE_MUC_CALL)
                .attr(
                    minidom::rxml::xml_ncname!("room").to_owned(),
                    room_jid.as_str(),
                )
                .build();
            let stanza = Element::builder("iq", NS_JABBER_CLIENT)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    uuid::Uuid::new_v4().to_string(),
                )
                .attr(
                    minidom::rxml::xml_ncname!("to").to_owned(),
                    room_jid.as_str(),
                )
                .append(payload)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::FullJid;
    use std::str::FromStr;

    fn canonical_join_iq_result() -> Element {
        // Mirrors the exact byte layout emitted by
        // `MucCallHandler::handle_join` (see
        // `waddle-xmpp/src/protocol/handlers/muc_call.rs`) — keeps
        // this parser locked to the real wire shape rather than a
        // hand-typed approximation.
        let xml = "<iq xmlns='jabber:client' type='result' id='abc'>\
                <joined xmlns='urn:waddle:muc-call:0'>\
                    <transport xmlns='urn:waddle:transports:livekit:0' \
                               url='wss://livekit.waddle.test/' \
                               room='chat@muc.waddle.test' \
                               identity='alice@waddle.test/web-1'>\
                        <token>eyJhbGciOiJIUzI1NiJ9.payload.sig</token>\
                    </transport>\
                </joined>\
            </iq>";
        xml.parse().expect("test fixture parses")
    }

    #[test]
    fn parse_muc_call_join_result_reads_canonical_response() {
        let iq = canonical_join_iq_result();
        let join = parse_muc_call_join_result(&iq).expect("canonical response parses");
        assert_eq!(join.url, "wss://livekit.waddle.test/");
        assert_eq!(join.room, "chat@muc.waddle.test");
        assert_eq!(join.identity, "alice@waddle.test/web-1");
        assert_eq!(join.token, "eyJhbGciOiJIUzI1NiJ9.payload.sig");
    }

    #[test]
    fn parse_muc_call_join_result_rejects_missing_joined() {
        let iq: Element = "<iq xmlns='jabber:client' type='result' id='abc'/>"
            .parse()
            .unwrap();
        let err = parse_muc_call_join_result(&iq).unwrap_err();
        assert!(err.contains("missing <joined/>"), "got: {err}");
    }

    #[test]
    fn parse_muc_call_join_result_rejects_missing_transport() {
        let iq: Element = "<iq xmlns='jabber:client' type='result' id='abc'>\
                <joined xmlns='urn:waddle:muc-call:0'/>\
            </iq>"
            .parse()
            .unwrap();
        let err = parse_muc_call_join_result(&iq).unwrap_err();
        assert!(err.contains("missing <transport/>"), "got: {err}");
    }

    #[test]
    fn parse_muc_call_join_result_rejects_missing_attribute() {
        // Drop the `url` attribute — parser must reject rather than
        // silently leaving the field blank and propagating a half-
        // valid join object to the chat client.
        let iq: Element = "<iq xmlns='jabber:client' type='result' id='abc'>\
                <joined xmlns='urn:waddle:muc-call:0'>\
                    <transport xmlns='urn:waddle:transports:livekit:0' \
                               room='chat@muc.waddle.test' \
                               identity='alice@waddle.test/web-1'>\
                        <token>jwt</token>\
                    </transport>\
                </joined>\
            </iq>"
            .parse()
            .unwrap();
        let err = parse_muc_call_join_result(&iq).unwrap_err();
        assert!(err.contains("@url"), "got: {err}");
    }

    #[test]
    fn parse_muc_call_join_result_rejects_missing_token() {
        let iq: Element = "<iq xmlns='jabber:client' type='result' id='abc'>\
                <joined xmlns='urn:waddle:muc-call:0'>\
                    <transport xmlns='urn:waddle:transports:livekit:0' \
                               url='wss://livekit.waddle.test/' \
                               room='chat@muc.waddle.test' \
                               identity='alice@waddle.test/web-1'/>\
                </joined>\
            </iq>"
            .parse()
            .unwrap();
        let err = parse_muc_call_join_result(&iq).unwrap_err();
        assert!(err.contains("missing <token/>"), "got: {err}");
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
        let stanza = super::message_with_jmi("bob@waddle.test", body).expect("valid JID accepted");
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
