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

use jid::{FullJid, Jid};
use minidom::Element;
use xmpp_parsers::jingle::{Reason as JingleReason, SessionId};
use xmpp_parsers::message::{Message, MessageType};

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

// `xmpp_parsers::jingle::Reason` is `PartialEq` but not `Eq`, so the
// enum drops `Eq` to compose. Equality semantics are unchanged for
// the call-event use cases (we compare variants and IDs, never any
// f-prefixed numerics).
#[derive(Debug, Clone, PartialEq)]
pub enum CallEventKind {
    Propose {
        media: CallMedia,
    },
    Proceed,
    Reject {
        reason: Option<JingleReason>,
        tie_break: bool,
    },
    Retract {
        reason: Option<JingleReason>,
        tie_break: bool,
    },
    Finish {
        reason: Option<JingleReason>,
        migrated_to: Option<SessionId>,
    },
    SessionInitiate {
        join: LiveKitJoin,
        media: CallMedia,
    },
    SessionAccept {
        join: LiveKitJoin,
        media: CallMedia,
    },
    /// XEP-0166 §7.4 session-terminate. `reason` is the typed
    /// `JingleReason` condition; unknown wire conditions are
    /// surfaced as `None` (the parser drops untyped strings rather
    /// than passing them through — typed-payloads hard rule in
    /// `CLAUDE.md`).
    SessionTerminate {
        reason: Option<JingleReason>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboundCallEvent {
    /// The originator JID, preserved as-sent by the server's `from`
    /// stamp. For `<propose/>` and Jingle session-initiate this is
    /// the initiator's *full* JID — XEP-0353 §0.5 / §0.6 require
    /// JMI responses (`<proceed/>`, `<reject/>`, `<retract/>`,
    /// `<finish/>`) and Jingle session-accept / session-terminate
    /// to be addressed to that full JID so the originating resource
    /// receives the answer, not every resource of the bare JID.
    pub from: Jid,
    pub sid: SessionId,
    pub kind: CallEventKind,
}

const NS_JINGLE: &str = "urn:xmpp:jingle:1";
const NS_JINGLE_MESSAGE: &str = "urn:xmpp:jingle-message:0";
const NS_JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";
const NS_WADDLE_LIVEKIT_TRANSPORT: &str = "urn:waddle:transports:livekit:0";
/// XEP-0334 Message Processing Hints namespace. The `<store/>` hint
/// is mandatory on every JMI envelope per XEP-0353 §3 so MAM
/// archives keep the call timeline reconstructible even when the
/// stanza carries no body.
const NS_HINTS: &str = "urn:xmpp:hints";

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
        let sid = SessionId(child.attr("id")?.to_string());
        let kind = match child.name() {
            "propose" => CallEventKind::Propose {
                media: media_from_descriptions(child),
            },
            "proceed" => CallEventKind::Proceed,
            "reject" => CallEventKind::Reject {
                reason: extract_jmi_reason(child),
                tie_break: has_tie_break(child),
            },
            "retract" => CallEventKind::Retract {
                reason: extract_jmi_reason(child),
                tie_break: has_tie_break(child),
            },
            "finish" => CallEventKind::Finish {
                reason: extract_jmi_reason(child),
                migrated_to: extract_migrated_to(child),
            },
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
    let sid = SessionId(jingle.attr("sid")?.to_string());
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

/// Pull the typed XEP-0166 §7.4 condition out of a `<reason/>`
/// child. Unknown condition names (e.g. a non-conforming server)
/// resolve to `None` rather than passing through as an opaque
/// string — the typed-payloads hard rule says protocol data
/// crosses the parser boundary as typed values exactly once, and
/// the untyped form is dropped immediately.
fn extract_terminate_reason(jingle: &Element) -> Option<JingleReason> {
    let reason = jingle.children().find(|c| c.name() == "reason")?;
    let condition = reason.children().next()?;
    jingle_reason_from_wire_name(condition.name())
}

fn extract_jmi_reason(envelope: &Element) -> Option<JingleReason> {
    let reason = envelope
        .children()
        .find(|c| c.name() == "reason" && c.ns() == NS_JINGLE)?;
    let condition = reason.children().next()?;
    jingle_reason_from_wire_name(condition.name())
}

fn has_tie_break(envelope: &Element) -> bool {
    envelope
        .children()
        .any(|c| c.name() == "tie-break" && c.ns() == NS_JINGLE_MESSAGE)
}

fn extract_migrated_to(envelope: &Element) -> Option<SessionId> {
    envelope
        .children()
        .find(|c| c.name() == "migrated" && c.ns() == NS_JINGLE_MESSAGE)
        .and_then(|c| c.attr("to"))
        .map(|to| SessionId(to.to_string()))
}

/// Canonical XEP-0166 §7.4 wire name for a typed `JingleReason`.
/// Public so wasm/FFI consumers can emit the typed value as the
/// stable XEP-defined string when crossing into untyped languages
/// (the JS side carries it as a string; UniFFI carries the typed
/// enum and never needs this).
pub fn jingle_reason_wire_name(reason: JingleReason) -> &'static str {
    match reason {
        JingleReason::AlternativeSession { .. } => "alternative-session",
        JingleReason::Busy => "busy",
        JingleReason::Cancel => "cancel",
        JingleReason::ConnectivityError => "connectivity-error",
        JingleReason::Decline => "decline",
        JingleReason::Expired => "expired",
        JingleReason::FailedApplication => "failed-application",
        JingleReason::FailedTransport => "failed-transport",
        JingleReason::GeneralError => "general-error",
        JingleReason::Gone => "gone",
        JingleReason::IncompatibleParameters => "incompatible-parameters",
        JingleReason::MediaError => "media-error",
        JingleReason::SecurityError => "security-error",
        JingleReason::Success => "success",
        JingleReason::Timeout => "timeout",
        JingleReason::UnsupportedApplications => "unsupported-applications",
        JingleReason::UnsupportedTransports => "unsupported-transports",
    }
}

/// Parse a XEP-0166 §7.4 wire condition name back into a typed
/// `JingleReason`. xmpp-parsers 0.22 dropped the `FromStr` impl on
/// `Reason`, so we own the table here. Unknown names resolve to
/// `None` per the typed-payloads hard rule (no opaque-string
/// fallback into typed code).
pub fn jingle_reason_from_wire_name(name: &str) -> Option<JingleReason> {
    Some(match name {
        "alternative-session" => JingleReason::AlternativeSession { sid: None },
        "busy" => JingleReason::Busy,
        "cancel" => JingleReason::Cancel,
        "connectivity-error" => JingleReason::ConnectivityError,
        "decline" => JingleReason::Decline,
        "expired" => JingleReason::Expired,
        "failed-application" => JingleReason::FailedApplication,
        "failed-transport" => JingleReason::FailedTransport,
        "general-error" => JingleReason::GeneralError,
        "gone" => JingleReason::Gone,
        "incompatible-parameters" => JingleReason::IncompatibleParameters,
        "media-error" => JingleReason::MediaError,
        "security-error" => JingleReason::SecurityError,
        "success" => JingleReason::Success,
        "timeout" => JingleReason::Timeout,
        "unsupported-applications" => JingleReason::UnsupportedApplications,
        "unsupported-transports" => JingleReason::UnsupportedTransports,
        _ => return None,
    })
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
pub fn build_propose(sid: &SessionId, media: CallMedia) -> Element {
    let mut builder = Element::builder("propose", NS_JINGLE_MESSAGE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), sid.0.as_str());
    if media.audio {
        builder = builder.append(rtp_description_element("audio"));
    }
    if media.video {
        builder = builder.append(rtp_description_element("video"));
    }
    builder.build()
}

pub fn build_proceed(sid: &SessionId) -> Element {
    Element::builder("proceed", NS_JINGLE_MESSAGE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), sid.0.as_str())
        .build()
}

pub fn build_reject(sid: &SessionId) -> Element {
    build_reject_with_options(sid, None, false)
}

pub fn build_reject_with_options(
    sid: &SessionId,
    reason: Option<JingleReason>,
    tie_break: bool,
) -> Element {
    let mut builder = Element::builder("reject", NS_JINGLE_MESSAGE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), sid.0.as_str())
        .append_all(reason.map(reason_element));
    if tie_break {
        builder = builder.append(Element::builder("tie-break", NS_JINGLE_MESSAGE).build());
    }
    builder.build()
}

