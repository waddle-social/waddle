//! iCalendar in XML (xCal) for community events.
//!
//! Implements the wire shape required by the XSF ProtoXEP
//! "Calendaring Extensions to Publish-Subscribe" (currently inbox/
//! calendaring.xml, no assigned XEP number). The ProtoXEP wraps
//! iCalendar (RFC 5545) calendar objects in XML using the xCal
//! namespace `urn:ietf:params:xml:ns:xcal`. We model the subset
//! needed for community-event use cases: `<vevent/>` with
//! `<uid/>`, `<dtstamp/>`, `<dtstart/>`, optional `<dtend/>` (or
//! `<duration/>`), `<summary/>`, `<description/>`, `<location/>`,
//! `<organizer/>`, and `<rrule/>` for recurrence.
//!
//! ## Wire example
//!
//! Weekly community game night, repeating every Friday at 19:00
//! UTC, ten times:
//!
//! ```xml
//! <item id='evt-1'>
//!   <vcalendar xmlns='urn:ietf:params:xml:ns:xcal'>
//!     <vevent>
//!       <uid>evt-1</uid>
//!       <dtstamp>2026-06-01T12:00:00Z</dtstamp>
//!       <dtstart>2026-06-05T19:00:00Z</dtstart>
//!       <dtend>2026-06-05T22:00:00Z</dtend>
//!       <summary>Game Night</summary>
//!       <description>Weekly gaming session</description>
//!       <location>Voice #gaming</location>
//!       <organizer>xmpp:alice@example.com</organizer>
//!       <rrule>
//!         <freq>WEEKLY</freq>
//!         <interval>1</interval>
//!         <byday><weekday>FR</weekday></byday>
//!         <count>10</count>
//!       </rrule>
//!     </vevent>
//!   </vcalendar>
//! </item>
//! ```
//!
//! ## Out of scope (today)
//!
//! VTODO, VJOURNAL, VALARM, VTIMEZONE, EXRULE. ATTENDEE (RSVP),
//! RECURRENCE-ID overrides and EXDATE are modelled; the chat
//! aggregates per-user RSVP items into the master event and expands
//! recurring instances client-side.

use chrono::{DateTime, Utc};
use minidom::Element;

/// xCal namespace (RFC 5545 in XML serialisation — xCal-Basic
/// `draft-royer-calsch-xcal`).
pub const NS_XCAL: &str = "urn:ietf:params:xml:ns:xcal";

/// Conventional node name for the community-wide events calendar
/// hosted on the community service. The ProtoXEP doesn't mandate a
/// fixed node name (calendar nodes carry `pubsub#type` config), so
/// we pin one for Waddle's deployment topology.
pub const PUBSUB_NODE_EVENTS: &str = "urn:xmpp:calendar:0";

/// `pubsub#type` form value the calendar node MUST advertise per
/// the ProtoXEP §"Creating Calendars".
pub const PUBSUB_TYPE_XCAL: &str = "urn:ietf:params:xml:ns:xcal";

// ── RRULE (RFC 5545 §3.3.10) ────────────────────────────────────────

/// Recurrence frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "DAILY",
            Self::Weekly => "WEEKLY",
            Self::Monthly => "MONTHLY",
            Self::Yearly => "YEARLY",
        }
    }

    pub fn from_str_value(s: &str) -> Option<Self> {
        match s {
            "DAILY" => Some(Self::Daily),
            "WEEKLY" => Some(Self::Weekly),
            "MONTHLY" => Some(Self::Monthly),
            "YEARLY" => Some(Self::Yearly),
            _ => None,
        }
    }
}

/// ISO weekday abbreviations used in RFC 5545 BYDAY (SU, MO, TU,
/// WE, TH, FR, SA).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weekday {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl Weekday {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sunday => "SU",
            Self::Monday => "MO",
            Self::Tuesday => "TU",
            Self::Wednesday => "WE",
            Self::Thursday => "TH",
            Self::Friday => "FR",
            Self::Saturday => "SA",
        }
    }

    pub fn from_str_value(s: &str) -> Option<Self> {
        match s {
            "SU" => Some(Self::Sunday),
            "MO" => Some(Self::Monday),
            "TU" => Some(Self::Tuesday),
            "WE" => Some(Self::Wednesday),
            "TH" => Some(Self::Thursday),
            "FR" => Some(Self::Friday),
            "SA" => Some(Self::Saturday),
            _ => None,
        }
    }
}

/// Either-or terminator for a recurrence series (RFC 5545 §3.3.10).
/// COUNT and UNTIL are mutually exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RruleEnd {
    Count(u32),
    Until(DateTime<Utc>),
}

