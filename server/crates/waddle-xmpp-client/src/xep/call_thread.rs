//! Waddle call-thread anchor marker.
//!
//! The thread itself is XEP-0201's `<thread/>`; this marker only links that
//! thread to the call session that created it.

use chrono::{DateTime, Utc};
use jid::BareJid;
use minidom::Element;
use xmpp_parsers::jingle::SessionId;

/// Protocol token `urn:waddle:call-thread:0`.
pub const NS_WADDLE_CALL_THREAD: &str = "urn:waddle:call-thread:0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Whether a call-thread anchor belongs to a direct message or MUC room.
pub enum CallThreadKind {
    /// A direct-message conversation with a single peer.
    Dm,
    /// A multi-user chat room or service.
    Muc,
}

impl CallThreadKind {
    /// Canonical wire/storage token for the kind: `"dm"` or `"muc"`.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            CallThreadKind::Dm => "dm",
            CallThreadKind::Muc => "muc",
        }
    }

    /// Parse a wire token (`"dm"`/`"muc"`) into a typed kind.
    ///
    /// # Errors
    ///
    /// Returns `InvalidKind` unless the token names a supported call-thread kind.
    pub fn parse_token(token: &str) -> Result<Self, CallThreadParseError> {
        match token {
            "dm" => Ok(CallThreadKind::Dm),
            "muc" => Ok(CallThreadKind::Muc),
            _ => Err(CallThreadParseError::InvalidKind),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Supported media advertised by a call-thread anchor.
pub struct CallThreadMedia {
    /// Whether this call supports audio media.
    pub audio: bool,
    /// Whether this call supports video media.
    pub video: bool,
}

impl CallThreadMedia {
    /// Create media capabilities with audio enabled and video disabled.
    #[must_use]
    pub const fn audio_only() -> Self {
        Self {
            audio: true,
            video: false,
        }
    }

    /// Create media capabilities with audio and video enabled.
    #[must_use]
    pub const fn audio_video() -> Self {
        Self {
            audio: true,
            video: true,
        }
    }

    /// Parse a space-separated media token list (e.g. `"audio video"`).
    ///
    /// # Errors
    ///
    /// Returns `InvalidMedia` for an empty media list or tokens other than `audio` and `video`; repeated supported tokens are accepted.
    pub fn parse_tokens(value: &str) -> Result<Self, CallThreadParseError> {
        parse_media(value)
    }

    /// Ordered media tokens (`audio` before `video`) for the wasm boundary.
    #[must_use]
    pub fn token_list(self) -> Vec<String> {
        let mut tokens = Vec::with_capacity(2);
        if self.audio {
            tokens.push("audio".to_owned());
        }
        if self.video {
            tokens.push("video".to_owned());
        }
        tokens
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Stable call-session identity, initiator, media, and start time attached to a conversation thread.
pub struct CallThreadAnchor {
    /// Discriminator selecting the interpretation of this payload.
    pub kind: CallThreadKind,
    /// Call-session identifier used to correlate call signals.
    pub sid: SessionId,
    /// Media types supported by this call.
    pub media: CallThreadMedia,
    /// XMPP address of the user who initiated the call.
    pub initiator: BareJid,
    /// Timestamp when the call started.
    pub started: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// End timestamp and duration targeting a previously announced call-thread anchor.
pub struct CallThreadEnded {
    /// Identifier of the anchor associated with this value.
    pub anchor_id: String,
    /// Timestamp when the call ended.
    pub ended: DateTime<Utc>,
    /// Duration associated with this operation.
    pub duration: CallThreadDuration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Validated duration token used by a call-thread end annotation.
pub struct CallThreadDuration(String);

impl CallThreadDuration {
    /// Parse a supported wire value, rejecting unrecognized or malformed input.
    ///
    /// # Errors
    ///
    /// Returns `InvalidDuration` unless the token uses the supported nonnegative duration syntax.
    pub fn parse(value: &str) -> Result<Self, CallThreadParseError> {
        if is_valid_call_thread_duration(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(CallThreadParseError::InvalidDuration)
        }
    }

    /// Borrow the stored text without allocating.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Validation failures for call-thread roots, required attributes, media, identities, and timestamps.
#[non_exhaustive]
pub enum CallThreadParseError {
    /// The payload is not a supported call-thread anchor.
    NotCallThread,
    /// A required call-thread attribute was missing; contains its local name.
    MissingAttribute(&'static str),
    /// The call-thread kind is unsupported.
    InvalidKind,
    /// The call advertises unsupported media.
    InvalidMedia,
    /// The call initiator is not a valid JID.
    InvalidInitiator,
    /// The call start timestamp is not valid.
    InvalidStarted,
    /// The call end timestamp is not valid.
    InvalidEnded,
    /// The call duration is not in the supported format.
    InvalidDuration,
}

/// Build the call-thread anchor linking a conversation thread to its media session.
///
/// # Panics
///
/// Panics if both `anchor.media.audio` and `anchor.media.video` are false.
#[must_use]
pub fn build_call_thread_anchor(anchor: &CallThreadAnchor) -> Element {
    let kind = match anchor.kind {
        CallThreadKind::Dm => "dm",
        CallThreadKind::Muc => "muc",
    };
    let media = media_attr(anchor.media).expect("call-thread marker requires audio or video media");

    Element::builder("call-thread", NS_WADDLE_CALL_THREAD)
        .attr(minidom::rxml::xml_ncname!("kind").to_owned(), kind)
        .attr(
            minidom::rxml::xml_ncname!("sid").to_owned(),
            anchor.sid.0.as_str(),
        )
        .attr(minidom::rxml::xml_ncname!("media").to_owned(), media)
        .attr(
            minidom::rxml::xml_ncname!("initiator").to_owned(),
            anchor.initiator.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("started").to_owned(),
            anchor
                .started
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
        .build()
}

/// Validate a call-thread marker and decode its session, media, initiator, and start timestamp.
///
/// # Errors
///
/// Returns a typed parse error for a non-call-thread element or a missing/invalid identifier, kind, media, initiator JID, or start timestamp.
pub fn parse_call_thread_anchor(
    element: &Element,
) -> Result<CallThreadAnchor, CallThreadParseError> {
    if element.name() != "call-thread" || element.ns() != NS_WADDLE_CALL_THREAD {
        return Err(CallThreadParseError::NotCallThread);
    }

    let kind = CallThreadKind::parse_token(required_attr(element, "kind")?)?;

    let sid = SessionId(required_attr(element, "sid")?.to_string());
    let media = parse_media(required_attr(element, "media")?)?;
    let initiator = required_attr(element, "initiator")?
        .parse::<BareJid>()
        .map_err(|_| CallThreadParseError::InvalidInitiator)?;
    let started = DateTime::parse_from_rfc3339(required_attr(element, "started")?)
        .map_err(|_| CallThreadParseError::InvalidStarted)?
        .with_timezone(&Utc);

    Ok(CallThreadAnchor {
        kind,
        sid,
        media,
        initiator,
        started,
    })
}

/// Find a call-thread anchor among the message children; absent or malformed anchors return None.
#[must_use]
pub fn parse_call_thread_anchor_child(message: &Element) -> Option<CallThreadAnchor> {
    message
        .get_child("call-thread", NS_WADDLE_CALL_THREAD)
        .and_then(|child| parse_call_thread_anchor(child).ok())
}

/// Find and decode a call-ended annotation among the message children.
#[must_use]
pub fn parse_call_thread_ended_child(message: &Element) -> Option<CallThreadEnded> {
    let apply_to = message.get_child("apply-to", "urn:xmpp:fasten:0")?;
    let anchor_id = required_attr(apply_to, "id").ok()?.to_string();
    let ended = apply_to
        .get_child("call-thread-ended", NS_WADDLE_CALL_THREAD)
        .and_then(|child| parse_call_thread_ended(child, anchor_id).ok())?;
    Some(ended)
}

fn parse_call_thread_ended(
    element: &Element,
    anchor_id: String,
) -> Result<CallThreadEnded, CallThreadParseError> {
    if element.name() != "call-thread-ended" || element.ns() != NS_WADDLE_CALL_THREAD {
        return Err(CallThreadParseError::NotCallThread);
    }
    let ended = DateTime::parse_from_rfc3339(required_attr(element, "ended")?)
        .map_err(|_| CallThreadParseError::InvalidEnded)?
        .with_timezone(&Utc);
    let duration = CallThreadDuration::parse(required_attr(element, "duration")?)?;
    Ok(CallThreadEnded {
        anchor_id,
        ended,
        duration,
    })
}

fn media_attr(media: CallThreadMedia) -> Result<&'static str, CallThreadParseError> {
    match (media.audio, media.video) {
        (true, true) => Ok("audio video"),
        (true, false) => Ok("audio"),
        (false, true) => Ok("video"),
        (false, false) => Err(CallThreadParseError::InvalidMedia),
    }
}

fn required_attr<'a>(
    element: &'a Element,
    name: &'static str,
) -> Result<&'a str, CallThreadParseError> {
    element
        .attr(name)
        .ok_or(CallThreadParseError::MissingAttribute(name))
}

fn parse_media(value: &str) -> Result<CallThreadMedia, CallThreadParseError> {
    let mut audio = false;
    let mut video = false;
    for part in value.split_ascii_whitespace() {
        match part {
            "audio" => audio = true,
            "video" => video = true,
            _ => return Err(CallThreadParseError::InvalidMedia),
        }
    }
    if !audio && !video {
        return Err(CallThreadParseError::InvalidMedia);
    }
    Ok(CallThreadMedia { audio, video })
}

fn is_valid_call_thread_duration(value: &str) -> bool {
    if !value.starts_with("PT") || value.len() <= 2 {
        return false;
    }
    let mut chars = value[2..].chars().peekable();
    let mut saw_component = false;
    while chars.peek().is_some() {
        let mut saw_digit = false;
        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
            saw_digit = true;
            chars.next();
        }
        if !saw_digit {
            return false;
        }
        match chars.next() {
            Some('H' | 'M' | 'S') => saw_component = true,
            _ => return false,
        }
    }
    saw_component
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor() -> CallThreadAnchor {
        CallThreadAnchor {
            kind: CallThreadKind::Muc,
            sid: SessionId("session-123".to_string()),
            media: CallThreadMedia::audio_video(),
            initiator: "alice@example.test".parse().expect("initiator"),
            started: "2026-06-07T14:30:00Z"
                .parse::<DateTime<Utc>>()
                .expect("started"),
        }
    }

    #[test]
    fn build_parse_round_trips_call_thread_anchor_marker() {
        let original = anchor();
        let element = build_call_thread_anchor(&original);

        assert_eq!(element.name(), "call-thread");
        assert_eq!(element.ns(), NS_WADDLE_CALL_THREAD);
        assert_eq!(element.attr("kind"), Some("muc"));
        assert_eq!(element.attr("sid"), Some("session-123"));
        assert_eq!(element.attr("media"), Some("audio video"));
        assert_eq!(element.attr("initiator"), Some("alice@example.test"));
        assert_eq!(element.attr("started"), Some("2026-06-07T14:30:00Z"));

        let parsed = parse_call_thread_anchor(&element).expect("marker parses");
        assert_eq!(parsed, original);
    }

    #[test]
    #[should_panic(expected = "call-thread marker requires audio or video media")]
    fn builder_rejects_empty_media() {
        let mut anchor = anchor();
        anchor.media = CallThreadMedia {
            audio: false,
            video: false,
        };

        let _ = build_call_thread_anchor(&anchor);
    }

    #[test]
    fn parses_call_thread_anchor_child_from_message() {
        let xml = r#"<message xmlns='jabber:client' type='groupchat' from='general@muc.example'>
            <body>Alice started a call</body>
            <thread>call-thread-uuid</thread>
            <call-thread xmlns='urn:waddle:call-thread:0'
                         kind='muc'
                         sid='session-uuid'
                         media='audio'
                         initiator='alice@example'
                         started='2026-06-07T14:30:00Z'/>
            <store xmlns='urn:xmpp:hints'/>
        </message>"#;
        let message: Element = xml.parse().expect("message XML");

        let parsed = parse_call_thread_anchor_child(&message).expect("call-thread child");

        assert_eq!(parsed.kind, CallThreadKind::Muc);
        assert_eq!(parsed.sid.0, "session-uuid");
        assert_eq!(parsed.media, CallThreadMedia::audio_only());
        assert_eq!(parsed.initiator.as_str(), "alice@example");
        assert_eq!(
            parsed
                .started
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-06-07T14:30:00Z"
        );
    }
}
