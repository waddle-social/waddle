//! XEP-0483: HTTP Online Meetings
//!
//! Generate and share meeting room URLs for voice/video calls.
//! Integrates with external meeting services (Jitsi, BigBlueButton, etc.).
//!
//! ## XML Format
//!
//! Request a meeting URL:
//! ```xml
//! <iq type='get' to='example.com' id='meet-1'>
//!   <request xmlns='urn:xmpp:http:online-meetings:invite:0'
//!            type='jitsi'/>
//! </iq>
//! ```
//!
//! Server responds with meeting URL:
//! ```xml
//! <iq type='result' from='example.com' id='meet-1'>
//!   <meeting xmlns='urn:xmpp:http:online-meetings:invite:0'
//!            type='jitsi'
//!            url='https://meet.example.com/room-abc123'
//!            desc='Video Call'/>
//! </iq>
//! ```
//!
//! Share in chat:
//! ```xml
//! <message type='groupchat' to='room@muc.example.com'>
//!   <body>Join the call: https://meet.example.com/room-abc123</body>
//!   <meeting xmlns='urn:xmpp:http:online-meetings:invite:0'
//!            type='jitsi'
//!            url='https://meet.example.com/room-abc123'
//!            desc='Team Standup'/>
//! </message>
//! ```

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::Message;

/// Namespace for XEP-0483 HTTP Online Meetings.
pub const NS_ONLINE_MEETINGS: &str = "urn:xmpp:http:online-meetings:invite:0";

/// Meeting service types.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MeetingType {
    /// Jitsi Meet.
    Jitsi,
    /// BigBlueButton.
    BigBlueButton,
    /// Galene.
    Galene,
    /// Custom/other service.
    Custom(String),
}

impl MeetingType {
    /// Parse from string.
    pub fn from_str_attr(s: &str) -> Self {
        match s {
            "jitsi" => Self::Jitsi,
            "bigbluebutton" | "bbb" => Self::BigBlueButton,
            "galene" => Self::Galene,
            other => Self::Custom(other.to_owned()),
        }
    }

    /// Convert to attribute string.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Jitsi => "jitsi",
            Self::BigBlueButton => "bigbluebutton",
            Self::Galene => "galene",
            Self::Custom(s) => s,
        }
    }
}

impl std::fmt::Display for MeetingType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A meeting invitation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meeting {
    /// The meeting service type.
    pub meeting_type: MeetingType,
    /// The meeting URL.
    pub url: String,
    /// Optional description.
    pub desc: Option<String>,
}

impl Meeting {
    /// Create a new meeting.
    pub fn new(meeting_type: MeetingType, url: impl Into<String>) -> Self {
        Self {
            meeting_type,
            url: url.into(),
            desc: None,
        }
    }

    /// Create a Jitsi meeting.
    pub fn jitsi(url: impl Into<String>) -> Self {
        Self::new(MeetingType::Jitsi, url)
    }

    /// Set the description.
    pub fn with_desc(mut self, desc: impl Into<String>) -> Self {
        self.desc = Some(desc.into());
        self
    }
}

/// Trait for types that can carry meeting elements.
pub trait MeetingCarrier {
    /// Extract the meeting from this carrier.
    fn meeting(&self) -> Option<Meeting>;

    /// Returns `true` if this carrier has a meeting element.
    fn has_meeting(&self) -> bool {
        self.meeting().is_some()
    }
}

