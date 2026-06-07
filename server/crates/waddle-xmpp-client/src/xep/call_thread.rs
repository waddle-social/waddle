//! Waddle call-thread anchor marker.
//!
//! The thread itself is XEP-0201's `<thread/>`; this marker only links that
//! thread to the call session that created it.

use chrono::{DateTime, Utc};
use jid::BareJid;
use minidom::Element;
use xmpp_parsers::jingle::SessionId;

pub const NS_WADDLE_CALL_THREAD: &str = "urn:waddle:call-thread:0";

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
pub enum CallThreadParseError {
    NotCallThread,
    MissingAttribute(&'static str),
    InvalidKind,
    InvalidMedia,
    InvalidInitiator,
    InvalidStarted,
}

pub fn build_call_thread_anchor(anchor: &CallThreadAnchor) -> Element {
    let kind = match anchor.kind {
        CallThreadKind::Dm => "dm",
        CallThreadKind::Muc => "muc",
    };
    let media = if anchor.media.audio && anchor.media.video {
        "audio video"
    } else if anchor.media.audio {
        "audio"
    } else if anchor.media.video {
        "video"
    } else {
        ""
    };

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

pub fn parse_call_thread_anchor_child(message: &Element) -> Option<CallThreadAnchor> {
    message
        .get_child("call-thread", NS_WADDLE_CALL_THREAD)
        .and_then(|child| parse_call_thread_anchor(child).ok())
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
