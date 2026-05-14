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

use jid::{BareJid, Jid};
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
    pub from: BareJid,
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
    let from = stanza
        .attr("from")
        .and_then(|s| s.parse::<Jid>().ok())
        .map(|j| j.to_bare())?;

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
    let from = stanza
        .attr("from")
        .and_then(|s| s.parse::<Jid>().ok())
        .map(|j| j.to_bare())?;
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
        assert_eq!(ev.from.to_string(), "alice@waddle.test");
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
