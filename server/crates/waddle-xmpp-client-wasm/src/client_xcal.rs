//! xCal calendar events — wasm bridge surface.
//!
//! Wraps the typed `waddle_xmpp_core::xcal` model behind JS-friendly
//! shapes. Items are full `<vcalendar><vevent/></vcalendar>` payloads
//! per the XSF ProtoXEP "Calendaring Extensions to Publish-Subscribe";
//! recurrence flows through an `<rrule>` element with FREQ/INTERVAL/
//! BYDAY/BYMONTHDAY/COUNT|UNTIL.

use chrono::{DateTime, Utc};
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_core::xcal::{
    build_vcalendar_with_event, parse_vcalendar_event, Freq, Rrule, RruleEnd, VEvent, Weekday,
    NS_XCAL, PUBSUB_NODE_EVENTS,
};
use wasm_bindgen::prelude::*;

use super::{js_error, send_iq_command, to_js_value, WaddleClient};
use crate::NS_CLIENT;

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

// ── JS-facing shapes ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsRrule {
    /// FREQ value: "DAILY"|"WEEKLY"|"MONTHLY"|"YEARLY".
    pub freq: String,
    pub interval: Option<u32>,
    /// Weekday two-letter codes ("SU"|"MO"|...|"SA").
    #[serde(default)]
    pub by_day: Vec<String>,
    /// Days of month, 1..=31.
    #[serde(default)]
    pub by_month_day: Vec<i32>,
    pub count: Option<u32>,
    /// RFC3339 string.
    pub until: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsVEvent {
    pub id: String,
    pub uid: String,
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub organizer: Option<String>,
    pub dtstamp: Option<String>,
    pub dtstart: Option<String>,
    pub dtend: Option<String>,
    pub rrule: Option<JsRrule>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsVEventInput {
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub organizer: Option<String>,
    /// RFC3339 strings.
    pub dtstart: Option<String>,
    pub dtend: Option<String>,
    pub rrule: Option<JsRrule>,
}

impl From<&Rrule> for JsRrule {
    fn from(rrule: &Rrule) -> Self {
        let (count, until) = match rrule.end {
            Some(RruleEnd::Count(n)) => (Some(n), None),
            Some(RruleEnd::Until(ts)) => (None, Some(ts.to_rfc3339())),
            None => (None, None),
        };
        Self {
            freq: rrule.freq.as_str().to_string(),
            interval: rrule.interval,
            by_day: rrule
                .by_day
                .iter()
                .map(|w| w.as_str().to_string())
                .collect(),
            by_month_day: rrule.by_month_day.clone(),
            count,
            until,
        }
    }
}

impl JsRrule {
    fn into_rrule(self) -> Result<Rrule, JsValue> {
        let freq = Freq::from_str_value(&self.freq)
            .ok_or_else(|| js_error(format!("invalid FREQ: {}", self.freq)))?;
        let mut rule = Rrule::new(freq);
        if let Some(interval) = self.interval {
            rule = rule.with_interval(interval);
        }
        if !self.by_day.is_empty() {
            let weekdays = self
                .by_day
                .iter()
                .map(|s| {
                    Weekday::from_str_value(s)
                        .ok_or_else(|| js_error(format!("invalid weekday: {s}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            rule = rule.with_by_day(weekdays);
        }
        if !self.by_month_day.is_empty() {
            rule = rule.with_by_month_day(self.by_month_day);
        }
        match (self.count, self.until) {
            (Some(_), Some(_)) => {
                return Err(js_error("COUNT and UNTIL are mutually exclusive"));
            }
            (Some(count), None) => rule = rule.with_count(count),
            (None, Some(until)) => {
                let parsed: DateTime<Utc> = until
                    .parse()
                    .map_err(|err| js_error(format!("invalid UNTIL: {err}")))?;
                rule = rule.with_until(parsed);
            }
            (None, None) => {}
        }
        Ok(rule)
    }
}

impl JsVEvent {
    fn from_vevent(item_id: &str, event: VEvent) -> Self {
        Self {
            id: item_id.to_string(),
            uid: event.uid,
            summary: event.summary,
            description: event.description,
            location: event.location,
            organizer: event.organizer,
            dtstamp: event.dtstamp.map(|ts| ts.to_rfc3339()),
            dtstart: event.dtstart.map(|ts| ts.to_rfc3339()),
            dtend: event.dtend.map(|ts| ts.to_rfc3339()),
            rrule: event.rrule.as_ref().map(JsRrule::from),
        }
    }
}

// ── IQ builders / parsers ───────────────────────────────────────────

fn build_events_items_iq(community_jid: &str, max_items: Option<u32>) -> Element {
    let id = format!("xcal-items-{}", Uuid::new_v4());
    let mut items_builder = Element::builder("items", NS_PUBSUB).attr("node", PUBSUB_NODE_EVENTS);
    let max_items_value;
    if let Some(max) = max_items {
        max_items_value = max.to_string();
        items_builder = items_builder.attr("max_items", max_items_value.as_str());
    }
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(items_builder.build())
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr("type", "get")
        .attr("id", id)
        .attr("to", community_jid)
        .append(pubsub)
        .build()
}

fn build_event_publish_iq(community_jid: &str, item_id: &str, event: &VEvent) -> Element {
    let id = format!("xcal-publish-{}", Uuid::new_v4());
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", item_id)
        .append(build_vcalendar_with_event(event))
        .build();
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr("node", PUBSUB_NODE_EVENTS)
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(publish)
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr("type", "set")
        .attr("id", id)
        .attr("to", community_jid)
        .append(pubsub)
        .build()
}

fn parse_events_items_result(iq: &Element) -> Vec<JsVEvent> {
    let Some(pubsub) = iq.get_child("pubsub", NS_PUBSUB) else {
        return Vec::new();
    };
    let Some(items) = pubsub.get_child("items", NS_PUBSUB) else {
        return Vec::new();
    };
    items
        .children()
        .filter(|el| el.name() == "item" && el.ns() == NS_PUBSUB)
        .filter_map(|item| {
            let item_id = item.attr("id")?;
            let vcal = item
                .children()
                .find(|child| child.name() == "vcalendar" && child.ns() == NS_XCAL)?;
            parse_vcalendar_event(item_id, vcal).map(|ev| JsVEvent::from_vevent(item_id, ev))
        })
        .collect()
}

// ── Wasm methods ────────────────────────────────────────────────────

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest calendar events from the community events
    /// node. Returns ALL items including past events; chat-side
    /// composables filter by DTSTART for upcoming-only views.
    pub fn xcal_items(&self, community_jid: String, max_items: Option<u32>) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let iq = build_events_items_iq(&community_jid, max_items);
            let result = send_iq_command(inner, iq).await?;
            let events = parse_events_items_result(&result);
            to_js_value(&events)
        })
    }

    /// Publish a new calendar event, optionally with an RRULE for
    /// recurrence. SUMMARY is required (per RFC 5545); DTSTART is
    /// required for the event to be useful on a timeline.
    pub fn xcal_publish(&self, community_jid: String, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let input: JsVEventInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid event input: {err}")))?;
            let item_id = format!("evt-{}", Uuid::new_v4());
            let mut event = VEvent::new(&item_id, input.summary).with_dtstamp(Utc::now());
            if let Some(dtstart) = input.dtstart {
                let parsed: DateTime<Utc> = dtstart
                    .parse()
                    .map_err(|err| js_error(format!("invalid dtstart: {err}")))?;
                event = event.with_dtstart(parsed);
            }
            if let Some(dtend) = input.dtend {
                let parsed: DateTime<Utc> = dtend
                    .parse()
                    .map_err(|err| js_error(format!("invalid dtend: {err}")))?;
                event = event.with_dtend(parsed);
            }
            if let Some(description) = input.description {
                event = event.with_description(description);
            }
            if let Some(location) = input.location {
                event = event.with_location(location);
            }
            if let Some(organizer) = input.organizer {
                event = event.with_organizer(organizer);
            }
            if let Some(rrule) = input.rrule {
                event = event.with_rrule(rrule.into_rrule()?);
            }
            let iq = build_event_publish_iq(&community_jid, &item_id, &event);
            send_iq_command(inner, iq).await?;
            let js_event = JsVEvent::from_vevent(&item_id, event);
            to_js_value(&js_event)
        })
    }
}
