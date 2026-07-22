//! xCal calendar events — wasm bridge surface.
//!
//! Wraps the typed `waddle_xmpp_core::xcal` model behind JS-friendly
//! shapes. Items are full `<vcalendar><vevent/></vcalendar>` payloads
//! per the XSF ProtoXEP "Calendaring Extensions to Publish-Subscribe";
//! recurrence flows through an `<rrule>` element with FREQ/INTERVAL/
//! BYDAY/BYMONTHDAY/COUNT|UNTIL.

use chrono::{DateTime, NaiveDate, Utc};
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_client::pubsub::{
    build_pubsub_items_iq, build_pubsub_publish_iq, build_pubsub_retract_iq,
    parse_pubsub_items_result,
};
use waddle_xmpp_core::xcal::{
    build_vcalendar_with_event, build_vcalendar_with_item, parse_vcalendar_item, Attendee,
    CalendarDateValue, CalendarItem, Freq, PartStat, Rrule, RruleEnd, VEvent, Weekday, NS_XCAL,
    PUBSUB_NODE_EVENTS,
};
use wasm_bindgen::prelude::*;

use super::{js_error, send_iq_command, service_bare_jid, to_js_value, WaddleClient};

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
    pub dtstart: Option<JsCalendarDateValue>,
    pub dtend: Option<JsCalendarDateValue>,
    pub rrule: Option<JsRrule>,
    /// ATTENDEE list — empty for events with no RSVPs yet. The chat
    /// folds sibling `-rsvp-*` items into this list before render.
    #[serde(default)]
    pub attendees: Vec<JsAttendee>,
    /// Typed RECURRENCE-ID — set on override VEVENTs (one occurrence
    /// of a recurring master). `None` on the master event itself.
    pub recurrence_id: Option<JsCalendarDateValue>,
    /// Typed EXDATE values — per-instance cancellations on the
    /// master event.
    #[serde(default)]
    pub exdates: Vec<JsCalendarDateValue>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid test date")
    }

    #[test]
    fn client_xcal_js_calendar_date_round_trips_date_and_date_time() {
        let date = CalendarDateValue::try_from(JsCalendarDateValue::Date {
            date: "2026-06-05".to_string(),
        })
        .expect("date converts");
        assert_eq!(date, CalendarDateValue::Date(day(2026, 6, 5)));

        let date_time = CalendarDateValue::try_from(JsCalendarDateValue::DateTime {
            ms: 1_781_030_400_000.0,
        })
        .expect("date-time converts");
        assert_eq!(
            JsCalendarDateValue::from(&date_time),
            JsCalendarDateValue::DateTime {
                ms: 1_781_030_400_000.0
            }
        );
    }

    #[test]
    fn client_xcal_vevent_from_vevent_emits_typed_all_day_shapes() {
        let event = VEvent::new("evt-all-day", "Festival")
            .with_dtstart_date(day(2026, 6, 5))
            .with_dtend_date(day(2026, 6, 8))
            .with_exdate_dates([day(2026, 6, 6)]);

        let js = JsVEvent::from_vevent("item-1", event);

        assert_eq!(
            js.dtstart,
            Some(JsCalendarDateValue::Date {
                date: "2026-06-05".to_string()
            })
        );
        assert_eq!(
            js.dtend,
            Some(JsCalendarDateValue::Date {
                date: "2026-06-08".to_string()
            })
        );
        assert_eq!(
            js.exdates,
            vec![JsCalendarDateValue::Date {
                date: "2026-06-06".to_string()
            }]
        );
    }

    #[test]
    fn client_xcal_rejects_mixed_date_and_date_time_values() {
        let event = VEvent::new("evt-mixed", "Mixed")
            .with_dtstart_date(day(2026, 6, 5))
            .with_dtend(DateTime::<Utc>::from_timestamp_millis(1_781_030_400_000).unwrap());

        assert!(calendar_date_type_mismatch(&event));
    }

    #[test]
    fn client_xcal_rejects_mixed_master_exdate_values() {
        let master = VEvent::new("evt-mixed-exdate", "Mixed EXDATE")
            .with_dtstart(DateTime::<Utc>::from_timestamp_millis(1_781_030_400_000).unwrap())
            .add_exdate_date(day(2026, 6, 12));
        let item = CalendarItem::new(master);

        assert!(validate_calendar_item_data(&item).is_err());
    }

    #[test]
    fn client_xcal_rejects_override_recurrence_id_type_mismatch() {
        let master = VEvent::new("evt-override", "Override")
            .with_dtstart(DateTime::<Utc>::from_timestamp_millis(1_781_030_400_000).unwrap());
        let override_event = VEvent::new("evt-override", "All-day override")
            .with_recurrence_id_date(day(2026, 6, 5));
        let item = CalendarItem::new(master).add_override(override_event);

        assert!(validate_calendar_item_data(&item).is_err());
    }

    #[test]
    fn client_xcal_rejects_non_positive_dtend_duration() {
        let event = VEvent::new("evt-order", "Order")
            .with_dtstart_date(day(2026, 6, 5))
            .with_dtend_date(day(2026, 6, 5));

        assert!(validate_calendar_event(&event).is_err());
    }

    #[test]
    fn client_xcal_rejects_all_day_rrule_until_date_time() {
        let until = DateTime::<Utc>::from_timestamp_millis(1_781_030_400_000).unwrap();
        let event = VEvent::new("evt-until", "Until")
            .with_dtstart_date(day(2026, 6, 5))
            .with_rrule(Rrule::new(Freq::Daily).with_until(until));

        assert!(validate_calendar_event(&event).is_err());
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsVEventInput {
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub organizer: Option<String>,
    pub dtstart: Option<JsCalendarDateValue>,
    pub dtend: Option<JsCalendarDateValue>,
    pub rrule: Option<JsRrule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum JsCalendarDateValue {
    Date { date: String },
    DateTime { ms: f64 },
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
    /// Occurrence DTSTART values to skip on the master.
    #[serde(default)]
    pub exdates: Vec<JsCalendarDateValue>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsOverrideInput {
    /// The occurrence this override replaces.
    pub recurrence_id: JsCalendarDateValue,
    /// All fields optional — overrides patch a subset of the master.
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub dtstart: Option<JsCalendarDateValue>,
    pub dtend: Option<JsCalendarDateValue>,
}

impl TryFrom<JsCalendarDateValue> for CalendarDateValue {
    type Error = JsValue;

    fn try_from(value: JsCalendarDateValue) -> Result<Self, Self::Error> {
        match value {
            JsCalendarDateValue::Date { date } => NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map(CalendarDateValue::Date)
                .map_err(|err| js_error(format!("invalid date: {err}"))),
            JsCalendarDateValue::DateTime { ms } => {
                if !ms.is_finite() {
                    return Err(js_error("invalid date-time: non-finite timestamp"));
                }
                let millis = ms.round();
                if (millis - ms).abs() > f64::EPSILON {
                    return Err(js_error(
                        "invalid date-time: timestamp must be milliseconds",
                    ));
                }
                DateTime::<Utc>::from_timestamp_millis(millis as i64)
                    .map(CalendarDateValue::DateTime)
                    .ok_or_else(|| js_error("invalid date-time: timestamp out of range"))
            }
        }
    }
}

impl From<&CalendarDateValue> for JsCalendarDateValue {
    fn from(value: &CalendarDateValue) -> Self {
        match value {
            CalendarDateValue::Date(date) => Self::Date {
                date: date.format("%Y-%m-%d").to_string(),
            },
            CalendarDateValue::DateTime(date_time) => Self::DateTime {
                ms: date_time.timestamp_millis() as f64,
            },
        }
    }
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
            dtstart: event.dtstart.as_ref().map(JsCalendarDateValue::from),
            dtend: event.dtend.as_ref().map(JsCalendarDateValue::from),
            rrule: event.rrule.as_ref().map(JsRrule::from),
            attendees: event.attendees.iter().map(JsAttendee::from).collect(),
            recurrence_id: event.recurrence_id.as_ref().map(JsCalendarDateValue::from),
            exdates: event
                .exdates
                .iter()
                .map(JsCalendarDateValue::from)
                .collect(),
        }
    }
}

// ── IQ builders / parsers ───────────────────────────────────────────

fn build_events_items_iq(community_service: &jid::BareJid, max_items: Option<u32>) -> Element {
    let id = format!("xcal-items-{}", Uuid::new_v4());
    build_pubsub_items_iq(&id, Some(community_service), PUBSUB_NODE_EVENTS, max_items)
}

/// Build an RSVP item payload for a master event: a VEVENT carrying
/// only the master UID and a single `<attendee/>` with the chosen
/// PartStat. Item id uses the canonical `<master-uid>-rsvp-<localpart>`
/// shape so the chat can fold sibling RSVP items back into the master.
fn build_rsvp_publish_iq(
    community_service: &jid::BareJid,
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
    build_pubsub_publish_iq(
        &id,
        Some(community_service),
        PUBSUB_NODE_EVENTS,
        Some(item_id.as_str()),
        build_vcalendar_with_event(&rsvp_event),
        None,
    )
}

/// Build a publish IQ for a CalendarItem (master + overrides) at
/// the given item id. Used by `xcal_publish_item` to atomically
/// write a master event plus its per-instance overrides + EXDATEs.
fn build_item_publish_iq(
    community_service: &jid::BareJid,
    item_id: &str,
    item: &CalendarItem,
) -> Element {
    let id = format!("xcal-publish-{}", Uuid::new_v4());
    build_pubsub_publish_iq(
        &id,
        Some(community_service),
        PUBSUB_NODE_EVENTS,
        Some(item_id),
        build_vcalendar_with_item(item),
        None,
    )
}

fn vevent_from_input(uid: &str, input: JsVEventInput) -> Result<VEvent, JsValue> {
    let mut event = VEvent::new(uid, input.summary).with_dtstamp(Utc::now());
    if let Some(dtstart) = input.dtstart {
        event = event.with_dtstart_value(dtstart.try_into()?);
    }
    if let Some(dtend) = input.dtend {
        event = event.with_dtend_value(dtend.try_into()?);
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
    validate_calendar_date_types(&event)?;
    Ok(event)
}

fn override_from_input(uid: &str, input: JsOverrideInput) -> Result<VEvent, JsValue> {
    let recurrence_id: CalendarDateValue = input.recurrence_id.try_into()?;
    let mut event = VEvent::new(uid, input.summary.unwrap_or_default())
        .with_dtstamp(Utc::now())
        .with_recurrence_id_value(recurrence_id);
    if let Some(dtstart) = input.dtstart {
        event = event.with_dtstart_value(dtstart.try_into()?);
    }
    if let Some(dtend) = input.dtend {
        event = event.with_dtend_value(dtend.try_into()?);
    }
    if let Some(description) = input.description {
        event = event.with_description(description);
    }
    if let Some(location) = input.location {
        event = event.with_location(location);
    }
    validate_calendar_date_types(&event)?;
    Ok(event)
}

fn validate_calendar_date_types(event: &VEvent) -> Result<(), JsValue> {
    validate_calendar_event(event).map_err(js_error)
}

fn validate_calendar_event(event: &VEvent) -> Result<(), &'static str> {
    if calendar_date_type_mismatch(event) {
        return Err("DTSTART, DTEND, RECURRENCE-ID, and EXDATE must use the same date value type");
    }
    if calendar_date_order_invalid(event) {
        return Err("DTEND must be after DTSTART");
    }
    if all_day_until_invalid(event) {
        return Err("All-day recurring events must use COUNT until DATE UNTIL is supported");
    }
    Ok(())
}

fn calendar_date_type_mismatch(event: &VEvent) -> bool {
    let expected = event
        .dtstart
        .or(event.recurrence_id)
        .map(CalendarDateValue::kind);
    let Some(expected) = expected else {
        return false;
    };
    event.dtend.is_some_and(|value| value.kind() != expected)
        || event
            .recurrence_id
            .is_some_and(|value| value.kind() != expected)
        || event.exdates.iter().any(|value| value.kind() != expected)
}

fn calendar_date_order_invalid(event: &VEvent) -> bool {
    let Some(start) = event.dtstart.or(event.recurrence_id) else {
        return false;
    };
    event
        .dtend
        .is_some_and(|end| !calendar_date_after(end, start))
}

fn all_day_until_invalid(event: &VEvent) -> bool {
    matches!(event.dtstart, Some(CalendarDateValue::Date(_)))
        && matches!(
            event.rrule.as_ref().and_then(|rule| rule.end.clone()),
            Some(RruleEnd::Until(_))
        )
}

fn calendar_date_after(end: CalendarDateValue, start: CalendarDateValue) -> bool {
    match (end, start) {
        (CalendarDateValue::Date(end), CalendarDateValue::Date(start)) => end > start,
        (CalendarDateValue::DateTime(end), CalendarDateValue::DateTime(start)) => end > start,
        _ => false,
    }
}

fn validate_calendar_item(item: &CalendarItem) -> Result<(), JsValue> {
    validate_calendar_item_data(item).map_err(js_error)
}

fn validate_calendar_item_data(item: &CalendarItem) -> Result<(), &'static str> {
    validate_calendar_event(&item.master)?;
    let master_kind = item.master.dtstart.map(CalendarDateValue::kind);
    for override_event in &item.overrides {
        validate_calendar_event(override_event)?;
        if master_kind.is_some_and(|kind| {
            override_event
                .recurrence_id
                .is_some_and(|value| value.kind() != kind)
        }) {
            return Err(
                "Override RECURRENCE-ID must use the same date value type as the master DTSTART",
            );
        }
    }
    Ok(())
}