pub fn build_retract(sid: &SessionId) -> Element {
    build_retract_with_options(sid, None, false)
}

pub fn build_retract_with_options(
    sid: &SessionId,
    reason: Option<JingleReason>,
    tie_break: bool,
) -> Element {
    let mut builder = Element::builder("retract", NS_JINGLE_MESSAGE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), sid.0.as_str())
        .append_all(reason.map(reason_element));
    if tie_break {
        builder = builder.append(Element::builder("tie-break", NS_JINGLE_MESSAGE).build());
    }
    builder.build()
}

pub fn build_finish(sid: &SessionId) -> Element {
    build_finish_with_reason(sid, None)
}

pub fn build_finish_with_reason(sid: &SessionId, reason: Option<JingleReason>) -> Element {
    build_finish_with_options(sid, reason, None)
}

pub fn build_finish_with_options(
    sid: &SessionId,
    reason: Option<JingleReason>,
    migrated_to: Option<&SessionId>,
) -> Element {
    let mut builder = Element::builder("finish", NS_JINGLE_MESSAGE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), sid.0.as_str())
        .append_all(reason.map(reason_element));
    if let Some(to) = migrated_to {
        builder = builder.append(
            Element::builder("migrated", NS_JINGLE_MESSAGE)
                .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.0.as_str())
                .build(),
        );
    }
    builder.build()
}

