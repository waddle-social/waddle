//! XEP-0471: Calendar Events
//!
//! Community events scheduling via PubSub. Allows creating, sharing,
//! and RSVPing to events within XMPP communities.
//!
//! ## XML Format
//!
//! Event published to a PubSub node:
//! ```xml
//! <item id='event-123' xmlns='http://jabber.org/protocol/pubsub'>
//!   <event xmlns='urn:xmpp:calendar:0'>
//!     <title>Community Game Night</title>
//!     <description>Weekly gaming session</description>
//!     <start>2024-06-15T19:00:00Z</start>
//!     <end>2024-06-15T22:00:00Z</end>
//!     <location>Voice Channel #gaming</location>
//!   </event>
//! </item>
//! ```
//!
//! ## Use Cases
//!
//! - Schedule community meetups and events
//! - Show upcoming events in channels
//! - RSVP tracking (going, interested, not going)
//! - Recurring events

use chrono::{DateTime, Utc};
use minidom::Element;

/// Namespace for XEP-0471 Calendar Events.
pub const NS_CALENDAR: &str = "urn:xmpp:calendar:0";

/// PEP/PubSub node for calendar events.
pub const PUBSUB_NODE_EVENTS: &str = "urn:xmpp:calendar:0";

/// RSVP status for an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RsvpStatus {
    /// Attending the event.
    Going,
    /// Interested but not confirmed.
    Interested,
    /// Not attending.
    NotGoing,
}

impl RsvpStatus {
    /// Parse from string.
    pub fn from_str_attr(s: &str) -> Option<Self> {
        match s {
            "going" => Some(Self::Going),
            "interested" => Some(Self::Interested),
            "not-going" => Some(Self::NotGoing),
            _ => None,
        }
    }

    /// Convert to string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Going => "going",
            Self::Interested => "interested",
            Self::NotGoing => "not-going",
        }
    }
}

impl std::fmt::Display for RsvpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A calendar event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    /// Unique event ID.
    pub id: String,
    /// Event title.
    pub title: String,
    /// Event description.
    pub description: Option<String>,
    /// Start time.
    pub start: Option<DateTime<Utc>>,
    /// End time.
    pub end: Option<DateTime<Utc>>,
    /// Location (room, address, URL).
    pub location: Option<String>,
    /// Event organizer JID.
    pub organizer: Option<String>,
}

impl CalendarEvent {
    /// Create a new event.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
            start: None,
            end: None,
            location: None,
            organizer: None,
        }
    }

    /// Set the description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set start and end times.
    pub fn with_times(mut self, start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        self.start = Some(start);
        self.end = Some(end);
        self
    }

    /// Set the location.
    pub fn with_location(mut self, loc: impl Into<String>) -> Self {
        self.location = Some(loc.into());
        self
    }

    /// Set the organizer.
    pub fn with_organizer(mut self, org: impl Into<String>) -> Self {
        self.organizer = Some(org.into());
        self
    }

    /// Returns `true` if the event is in the future.
    pub fn is_upcoming(&self) -> bool {
        self.start.is_some_and(|s| s > Utc::now())
    }

    /// Returns `true` if the event is currently happening.
    pub fn is_ongoing(&self) -> bool {
        let now = Utc::now();
        self.start.is_some_and(|s| s <= now) && self.end.is_some_and(|e| e > now)
    }
}

/// An RSVP entry for an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rsvp {
    /// The user JID.
    pub jid: String,
    /// Their RSVP status.
    pub status: RsvpStatus,
}

impl Rsvp {
    /// Create a new RSVP.
    pub fn new(jid: impl Into<String>, status: RsvpStatus) -> Self {
        Self {
            jid: jid.into(),
            status,
        }
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a calendar event element.
pub fn is_event_element(elem: &Element) -> bool {
    elem.ns() == NS_CALENDAR && elem.name() == "event"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a calendar event from an `<event/>` element.
pub fn parse_event(item_id: &str, elem: &Element) -> Option<CalendarEvent> {
    if !is_event_element(elem) {
        return None;
    }

    let text_child = |name: &str| -> Option<String> {
        elem.children()
            .find(|c| c.name() == name && c.ns() == NS_CALENDAR)
            .map(|c| c.text())
            .filter(|t| !t.is_empty())
    };

    let dt_child =
        |name: &str| -> Option<DateTime<Utc>> { text_child(name).and_then(|t| t.parse().ok()) };

    let title = text_child("title")?;

    Some(CalendarEvent {
        id: item_id.to_owned(),
        title,
        description: text_child("description"),
        start: dt_child("start"),
        end: dt_child("end"),
        location: text_child("location"),
        organizer: text_child("organizer"),
    })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build an `<event/>` element.
pub fn build_event_element(event: &CalendarEvent) -> Element {
    let mut elem = Element::builder("event", NS_CALENDAR).build();

    let append_text = |parent: &mut Element, name: &str, value: &str| {
        let mut child = Element::builder(name, NS_CALENDAR).build();
        child.append_text_node(value);
        parent.append_child(child);
    };

    append_text(&mut elem, "title", &event.title);

    if let Some(ref desc) = event.description {
        append_text(&mut elem, "description", desc);
    }
    if let Some(start) = event.start {
        append_text(&mut elem, "start", &start.to_rfc3339());
    }
    if let Some(end) = event.end {
        append_text(&mut elem, "end", &end.to_rfc3339());
    }
    if let Some(ref loc) = event.location {
        append_text(&mut elem, "location", loc);
    }
    if let Some(ref org) = event.organizer {
        append_text(&mut elem, "organizer", org);
    }

    elem
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn test_start() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 15, 19, 0, 0)
            .single()
            .expect("valid date")
    }

    fn test_end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 15, 22, 0, 0)
            .single()
            .expect("valid date")
    }

