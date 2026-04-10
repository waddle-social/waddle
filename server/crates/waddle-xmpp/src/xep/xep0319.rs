//! XEP-0319: Last User Interaction in Presence
//!
//! Provides helpers for detecting, parsing, and building idle indicators
//! in presence stanzas. Shows when the user last interacted with their
//! device, enabling "idle" or "away since" display in chat UIs.
//!
//! ## XML Format
//!
//! ```xml
//! <presence from='romeo@example.com/mobile'>
//!   <show>away</show>
//!   <idle xmlns='urn:xmpp:idle:1' since='2024-06-01T12:00:00Z'/>
//! </presence>
//! ```
//!
//! ## Use Cases
//!
//! - Show "idle for 5 minutes" or "last active 2 hours ago" in user lists
//! - Distinguish between "away" (manual) and "idle" (automatic)
//! - Help users decide whether to expect a timely response
//!
//! ## Server Behavior
//!
//! The server transparently routes presence stanzas containing idle info.
//! It may also track the last interaction time internally for XEP-0012.

use chrono::{DateTime, Utc};
use minidom::Element;
use thiserror::Error;
use xmpp_parsers::presence::Presence;

/// Namespace for XEP-0319 Last User Interaction.
pub const NS_IDLE: &str = "urn:xmpp:idle:1";

/// Errors that can occur when parsing idle elements.
#[derive(Debug, Error)]
pub enum IdleError {
    /// The `since` attribute is missing or invalid.
    #[error("idle element has invalid or missing since: {0}")]
    InvalidSince(String),
}

/// Idle state information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleInfo {
    /// The timestamp when the user last interacted.
    pub since: DateTime<Utc>,
}

impl IdleInfo {
    /// Create a new idle info.
    pub fn new(since: DateTime<Utc>) -> Self {
        Self { since }
    }

    /// Create idle info for "idle since now".
    pub fn now() -> Self {
        Self { since: Utc::now() }
    }

    /// How long the user has been idle (from now).
    pub fn idle_duration(&self) -> chrono::Duration {
        Utc::now().signed_duration_since(self.since)
    }

    /// Human-readable idle duration string.
    pub fn human_idle(&self) -> String {
        let dur = self.idle_duration();
        let secs = dur.num_seconds();
        if secs < 60 {
            "just now".to_owned()
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else if secs < 86400 {
            format!("{}h ago", secs / 3600)
        } else {
            format!("{}d ago", secs / 86400)
        }
    }
}

/// Trait for types that can carry idle information.
pub trait IdleCarrier {
    /// Extract idle info from this carrier, if present.
    fn idle_info(&self) -> Option<IdleInfo>;

    /// Returns `true` if this carrier has idle information.
    fn is_idle(&self) -> bool {
        self.idle_info().is_some()
    }