pub fn build_finish_migrated(
    sid: &SessionId,
    reason: JingleReason,
    migrated_to: &SessionId,
) -> Element {
    build_finish_with_options(sid, Some(reason), Some(migrated_to))
}

fn reason_element(reason: JingleReason) -> Element {
    Element::builder("reason", NS_JINGLE)
        .append(Element::builder(jingle_reason_wire_name(reason), NS_JINGLE).build())
        .build()
}

/// Wrap a JMI body (`<propose/>` / `<proceed/>` / `<reject/>` /
/// `<retract/>` / `<finish/>`) in the XEP-0353 §3-conformant
/// `<message type='chat'>` envelope. The envelope MUST be `type='chat'`
/// and MUST carry the XEP-0334 `<store/>` hint so MAM archives keep
/// the call timeline reconstructible even when the body is empty.
///
/// The envelope is constructed via the typed
/// [`xmpp_parsers::message::Message`] so `type='chat'` flows through the
/// dedicated `MessageType::Chat` variant instead of an ad-hoc attribute
/// (typed-payloads rule). The store hint is the only payload Waddle
/// adds — the JMI body itself is appended next so the responder's
/// parser still sees it as the first non-hint child.
pub fn wrap_jmi_message(to: &Jid, jmi: Element) -> Element {
    let mut msg = Message::new_with_type(MessageType::Chat, Some(to.clone()));
    msg.payloads.push(store_hint_element());
    msg.payloads.push(jmi);
    Element::from(msg)
}

fn store_hint_element() -> Element {
    Element::builder("store", NS_HINTS).build()
}