impl MeetingCarrier for Message {
    fn meeting(&self) -> Option<Meeting> {
        extract_meeting_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a meeting element.
pub fn is_meeting_element(elem: &Element) -> bool {
    elem.ns() == NS_ONLINE_MEETINGS
        && matches!(elem.name(), "meeting" | "request")
}

/// Check if an IQ is a meeting request.
pub fn is_meeting_request(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Get(elem)
        if elem.name() == "request" && elem.ns() == NS_ONLINE_MEETINGS)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract a meeting from a message.
pub fn extract_meeting_from_message(msg: &Message) -> Option<Meeting> {
    let elem = msg.payloads.iter().find(|e| {
        e.ns() == NS_ONLINE_MEETINGS && e.name() == "meeting"
    })?;
    parse_meeting_element(elem)
}

/// Extract a meeting from an IQ result.
pub fn extract_meeting_from_iq(iq: &Iq) -> Option<Meeting> {
    let elem = match &iq.payload {
        IqType::Result(Some(elem))
            if elem.name() == "meeting" && elem.ns() == NS_ONLINE_MEETINGS =>
        {
            elem
        }
        _ => return None,
    };
    parse_meeting_element(elem)
}

/// Parse a meeting element.
fn parse_meeting_element(elem: &Element) -> Option<Meeting> {
    let mt = elem.attr("type").map(MeetingType::from_str_attr)?;
    let url = elem.attr("url").filter(|u| !u.is_empty())?.to_owned();
    let desc = elem.attr("desc").filter(|d| !d.is_empty()).map(|d| d.to_owned());

    Some(Meeting {
        meeting_type: mt,
        url,
        desc,
    })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a meeting request IQ.
pub fn build_meeting_request(to: jid::Jid, meeting_type: &MeetingType, id: &str) -> Iq {
    let elem = Element::builder("request", NS_ONLINE_MEETINGS)
        .attr("type", meeting_type.as_str())
        .build();
    Iq {
        from: None,
        to: Some(to),
        id: id.to_owned(),
        payload: IqType::Get(elem),
    }
}

/// Build a meeting response IQ.
pub fn build_meeting_response(original_iq: &Iq, meeting: &Meeting) -> Iq {
    let mut elem = Element::builder("meeting", NS_ONLINE_MEETINGS)
        .attr("type", meeting.meeting_type.as_str())
        .attr("url", meeting.url.as_str())
        .build();
    if let Some(ref desc) = meeting.desc {
        elem.set_attr("desc", desc);
    }

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(elem)),
    }
}

/// Build a meeting element for inclusion in a message.
pub fn build_meeting_message_element(meeting: &Meeting) -> Element {
    let mut elem = Element::builder("meeting", NS_ONLINE_MEETINGS)
        .attr("type", meeting.meeting_type.as_str())
        .attr("url", meeting.url.as_str())
        .build();
    if let Some(ref desc) = meeting.desc {
        elem.set_attr("desc", desc);
    }
    elem
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a meeting to a message.
pub fn set_meeting(msg: &mut Message, meeting: &Meeting) {
    msg.payloads.retain(|e| e.ns() != NS_ONLINE_MEETINGS);
    msg.payloads.push(build_meeting_message_element(meeting));
}

/// Remove meeting from a message.
pub fn strip_meeting(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_ONLINE_MEETINGS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_meeting_type_roundtrip() {
        assert_eq!(MeetingType::from_str_attr("jitsi"), MeetingType::Jitsi);
        assert_eq!(
            MeetingType::from_str_attr("bigbluebutton"),
            MeetingType::BigBlueButton
        );
        assert_eq!(MeetingType::from_str_attr("galene"), MeetingType::Galene);
        assert_eq!(
            MeetingType::from_str_attr("zoom"),
            MeetingType::Custom("zoom".into())
        );
    }

    #[test]
    fn test_meeting_type_display() {
        assert_eq!(MeetingType::Jitsi.to_string(), "jitsi");
    }

    #[test]
    fn test_is_meeting_request() {
        let iq = build_meeting_request(
            "example.com".parse().expect("valid"),
            &MeetingType::Jitsi,
            "m-1",
        );
        assert!(is_meeting_request(&iq));
    }

    #[test]
    fn test_build_and_extract_iq() {
        let request = build_meeting_request(
            "example.com".parse().expect("valid"),
            &MeetingType::Jitsi,
            "m-1",
        );
        let meeting = Meeting::jitsi("https://meet.example.com/room-1")
            .with_desc("Team Call");
        let response = build_meeting_response(&request, &meeting);

        let extracted = extract_meeting_from_iq(&response).expect("has meeting");
        assert_eq!(extracted.meeting_type, MeetingType::Jitsi);
        assert_eq!(extracted.url, "https://meet.example.com/room-1");
        assert_eq!(extracted.desc.as_deref(), Some("Team Call"));
    }

    #[test]
    fn test_extract_from_message() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Join the call!</body>\
                    <meeting xmlns='urn:xmpp:http:online-meetings:invite:0' type='jitsi' url='https://meet.example.com/abc' desc='Standup'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let m = extract_meeting_from_message(&msg).expect("has meeting");
        assert_eq!(m.meeting_type, MeetingType::Jitsi);
        assert_eq!(m.url, "https://meet.example.com/abc");
        assert_eq!(m.desc.as_deref(), Some("Standup"));
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_meeting_from_message(&msg).is_none());
    }

    #[test]
    fn test_set_meeting() {
        let mut msg = Message::new(None::<jid::Jid>);
        let meeting = Meeting::jitsi("https://meet.example.com/room");
        set_meeting(&mut msg, &meeting);
        assert!(msg.has_meeting());

        // Replace
        let m2 = Meeting::new(MeetingType::Galene, "https://galene.example.com/room");
        set_meeting(&mut msg, &m2);
        let extracted = extract_meeting_from_message(&msg).expect("has meeting");
        assert_eq!(extracted.meeting_type, MeetingType::Galene);
    }

    #[test]
    fn test_strip_meeting() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_meeting(&mut msg, &Meeting::jitsi("https://example.com"));
        strip_meeting(&mut msg);
        assert!(!msg.has_meeting());
    }

    #[test]
    fn test_meeting_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <meeting xmlns='urn:xmpp:http:online-meetings:invite:0' type='jitsi' url='https://example.com/room'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_meeting());
        assert_eq!(msg.meeting().expect("meeting").url, "https://example.com/room");
    }

    #[test]
    fn test_meeting_builders() {
        let m = Meeting::jitsi("https://example.com/room").with_desc("Test");
        assert_eq!(m.meeting_type, MeetingType::Jitsi);
        assert_eq!(m.desc.as_deref(), Some("Test"));
    }
}