fn build_event_publish_iq(
    community_service: &jid::BareJid,
    item_id: &str,
    event: &VEvent,
) -> Element {
    let id = format!("xcal-publish-{}", Uuid::new_v4());
    build_pubsub_publish_iq(
        &id,
        Some(community_service),
        PUBSUB_NODE_EVENTS,
        Some(item_id),
        build_vcalendar_with_event(event),
        None,
    )
}

fn parse_events_items_result(iq: &Element) -> Vec<JsVEvent> {
    let mut flattened = Vec::new();
    for item in parse_pubsub_items_result(iq) {
        let item_id = item.id.as_str();
        let Some(vcal) = item.payload("vcalendar", NS_XCAL) else {
            continue;
        };
        let Some(parsed) = parse_vcalendar_item(item_id, vcal) else {
            continue;
        };
        // Master comes back under the item id; each override gets a
        // synthetic id `<item-id>::override::<typed-recurrence-id>`
        // so the chat can address them individually without losing
        // the master/override correlation (preserved via `uid`).
        flattened.push(JsVEvent::from_vevent(item_id, parsed.master));
        for ov in parsed.overrides {
            let synthetic = ov
                .recurrence_id
                .map(|value| format!("{item_id}::override::{}", calendar_date_key(value)))
                .unwrap_or_else(|| item_id.to_string());
            flattened.push(JsVEvent::from_vevent(&synthetic, ov));
        }
    }
    flattened
}