    #[test]
    fn test_is_event_element() {
        let elem = Element::builder("event", NS_CALENDAR).build();
        assert!(is_event_element(&elem));

        let wrong = Element::builder("event", "jabber:client").build();
        assert!(!is_event_element(&wrong));
    }

    #[test]
    fn test_build_and_parse() {
        let event = CalendarEvent::new("evt-1", "Game Night")
            .with_description("Weekly gaming")
            .with_times(test_start(), test_end())
            .with_location("Voice #gaming")
            .with_organizer("alice@example.com");

        let elem = build_event_element(&event);
        assert_eq!(elem.name(), "event");
        assert_eq!(elem.ns(), NS_CALENDAR);

        let parsed = parse_event("evt-1", &elem).expect("parseable");
        assert_eq!(parsed.id, "evt-1");
        assert_eq!(parsed.title, "Game Night");
        assert_eq!(parsed.description.as_deref(), Some("Weekly gaming"));
        assert_eq!(parsed.start, Some(test_start()));
        assert_eq!(parsed.end, Some(test_end()));
        assert_eq!(parsed.location.as_deref(), Some("Voice #gaming"));
        assert_eq!(parsed.organizer.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn test_parse_minimal() {
        let xml = "<event xmlns='urn:xmpp:calendar:0'>\
                    <title>Quick Meetup</title>\
                    </event>";
        let elem: Element = xml.parse().expect("valid xml");
        let event = parse_event("e-1", &elem).expect("parseable");
        assert_eq!(event.title, "Quick Meetup");
        assert_eq!(event.description, None);
        assert_eq!(event.start, None);
    }

    #[test]
    fn test_parse_missing_title() {
        let xml = "<event xmlns='urn:xmpp:calendar:0'>\
                    <description>No title</description>\
                    </event>";
        let elem: Element = xml.parse().expect("valid xml");
        assert!(parse_event("e-1", &elem).is_none());
    }

    #[test]
    fn test_is_upcoming() {
        let future = CalendarEvent::new("e", "Future").with_times(test_start(), test_end());
        assert!(future.is_upcoming());

        let past = CalendarEvent::new("e", "Past").with_times(
            Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
                .single()
                .expect("valid"),
            Utc.with_ymd_and_hms(2020, 1, 1, 1, 0, 0)
                .single()
                .expect("valid"),
        );
        assert!(!past.is_upcoming());
    }

    #[test]
    fn test_rsvp_status() {
        assert_eq!(RsvpStatus::from_str_attr("going"), Some(RsvpStatus::Going));
        assert_eq!(
            RsvpStatus::from_str_attr("interested"),
            Some(RsvpStatus::Interested)
        );
        assert_eq!(
            RsvpStatus::from_str_attr("not-going"),
            Some(RsvpStatus::NotGoing)
        );
        assert_eq!(RsvpStatus::from_str_attr("maybe"), None);
    }

    #[test]
    fn test_rsvp_display() {
        assert_eq!(RsvpStatus::Going.to_string(), "going");
        assert_eq!(RsvpStatus::Interested.to_string(), "interested");
        assert_eq!(RsvpStatus::NotGoing.to_string(), "not-going");
    }

    #[test]
    fn test_rsvp_new() {
        let rsvp = Rsvp::new("alice@example.com", RsvpStatus::Going);
        assert_eq!(rsvp.jid, "alice@example.com");
        assert_eq!(rsvp.status, RsvpStatus::Going);
    }

    #[test]
    fn test_event_builder() {
        let e = CalendarEvent::new("id", "Title")
            .with_description("Desc")
            .with_location("Loc")
            .with_organizer("Org");
        assert_eq!(e.title, "Title");
        assert_eq!(e.description.as_deref(), Some("Desc"));
        assert_eq!(e.location.as_deref(), Some("Loc"));
        assert_eq!(e.organizer.as_deref(), Some("Org"));
    }

    #[test]
    fn test_pubsub_node() {
        assert_eq!(PUBSUB_NODE_EVENTS, "urn:xmpp:calendar:0");
    }
}