/// Build the `<jingle/>` body of a session-initiate IQ. The
/// `initiator` attribute is required so the receiving server's Jingle
/// handler can verify the call originator and namespace the LiveKit
/// room scope. One `<content/>` per offered media kind, each
/// carrying an empty Waddle LiveKit transport for the server to
/// populate.
pub fn build_session_initiate(sid: &SessionId, initiator: &FullJid, media: CallMedia) -> Element {
    let mut builder = Element::builder("jingle", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-initiate",
        )
        .attr(
            minidom::rxml::xml_ncname!("initiator").to_owned(),
            initiator.to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid.0.as_str());
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
/// party. The `initiator` attribute is intentionally omitted
/// because XEP-0166 recommends it only for `session-initiate` and
/// says recipients should ignore it on other actions.
pub fn build_session_accept(sid: &SessionId, responder: &FullJid, media: CallMedia) -> Element {
    let mut builder = Element::builder("jingle", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-accept",
        )
        .attr(
            minidom::rxml::xml_ncname!("responder").to_owned(),
            responder.to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid.0.as_str());
    if media.audio {
        builder = builder.append(content_element("audio"));
    }
    if media.video {
        builder = builder.append(content_element("video"));
    }
    builder.build()
}

/// Build the `<jingle/>` body of a session-terminate IQ. `reason`
/// is one of the XEP-0166 §7.4 condition values; `None` omits the
/// `<reason/>` child. Using the typed [`xmpp_parsers::jingle::Reason`]
/// enum here means we cannot ship a malformed condition over the
/// wire even if a string argument got mishandled at a higher layer.
pub fn build_session_terminate(
    sid: &SessionId,
    reason: Option<xmpp_parsers::jingle::Reason>,
) -> Element {
    let mut builder = Element::builder("jingle", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-terminate",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid.0.as_str());
    if let Some(condition) = reason {
        let reason_elem = Element::builder("reason", NS_JINGLE)
            .append(Element::from(condition))
            .build();
        builder = builder.append(reason_elem);
    }
    builder.build()
}

fn rtp_description_element(media: &str) -> Element {
    // XEP-0167 §3.3: advertise `<rtcp-mux/>` on every RTP description.
    // LiveKit multiplexes RTP and RTCP on a single port and refuses
    // separate RTCP, so omitting this is a protocol downgrade against
    // every modern WebRTC peer.
    Element::builder("description", NS_JINGLE_RTP)
        .attr(minidom::rxml::xml_ncname!("media").to_owned(), media)
        .append(Element::builder("rtcp-mux", NS_JINGLE_RTP).build())
        .build()
}

fn content_element(media: &str) -> Element {
    Element::builder("content", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("creator").to_owned(),
            "initiator",
        )
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), media)
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
        assert_eq!(ev.sid.0, "c1");
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
        assert!(matches!(
            ev.kind,
            CallEventKind::Finish {
                reason: None,
                migrated_to: None
            }
        ));
    }

    #[test]
    fn parses_tie_break_reject_and_retract_metadata() {
        let xml = r#"<message xmlns='jabber:client' from='alice@waddle.test/desktop'>
            <reject xmlns='urn:xmpp:jingle-message:0' id='c1'>
              <reason xmlns='urn:xmpp:jingle:1'><expired/></reason>
              <tie-break xmlns='urn:xmpp:jingle-message:0'/>
            </reject>
        </message>"#;
        let elem: Element = xml.parse().unwrap();
        let ev = parse_call_event(&elem).expect("reject parses");
        match ev.kind {
            CallEventKind::Reject { reason, tie_break } => {
                assert_eq!(reason, Some(JingleReason::Expired));
                assert!(tie_break);
            }
            other => panic!("expected Reject, got {other:?}"),
        }

        let xml = r#"<message xmlns='jabber:client' from='alice@waddle.test/desktop'>
            <retract xmlns='urn:xmpp:jingle-message:0' id='c1'>
              <reason xmlns='urn:xmpp:jingle:1'><expired/></reason>
              <tie-break xmlns='urn:xmpp:jingle-message:0'/>
            </retract>
        </message>"#;
        let elem: Element = xml.parse().unwrap();
        let ev = parse_call_event(&elem).expect("retract parses");
        match ev.kind {
            CallEventKind::Retract { reason, tie_break } => {
                assert_eq!(reason, Some(JingleReason::Expired));
                assert!(tie_break);
            }
            other => panic!("expected Retract, got {other:?}"),
        }
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
        assert_eq!(ev.sid.0, "c1");
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
                // The wire `<success/>` parses into the typed
                // variant — not a raw string.
                assert_eq!(reason, Some(JingleReason::Success));
            }
            other => panic!("expected SessionTerminate, got {other:?}"),
        }
    }

    #[test]
    fn jingle_reason_wire_names_match_xep_0166_spec() {
        // XEP-0166 §7.4 normative wire condition names. Every
        // typed variant must serialise to the spec-defined string;
        // every spec-defined string must round-trip back to the
        // same variant via `JingleReason::from_str`. A typo in any
        // single arm of `jingle_reason_wire_name` (the table used
        // by the wasm chat client to emit the reason to JS) would
        // be invisible at runtime until a peer rejected the
        // stanza — this table-driven test makes the failure
        // catchable at PR time instead.
        let cases: &[(JingleReason, &str)] = &[
            (
                JingleReason::AlternativeSession { sid: None },
                "alternative-session",
            ),
            (JingleReason::Busy, "busy"),
            (JingleReason::Cancel, "cancel"),
            (JingleReason::ConnectivityError, "connectivity-error"),
            (JingleReason::Decline, "decline"),
            (JingleReason::Expired, "expired"),
            (JingleReason::FailedApplication, "failed-application"),
            (JingleReason::FailedTransport, "failed-transport"),
            (JingleReason::GeneralError, "general-error"),
            (JingleReason::Gone, "gone"),
            (
                JingleReason::IncompatibleParameters,
                "incompatible-parameters",
            ),
            (JingleReason::MediaError, "media-error"),
            (JingleReason::SecurityError, "security-error"),
            (JingleReason::Success, "success"),
            (JingleReason::Timeout, "timeout"),
            (
                JingleReason::UnsupportedApplications,
                "unsupported-applications",
            ),
            (
                JingleReason::UnsupportedTransports,
                "unsupported-transports",
            ),
        ];
        for (variant, expected) in cases {
            assert_eq!(
                jingle_reason_wire_name(variant.clone()),
                *expected,
                "wire name for {variant:?} must match XEP-0166 §7.4"
            );
            let round_tripped = jingle_reason_from_wire_name(expected)
                .expect("XEP wire name parses back to JingleReason");
            assert_eq!(
                &round_tripped, variant,
                "{expected} must round-trip to {variant:?} via FromStr"
            );
        }
    }

    #[test]
    fn session_terminate_unknown_condition_drops_to_none() {
        // Non-conforming servers MUST NOT leak unknown reason
        // names through the typed boundary. Parser surfaces None.
        let xml = r#"<iq xmlns='jabber:client' type='set' from='bob@waddle.test/d' id='t1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-terminate' sid='c1'>
              <reason><not-a-real-condition/></reason>
            </jingle>
        </iq>"#;
        let elem: Element = xml.parse().unwrap();
        let ev = parse_call_event(&elem).expect("session-terminate parses");
        match ev.kind {
            CallEventKind::SessionTerminate { reason } => assert_eq!(reason, None),
            other => panic!("expected SessionTerminate, got {other:?}"),
        }
    }

    fn sid(s: &str) -> SessionId {
        SessionId(s.to_string())
    }

    fn full(s: &str) -> FullJid {
        s.parse().unwrap()
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
        let elem = build_propose(&sid("c1"), CallMedia::audio_video());
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
        let elem = build_propose(&sid("c1"), CallMedia::audio_only());
        let media: Vec<_> = elem
            .children()
            .filter(|c| c.name() == "description")
            .filter_map(|c| c.attr("media"))
            .collect();
        assert_eq!(media, vec!["audio"]);
    }

    #[test]
    fn build_propose_descriptions_include_rtcp_mux() {
        let elem = build_propose(&sid("c1"), CallMedia::audio_video());
        for desc in elem
            .children()
            .filter(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
        {
            assert!(
                desc.children()
                    .any(|c| c.name() == "rtcp-mux" && c.ns() == NS_JINGLE_RTP),
                "XEP-0167 §3.3: <description/> must advertise <rtcp-mux/>"
            );
        }
    }

    #[test]
    fn build_session_initiate_descriptions_include_rtcp_mux() {
        let initiator: FullJid = "alice@waddle.test/desktop".parse().unwrap();
        let elem = build_session_initiate(&sid("c1"), &initiator, CallMedia::audio_video());
        for content in elem.children().filter(|c| c.name() == "content") {
            let desc = content
                .children()
                .find(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
                .expect("each content carries an RTP description");
            assert!(
                desc.children()
                    .any(|c| c.name() == "rtcp-mux" && c.ns() == NS_JINGLE_RTP),
                "XEP-0167 §3.3 conformance on session-initiate"
            );
        }
    }

    #[test]
    fn build_session_accept_descriptions_include_rtcp_mux() {
        let responder: FullJid = "bob@waddle.test/desktop".parse().unwrap();
        let elem = build_session_accept(&sid("c1"), &responder, CallMedia::audio_video());
        for content in elem.children().filter(|c| c.name() == "content") {
            let desc = content
                .children()
                .find(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
                .expect("each content carries an RTP description");
            assert!(
                desc.children()
                    .any(|c| c.name() == "rtcp-mux" && c.ns() == NS_JINGLE_RTP),
                "XEP-0167 §3.3 conformance on session-accept"
            );
        }
    }

    #[test]
    fn build_jmi_helpers_roundtrip_through_parser() {
        // proceed: wrapping in a <message/> with a `from` makes the
        // inbound parser pick it up.
        let stanza = Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "bob@waddle.test/desktop",
            )
            .append(build_proceed(&sid("c1")))
            .build();
        let ev = parse_call_event(&stanza).expect("proceed parses");
        assert!(matches!(ev.kind, CallEventKind::Proceed));

        let stanza = Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "bob@waddle.test/desktop",
            )
            .append(build_reject(&sid("c1")))
            .build();
        let ev = parse_call_event(&stanza).expect("reject parses");
        assert!(matches!(
            ev.kind,
            CallEventKind::Reject {
                reason: None,
                tie_break: false
            }
        ));

        let stanza = Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "alice@waddle.test/desktop",
            )
            .append(build_retract(&sid("c1")))
            .build();
        let ev = parse_call_event(&stanza).expect("retract parses");
        assert!(matches!(
            ev.kind,
            CallEventKind::Retract {
                reason: None,
                tie_break: false
            }
        ));

        let stanza = Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "alice@waddle.test/desktop",
            )
            .append(build_finish(&sid("c1")))
            .build();
        let ev = parse_call_event(&stanza).expect("finish parses");
        assert!(matches!(
            ev.kind,
            CallEventKind::Finish {
                reason: None,
                migrated_to: None
            }
        ));
    }

    #[test]
    fn build_tie_break_jmi_helpers_emit_expired_reason_and_tie_break() {
        let reject = build_reject_with_options(&sid("c1"), Some(JingleReason::Expired), true);
        assert!(
            reject.get_child("tie-break", NS_JINGLE_MESSAGE).is_some(),
            "XEP-0353 tie-break reject carries <tie-break/>"
        );
        assert!(
            reject
                .get_child("reason", NS_JINGLE)
                .and_then(|reason| reason.get_child("expired", NS_JINGLE))
                .is_some(),
            "XEP-0353 tie-break reject carries <reason><expired/></reason>"
        );

        let retract = build_retract_with_options(&sid("c1"), Some(JingleReason::Expired), true);
        assert!(retract.get_child("tie-break", NS_JINGLE_MESSAGE).is_some());
        assert!(retract
            .get_child("reason", NS_JINGLE)
            .and_then(|reason| reason.get_child("expired", NS_JINGLE))
            .is_some());
    }

    #[test]
    fn build_finish_migrated_emits_expired_reason_and_migrated_target() {
        let finish = build_finish_migrated(&sid("old"), JingleReason::Expired, &sid("new"));
        assert!(finish
            .get_child("reason", NS_JINGLE)
            .and_then(|reason| reason.get_child("expired", NS_JINGLE))
            .is_some());
        let migrated = finish
            .get_child("migrated", NS_JINGLE_MESSAGE)
            .expect("finish carries migrated child");
        assert_eq!(migrated.attr("to"), Some("new"));

        let stanza = Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "alice@waddle.test/desktop",
            )
            .append(finish)
            .build();
        let ev = parse_call_event(&stanza).expect("finish parses");
        match ev.kind {
            CallEventKind::Finish {
                reason,
                migrated_to,
            } => {
                assert_eq!(reason, Some(JingleReason::Expired));
                assert_eq!(migrated_to.map(|sid| sid.0).as_deref(), Some("new"));
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn build_session_initiate_carries_empty_waddle_transport_per_content() {
        let jingle = build_session_initiate(
            &sid("c1"),
            &full("alice@waddle.test/desktop"),
            CallMedia::audio_video(),
        );
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
            &sid("c1"),
            &full("bob@waddle.test/desktop"),
            CallMedia::audio_only(),
        );
        assert_eq!(jingle.attr("action"), Some("session-accept"));
        assert_eq!(jingle.attr("initiator"), None);
        assert_eq!(jingle.attr("responder"), Some("bob@waddle.test/desktop"));
        let contents: Vec<_> = jingle
            .children()
            .filter(|c| c.name() == "content")
            .collect();
        assert_eq!(contents.len(), 1);
    }

    #[test]
    fn build_session_terminate_includes_reason_when_supplied() {
        let with_reason =
            build_session_terminate(&sid("c1"), Some(xmpp_parsers::jingle::Reason::Success));
        assert_eq!(with_reason.attr("initiator"), None);
        let reason_elem = with_reason
            .children()
            .find(|c| c.name() == "reason")
            .expect("reason child");
        assert!(reason_elem.children().any(|c| c.name() == "success"));

        let without = build_session_terminate(&sid("c1"), None);
        assert!(without.children().all(|c| c.name() != "reason"));
    }

    #[test]
    fn wrap_jmi_message_stamps_type_chat_and_store_hint() {
        // XEP-0353 §3: every JMI message (propose / proceed / reject /
        // retract / finish) MUST be `type='chat'` and MUST contain a
        // XEP-0334 `<store/>` hint. Without this envelope, JMI stanzas
        // ship as `type='normal'` and skip MAM archival, breaking call
        // history reconstruction.
        let to: Jid = "bob@waddle.test".parse().unwrap();
        let stanza = wrap_jmi_message(&to, build_propose(&sid("c1"), CallMedia::audio_video()));
        assert_eq!(stanza.name(), "message");
        assert_eq!(stanza.attr("type"), Some("chat"));
        assert_eq!(stanza.attr("to"), Some("bob@waddle.test"));
        let store = stanza
            .children()
            .find(|c| c.name() == "store" && c.ns() == NS_HINTS)
            .expect("XEP-0334 <store/> hint required by XEP-0353 §3");
        assert!(store.children().next().is_none());
        // The JMI body itself rides along, so the responder's parser
        // still surfaces it via parse_jmi_message → CallEventKind.
        let ev = parse_call_event(
            &Element::builder("message", "jabber:client")
                .attr(
                    minidom::rxml::xml_ncname!("from").to_owned(),
                    "alice@waddle.test/desktop",
                )
                .attr(
                    minidom::rxml::xml_ncname!("type").to_owned(),
                    stanza.attr("type").unwrap_or_default(),
                )
                .append_all(stanza.children().cloned())
                .build(),
        )
        .expect("wrapped JMI body still parses as a call event");
        assert!(matches!(ev.kind, CallEventKind::Propose { .. }));
    }

    #[test]
    fn wrap_jmi_message_preserves_jmi_body_for_every_variant() {
        // Every JMI variant must survive the envelope.
        let to: Jid = "bob@waddle.test".parse().unwrap();
        let cases: Vec<(&str, Element)> = vec![
            (
                "propose",
                build_propose(&sid("c1"), CallMedia::audio_only()),
            ),
            ("proceed", build_proceed(&sid("c1"))),
            ("reject", build_reject(&sid("c1"))),
            ("retract", build_retract(&sid("c1"))),
            ("finish", build_finish(&sid("c1"))),
        ];
        for (name, body) in cases {
            let stanza = wrap_jmi_message(&to, body);
            assert_eq!(stanza.attr("type"), Some("chat"), "{name}: type=chat");
            assert!(
                stanza
                    .children()
                    .any(|c| c.name() == name && c.ns() == NS_JINGLE_MESSAGE),
                "{name}: JMI body preserved"
            );
            assert!(
                stanza
                    .children()
                    .any(|c| c.name() == "store" && c.ns() == NS_HINTS),
                "{name}: store hint attached"
            );
        }
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
