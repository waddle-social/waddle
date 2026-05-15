//! Inbound A/V call event types + parser.
//!
//! Detects two shapes the client receives during a call:
//!
//! 1. **XEP-0353 Jingle Message Initiation** envelopes carried on
//!    `<message>` stanzas — `<propose>`, `<proceed>`, `<reject>`,
//!    `<retract>`, `<finish>`. These drive the ringing UI before
//!    media starts.
//! 2. **XEP-0166 Jingle session control** carried on `<iq type='set'>`
//!    stanzas — `session-initiate`, `session-accept`,
//!    `session-terminate`. The server's Jingle handler rewrites the
//!    `urn:waddle:transports:livekit:0` transport with a populated
//!    [`LiveKitJoin`] before forwarding, so the typed event below
//!    carries the credentials a `livekit-client` SDK needs directly.
//!
//! The parser intentionally returns typed Waddle values rather than
//! raw [`minidom::Element`] so the client UI layer doesn't have to
//! know XML.

use jid::Jid;
use minidom::Element;

/// Media kinds offered or accepted on a call. Inferred from the
/// JMI `<description media='…'/>` child or from the Jingle content
/// list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallMedia {
    pub audio: bool,
    pub video: bool,
}

impl CallMedia {
    pub const fn none() -> Self {
        Self {
            audio: false,
            video: false,
        }
    }
    pub const fn audio_only() -> Self {
        Self {
            audio: true,
            video: false,
        }
    }
    pub const fn audio_video() -> Self {
        Self {
            audio: true,
            video: true,
        }
    }
}

