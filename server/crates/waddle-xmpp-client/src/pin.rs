//! Channel pin client helpers (#414).
//!
//! Builders and parsers for the `urn:waddle:pin:0` IQ pin-list query
//! and response. The wire shape is defined by the server-side PR
//! #416; this module is the client/wasm-binding consumer.

use chrono::{DateTime, Utc};
use minidom::Element;
use uuid::Uuid;

use crate::messaging::namespaces::{NS_CLIENT, NS_WADDLE_PIN_V0};

/// Build `<iq type='get' to='room@conf'><query xmlns='urn:waddle:pin:0'/></iq>`.
pub fn build_pin_list_iq(room_jid: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr("type", "get")
        .attr("to", room_jid)
        .attr("id", Uuid::new_v4().to_string())
        .append(Element::builder("query", NS_WADDLE_PIN_V0).build())
        .build()
}

/// One pin entry returned by [`build_pin_list_iq`]'s response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinEntry {
    /// XEP-0359 stanza-id of the pinned message in the room archive.
    pub target_stanza_id: String,
    /// Bare JID of the user who pinned the message.
    pub pinner_jid: String,
    /// When the pin was applied (rfc3339 from the server).
    pub pinned_at: DateTime<Utc>,
    /// Frozen preview snapshot.
    pub preview: PinPreview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinPreview {
    /// Bare JID of the message author at pin time.
    pub author_jid: String,
    /// Author's MUC nick at pin time, if known.
    pub author_nick: Option<String>,
    /// Truncated body text (≤280 chars).
    pub text: String,
    /// Original message timestamp (not pin time).
    pub message_timestamp: DateTime<Utc>,
}

/// Parse the `<query xmlns='urn:waddle:pin:0'>` payload of a pin-list
/// IQ response into typed [`PinEntry`] values. Items missing required
/// attributes or with malformed timestamps are skipped (logged
/// upstream); the rest are returned in document order, which the
/// server emits in pin-time-desc.
pub fn parse_pin_list_response(iq: &Element) -> Vec<PinEntry> {
    let Some(query) = iq.get_child("query", NS_WADDLE_PIN_V0) else {
        return Vec::new();
    };
    query
        .children()
        .filter(|c| c.name() == "pin" && c.ns() == NS_WADDLE_PIN_V0)
        .filter_map(parse_pin_entry)
        .collect()
}

fn parse_pin_entry(elem: &Element) -> Option<PinEntry> {
    let target_stanza_id = elem.attr("id")?.to_owned();
    let pinner_jid = elem.attr("by")?.to_owned();
    let pinned_at = elem
        .attr("at")
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))?;
    let preview = elem
        .get_child("preview", NS_WADDLE_PIN_V0)
        .and_then(parse_pin_preview)?;
    Some(PinEntry {
        target_stanza_id,
        pinner_jid,
        pinned_at,
        preview,
    })
}

fn parse_pin_preview(elem: &Element) -> Option<PinPreview> {
    let author = elem.get_child("author", NS_WADDLE_PIN_V0)?;
    let author_jid = author.attr("jid")?.to_owned();
    let author_nick = author.attr("nick").map(str::to_owned);
    let text = elem
        .get_child("text", NS_WADDLE_PIN_V0)
        .map(|t| t.text())
        .unwrap_or_default();
    let message_timestamp = elem
        .get_child("ts", NS_WADDLE_PIN_V0)
        .map(|t| t.text())
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))?;
    Some(PinPreview {
        author_jid,
        author_nick,
        text,
        message_timestamp,
    })
}

/// A pin/unpin event surfaced from an inbound `<message>` carrying a
/// `<pin-event xmlns='urn:waddle:pin:0' action='pinned'|'unpinned'>`
/// payload (the server's broadcast system message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinEvent {
    /// `pinned` or `unpinned`.
    pub action: PinEventAction,
    /// Stanza-id of the targeted message.
    pub target_stanza_id: String,
    /// Bare JID of the pinner/unpinner.
    pub by: String,
    /// `Some("retracted")` when the unpin was triggered by an
    /// XEP-0424 retraction cascade.
    pub reason: Option<String>,
    /// Frozen preview, present only on `pinned` events.
    pub preview: Option<PinPreview>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinEventAction {
    Pinned,
    Unpinned,
}