/// Recurrence rule subset suitable for community events. RFC 5545
/// defines many more parameters (BYMONTHDAY, BYHOUR, BYSETPOS,
/// WKST, ...); we model the ones with concrete UI affordances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rrule {
    pub freq: Freq,
    /// Interval between occurrences. Defaults to 1 per RFC 5545
    /// when omitted on the wire.
    pub interval: Option<u32>,
    /// Days of the week the rule fires on. Empty ⇒ inherited from
    /// DTSTART (single-day weekly, etc.) per RFC 5545.
    pub by_day: Vec<Weekday>,
    /// Days of the month the rule fires on (1–31).
    pub by_month_day: Vec<i32>,
    /// Series terminator (COUNT or UNTIL). Open-ended when absent.
    pub end: Option<RruleEnd>,
}

impl Rrule {
    pub fn new(freq: Freq) -> Self {
        Self {
            freq,
            interval: None,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            end: None,
        }
    }

    pub fn with_interval(mut self, interval: u32) -> Self {
        self.interval = Some(interval);
        self
    }

    pub fn with_by_day<I: IntoIterator<Item = Weekday>>(mut self, days: I) -> Self {
        self.by_day = days.into_iter().collect();
        self
    }

    pub fn with_by_month_day<I: IntoIterator<Item = i32>>(mut self, days: I) -> Self {
        self.by_month_day = days.into_iter().collect();
        self
    }

    pub fn with_count(mut self, count: u32) -> Self {
        self.end = Some(RruleEnd::Count(count));
        self
    }

    pub fn with_until(mut self, until: DateTime<Utc>) -> Self {
        self.end = Some(RruleEnd::Until(until));
        self
    }
}

// ── ATTENDEE / PARTSTAT (RFC 5545 §3.2.12 + §3.8.4.1) ──────────────

/// Participation status of an attendee. RFC 5545 §3.2.12 defines a
/// small enum; we model the four states the chat UX exposes (Going /
/// Maybe / Not going + the initial "no answer yet" placeholder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartStat {
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
}

impl PartStat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsAction => "NEEDS-ACTION",
            Self::Accepted => "ACCEPTED",
            Self::Declined => "DECLINED",
            Self::Tentative => "TENTATIVE",
        }
    }

    pub fn from_str_value(s: &str) -> Option<Self> {
        match s {
            "NEEDS-ACTION" => Some(Self::NeedsAction),
            "ACCEPTED" => Some(Self::Accepted),
            "DECLINED" => Some(Self::Declined),
            "TENTATIVE" => Some(Self::Tentative),
            _ => None,
        }
    }
}

/// VEVENT ATTENDEE (RFC 5545 §3.8.4.1) — a single participant and
/// their RSVP status. URI is typically `xmpp:user@example.com`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attendee {
    pub uri: String,
    pub partstat: PartStat,
    pub role: Option<String>,
    pub rsvp: Option<bool>,
}

impl Attendee {
    pub fn new(uri: impl Into<String>, partstat: PartStat) -> Self {
        Self {
            uri: uri.into(),
            partstat,
            role: None,
            rsvp: None,
        }
    }

    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    pub fn with_rsvp(mut self, rsvp: bool) -> Self {
        self.rsvp = Some(rsvp);
        self
    }
}

// ── VEVENT ──────────────────────────────────────────────────────────

/// Calendar event modelled as the iCalendar VEVENT component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VEvent {
    /// RFC 5545 UID. We reuse the pubsub item id by convention.
    pub uid: String,
    /// RFC 5545 DTSTAMP — when the calendar item was created /
    /// last modified. Defaults to publish time at the call site.
    pub dtstamp: Option<DateTime<Utc>>,
    /// DTSTART (required for a usable event).
    pub dtstart: Option<DateTime<Utc>>,
    /// DTEND — optional, mutually exclusive with DURATION (we model
    /// only DTEND for now).
    pub dtend: Option<DateTime<Utc>>,
    /// SUMMARY — required for a displayable event.
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    /// Organizer URI — typically `xmpp:user@example.com`.
    pub organizer: Option<String>,
    /// Optional RRULE for recurring events.
    pub rrule: Option<Rrule>,
    /// ATTENDEE list. Empty for events with no RSVPs yet; the chat
    /// aggregates per-attendee RSVPs from sibling pubsub items into
    /// this list on the master event before rendering.
    pub attendees: Vec<Attendee>,
    /// RFC 5545 §3.8.4.4 RECURRENCE-ID — set on override VEVENT
    /// components that replace a single occurrence of the master
    /// series. `None` on the master event itself.
    pub recurrence_id: Option<DateTime<Utc>>,
    /// RFC 5545 §3.8.5.1 EXDATE — occurrence DTSTART values that
    /// should be skipped (per-instance cancellations). Only
    /// meaningful on the master event.
    pub exdates: Vec<DateTime<Utc>>,
}

