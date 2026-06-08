//! Waddle call-thread anchor marker (`urn:waddle:call-thread:0`).
//!
//! The conversation thread remains a standard XEP-0201 `<thread/>`. This
//! project-local marker only links that thread to the call session.

use chrono::{DateTime, Utc};
use jid::BareJid;
use minidom::Element;
use xmpp_parsers::jingle::SessionId;
use xmpp_parsers::message::Message;

pub const NS_WADDLE_CALL_THREAD: &str = "urn:waddle:call-thread:0";
pub const NS_FASTEN: &str = "urn:xmpp:fasten:0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallThreadKind {
    Dm,
    Muc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallThreadMedia {
    pub audio: bool,
    pub video: bool,
}

impl CallThreadMedia {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallThreadAnchor {
    pub kind: CallThreadKind,
    pub sid: SessionId,
    pub media: CallThreadMedia,
    pub initiator: BareJid,
    pub started: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallThreadDuration(String);

impl CallThreadDuration {
    pub fn parse(value: &str) -> Result<Self, CallThreadParseError> {
        if is_valid_call_thread_duration(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(CallThreadParseError::InvalidDuration)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallThreadEnded {
    pub ended: DateTime<Utc>,
    pub duration: CallThreadDuration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallThreadParseError {
    NotCallThread,
    MissingAttribute(&'static str),
    InvalidKind,
    InvalidMedia,
    InvalidInitiator,
    InvalidStarted,
    InvalidEnded,
    InvalidDuration,
}

pub fn build_call_thread_anchor(anchor: &CallThreadAnchor) -> Element {
    let kind = match anchor.kind {
        CallThreadKind::Dm => "dm",
        CallThreadKind::Muc => "muc",
    };
    let media = media_attr(anchor.media);

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

pub fn parse_call_thread_anchor(
    element: &Element,
) -> Result<CallThreadAnchor, CallThreadParseError> {
    if element.name() != "call-thread" || element.ns() != NS_WADDLE_CALL_THREAD {
        return Err(CallThreadParseError::NotCallThread);
    }

    let kind = match required_attr(element, "kind")? {
        "dm" => CallThreadKind::Dm,
        "muc" => CallThreadKind::Muc,
        _ => return Err(CallThreadParseError::InvalidKind),
    };

    let sid = SessionId(required_attr(element, "sid")?.to_owned());
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

pub fn build_call_thread_ended(ended: &CallThreadEnded) -> Element {
    Element::builder("call-thread-ended", NS_WADDLE_CALL_THREAD)
        .attr(
            minidom::rxml::xml_ncname!("ended").to_owned(),
            ended
                .ended
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
        .attr(
            minidom::rxml::xml_ncname!("duration").to_owned(),
            ended.duration.as_str(),
        )
        .build()
}

pub fn parse_call_thread_ended(element: &Element) -> Result<CallThreadEnded, CallThreadParseError> {
    if element.name() != "call-thread-ended" || element.ns() != NS_WADDLE_CALL_THREAD {
        return Err(CallThreadParseError::NotCallThread);
    }

    let ended = DateTime::parse_from_rfc3339(required_attr(element, "ended")?)
        .map_err(|_| CallThreadParseError::InvalidEnded)?
        .with_timezone(&Utc);
    let duration = CallThreadDuration::parse(required_attr(element, "duration")?)?;

    Ok(CallThreadEnded { ended, duration })
}

pub fn parse_call_thread_anchor_child(message: &Message) -> Option<CallThreadAnchor> {
    message
        .payloads
        .iter()
        .find(|payload| payload.name() == "call-thread" && payload.ns() == NS_WADDLE_CALL_THREAD)
        .and_then(|payload| parse_call_thread_anchor(payload).ok())
}

pub fn parse_call_thread_ended_child(message: &Message) -> Option<CallThreadEnded> {
    message
        .payloads
        .iter()
        .find_map(call_thread_ended_payload)
        .and_then(|payload| parse_call_thread_ended(payload).ok())
}

fn call_thread_ended_payload(payload: &Element) -> Option<&Element> {
    if payload.name() == "call-thread-ended" && payload.ns() == NS_WADDLE_CALL_THREAD {
        return Some(payload);
    }
    if payload.name() != "apply-to" || payload.ns() != NS_FASTEN {
        return None;
    }
    payload
        .children()
        .find(|child| child.name() == "call-thread-ended" && child.ns() == NS_WADDLE_CALL_THREAD)
}

fn media_attr(media: CallThreadMedia) -> &'static str {
    match (media.audio, media.video) {
        (true, true) => "audio video",
        (true, false) => "audio",
        (false, true) => "video",
        (false, false) => {
            panic!("call-thread marker requires audio or video media")
        }
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
            sid: SessionId("session-123".to_owned()),
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
}
