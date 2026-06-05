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
    /// The original stanza recipient, when present. Message carbons
    /// need this to let sibling resources associate self-originated
    /// JMI events with the peer conversation.
    pub to: Option<Jid>,
    pub sid: SessionId,
    pub kind: CallEventKind,
}

const NS_JINGLE: &str = "urn:xmpp:jingle:1";
const NS_JINGLE_MESSAGE: &str = "urn:xmpp:jingle-message:0";
const NS_JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";
const NS_MUJI: &str = "urn:xmpp:jingle:muji:0";
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
    let to = stanza.attr("to").and_then(|s| s.parse::<Jid>().ok());

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
        return Some(InboundCallEvent {
            from,
            to: to.clone(),
            sid,
            kind,
        });
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
    let to = stanza.attr("to").and_then(|s| s.parse::<Jid>().ok());
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
    Some(InboundCallEvent {
        from,
        to,
        sid,
        kind,
    })
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

/// Build the typed Muji `<content/>` advertised inside a Jingle
/// session-initiate to the SFU mixer. This mirrors the MUC presence
/// Muji content shape, but the Jingle content lives in the
/// `urn:xmpp:jingle:1` namespace and includes the empty Waddle LiveKit
/// transport placeholder the server rewrites with join credentials.
pub fn build_muji_jingle_content(media: &str) -> Element {
    let mut description = Element::builder("description", NS_JINGLE_RTP)
        .attr(minidom::rxml::xml_ncname!("media").to_owned(), media);
    match media {
        "audio" => {
            description = description.append(
                Element::builder("payload-type", NS_JINGLE_RTP)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "111")
                    .attr(minidom::rxml::xml_ncname!("name").to_owned(), "opus")
                    .attr(minidom::rxml::xml_ncname!("clockrate").to_owned(), "48000")
                    .attr(minidom::rxml::xml_ncname!("channels").to_owned(), "2")
                    .build(),
            );
        }
        "video" => {
            description = description.append(
                Element::builder("payload-type", NS_JINGLE_RTP)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "96")
                    .attr(minidom::rxml::xml_ncname!("name").to_owned(), "VP8")
                    .attr(minidom::rxml::xml_ncname!("clockrate").to_owned(), "90000")
                    .build(),
            );
        }
        _ => {}
    }
    description = description.append(Element::builder("rtcp-mux", NS_JINGLE_RTP).build());

    Element::builder("content", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("creator").to_owned(),
            "initiator",
        )
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), media)
        .attr(minidom::rxml::xml_ncname!("senders").to_owned(), "both")
        .append(description.build())
        .append(Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT).build())
        .build()
}

pub fn build_muji_session_initiate(
    sid: &SessionId,
    initiator: &FullJid,
    room_jid: &str,
    video: bool,
) -> Element {
    let muji = Element::builder("muji", NS_MUJI)
        .attr(minidom::rxml::xml_ncname!("room").to_owned(), room_jid)
        .build();
    let mut jingle = Element::builder("jingle", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-initiate",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid.0.as_str())
        .attr(
            minidom::rxml::xml_ncname!("initiator").to_owned(),
            initiator.to_string(),
        )
        .append(muji)
        .append(build_muji_jingle_content("audio"));
    if video {
        jingle = jingle.append(build_muji_jingle_content("video"));
    }
    jingle.build()
}

pub fn build_muji_session_terminate(room_jid: &str, sid: &SessionId) -> Element {
    let muji = Element::builder("muji", NS_MUJI)
        .attr(minidom::rxml::xml_ncname!("room").to_owned(), room_jid)
        .build();
    let reason = Element::builder("reason", NS_JINGLE)
        .append(Element::builder("success", NS_JINGLE).build())
        .build();
    Element::builder("jingle", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-terminate",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid.0.as_str())
        .append(muji)
        .append(reason)
        .build()
}

#[cfg(test)]
mod tests;