/// Parse a `<pin-event/>` element into a typed [`PinEvent`]. Returns
/// `None` for malformed events.
pub fn parse_pin_event_payload(elem: &Element) -> Option<PinEvent> {
    if elem.name() != "pin-event" || elem.ns() != NS_WADDLE_PIN_V0 {
        return None;
    }
    let action = match elem.attr("action")? {
        "pinned" => PinEventAction::Pinned,
        "unpinned" => PinEventAction::Unpinned,
        _ => return None,
    };
    let target_stanza_id = elem.attr("target")?.to_owned();
    let by = elem.attr("by")?.to_owned();
    let reason = elem.attr("reason").map(str::to_owned);
    let preview = elem
        .get_child("preview", NS_WADDLE_PIN_V0)
        .and_then(parse_pin_preview);
    Some(PinEvent {
        action,
        target_stanza_id,
        by,
        reason,
        preview,
    })
}

/// Extract the first `<pin-event/>` payload from a `<message>` (the
/// system messages the server broadcasts on pin/unpin). Returns
/// `None` when the message carries no pin-event.
pub fn extract_pin_event_from_message(message: &Element) -> Option<PinEvent> {
    message
        .children()
        .filter(|c| c.name() == "pin-event" && c.ns() == NS_WADDLE_PIN_V0)
        .find_map(parse_pin_event_payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin_query_response_xml() -> &'static str {
        r#"<iq xmlns='jabber:client' type='result' id='abc'>
          <query xmlns='urn:waddle:pin:0'>
            <pin id='stanza-1' by='admin@example.com' at='2026-05-08T12:00:00+00:00'>
              <preview>
                <author jid='alice@example.com' nick='alice'/>
                <text>important update</text>
                <ts>2026-05-08T11:55:00+00:00</ts>
              </preview>
            </pin>
            <pin id='stanza-2' by='admin@example.com' at='2026-05-08T11:30:00+00:00'>
              <preview>
                <author jid='bob@example.com'/>
                <text>another message</text>
                <ts>2026-05-08T11:25:00+00:00</ts>
              </preview>
            </pin>
          </query>
        </iq>"#
    }

    #[test]
    fn build_pin_list_iq_targets_room_with_query_payload() {
        let iq = build_pin_list_iq("room@conf.example");
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("room@conf.example"));
        assert!(iq.attr("id").is_some());
        let query = iq
            .get_child("query", NS_WADDLE_PIN_V0)
            .expect("query payload");
        assert_eq!(query.children().count(), 0);
    }

    #[test]
    fn parse_pin_list_response_returns_typed_entries() {
        let iq: Element = pin_query_response_xml().parse().expect("parse iq");
        let entries = parse_pin_list_response(&iq);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].target_stanza_id, "stanza-1");
        assert_eq!(entries[0].pinner_jid, "admin@example.com");
        assert_eq!(entries[0].preview.author_jid, "alice@example.com");
        assert_eq!(entries[0].preview.author_nick.as_deref(), Some("alice"));
        assert_eq!(entries[0].preview.text, "important update");
        assert_eq!(entries[1].target_stanza_id, "stanza-2");
        assert!(entries[1].preview.author_nick.is_none());
    }

    #[test]
    fn parse_pin_list_response_skips_malformed_entries() {
        let iq: Element = "<iq xmlns='jabber:client' type='result' id='x'>\
          <query xmlns='urn:waddle:pin:0'>\
            <pin id='ok' by='admin@example.com' at='2026-05-08T12:00:00+00:00'>\
              <preview><author jid='alice@example.com'/><text>ok</text>\
              <ts>2026-05-08T11:55:00+00:00</ts></preview></pin>\
            <pin by='admin@example.com'></pin>\
            <pin id='bad-ts' by='admin@example.com' at='not-a-date'>\
              <preview><author jid='x@y'/><text></text><ts>also-bad</ts></preview>\
            </pin>\
          </query>\
        </iq>"
            .parse()
            .expect("parse iq");
        let entries = parse_pin_list_response(&iq);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target_stanza_id, "ok");
    }

    #[test]
    fn parse_pin_list_response_empty_when_no_query() {
        let iq: Element = "<iq xmlns='jabber:client' type='result' id='x'/>"
            .parse()
            .expect("parse iq");
        assert!(parse_pin_list_response(&iq).is_empty());
    }

    fn build_pin_event_message(action: &str) -> Element {
        let author = Element::builder("author", NS_WADDLE_PIN_V0)
            .attr("jid", "alice@example.com")
            .attr("nick", "alice")
            .build();
        let mut text = Element::builder("text", NS_WADDLE_PIN_V0).build();
        text.append_text_node("important");
        let mut ts = Element::builder("ts", NS_WADDLE_PIN_V0).build();
        ts.append_text_node("2026-05-08T11:55:00+00:00");
        let preview = Element::builder("preview", NS_WADDLE_PIN_V0)
            .append(author)
            .append(text)
            .append(ts)
            .build();
        let pin_event = Element::builder("pin-event", NS_WADDLE_PIN_V0)
            .attr("action", action)
            .attr("target", "stanza-1")
            .attr("by", "admin@example.com")
            .append(preview)
            .build();
        let mut body = Element::builder("body", "jabber:client").build();
        body.append_text_node(format!("alice {action} a message"));
        Element::builder("message", "jabber:client")
            .attr("type", "groupchat")
            .attr("from", "room@conf.example")
            .append(body)
            .append(pin_event)
            .build()
    }

    #[test]
    fn extract_pin_event_decodes_pinned_with_preview() {
        let msg = build_pin_event_message("pinned");
        let event = extract_pin_event_from_message(&msg).expect("event present");
        assert_eq!(event.action, PinEventAction::Pinned);
        assert_eq!(event.target_stanza_id, "stanza-1");
        assert_eq!(event.by, "admin@example.com");
        assert!(event.reason.is_none());
        assert!(event.preview.is_some());
    }

    #[test]
    fn extract_pin_event_decodes_unpinned_without_preview() {
        let msg: Element = "<message xmlns='jabber:client' type='groupchat' from='r@x'>\
          <pin-event xmlns='urn:waddle:pin:0' action='unpinned' target='s' by='a@b'/>\
        </message>"
            .parse()
            .expect("parse msg");
        let event = extract_pin_event_from_message(&msg).expect("event present");
        assert_eq!(event.action, PinEventAction::Unpinned);
        assert!(event.preview.is_none());
    }

    #[test]
    fn extract_pin_event_decodes_retracted_reason() {
        let msg: Element = "<message xmlns='jabber:client' type='groupchat' from='r@x'>\
          <pin-event xmlns='urn:waddle:pin:0' action='unpinned' target='s' by='a@b' reason='retracted'/>\
        </message>"
            .parse()
            .expect("parse msg");
        let event = extract_pin_event_from_message(&msg).expect("event present");
        assert_eq!(event.reason.as_deref(), Some("retracted"));
    }

    #[test]
    fn extract_pin_event_returns_none_for_unrelated_message() {
        let msg: Element = "<message xmlns='jabber:client' type='groupchat' from='r@x'>\
          <body>just chatter</body>\
        </message>"
            .parse()
            .expect("parse msg");
        assert!(extract_pin_event_from_message(&msg).is_none());
    }

    #[test]
    fn parse_pin_event_rejects_unknown_action() {
        let elem: Element =
            "<pin-event xmlns='urn:waddle:pin:0' action='bogus' target='s' by='a@b'/>"
                .parse()
                .expect("parse elem");
        assert!(parse_pin_event_payload(&elem).is_none());
    }
}
