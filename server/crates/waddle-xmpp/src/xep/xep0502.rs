//! XEP-0502: MUC Activity Indicator
//!
//! XEP-0502 exposes room activity through a disco#info data-form field. It
//! does not define a subscribe/notify stanza protocol.

use chrono::{DateTime, Utc};
use minidom::Element;

use super::xep0004::{DataForm, Field, FormType, ToElement, NS_DATA_FORMS};

/// Namespace for XEP-0502 MUC Activity Indicator.
pub const NS_MUC_ACTIVITY: &str = "urn:xmpp:muc-activity";

/// MUC roominfo field containing the room's messages/hour value.
pub const FIELD_MESSAGE_ACTIVITY: &str = "{urn:xmpp:muc-activity}message-activity";

const FORM_TYPE_MUC_ROOMINFO: &str = "http://jabber.org/protocol/muc#roominfo";

/// Activity state for a room. This is a local model, not a XEP-0502 stanza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomActivity {
    /// The room JID.
    pub room_jid: String,
    /// Timestamp of last activity.
    pub last_activity: Option<DateTime<Utc>>,
    /// Whether the room currently has new messages.
    pub has_new_messages: bool,
}

impl RoomActivity {
    /// Create a new room activity entry.
    pub fn new(room_jid: impl Into<String>) -> Self {
        Self {
            room_jid: room_jid.into(),
            last_activity: None,
            has_new_messages: false,
        }
    }

    /// Mark as having new activity now.
    pub fn with_activity_now(mut self) -> Self {
        self.last_activity = Some(Utc::now());
        self.has_new_messages = true;
        self
    }

    /// Set the last activity timestamp.
    pub fn with_last_activity(mut self, ts: DateTime<Utc>) -> Self {
        self.last_activity = Some(ts);
        self.has_new_messages = true;
        self
    }

    /// Mark as read (no new messages).
    pub fn mark_read(&mut self) {
        self.has_new_messages = false;
    }
}

/// Tracks activity across multiple rooms locally.
#[derive(Debug, Default)]
pub struct ActivityTracker {
    rooms: std::collections::HashMap<String, RoomActivity>,
}

impl ActivityTracker {
    /// Create a new tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record activity in a room.
    pub fn record_activity(&mut self, room_jid: &str, timestamp: DateTime<Utc>) {
        let entry = self
            .rooms
            .entry(room_jid.to_owned())
            .or_insert_with(|| RoomActivity::new(room_jid));
        entry.last_activity = Some(timestamp);
        entry.has_new_messages = true;
    }

    /// Mark a room as read.
    pub fn mark_read(&mut self, room_jid: &str) {
        if let Some(entry) = self.rooms.get_mut(room_jid) {
            entry.mark_read();
        }
    }

    /// Check if a room has new messages.
    pub fn has_activity(&self, room_jid: &str) -> bool {
        self.rooms.get(room_jid).is_some_and(|r| r.has_new_messages)
    }

    /// Get all rooms with new activity.
    pub fn active_rooms(&self) -> Vec<&RoomActivity> {
        self.rooms.values().filter(|r| r.has_new_messages).collect()
    }

    /// Get the activity state for a room.
    pub fn get(&self, room_jid: &str) -> Option<&RoomActivity> {
        self.rooms.get(room_jid)
    }

    /// Number of rooms with new activity.
    pub fn active_count(&self) -> usize {
        self.rooms.values().filter(|r| r.has_new_messages).count()
    }

    /// Remove a room from tracking.
    pub fn remove(&mut self, room_jid: &str) {
        self.rooms.remove(room_jid);
    }

    /// Clear all activity.
    pub fn clear(&mut self) {
        self.rooms.clear();
    }
}

fn message_activity_value(messages_per_hour: f64) -> String {
    if messages_per_hour.is_finite() && messages_per_hour >= 0.0 {
        messages_per_hour.to_string()
    } else {
        "0".to_owned()
    }
}