fn calendar_date_key(value: CalendarDateValue) -> String {
    match value {
        CalendarDateValue::Date(date) => format!("date:{}", date.format("%Y-%m-%d")),
        CalendarDateValue::DateTime(ts) => format!("date-time:{}", ts.to_rfc3339()),
    }
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
            let service = service_bare_jid(&community_jid)?;
            let iq = build_events_items_iq(&service, max_items);
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
            let service = service_bare_jid(&community_jid)?;
            let input: JsVEventInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid event input: {err}")))?;
            let item_id = format!("evt-{}", Uuid::new_v4());
            let event = vevent_from_input(&item_id, input)?;
            let iq = build_event_publish_iq(&service, &item_id, &event);
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
            let service = service_bare_jid(&community_jid)?;
            let input: JsCalendarItemInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid calendar item input: {err}")))?;
            if item_id.is_empty() {
                return Err(js_error("item_id must not be empty"));
            }
            let mut master = vevent_from_input(&item_id, input.master)?;
            for exdate in input.exdates {
                master = master.add_exdate_value(exdate.try_into()?);
            }
            let mut item = CalendarItem::new(master);
            for ov in input.overrides {
                item = item.add_override(override_from_input(&item_id, ov)?);
            }
            validate_calendar_item(&item)?;
            let iq = build_item_publish_iq(&service, &item_id, &item);
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
            let service = service_bare_jid(&community_jid)?;
            let id = format!("xcal-retract-{}", Uuid::new_v4());
            let iq =
                build_pubsub_retract_iq(&id, Some(&service), PUBSUB_NODE_EVENTS, item_id.as_str());
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
            let service = service_bare_jid(&community_jid)?;
            let iq =
                build_rsvp_publish_iq(&service, &master_uid, &self_localpart, &self_jid, partstat);
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