/// A logical calendar item: one master VEVENT plus any per-instance
/// override VEVENTs (each identified by `recurrence_id`). All
/// components share a UID. Per ProtoXEP §"Calendar Items" they MUST
/// ship inside the same pubsub item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarItem {
    pub master: VEvent,
    pub overrides: Vec<VEvent>,
}

impl CalendarItem {
    pub fn new(master: VEvent) -> Self {
        Self {
            master,
            overrides: Vec::new(),
        }
    }

    pub fn add_override(mut self, override_event: VEvent) -> Self {
        self.overrides.push(override_event);
        self
    }
}

impl VEvent {
    pub fn new(uid: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            dtstamp: None,
            dtstart: None,
            dtend: None,
            summary: summary.into(),
            description: None,
            location: None,
            organizer: None,
            rrule: None,
            attendees: Vec::new(),
            recurrence_id: None,
            exdates: Vec::new(),
        }
    }

    pub fn with_dtstamp(mut self, dtstamp: DateTime<Utc>) -> Self {
        self.dtstamp = Some(dtstamp);
        self
    }

    pub fn with_dtstart(mut self, dtstart: DateTime<Utc>) -> Self {
        self.dtstart = Some(dtstart);
        self
    }

    pub fn with_dtend(mut self, dtend: DateTime<Utc>) -> Self {
        self.dtend = Some(dtend);
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    pub fn with_organizer(mut self, organizer: impl Into<String>) -> Self {
        self.organizer = Some(organizer.into());
        self
    }

    pub fn with_rrule(mut self, rrule: Rrule) -> Self {
        self.rrule = Some(rrule);
        self
    }

    pub fn with_attendees<I: IntoIterator<Item = Attendee>>(mut self, attendees: I) -> Self {
        self.attendees = attendees.into_iter().collect();
        self
    }

    pub fn add_attendee(mut self, attendee: Attendee) -> Self {
        self.attendees.push(attendee);
        self
    }

    pub fn with_recurrence_id(mut self, recurrence_id: DateTime<Utc>) -> Self {
        self.recurrence_id = Some(recurrence_id);
        self
    }

    pub fn with_exdates<I: IntoIterator<Item = DateTime<Utc>>>(mut self, exdates: I) -> Self {
        self.exdates = exdates.into_iter().collect();
        self
    }

    pub fn add_exdate(mut self, exdate: DateTime<Utc>) -> Self {
        self.exdates.push(exdate);
        self
    }

    /// `true` when the event starts in the future. For recurring
    /// events this only reflects the DTSTART of the master series;
    /// upcoming-instance expansion is a separate concern from the
    /// wire shape.
    pub fn is_upcoming(&self) -> bool {
        self.dtstart.is_some_and(|s| s > Utc::now())
    }
}

// ── Build / parse ───────────────────────────────────────────────────

fn append_text_child(parent: &mut Element, name: &str, value: &str) {
    let mut child = Element::builder(name, NS_XCAL).build();
    child.append_text_node(value);
    parent.append_child(child);
}

fn build_rrule_element(rrule: &Rrule) -> Element {
    let mut elem = Element::builder("rrule", NS_XCAL).build();
    append_text_child(&mut elem, "freq", rrule.freq.as_str());
    if let Some(interval) = rrule.interval {
        append_text_child(&mut elem, "interval", &interval.to_string());
    }
    if !rrule.by_day.is_empty() {
        let mut by_day = Element::builder("byday", NS_XCAL).build();
        for wd in &rrule.by_day {
            append_text_child(&mut by_day, "weekday", wd.as_str());
        }
        elem.append_child(by_day);
    }
    if !rrule.by_month_day.is_empty() {
        let mut by_month = Element::builder("bymonthday", NS_XCAL).build();
        for d in &rrule.by_month_day {
            append_text_child(&mut by_month, "monthday", &d.to_string());
        }
        elem.append_child(by_month);
    }
    match rrule.end {
        Some(RruleEnd::Count(n)) => append_text_child(&mut elem, "count", &n.to_string()),
        Some(RruleEnd::Until(ts)) => append_text_child(&mut elem, "until", &ts.to_rfc3339()),
        None => {}
    }
    elem
}

/// Build a `<vcalendar><vevent/></vcalendar>` payload for a single
/// event. Wraps the VEVENT in a VCALENDAR with `<version>2.0</version>`
/// per RFC 5545. Equivalent to `build_vcalendar_with_item` for a
/// non-recurring item with no overrides.
pub fn build_vcalendar_with_event(event: &VEvent) -> Element {
    build_vcalendar_with_item(&CalendarItem {
        master: event.clone(),
        overrides: Vec::new(),
    })
}