/// Build the XEP-0502 roominfo field containing messages/hour.
pub fn build_message_activity_field(messages_per_hour: f64) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(
            minidom::rxml::xml_ncname!("var").to_owned(),
            FIELD_MESSAGE_ACTIVITY,
        )
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(message_activity_value(messages_per_hour))
                .build(),
        )
        .build()
}

/// Parse the XEP-0502 roominfo activity field.
pub fn parse_message_activity_field(field: &Element) -> Option<f64> {
    if !field.is("field", NS_DATA_FORMS) || field.attr("var") != Some(FIELD_MESSAGE_ACTIVITY) {
        return None;
    }
    field
        .get_child("value", NS_DATA_FORMS)
        .and_then(|value| value.text().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Build a MUC roominfo extension form containing XEP-0502 activity.
pub fn build_muc_activity_roominfo_form(messages_per_hour: f64) -> Element {
    DataForm::new(FormType::Result)
        .add_field(Field::form_type(FORM_TYPE_MUC_ROOMINFO))
        .add_field(Field::text_single(
            FIELD_MESSAGE_ACTIVITY,
            message_activity_value(messages_per_hour),
        ))
        .to_element()
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
    fn test_room_activity_new() {
        let ra = RoomActivity::new("room@muc");
        assert_eq!(ra.room_jid, "room@muc");
        assert!(!ra.has_new_messages);
        assert_eq!(ra.last_activity, None);
    }

    #[test]
    fn test_room_activity_with_activity() {
        let ra = RoomActivity::new("room@muc").with_last_activity(test_time());
        assert!(ra.has_new_messages);
        assert_eq!(ra.last_activity, Some(test_time()));
    }

    #[test]
    fn test_room_activity_mark_read() {
        let mut ra = RoomActivity::new("room@muc").with_activity_now();
        assert!(ra.has_new_messages);
        ra.mark_read();
        assert!(!ra.has_new_messages);
    }

    #[test]
    fn test_activity_tracker() {
        let mut tracker = ActivityTracker::new();

        assert!(!tracker.has_activity("room1@muc"));
        assert_eq!(tracker.active_count(), 0);

        tracker.record_activity("room1@muc", test_time());
        tracker.record_activity("room2@muc", test_time());

        assert!(tracker.has_activity("room1@muc"));
        assert_eq!(tracker.active_count(), 2);
        assert_eq!(tracker.active_rooms().len(), 2);

        tracker.mark_read("room1@muc");
        assert!(!tracker.has_activity("room1@muc"));
        assert!(tracker.has_activity("room2@muc"));
        assert_eq!(tracker.active_count(), 1);

        tracker.remove("room2@muc");
        assert_eq!(tracker.active_count(), 0);

        tracker.clear();
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn test_build_message_activity_field() {
        let field = build_message_activity_field(12.5);
        assert_eq!(field.name(), "field");
        assert_eq!(field.ns(), NS_DATA_FORMS);
        assert_eq!(field.attr("var"), Some(FIELD_MESSAGE_ACTIVITY));
        assert_eq!(parse_message_activity_field(&field), Some(12.5));
    }

    #[test]
    fn test_message_activity_field_rejects_foreign_fields() {
        let field = Element::builder("field", NS_DATA_FORMS)
            .attr(minidom::rxml::xml_ncname!("var").to_owned(), "other")
            .append(Element::builder("value", NS_DATA_FORMS).append("1").build())
            .build();
        assert_eq!(parse_message_activity_field(&field), None);
    }

    #[test]
    fn test_build_roominfo_activity_form() {
        let form = build_muc_activity_roominfo_form(3.25);
        assert_eq!(form.name(), "x");
        assert_eq!(form.ns(), NS_DATA_FORMS);
        assert_eq!(form.attr("type"), Some("result"));

        let field = form
            .children()
            .find(|child| child.attr("var") == Some(FIELD_MESSAGE_ACTIVITY))
            .expect("message activity field");
        assert_eq!(
            field
                .get_child("value", NS_DATA_FORMS)
                .map(|value| value.text()),
            Some("3.25".to_owned())
        );
    }
}