/// LiveKit join credentials extracted from the server-issued
/// `urn:waddle:transports:livekit:0` transport. Fields are plain
/// `String` because they cross the WASM boundary into JavaScript /
/// the `livekit-client` SDK; the typed XMPP layer never compares or
/// re-parses them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveKitJoin {
    pub url: String,
    pub room: String,
    pub identity: String,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEventKind {
    Propose { media: CallMedia },
    Proceed,
    Reject,
    Retract,
    Finish,
    SessionInitiate { join: LiveKitJoin, media: CallMedia },
    SessionAccept { join: LiveKitJoin, media: CallMedia },
    SessionTerminate { reason: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundCallEvent {
    /// The originator JID, preserved as-sent by the server's `from`
    /// stamp. For `<propose/>` and Jingle session-initiate this is
    /// the initiator's *full* JID — XEP-0353 §0.5 / §0.6 require
    /// JMI responses (`<proceed/>`, `<reject/>`, `<retract/>`,
    /// `<finish/>`) and Jingle session-accept / session-terminate
    /// to be addressed to that full JID so the originating resource
    /// receives the answer, not every resource of the bare JID.
    pub from: Jid,
    pub sid: String,
    pub kind: CallEventKind,
}

const NS_JINGLE: &str = "urn:xmpp:jingle:1";
const NS_JINGLE_MESSAGE: &str = "urn:xmpp:jingle-message:0";
const NS_JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";
const NS_WADDLE_LIVEKIT_TRANSPORT: &str = "urn:waddle:transports:livekit:0";

/// Parse a `<message>` stanza for a JMI envelope. Returns `None`
/// when no `urn:xmpp:jingle-message:0` child is present.
pub fn parse_jmi_message(stanza: &Element) -> Option<InboundCallEvent> {
    if stanza.name() != "message" {
        return None;
    }
    let from = stanza.attr("from").and_then(|s| s.parse::<Jid>().ok())?;

    for child in stanza.children() {
        if child.ns() != NS_JINGLE_MESSAGE {
            continue;
        }
        let sid = child.attr("id")?.to_string();
        let kind = match child.name() {
            "propose" => CallEventKind::Propose {
                media: media_from_descriptions(child),
            },
            "proceed" => CallEventKind::Proceed,
            "reject" => CallEventKind::Reject,
            "retract" => CallEventKind::Retract,
            "finish" => CallEventKind::Finish,
            _ => continue,
        };
        return Some(InboundCallEvent { from, sid, kind });
    }
    None
}

/// Parse an `<iq>` stanza for a Jingle payload. Returns `None` when
/// no `urn:xmpp:jingle:1` child is present.
pub fn parse_jingle_iq(stanza: &Element) -> Option<InboundCallEvent> {
    if stanza.name() != "iq" {
        return None;
    }
    let from = stanza.attr("from").and_then(|s| s.parse::<Jid>().ok())?;
    let jingle = stanza
        .children()
        .find(|c| c.name() == "jingle" && c.ns() == NS_JINGLE)?;
    let sid = jingle.attr("sid")?.to_string();
    let action = jingle.attr("action")?;

    let kind = match action {
        "session-initiate" | "session-accept" => {
            let join = extract_livekit_join(jingle)?;
            let media = media_from_contents(jingle);
            if action == "session-initiate" {
                CallEventKind::SessionInitiate { join, media }
            } else {
                CallEventKind::SessionAccept { join, media }
            }
        }
        "session-terminate" => CallEventKind::SessionTerminate {
            reason: extract_terminate_reason(jingle),
        },
        _ => return None,
    };
    Some(InboundCallEvent { from, sid, kind })
}

/// Convenience dispatcher: try JMI first, then Jingle.
pub fn parse_call_event(stanza: &Element) -> Option<InboundCallEvent> {
    parse_jmi_message(stanza).or_else(|| parse_jingle_iq(stanza))
}

fn media_from_descriptions(envelope: &Element) -> CallMedia {
    let mut media = CallMedia::none();
    for desc in envelope.children() {
        if desc.name() != "description" || desc.ns() != NS_JINGLE_RTP {
            continue;
        }
        match desc.attr("media") {
            Some("audio") => media.audio = true,
            Some("video") => media.video = true,
            _ => {}
        }
    }
    media
}

fn media_from_contents(jingle: &Element) -> CallMedia {
    let mut media = CallMedia::none();
    for content in jingle.children() {
        if content.name() != "content" {
            continue;
        }
        for desc in content.children() {
            if desc.name() != "description" || desc.ns() != NS_JINGLE_RTP {
                continue;
            }
            match desc.attr("media") {
                Some("audio") => media.audio = true,
                Some("video") => media.video = true,
                _ => {}
            }
        }
    }
    media
}

fn extract_livekit_join(jingle: &Element) -> Option<LiveKitJoin> {
    for content in jingle.children() {
        if content.name() != "content" {
            continue;
        }
        for transport in content.children() {
            if transport.name() != "transport" || transport.ns() != NS_WADDLE_LIVEKIT_TRANSPORT {
                continue;
            }
            let url = transport.attr("url")?.to_string();
            let room = transport.attr("room")?.to_string();
            let identity = transport.attr("identity")?.to_string();
            let token = transport
                .children()
                .find(|c| c.name() == "token" && c.ns() == NS_WADDLE_LIVEKIT_TRANSPORT)?
                .text();
            return Some(LiveKitJoin {
                url,
                room,
                identity,
                token,
            });
        }
    }
    None
}

fn extract_terminate_reason(jingle: &Element) -> Option<String> {
    let reason = jingle.children().find(|c| c.name() == "reason")?;
    reason.children().next().map(|c| c.name().to_string())
}

// ---------------------------------------------------------------------
// Outbound builders
//
// These mirror the inbound parsers above so the chat UI can originate
// the same wire shapes it can already decode. Returned values are
// `minidom::Element`s ready to be wrapped in a `<message>` or `<iq>`
// envelope by the wasm send pipeline.
//
// The server is the one that mints LiveKit join tokens, so outbound
// transports are always built in the **request** shape (empty
// `<transport xmlns='urn:waddle:transports:livekit:0'/>`); the server's
// Jingle handler rewrites them with a populated transport before
// forwarding to the peer.

/// Build a `<propose/>` JMI body for a 1:1 call. The propose carries
/// one `<description/>` per offered media kind so the responder's UI
/// can show "audio call" vs. "video call" without waiting for the
/// session-initiate.
pub fn build_propose(sid: &str, media: CallMedia) -> Element {
    let mut builder = Element::builder("propose", NS_JINGLE_MESSAGE).attr("id", sid);
    if media.audio {
        builder = builder.append(rtp_description_element("audio"));
    }
    if media.video {
        builder = builder.append(rtp_description_element("video"));
    }
    builder.build()
}

pub fn build_proceed(sid: &str) -> Element {
    Element::builder("proceed", NS_JINGLE_MESSAGE)
        .attr("id", sid)
        .build()
}

pub fn build_reject(sid: &str) -> Element {
    Element::builder("reject", NS_JINGLE_MESSAGE)
        .attr("id", sid)
        .build()
}

pub fn build_retract(sid: &str) -> Element {
    Element::builder("retract", NS_JINGLE_MESSAGE)
        .attr("id", sid)
        .build()
}

pub fn build_finish(sid: &str) -> Element {
    Element::builder("finish", NS_JINGLE_MESSAGE)
        .attr("id", sid)
        .build()
}

/// Build the `<jingle/>` body of a session-initiate IQ. The
/// `initiator` attribute is required so the receiving server's Jingle
/// handler can verify the call originator and namespace the LiveKit
/// room scope. One `<content/>` per offered media kind, each
/// carrying an empty Waddle LiveKit transport for the server to
/// populate.
pub fn build_session_initiate(sid: &str, initiator_full_jid: &str, media: CallMedia) -> Element {
    let mut builder = Element::builder("jingle", NS_JINGLE)
        .attr("action", "session-initiate")
        .attr("initiator", initiator_full_jid)
        .attr("sid", sid);
    if media.audio {
        builder = builder.append(content_element("audio"));
    }
    if media.video {
        builder = builder.append(content_element("video"));
    }
    builder.build()
}

/// Build the `<jingle/>` body of a session-accept IQ. Per
/// XEP-0166 §7.1 the `responder` attribute names the accepting
/// party; the initiator attribute mirrors the originator so the
/// server can re-derive the call scope.
pub fn build_session_accept(
    sid: &str,
    initiator_full_jid: &str,
    responder_full_jid: &str,
    media: CallMedia,
) -> Element {
    let mut builder = Element::builder("jingle", NS_JINGLE)
        .attr("action", "session-accept")
        .attr("initiator", initiator_full_jid)
        .attr("responder", responder_full_jid)
        .attr("sid", sid);
    if media.audio {
        builder = builder.append(content_element("audio"));
    }
    if media.video {
        builder = builder.append(content_element("video"));
    }
    builder.build()
}

/// Build the `<jingle/>` body of a session-terminate IQ. Reason is
/// one of the XEP-0166 §7.4 condition names (`success`, `decline`,
/// `cancel`, `busy`, `gone`, …). `None` omits the `<reason/>` child.
pub fn build_session_terminate(sid: &str, reason: Option<&str>) -> Element {
    let mut builder = Element::builder("jingle", NS_JINGLE)
        .attr("action", "session-terminate")
        .attr("sid", sid);
    if let Some(condition) = reason {
        let reason_elem = Element::builder("reason", NS_JINGLE)
            .append(Element::builder(condition, NS_JINGLE).build())
            .build();
        builder = builder.append(reason_elem);
    }
    builder.build()
}

fn rtp_description_element(media: &str) -> Element {
    Element::builder("description", NS_JINGLE_RTP)
        .attr("media", media)
        .build()
}

fn content_element(media: &str) -> Element {
    Element::builder("content", NS_JINGLE)
        .attr("creator", "initiator")
        .attr("name", media)
        .append(rtp_description_element(media))
        .append(Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT).build())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_propose_with_audio_video() {
        let xml = r#"<message xmlns='jabber:client' from='alice@waddle.test/desktop' to='bob@waddle.test'>
            <propose xmlns='urn:xmpp:jingle-message:0' id='c1'>
              <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
              <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='video'/>
            </propose>
        </message>"#;
        let elem: Element = xml.parse().unwrap();
        let ev = parse_call_event(&elem).expect("propose parses");
        // XEP-0353 §0.6: the propose's `from` is stamped by the
        // server as the initiator's *full* JID so the responder can
        // address its proceed/reject directly at that resource.
        assert_eq!(ev.from.to_string(), "alice@waddle.test/desktop");
        assert_eq!(ev.sid, "c1");
        match ev.kind {
            CallEventKind::Propose { media } => assert_eq!(media, CallMedia::audio_video()),
            other => panic!("expected Propose, got {other:?}"),
        }
    }

    #[test]
    fn parses_proceed() {
        let xml = "<message xmlns='jabber:client' from='bob@waddle.test/desktop'>
            <proceed xmlns='urn:xmpp:jingle-message:0' id='c1'/>
        </message>";
        let elem: Element = xml.parse().unwrap();
        let ev = parse_call_event(&elem).expect("proceed parses");
        assert!(matches!(ev.kind, CallEventKind::Proceed));
    }

    #[test]
    fn parses_finish() {
        let xml = "<message xmlns='jabber:client' from='alice@waddle.test/desktop'>
            <finish xmlns='urn:xmpp:jingle-message:0' id='c1'/>
        </message>";
        let elem: Element = xml.parse().unwrap();
        let ev = parse_call_event(&elem).expect("finish parses");
        assert!(matches!(ev.kind, CallEventKind::Finish));
    }

    #[test]
    fn parses_session_initiate_with_livekit_transport() {
        let xml = r#"<iq xmlns='jabber:client' type='set' from='alice@waddle.test/desktop' to='bob@waddle.test/desktop' id='i1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='c1' initiator='alice@waddle.test/desktop'>
              <content creator='initiator' name='audio'>
                <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
                <transport xmlns='urn:waddle:transports:livekit:0'
                           url='wss://livekit.waddle.test'
                           room='alice@waddle.test::c1'
                           identity='bob@waddle.test/desktop'>
                  <token xmlns='urn:waddle:transports:livekit:0'>eyJhbGc.payload.sig</token>
                </transport>
              </content>
            </jingle>
        </iq>"#;
        let elem: Element = xml.parse().unwrap();
        let ev = parse_call_event(&elem).expect("session-initiate parses");
        assert_eq!(ev.sid, "c1");
        match ev.kind {
            CallEventKind::SessionInitiate { join, media } => {
                assert_eq!(join.url, "wss://livekit.waddle.test");
                assert_eq!(join.room, "alice@waddle.test::c1");
                assert_eq!(join.identity, "bob@waddle.test/desktop");
                assert_eq!(join.token, "eyJhbGc.payload.sig");
                assert_eq!(media, CallMedia::audio_only());
            }
            other => panic!("expected SessionInitiate, got {other:?}"),
        }
    }

    #[test]
    fn parses_session_terminate_with_reason() {
        let xml = r#"<iq xmlns='jabber:client' type='set' from='bob@waddle.test/desktop' id='t1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-terminate' sid='c1'>
              <reason><success/></reason>
            </jingle>
        </iq>"#;
        let elem: Element = xml.parse().unwrap();
        let ev = parse_call_event(&elem).expect("session-terminate parses");
        match ev.kind {
            CallEventKind::SessionTerminate { reason } => {
                assert_eq!(reason.as_deref(), Some("success"))
            }
            other => panic!("expected SessionTerminate, got {other:?}"),
        }
    }

    #[test]
    fn returns_none_for_non_call_message() {
        let xml =
            "<message xmlns='jabber:client' from='alice@waddle.test'><body>hi</body></message>";
        let elem: Element = xml.parse().unwrap();
        assert!(parse_call_event(&elem).is_none());
    }

    #[test]
    fn returns_none_for_jingle_with_unknown_action() {
        let xml = r#"<iq xmlns='jabber:client' type='set' from='alice@waddle.test/d'>
            <jingle xmlns='urn:xmpp:jingle:1' action='transport-info' sid='c1'/>
        </iq>"#;
        let elem: Element = xml.parse().unwrap();
        // transport-info isn't surfaced as a call event yet — it's
        // mid-session signalling, handled internally.
        assert!(parse_call_event(&elem).is_none());
    }

    // --- outbound builders --------------------------------------------

    #[test]
    fn build_propose_emits_one_description_per_offered_media() {
        let elem = build_propose("c1", CallMedia::audio_video());
        assert_eq!(elem.name(), "propose");
        assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
        assert_eq!(elem.attr("id"), Some("c1"));
        let media: Vec<_> = elem
            .children()
            .filter(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
            .filter_map(|c| c.attr("media"))
            .collect();
        assert_eq!(media, vec!["audio", "video"]);
    }

    #[test]
    fn build_propose_audio_only_omits_video_description() {
        let elem = build_propose("c1", CallMedia::audio_only());
        let media: Vec<_> = elem
            .children()
            .filter(|c| c.name() == "description")
            .filter_map(|c| c.attr("media"))
            .collect();
        assert_eq!(media, vec!["audio"]);
    }

    #[test]
    fn build_jmi_helpers_roundtrip_through_parser() {
        // proceed: wrapping in a <message/> with a `from` makes the
        // inbound parser pick it up.
        let stanza = Element::builder("message", "jabber:client")
            .attr("from", "bob@waddle.test/desktop")
            .append(build_proceed("c1"))
            .build();
        let ev = parse_call_event(&stanza).expect("proceed parses");
        assert!(matches!(ev.kind, CallEventKind::Proceed));

        let stanza = Element::builder("message", "jabber:client")
            .attr("from", "bob@waddle.test/desktop")
            .append(build_reject("c1"))
            .build();
        let ev = parse_call_event(&stanza).expect("reject parses");
        assert!(matches!(ev.kind, CallEventKind::Reject));

        let stanza = Element::builder("message", "jabber:client")
            .attr("from", "alice@waddle.test/desktop")
            .append(build_retract("c1"))
            .build();
        let ev = parse_call_event(&stanza).expect("retract parses");
        assert!(matches!(ev.kind, CallEventKind::Retract));

        let stanza = Element::builder("message", "jabber:client")
            .attr("from", "alice@waddle.test/desktop")
            .append(build_finish("c1"))
            .build();
        let ev = parse_call_event(&stanza).expect("finish parses");
        assert!(matches!(ev.kind, CallEventKind::Finish));
    }

    #[test]
    fn build_session_initiate_carries_empty_waddle_transport_per_content() {
        let jingle =
            build_session_initiate("c1", "alice@waddle.test/desktop", CallMedia::audio_video());
        assert_eq!(jingle.name(), "jingle");
        assert_eq!(jingle.ns(), NS_JINGLE);
        assert_eq!(jingle.attr("action"), Some("session-initiate"));
        assert_eq!(jingle.attr("sid"), Some("c1"));
        assert_eq!(jingle.attr("initiator"), Some("alice@waddle.test/desktop"));

        let contents: Vec<_> = jingle
            .children()
            .filter(|c| c.name() == "content")
            .collect();
        assert_eq!(contents.len(), 2);
        for content in contents {
            // Every content has an empty Waddle transport request —
            // the server fills in url/room/identity/token before
            // forwarding to the peer.
            let transport = content
                .children()
                .find(|c| c.name() == "transport")
                .expect("content has transport");
            assert_eq!(transport.ns(), NS_WADDLE_LIVEKIT_TRANSPORT);
            assert!(
                transport.attr("url").is_none(),
                "outbound transport must be a request"
            );
            assert!(transport.attr("room").is_none());
            assert!(transport.attr("identity").is_none());
            assert!(transport.children().next().is_none());
        }
    }

    #[test]
    fn build_session_accept_carries_responder_attr_and_empty_transport() {
        let jingle = build_session_accept(
            "c1",
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            CallMedia::audio_only(),
        );
        assert_eq!(jingle.attr("action"), Some("session-accept"));
        assert_eq!(jingle.attr("initiator"), Some("alice@waddle.test/desktop"));
        assert_eq!(jingle.attr("responder"), Some("bob@waddle.test/desktop"));
        let contents: Vec<_> = jingle
            .children()
            .filter(|c| c.name() == "content")
            .collect();
        assert_eq!(contents.len(), 1);
    }

    #[test]
    fn build_session_terminate_includes_reason_when_supplied() {
        let with_reason = build_session_terminate("c1", Some("success"));
        let reason_elem = with_reason
            .children()
            .find(|c| c.name() == "reason")
            .expect("reason child");
        assert!(reason_elem.children().any(|c| c.name() == "success"));

        let without = build_session_terminate("c1", None);
        assert!(without.children().all(|c| c.name() != "reason"));
    }

    #[test]
    fn rejects_jingle_session_initiate_without_livekit_transport() {
        let xml = r#"<iq xmlns='jabber:client' type='set' from='alice@waddle.test/d' id='i1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='c1'>
              <content creator='initiator' name='audio'>
                <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
                <transport xmlns='urn:xmpp:jingle:transports:ice-udp:1'/>
              </content>
            </jingle>
        </iq>"#;
        let elem: Element = xml.parse().unwrap();
        // No Waddle transport → no LiveKit credentials → not a
        // surfaced call event (the chat UI has nothing actionable).
        assert!(parse_call_event(&elem).is_none());
    }
}