    /// Returns the idle-since timestamp if present.
    fn idle_since(&self) -> Option<DateTime<Utc>> {
        self.idle_info().map(|i| i.since)
    }
}

impl IdleCarrier for Presence {
    fn idle_info(&self) -> Option<IdleInfo> {
        extract_idle_from_presence(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is an `<idle/>` element.
pub fn is_idle_element(elem: &Element) -> bool {
    elem.ns() == NS_IDLE && elem.name() == "idle"
}

/// Check if a presence has idle information.
pub fn has_idle(presence: &Presence) -> bool {
    presence.payloads.iter().any(|e| is_idle_element(e))
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract idle info from a presence stanza.
pub fn extract_idle_from_presence(presence: &Presence) -> Option<IdleInfo> {
    let elem = presence.payloads.iter().find(|e| is_idle_element(e))?;
    parse_idle_element(elem).ok()
}

/// Parse an `<idle/>` element.
pub fn parse_idle_element(elem: &Element) -> Result<IdleInfo, IdleError> {
    let since_str = elem
        .attr("since")
        .ok_or_else(|| IdleError::InvalidSince("missing since attribute".into()))?;
    let since = since_str
        .parse::<DateTime<Utc>>()
        .map_err(|e| IdleError::InvalidSince(e.to_string()))?;
    Ok(IdleInfo::new(since))
}

// ── Building ─────────────────────────────────────────────────────────

/// Build an `<idle xmlns='urn:xmpp:idle:1' since='...'/>` element.
pub fn build_idle_element(since: DateTime<Utc>) -> Element {
    Element::builder("idle", NS_IDLE)
        .attr("since", since.to_rfc3339())
        .build()
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add idle information to a presence stanza.
pub fn add_idle(presence: &mut Presence, since: DateTime<Utc>) {
    presence.payloads.retain(|e| e.ns() != NS_IDLE);
    presence.payloads.push(build_idle_element(since));
}

/// Remove idle information from a presence stanza.
pub fn strip_idle(presence: &mut Presence) {
    presence.payloads.retain(|e| e.ns() != NS_IDLE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn test_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
            .single()
            .expect("valid test date")
    }

    #[test]
    fn test_is_idle_element() {
        let elem = Element::builder("idle", NS_IDLE)
            .attr("since", "2024-06-01T12:00:00Z")
            .build();
        assert!(is_idle_element(&elem));

        let wrong = Element::builder("idle", "jabber:client").build();
        assert!(!is_idle_element(&wrong));
    }

    #[test]
    fn test_extract_idle_from_presence() {
        let xml = "<presence xmlns='jabber:client'>\
                    <show>away</show>\
                    <idle xmlns='urn:xmpp:idle:1' since='2024-06-01T12:00:00Z'/>\
                    </presence>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        let idle = extract_idle_from_presence(&presence).expect("has idle");
        assert_eq!(idle.since, test_time());
    }

    #[test]
    fn test_extract_idle_absent() {
        let xml = "<presence xmlns='jabber:client'/>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");
        assert!(extract_idle_from_presence(&presence).is_none());
    }

    #[test]
    fn test_parse_idle_invalid_since() {
        let elem = Element::builder("idle", NS_IDLE)
            .attr("since", "not-a-date")
            .build();
        let err = parse_idle_element(&elem).expect_err("should fail");
        assert!(err.to_string().contains("invalid"));
    }

    #[test]
    fn test_parse_idle_missing_since() {
        let elem = Element::builder("idle", NS_IDLE).build();
        let err = parse_idle_element(&elem).expect_err("should fail");
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn test_build_idle_element() {
        let elem = build_idle_element(test_time());
        assert_eq!(elem.name(), "idle");
        assert_eq!(elem.ns(), NS_IDLE);
        assert!(elem.attr("since").is_some());
    }

    #[test]
    fn test_add_idle() {
        let xml = "<presence xmlns='jabber:client'/>";
        let mut presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        add_idle(&mut presence, test_time());
        assert!(has_idle(&presence));

        // Replace
        let new_time = Utc.with_ymd_and_hms(2024, 7, 1, 0, 0, 0)
            .single()
            .expect("valid test date");
        add_idle(&mut presence, new_time);
        let idle = extract_idle_from_presence(&presence).expect("has idle");
        assert_eq!(idle.since, new_time);
        assert_eq!(
            presence
                .payloads
                .iter()
                .filter(|e| e.ns() == NS_IDLE)
                .count(),
            1
        );
    }

    #[test]
    fn test_strip_idle() {
        let xml = "<presence xmlns='jabber:client'>\
                    <idle xmlns='urn:xmpp:idle:1' since='2024-06-01T12:00:00Z'/>\
                    </presence>";
        let mut presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        strip_idle(&mut presence);
        assert!(!has_idle(&presence));
    }

    #[test]
    fn test_idle_carrier_trait() {
        let xml = "<presence xmlns='jabber:client'>\
                    <idle xmlns='urn:xmpp:idle:1' since='2024-06-01T12:00:00Z'/>\
                    </presence>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        assert!(presence.is_idle());
        assert_eq!(presence.idle_since(), Some(test_time()));
    }

    #[test]
    fn test_idle_info_now() {
        let info = IdleInfo::now();
        assert!(info.idle_duration().num_seconds() < 2);
    }

    #[test]
    fn test_idle_info_human() {
        let recent = IdleInfo::new(Utc::now() - chrono::Duration::seconds(30));
        assert_eq!(recent.human_idle(), "just now");

        let minutes = IdleInfo::new(Utc::now() - chrono::Duration::minutes(15));
        assert!(minutes.human_idle().contains("m ago"));

        let hours = IdleInfo::new(Utc::now() - chrono::Duration::hours(3));
        assert!(hours.human_idle().contains("h ago"));

        let days = IdleInfo::new(Utc::now() - chrono::Duration::days(2));
        assert!(days.human_idle().contains("d ago"));
    }

    #[test]
    fn test_roundtrip() {
        let xml = "<presence xmlns='jabber:client'>\
                    <idle xmlns='urn:xmpp:idle:1' since='2024-06-01T12:00:00Z'/>\
                    </presence>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");
        let idle = extract_idle_from_presence(&presence).expect("has idle");
        assert_eq!(idle.since, test_time());
    }
}