/// Build a `<vcalendar/>` payload carrying a master VEVENT plus
/// zero or more sibling override VEVENTs (one per RECURRENCE-ID).
/// Per ProtoXEP §"Calendar Items", components sharing a UID MUST
/// live in the same pubsub item — this is the shape that lets a
/// recurring event carry per-instance edits.
pub fn build_vcalendar_with_item(item: &CalendarItem) -> Element {
    let mut vcalendar = Element::builder("vcalendar", NS_XCAL).build();
    append_text_child(&mut vcalendar, "version", "2.0");
    vcalendar.append_child(build_vevent_element(&item.master));
    for override_event in &item.overrides {
        vcalendar.append_child(build_vevent_element(override_event));
    }
    vcalendar
}

/// Build a standalone `<vevent>` element (no enclosing
/// `<vcalendar>`). Used by both `build_vcalendar_with_event` and
/// `build_vcalendar_with_item` so master + override events share
/// exactly one serializer.
fn build_vevent_element(event: &VEvent) -> Element {
    let mut vevent = Element::builder("vevent", NS_XCAL).build();
    append_text_child(&mut vevent, "uid", &event.uid);
    if let Some(dtstamp) = event.dtstamp {
        append_text_child(&mut vevent, "dtstamp", &dtstamp.to_rfc3339());
    }
    if let Some(dtstart) = event.dtstart {
        append_text_child(&mut vevent, "dtstart", &dtstart.to_rfc3339());
    }
    if let Some(dtend) = event.dtend {
        append_text_child(&mut vevent, "dtend", &dtend.to_rfc3339());
    }
    if !event.summary.is_empty() {
        append_text_child(&mut vevent, "summary", &event.summary);
    }
    if let Some(ref desc) = event.description {
        append_text_child(&mut vevent, "description", desc);
    }
    if let Some(ref loc) = event.location {
        append_text_child(&mut vevent, "location", loc);
    }
    if let Some(ref org) = event.organizer {
        append_text_child(&mut vevent, "organizer", org);
    }
    if let Some(ref rrule) = event.rrule {
        vevent.append_child(build_rrule_element(rrule));
    }
    if let Some(recurrence_id) = event.recurrence_id {
        append_text_child(&mut vevent, "recurrence-id", &recurrence_id.to_rfc3339());
    }
    for exdate in &event.exdates {
        append_text_child(&mut vevent, "exdate", &exdate.to_rfc3339());
    }
    for attendee in &event.attendees {
        vevent.append_child(build_attendee_element(attendee));
    }
    vevent
}

fn build_attendee_element(attendee: &Attendee) -> Element {
    let mut builder =
        Element::builder("attendee", NS_XCAL).attr("partstat", attendee.partstat.as_str());
    if let Some(ref role) = attendee.role {
        builder = builder.attr("role", role);
    }
    if let Some(rsvp) = attendee.rsvp {
        builder = builder.attr("rsvp", if rsvp { "TRUE" } else { "FALSE" });
    }
    let mut elem = builder.build();
    elem.append_text_node(attendee.uri.as_str());
    elem
}

/// `true` when an element is a `<vcalendar/>` in the xCal namespace.
pub fn is_vcalendar_element(elem: &Element) -> bool {
    elem.ns() == NS_XCAL && elem.name() == "vcalendar"
}

fn xcal_text(parent: &Element, name: &str) -> Option<String> {
    parent
        .children()
        .find(|c| c.name() == name && c.ns() == NS_XCAL)
        .map(|c| c.text())
        .filter(|t| !t.is_empty())
}

fn xcal_child<'a>(parent: &'a Element, name: &str) -> Option<&'a Element> {
    parent
        .children()
        .find(|c| c.name() == name && c.ns() == NS_XCAL)
}

