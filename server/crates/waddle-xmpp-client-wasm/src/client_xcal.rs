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
    build_vcalendar_with_event, build_vcalendar_with_item, parse_vcalendar_item, Attendee,
    CalendarItem, Freq, PartStat, Rrule, RruleEnd, VEvent, Weekday, NS_XCAL, PUBSUB_NODE_EVENTS,
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
    /// ATTENDEE list — empty for events with no RSVPs yet. The chat
    /// folds sibling `-rsvp-*` items into this list before render.
    #[serde(default)]
    pub attendees: Vec<JsAttendee>,
    /// RFC3339 — set on override VEVENTs (one occurrence of a
    /// recurring master). `None` on the master event itself.
    pub recurrence_id: Option<String>,
    /// RFC3339[] — per-instance cancellations on the master event.
    #[serde(default)]
    pub exdates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsAttendee {
    pub uri: String,
    /// PartStat: "ACCEPTED"|"DECLINED"|"TENTATIVE"|"NEEDS-ACTION".
    pub partstat: String,
    pub role: Option<String>,
    pub rsvp: Option<bool>,
}

impl From<&Attendee> for JsAttendee {
    fn from(a: &Attendee) -> Self {
        Self {
            uri: a.uri.clone(),
            partstat: a.partstat.as_str().to_string(),
            role: a.role.clone(),
            rsvp: a.rsvp,
        }
    }
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

/// Full CalendarItem write input — master VEVENT plus optional
/// per-instance overrides and EXDATE cancellations. Each override
/// MUST carry a `recurrence_id` so the server (and the chat
/// expander) can correlate it back to a specific occurrence of the
/// master series.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsCalendarItemInput {
    pub master: JsVEventInput,
    #[serde(default)]
    pub overrides: Vec<JsOverrideInput>,
    /// RFC3339[] — occurrence DTSTART values to skip on the master.
    #[serde(default)]
    pub exdates: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsOverrideInput {
    /// RFC3339. The occurrence this override replaces.
    pub recurrence_id: String,
    /// All fields optional — overrides patch a subset of the master.
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub dtstart: Option<String>,
    pub dtend: Option<String>,
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
            attendees: event.attendees.iter().map(JsAttendee::from).collect(),
            recurrence_id: event.recurrence_id.map(|ts| ts.to_rfc3339()),
            exdates: event.exdates.iter().map(|ts| ts.to_rfc3339()).collect(),
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

/// Build an RSVP item payload for a master event: a VEVENT carrying
/// only the master UID and a single `<attendee/>` with the chosen
/// PartStat. Item id uses the canonical `<master-uid>-rsvp-<localpart>`
/// shape so the chat can fold sibling RSVP items back into the master.
fn build_rsvp_publish_iq(
    community_jid: &str,
    master_uid: &str,
    self_localpart: &str,
    self_jid: &str,
    partstat: PartStat,
) -> Element {
    let id = format!("xcal-rsvp-{}", Uuid::new_v4());
    let item_id = format!("{master_uid}-rsvp-{self_localpart}");
    let rsvp_event = VEvent {
        uid: master_uid.to_string(),
        dtstamp: Some(Utc::now()),
        dtstart: None,
        dtend: None,
        summary: String::new(),
        description: None,
        location: None,
        organizer: None,
        rrule: None,
        attendees: vec![Attendee::new(format!("xmpp:{self_jid}"), partstat)],
        recurrence_id: None,
        exdates: Vec::new(),
    };
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", item_id.as_str())
        .append(build_vcalendar_with_event(&rsvp_event))
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

/// Build a publish IQ for a CalendarItem (master + overrides) at
/// the given item id. Used by `xcal_publish_item` to atomically
/// write a master event plus its per-instance overrides + EXDATEs.
fn build_item_publish_iq(community_jid: &str, item_id: &str, item: &CalendarItem) -> Element {
    let id = format!("xcal-publish-{}", Uuid::new_v4());
    let pubsub_item = Element::builder("item", NS_PUBSUB)
        .attr("id", item_id)
        .append(build_vcalendar_with_item(item))
        .build();
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr("node", PUBSUB_NODE_EVENTS)
        .append(pubsub_item)
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

fn vevent_from_input(uid: &str, input: JsVEventInput) -> Result<VEvent, JsValue> {
    let mut event = VEvent::new(uid, input.summary).with_dtstamp(Utc::now());
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
    Ok(event)
}

fn override_from_input(uid: &str, input: JsOverrideInput) -> Result<VEvent, JsValue> {
    let recurrence_id: DateTime<Utc> = input
        .recurrence_id
        .parse()
        .map_err(|err| js_error(format!("invalid recurrence_id: {err}")))?;
    let mut event = VEvent::new(uid, input.summary.unwrap_or_default())
        .with_dtstamp(Utc::now())
        .with_recurrence_id(recurrence_id);
    if let Some(dtstart) = input.dtstart {
        let parsed: DateTime<Utc> = dtstart
            .parse()
            .map_err(|err| js_error(format!("invalid override dtstart: {err}")))?;
        event = event.with_dtstart(parsed);
    }
    if let Some(dtend) = input.dtend {
        let parsed: DateTime<Utc> = dtend
            .parse()
            .map_err(|err| js_error(format!("invalid override dtend: {err}")))?;
        event = event.with_dtend(parsed);
    }
    if let Some(description) = input.description {
        event = event.with_description(description);
    }
    if let Some(location) = input.location {
        event = event.with_location(location);
    }
    Ok(event)
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
    let mut flattened = Vec::new();
    for item in items
        .children()
        .filter(|el| el.name() == "item" && el.ns() == NS_PUBSUB)
    {
        let Some(item_id) = item.attr("id") else {
            continue;
        };
        let Some(vcal) = item
            .children()
            .find(|child| child.name() == "vcalendar" && child.ns() == NS_XCAL)
        else {
            continue;
        };
        let Some(parsed) = parse_vcalendar_item(item_id, vcal) else {
            continue;
        };
        // Master comes back under the item id; each override gets a
        // synthetic id `<item-id>::override::<recurrence-id-rfc3339>`
        // so the chat can address them individually without losing
        // the master/override correlation (preserved via `uid`).
        flattened.push(JsVEvent::from_vevent(item_id, parsed.master));
        for ov in parsed.overrides {
            let synthetic = ov
                .recurrence_id
                .map(|ts| format!("{item_id}::override::{}", ts.to_rfc3339()))
                .unwrap_or_else(|| item_id.to_string());
            flattened.push(JsVEvent::from_vevent(&synthetic, ov));
        }
    }
    flattened
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

    /// Publish (or replace) a full CalendarItem at the given item
    /// id — master event plus optional per-instance overrides and
    /// EXDATE cancellations. Use this for the read-modify-write
    /// flows ("edit this occurrence", "edit all occurrences",
    /// "cancel this occurrence") after fetching current state via
    /// `xcal_items`. Passing an existing item id overwrites that
    /// item atomically; passing a new id creates a new item.
    pub fn xcal_publish_item(
        &self,
        community_jid: String,
        item_id: String,
        input: JsValue,
    ) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let input: JsCalendarItemInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid calendar item input: {err}")))?;
            if item_id.is_empty() {
                return Err(js_error("item_id must not be empty"));
            }
            let mut master = vevent_from_input(&item_id, input.master)?;
            for exdate in input.exdates {
                let parsed: DateTime<Utc> = exdate
                    .parse()
                    .map_err(|err| js_error(format!("invalid exdate: {err}")))?;
                master = master.add_exdate(parsed);
            }
            let mut item = CalendarItem::new(master);
            for ov in input.overrides {
                item = item.add_override(override_from_input(&item_id, ov)?);
            }
            let iq = build_item_publish_iq(&community_jid, &item_id, &item);
            send_iq_command(inner, iq).await?;
            let js_master = JsVEvent::from_vevent(&item_id, item.master);
            to_js_value(&js_master)
        })
    }

    /// Retract a calendar item from the events node. Used for
    /// "cancel entire series" — removes the master plus any
    /// overrides in one shot.
    pub fn xcal_retract(&self, community_jid: String, item_id: String) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            if item_id.is_empty() {
                return Err(js_error("item_id must not be empty"));
            }
            let id = format!("xcal-retract-{}", Uuid::new_v4());
            let item = Element::builder("item", NS_PUBSUB)
                .attr("id", item_id.as_str())
                .build();
            let retract = Element::builder("retract", NS_PUBSUB)
                .attr("node", PUBSUB_NODE_EVENTS)
                .attr("notify", "true")
                .append(item)
                .build();
            let pubsub = Element::builder("pubsub", NS_PUBSUB)
                .append(retract)
                .build();
            let iq = Element::builder("iq", NS_CLIENT)
                .attr("type", "set")
                .attr("id", id)
                .attr("to", community_jid)
                .append(pubsub)
                .build();
            send_iq_command(inner, iq).await?;
            Ok(JsValue::TRUE)
        })
    }

    /// Publish (or update) this session's RSVP for a calendar event.
    /// `partstat` must be one of "ACCEPTED" | "DECLINED" | "TENTATIVE"
    /// | "NEEDS-ACTION". The chat groups sibling `-rsvp-*` items back
    /// into the master event on the next items fetch.
    pub fn xcal_rsvp(
        &self,
        community_jid: String,
        master_uid: String,
        self_localpart: String,
        self_jid: String,
        partstat: String,
    ) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let partstat = PartStat::from_str_value(&partstat)
                .ok_or_else(|| js_error(format!("invalid partstat: {partstat}")))?;
            if master_uid.is_empty() {
                return Err(js_error("master_uid must not be empty"));
            }
            if self_localpart.is_empty() {
                return Err(js_error("self_localpart must not be empty"));
            }
            if !self_jid.contains('@') {
                return Err(js_error("self_jid must be a bare JID"));
            }
            let iq = build_rsvp_publish_iq(
                &community_jid,
                &master_uid,
                &self_localpart,
                &self_jid,
                partstat,
            );
            send_iq_command(inner, iq).await?;
            to_js_value(&JsAttendee {
                uri: format!("xmpp:{self_jid}"),
                partstat: partstat.as_str().to_string(),
                role: None,
                rsvp: None,
            })
        })
    }
}