fn parse_rrule(elem: &Element) -> Option<Rrule> {
    let freq = Freq::from_str_value(&xcal_text(elem, "freq")?)?;
    let interval = xcal_text(elem, "interval").and_then(|s| s.parse().ok());
    let by_day = xcal_child(elem, "byday")
        .map(|by| {
            by.children()
                .filter(|c| c.name() == "weekday" && c.ns() == NS_XCAL)
                .filter_map(|c| Weekday::from_str_value(&c.text()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let by_month_day = xcal_child(elem, "bymonthday")
        .map(|by| {
            by.children()
                .filter(|c| c.name() == "monthday" && c.ns() == NS_XCAL)
                .filter_map(|c| c.text().parse().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let end = if let Some(count) = xcal_text(elem, "count").and_then(|s| s.parse().ok()) {
        Some(RruleEnd::Count(count))
    } else {
        xcal_text(elem, "until")
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
            .map(RruleEnd::Until)
    };
    Some(Rrule {
        freq,
        interval,
        by_day,
        by_month_day,
        end,
    })
}

/// Parse a single VEVENT child off a `<vcalendar>` element. Returns
/// the master event (the one without a `<recurrence-id>` child) when
/// the calendar contains multiple sibling VEVENTs; per-instance
/// overrides are surfaced via `parse_vcalendar_item`. SUMMARY is
/// permitted to be empty so that override events (which only patch
/// a subset of fields) round-trip cleanly.
pub fn parse_vcalendar_event(item_id: &str, vcalendar: &Element) -> Option<VEvent> {
    let item = parse_vcalendar_item(item_id, vcalendar)?;
    Some(item.master)
}

/// Parse a `<vcalendar/>` into a `CalendarItem` (master + per-
/// instance overrides). When the calendar carries multiple sibling
/// VEVENTs, the one *without* a `<recurrence-id>` is the master and
/// the rest are overrides; when there's only one VEVENT it's the
/// master and `overrides` is empty.
pub fn parse_vcalendar_item(item_id: &str, vcalendar: &Element) -> Option<CalendarItem> {
    if !is_vcalendar_element(vcalendar) {
        return None;
    }
    let vevents: Vec<&Element> = vcalendar
        .children()
        .filter(|c| c.name() == "vevent" && c.ns() == NS_XCAL)
        .collect();
    if vevents.is_empty() {
        return None;
    }
    let mut master: Option<VEvent> = None;
    let mut overrides: Vec<VEvent> = Vec::new();
    for vevent in vevents {
        let parsed = parse_vevent_element(item_id, vevent);
        if parsed.recurrence_id.is_some() {
            overrides.push(parsed);
        } else if master.is_none() {
            master = Some(parsed);
        } else {
            // Multiple components without RECURRENCE-ID share a UID —
            // unusual but treat the extras as overrides anchored to
            // their own DTSTART to keep all components reachable.
            overrides.push(parsed);
        }
    }
    let master = master?;
    Some(CalendarItem { master, overrides })
}

fn parse_vevent_element(item_id: &str, vevent: &Element) -> VEvent {
    let summary = xcal_text(vevent, "summary").unwrap_or_default();
    let uid = xcal_text(vevent, "uid").unwrap_or_else(|| item_id.to_string());
    let dtstamp = xcal_text(vevent, "dtstamp").and_then(|s| s.parse().ok());
    let dtstart = xcal_text(vevent, "dtstart").and_then(|s| s.parse().ok());
    let dtend = xcal_text(vevent, "dtend").and_then(|s| s.parse().ok());
    let description = xcal_text(vevent, "description");
    let location = xcal_text(vevent, "location");
    let organizer = xcal_text(vevent, "organizer");
    let rrule = xcal_child(vevent, "rrule").and_then(parse_rrule);
    let attendees = vevent
        .children()
        .filter(|c| c.name() == "attendee" && c.ns() == NS_XCAL)
        .filter_map(parse_attendee)
        .collect();
    let recurrence_id = xcal_text(vevent, "recurrence-id").and_then(|s| s.parse().ok());
    let exdates = vevent
        .children()
        .filter(|c| c.name() == "exdate" && c.ns() == NS_XCAL)
        .filter_map(|c| {
            let t = c.text();
            t.trim().parse::<DateTime<Utc>>().ok()
        })
        .collect();
    VEvent {
        uid,
        dtstamp,
        dtstart,
        dtend,
        summary,
        description,
        location,
        organizer,
        rrule,
        attendees,
        recurrence_id,
        exdates,
    }
}

fn parse_attendee(elem: &Element) -> Option<Attendee> {
    let uri = elem.text();
    let uri = uri.trim();
    if uri.is_empty() {
        return None;
    }
    let partstat = elem
        .attr("partstat")
        .and_then(PartStat::from_str_value)
        .unwrap_or(PartStat::NeedsAction);
    let role = elem.attr("role").map(str::to_string);
    let rsvp = elem.attr("rsvp").and_then(|v| match v {
        "TRUE" | "true" => Some(true),
        "FALSE" | "false" => Some(false),
        _ => None,
    });
    Some(Attendee {
        uri: uri.to_string(),
        partstat,
        role,
        rsvp,
    })
}

/// Bare-JID localpart of an `xmpp:` URI, lower-cased. Returns `None`
/// for non-XMPP URIs or malformed values. Used by the chat to map
/// attendee URIs back to JIDs and by the server to authorise RSVP
/// publishes (an attendee URI must match the publisher's bare JID).
pub fn xmpp_uri_to_bare_jid(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("xmpp:")?;
    let stripped = rest.split(['?', '/']).next().unwrap_or(rest);
    if !stripped.contains('@') {
        return None;
    }
    Some(stripped.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid date")
    }

    #[test]
    fn freq_round_trip() {
        for f in [Freq::Daily, Freq::Weekly, Freq::Monthly, Freq::Yearly] {
            assert_eq!(Freq::from_str_value(f.as_str()), Some(f));
        }
        assert_eq!(Freq::from_str_value("hourly"), None);
    }

    #[test]
    fn weekday_round_trip() {
        for wd in [
            Weekday::Sunday,
            Weekday::Monday,
            Weekday::Tuesday,
            Weekday::Wednesday,
            Weekday::Thursday,
            Weekday::Friday,
            Weekday::Saturday,
        ] {
            assert_eq!(Weekday::from_str_value(wd.as_str()), Some(wd));
        }
        assert_eq!(Weekday::from_str_value("nope"), None);
    }

    #[test]
    fn build_and_parse_minimal_event() {
        let event = VEvent::new("evt-1", "Game Night")
            .with_dtstamp(ts(2026, 6, 1, 12, 0))
            .with_dtstart(ts(2026, 6, 15, 19, 0))
            .with_dtend(ts(2026, 6, 15, 22, 0))
            .with_description("Weekly gaming")
            .with_location("Voice #gaming")
            .with_organizer("xmpp:alice@example.com");
        let vcal = build_vcalendar_with_event(&event);
        assert_eq!(vcal.name(), "vcalendar");
        assert_eq!(vcal.ns(), NS_XCAL);

        let parsed = parse_vcalendar_event("evt-1", &vcal).expect("parseable");
        assert_eq!(parsed.uid, "evt-1");
        assert_eq!(parsed.summary, "Game Night");
        assert_eq!(parsed.dtstart, Some(ts(2026, 6, 15, 19, 0)));
        assert_eq!(parsed.dtend, Some(ts(2026, 6, 15, 22, 0)));
        assert_eq!(parsed.description.as_deref(), Some("Weekly gaming"));
        assert_eq!(parsed.location.as_deref(), Some("Voice #gaming"));
        assert_eq!(parsed.organizer.as_deref(), Some("xmpp:alice@example.com"));
        assert!(parsed.rrule.is_none());
    }

    #[test]
    fn build_and_parse_weekly_recurring_event_with_count() {
        let rrule = Rrule::new(Freq::Weekly)
            .with_interval(1)
            .with_by_day([Weekday::Friday])
            .with_count(10);
        let event = VEvent::new("evt-weekly", "Friday Game Night")
            .with_dtstart(ts(2026, 6, 5, 19, 0))
            .with_dtend(ts(2026, 6, 5, 22, 0))
            .with_rrule(rrule.clone());
        let vcal = build_vcalendar_with_event(&event);
        let serialised = String::from(&vcal);
        // minidom inherits the xCal namespace from the <vcalendar/>
        // root, so child elements serialise without redundant
        // xmlns= attributes. The wire shape is still correct per
        // RFC 5545 / xCal-Basic.
        assert!(
            serialised.contains(&format!("xmlns='{NS_XCAL}'"))
                || serialised.contains(&format!("xmlns=\"{NS_XCAL}\"")),
            "xCal namespace must be declared on the root: {serialised}"
        );
        assert!(
            serialised.contains("<freq>WEEKLY</freq>"),
            "WEEKLY freq missing: {serialised}"
        );
        assert!(
            serialised.contains("<count>10</count>"),
            "COUNT missing: {serialised}"
        );
        assert!(
            serialised.contains("<weekday>FR</weekday>"),
            "BYDAY=FR missing: {serialised}"
        );

        let parsed = parse_vcalendar_event("evt-weekly", &vcal).expect("parseable");
        assert_eq!(parsed.rrule, Some(rrule));
    }

    #[test]
    fn build_and_parse_monthly_recurring_event_with_until_and_bymonthday() {
        let rrule = Rrule::new(Freq::Monthly)
            .with_by_month_day([1, 15])
            .with_until(ts(2027, 1, 1, 0, 0));
        let event = VEvent::new("evt-monthly", "Book Club")
            .with_dtstart(ts(2026, 6, 1, 18, 0))
            .with_rrule(rrule.clone());
        let vcal = build_vcalendar_with_event(&event);
        let parsed = parse_vcalendar_event("evt-monthly", &vcal).expect("parseable");
        assert_eq!(parsed.rrule, Some(rrule));
    }

    #[test]
    fn parse_minimal_external_xml() {
        // Demonstrate that we can ingest a hand-rolled xCal item
        // matching the ProtoXEP example (Bastille Day Party).
        let xml = r#"<vcalendar xmlns='urn:ietf:params:xml:ns:xcal'>
  <version>2.0</version>
  <vevent>
    <uid>fcd5cb18</uid>
    <dtstamp>1997-06-11T19:00:00Z</dtstamp>
    <dtstart>1997-07-14T17:00:00Z</dtstart>
    <dtend>1997-07-15T03:59:59Z</dtend>
    <summary>Bastille Day Party</summary>
    <organizer>xmpp:a@example.com</organizer>
  </vevent>
</vcalendar>"#;
        let elem: Element = xml.parse().expect("valid xml");
        let event = parse_vcalendar_event("fcd5cb18", &elem).expect("parseable");
        assert_eq!(event.summary, "Bastille Day Party");
        assert_eq!(event.organizer.as_deref(), Some("xmpp:a@example.com"));
    }

    #[test]
    fn parse_tolerates_missing_summary_for_overrides() {
        // RFC 5545 makes SUMMARY optional. Overrides that only patch
        // a different field (DTSTART, LOCATION, …) ship without one,
        // so the parser surfaces an empty summary instead of dropping
        // the whole event.
        let xml = r#"<vcalendar xmlns='urn:ietf:params:xml:ns:xcal'>
  <vevent>
    <uid>no-summary</uid>
    <dtstart>2026-06-01T00:00:00Z</dtstart>
  </vevent>
</vcalendar>"#;
        let elem: Element = xml.parse().expect("valid xml");
        let event = parse_vcalendar_event("no-summary", &elem).expect("parseable");
        assert!(event.summary.is_empty());
    }

    #[test]
    fn parse_rejects_non_vcalendar_root() {
        let xml = r#"<event xmlns='urn:xmpp:calendar:0'><summary>Old shape</summary></event>"#;
        let elem: Element = xml.parse().expect("valid xml");
        assert!(parse_vcalendar_event("evt", &elem).is_none());
    }

    #[test]
    fn is_upcoming_matches_dtstart() {
        let future = VEvent::new("e", "Future").with_dtstart(ts(2050, 1, 1, 0, 0));
        assert!(future.is_upcoming());

        let past = VEvent::new("e", "Past").with_dtstart(ts(2020, 1, 1, 0, 0));
        assert!(!past.is_upcoming());
    }

    #[test]
    fn partstat_round_trip_covers_every_state() {
        for ps in [
            PartStat::NeedsAction,
            PartStat::Accepted,
            PartStat::Declined,
            PartStat::Tentative,
        ] {
            assert_eq!(PartStat::from_str_value(ps.as_str()), Some(ps));
        }
        assert_eq!(PartStat::from_str_value("UNKNOWN"), None);
    }

    #[test]
    fn build_and_parse_event_with_attendees() {
        let event = VEvent::new("evt-rsvp", "Game Night")
            .with_dtstart(ts(2026, 6, 5, 19, 0))
            .add_attendee(
                Attendee::new("xmpp:alice@example.com", PartStat::Accepted)
                    .with_role("REQ-PARTICIPANT")
                    .with_rsvp(true),
            )
            .add_attendee(Attendee::new("xmpp:bob@example.com", PartStat::Declined))
            .add_attendee(Attendee::new("xmpp:carol@example.com", PartStat::Tentative));

        let vcal = build_vcalendar_with_event(&event);
        let serialised = String::from(&vcal);
        assert!(
            serialised.contains("partstat=\"ACCEPTED\"")
                || serialised.contains("partstat='ACCEPTED'"),
            "ACCEPTED partstat must appear: {serialised}"
        );
        assert!(
            serialised.contains("xmpp:bob@example.com"),
            "attendee URI must appear: {serialised}"
        );

        let parsed = parse_vcalendar_event("evt-rsvp", &vcal).expect("parseable");
        assert_eq!(parsed.attendees.len(), 3);
        assert_eq!(parsed.attendees[0].uri, "xmpp:alice@example.com");
        assert_eq!(parsed.attendees[0].partstat, PartStat::Accepted);
        assert_eq!(parsed.attendees[0].role.as_deref(), Some("REQ-PARTICIPANT"));
        assert_eq!(parsed.attendees[0].rsvp, Some(true));
        assert_eq!(parsed.attendees[1].partstat, PartStat::Declined);
        assert_eq!(parsed.attendees[2].partstat, PartStat::Tentative);
    }

    #[test]
    fn attendee_without_partstat_defaults_to_needs_action() {
        let xml = r#"<vcalendar xmlns='urn:ietf:params:xml:ns:xcal'>
          <vevent>
            <uid>evt-default</uid>
            <summary>Default partstat</summary>
            <attendee>xmpp:alice@example.com</attendee>
          </vevent>
        </vcalendar>"#;
        let elem: Element = xml.parse().expect("valid xml");
        let event = parse_vcalendar_event("evt-default", &elem).expect("parseable");
        assert_eq!(event.attendees.len(), 1);
        assert_eq!(event.attendees[0].partstat, PartStat::NeedsAction);
    }

    #[test]
    fn attendee_with_empty_uri_is_dropped() {
        let xml = r#"<vcalendar xmlns='urn:ietf:params:xml:ns:xcal'>
          <vevent>
            <uid>evt-empty</uid>
            <summary>Empty attendee</summary>
            <attendee partstat='ACCEPTED'></attendee>
          </vevent>
        </vcalendar>"#;
        let elem: Element = xml.parse().expect("valid xml");
        let event = parse_vcalendar_event("evt-empty", &elem).expect("parseable");
        assert!(
            event.attendees.is_empty(),
            "empty URI attendee must be dropped"
        );
    }

    #[test]
    fn build_and_parse_recurring_item_with_overrides_and_exdates() {
        let rrule = Rrule::new(Freq::Weekly)
            .with_by_day([Weekday::Friday])
            .with_count(8);
        let master = VEvent::new("evt-series", "Game Night")
            .with_dtstart(ts(2026, 6, 5, 19, 0))
            .with_dtend(ts(2026, 6, 5, 22, 0))
            .with_rrule(rrule)
            .with_exdates([ts(2026, 6, 19, 19, 0), ts(2026, 7, 3, 19, 0)]);
        let override_a = VEvent::new("evt-series", "Special: Halo")
            .with_dtstart(ts(2026, 6, 12, 20, 0))
            .with_recurrence_id(ts(2026, 6, 12, 19, 0));
        let override_b = VEvent {
            uid: "evt-series".to_string(),
            summary: String::new(),
            location: Some("New venue".to_string()),
            recurrence_id: Some(ts(2026, 6, 26, 19, 0)),
            ..VEvent::new("evt-series", "")
        };
        let item = CalendarItem::new(master)
            .add_override(override_a.clone())
            .add_override(override_b.clone());

        let vcal = build_vcalendar_with_item(&item);
        let serialised = String::from(&vcal);
        assert_eq!(serialised.matches("<exdate>").count(), 2);
        assert_eq!(serialised.matches("<recurrence-id>").count(), 2);

        let parsed = parse_vcalendar_item("evt-series", &vcal).expect("parseable");
        assert_eq!(parsed.master.uid, "evt-series");
        assert_eq!(parsed.master.summary, "Game Night");
        assert_eq!(parsed.master.exdates.len(), 2);
        assert_eq!(parsed.master.exdates[0], ts(2026, 6, 19, 19, 0));
        assert_eq!(parsed.overrides.len(), 2);
        assert_eq!(
            parsed.overrides[0].recurrence_id,
            Some(ts(2026, 6, 12, 19, 0))
        );
        assert_eq!(parsed.overrides[0].summary, "Special: Halo");
        assert_eq!(
            parsed.overrides[1].recurrence_id,
            Some(ts(2026, 6, 26, 19, 0))
        );
        assert_eq!(parsed.overrides[1].location.as_deref(), Some("New venue"));
        assert!(parsed.overrides[1].summary.is_empty());
    }

    #[test]
    fn parse_vcalendar_event_returns_master_when_overrides_are_siblings() {
        // External xml with master + override mixed in arbitrary order.
        // The master is the one without RECURRENCE-ID.
        let xml = r#"<vcalendar xmlns='urn:ietf:params:xml:ns:xcal'>
  <vevent>
    <uid>evt-series</uid>
    <recurrence-id>2026-06-12T19:00:00Z</recurrence-id>
    <summary>Override</summary>
  </vevent>
  <vevent>
    <uid>evt-series</uid>
    <dtstart>2026-06-05T19:00:00Z</dtstart>
    <summary>Master</summary>
  </vevent>
</vcalendar>"#;
        let elem: Element = xml.parse().expect("valid xml");
        let master = parse_vcalendar_event("evt-series", &elem).expect("parseable");
        assert_eq!(master.summary, "Master");
        assert!(master.recurrence_id.is_none());

        let item = parse_vcalendar_item("evt-series", &elem).expect("parseable");
        assert_eq!(item.overrides.len(), 1);
        assert_eq!(item.overrides[0].summary, "Override");
    }

    #[test]
    fn xmpp_uri_to_bare_jid_handles_common_cases() {
        assert_eq!(
            xmpp_uri_to_bare_jid("xmpp:alice@example.com").as_deref(),
            Some("alice@example.com")
        );
        // Lowercases — JIDs are case-insensitive at the bare-JID layer.
        assert_eq!(
            xmpp_uri_to_bare_jid("xmpp:Alice@Example.COM").as_deref(),
            Some("alice@example.com")
        );
        // Strips resource and query.
        assert_eq!(
            xmpp_uri_to_bare_jid("xmpp:alice@example.com/Resource").as_deref(),
            Some("alice@example.com")
        );
        assert_eq!(
            xmpp_uri_to_bare_jid("xmpp:alice@example.com?message").as_deref(),
            Some("alice@example.com")
        );
        // Rejects non-xmpp URIs and malformed values.
        assert_eq!(xmpp_uri_to_bare_jid("mailto:alice@example.com"), None);
        assert_eq!(xmpp_uri_to_bare_jid("xmpp:not-a-jid"), None);
    }
}
