//! Community events: the calendar's dates, RSVPs and the event form.
//!
//! Events are xCal items in the community's `urn:waddle:*` events node, read
//! and written through the shared core (`calendar.rs` in the FFI crate). This
//! module holds what the window needs from them that is not drawing: where an
//! occurrence falls on the person's clock, the agenda and month views, the
//! RSVP counts, and the form's rules for turning fields into a record.
//!
//! The core walks recurrences in UTC (the same on every client) and places an
//! all-day occurrence at midnight UTC of its date, so every window asked of it
//! is widened by a day on each side and narrowed again here by local date.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
    Weekday as ChronoWeekday,
};
use waddle_xmpp_client_ffi::{
    expand_community_event, WaddleCalendarDate, WaddleError, WaddleEvent, WaddleEventAttendee,
    WaddleEventParticipation, WaddlePlanningItem, WaddlePlanningKind, WaddlePlanningStatus,
    WaddleRecurrence, WaddleRecurrenceEnd, WaddleRecurrenceFrequency, WaddleWeekday,
};

use crate::runtime;
use crate::session::Session;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// A moment of an event: a calendar day (all-day) or an instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum When {
    Day(NaiveDate),
    /// Milliseconds since the Unix epoch.
    At(i64),
}

impl When {
    #[must_use]
    pub fn from_wire(date: &WaddleCalendarDate) -> Option<Self> {
        match date {
            WaddleCalendarDate::Date { date } => NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .ok()
                .map(Self::Day),
            WaddleCalendarDate::DateTime { timestamp_ms } => Some(Self::At(*timestamp_ms)),
        }
    }

    #[must_use]
    pub fn to_wire(self) -> WaddleCalendarDate {
        match self {
            Self::Day(day) => WaddleCalendarDate::Date {
                date: day.to_string(),
            },
            Self::At(timestamp_ms) => WaddleCalendarDate::DateTime { timestamp_ms },
        }
    }

    #[must_use]
    pub fn all_day(self) -> bool {
        matches!(self, Self::Day(_))
    }

    /// The date and time on the person's clock; an all-day date is as written.
    #[must_use]
    pub fn local<Tz: TimeZone>(self, tz: &Tz) -> NaiveDateTime {
        match self {
            Self::Day(day) => day.and_time(NaiveTime::MIN),
            Self::At(ms) => DateTime::from_timestamp_millis(ms)
                .map(|instant| instant.with_timezone(tz).naive_local())
                .unwrap_or_default(),
        }
    }

    #[must_use]
    pub fn local_day<Tz: TimeZone>(self, tz: &Tz) -> NaiveDate {
        self.local(tz).date()
    }
}

/// One dated occurrence of an event (or the event, when it does not repeat).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occurrence {
    /// The stored item's id, which edit, cancel and delete name.
    pub event_id: String,
    /// What RSVPs answer.
    pub uid: String,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: When,
    /// An all-day end is the day after the last day, as the core stores it.
    pub end: Option<When>,
    /// The start the series gave this date: the value to cancel it by.
    pub recurrence_id: Option<When>,
}

/// Where an event with no date sits: a day no event has.
const UNDATED: When = When::Day(NaiveDate::MIN);

impl Occurrence {
    /// An event the community has not given a date yet.
    #[must_use]
    pub fn undated(event: &WaddleEvent) -> Self {
        Self {
            event_id: event.id.clone(),
            uid: event.uid.clone(),
            title: event.summary.clone(),
            description: event.description.clone(),
            location: event.location.clone(),
            start: UNDATED,
            end: None,
            recurrence_id: None,
        }
    }

    #[must_use]
    pub fn is_undated(&self) -> bool {
        self.start == UNDATED
    }

    #[must_use]
    pub fn repeating(&self) -> bool {
        self.recurrence_id.is_some()
    }

    #[must_use]
    pub fn all_day(&self) -> bool {
        self.start.all_day()
    }

    /// The local days the occurrence covers, first to last.
    #[must_use]
    pub fn days<Tz: TimeZone>(&self, tz: &Tz) -> Vec<NaiveDate> {
        let first = self.start.local_day(tz);
        let Some(end) = self.end else {
            return vec![first];
        };
        let end = end.local(tz);
        // The end is exclusive: an event ending at midnight, or an all-day
        // one ending on a date, does not reach that day.
        let last = if end.time() == NaiveTime::MIN {
            end.date() - Duration::days(1)
        } else {
            end.date()
        };
        let mut days = vec![first];
        let mut day = first;
        while day < last && days.len() < 62 {
            day += Duration::days(1);
            days.push(day);
        }
        days
    }

    #[must_use]
    pub fn occurs_on<Tz: TimeZone>(&self, day: NaiveDate, tz: &Tz) -> bool {
        self.days(tz).contains(&day)
    }
}

fn from_wire_occurrence(
    event: &WaddleEvent,
    occurrence: &waddle_xmpp_client_ffi::WaddleEventOccurrence,
) -> Option<Occurrence> {
    Some(Occurrence {
        event_id: event.id.clone(),
        uid: event.uid.clone(),
        title: occurrence.summary.clone(),
        description: occurrence.description.clone(),
        location: occurrence.location.clone(),
        start: When::from_wire(&occurrence.start)?,
        end: occurrence.end.as_ref().and_then(When::from_wire),
        recurrence_id: occurrence.recurrence_id.as_ref().and_then(When::from_wire),
    })
}

fn day_start_ms<Tz: TimeZone>(tz: &Tz, day: NaiveDate) -> i64 {
    let midnight = day.and_time(NaiveTime::MIN);
    tz.from_local_datetime(&midnight).earliest().map_or_else(
        || midnight.and_utc().timestamp_millis(),
        |t| t.timestamp_millis(),
    )
}

/// The occurrences that touch the local days `from..to`, earliest first.
#[must_use]
pub fn occurrences_between<Tz: TimeZone>(
    events: &[WaddleEvent],
    from: NaiveDate,
    to: NaiveDate,
    tz: &Tz,
) -> Vec<Occurrence> {
    let (from_ms, to_ms) = (
        day_start_ms(tz, from) - DAY_MS,
        day_start_ms(tz, to) + DAY_MS,
    );
    let mut found: Vec<Occurrence> = events
        .iter()
        .flat_map(|event| {
            // A record the core cannot walk has no dates to show.
            let walked = expand_community_event(event.clone(), from_ms, to_ms).unwrap_or_default();
            walked
                .iter()
                .filter_map(|occurrence| from_wire_occurrence(event, occurrence))
                .collect::<Vec<_>>()
        })
        .filter(|occurrence| {
            occurrence
                .days(tz)
                .iter()
                .any(|day| *day >= from && *day < to)
        })
        .collect();
    found.sort_by_key(|occurrence| sort_key(occurrence, tz));
    found
}

fn sort_key<Tz: TimeZone>(occurrence: &Occurrence, tz: &Tz) -> (NaiveDateTime, bool, String) {
    (
        occurrence.start.local(tz),
        !occurrence.all_day(),
        occurrence.title.to_lowercase(),
    )
}

/// Events the community has not given a date yet.
#[must_use]
pub fn undated(events: &[WaddleEvent]) -> Vec<&WaddleEvent> {
    let mut found: Vec<&WaddleEvent> = events
        .iter()
        .filter(|event| event.start.is_none())
        .collect();
    found.sort_by_key(|event| event.summary.to_lowercase());
    found
}

/// One day of the agenda.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayGroup {
    pub day: NaiveDate,
    pub items: Vec<Occurrence>,
}

/// The agenda for `days` days from `from`: each day with something on it,
/// earliest first. An occurrence is listed on the day it starts; one that
/// began before `from` and is still going is listed on `from`.
#[must_use]
pub fn agenda<Tz: TimeZone>(
    events: &[WaddleEvent],
    from: NaiveDate,
    days: i64,
    tz: &Tz,
) -> Vec<DayGroup> {
    let to = from + Duration::days(days);
    let mut groups: Vec<DayGroup> = Vec::new();
    for occurrence in occurrences_between(events, from, to, tz) {
        let start = occurrence.start.local_day(tz);
        let day = start.max(from);
        if day >= to {
            continue;
        }
        match groups.last_mut() {
            Some(group) if group.day == day => group.items.push(occurrence),
            _ => groups.push(DayGroup {
                day,
                items: vec![occurrence],
            }),
        }
    }
    groups
}

/// The days of a month with an event on them, as day numbers.
#[must_use]
pub fn marked_days<Tz: TimeZone>(
    events: &[WaddleEvent],
    year: i32,
    month: u32,
    tz: &Tz,
) -> BTreeSet<u32> {
    let Some(first) = NaiveDate::from_ymd_opt(year, month, 1) else {
        return BTreeSet::new();
    };
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    let Some(next) = next else {
        return BTreeSet::new();
    };
    occurrences_between(events, first, next, tz)
        .iter()
        .flat_map(|occurrence| occurrence.days(tz))
        .filter(|day| *day >= first && *day < next)
        .map(|day| day.day())
        .collect()
}

/// What happens on one local day, earliest first.
#[must_use]
pub fn on_day<Tz: TimeZone>(events: &[WaddleEvent], day: NaiveDate, tz: &Tz) -> Vec<Occurrence> {
    occurrences_between(events, day, day + Duration::days(1), tz)
        .into_iter()
        .filter(|occurrence| occurrence.occurs_on(day, tz))
        .collect()
}

/// "Today", "Tomorrow" or the weekday and date (with the year when it is not
/// this one).
#[must_use]
pub fn day_heading(day: NaiveDate, today: NaiveDate) -> String {
    if day == today {
        "Today".to_string()
    } else if day == today + Duration::days(1) {
        "Tomorrow".to_string()
    } else if day.year() == today.year() {
        day.format("%A, %B %-d").to_string()
    } else {
        day.format("%A, %B %-d, %Y").to_string()
    }
}

/// When an occurrence happens, on the person's clock.
#[must_use]
pub fn time_label<Tz: TimeZone>(occurrence: &Occurrence, tz: &Tz) -> String {
    if occurrence.is_undated() {
        return "No date yet".to_string();
    }
    let days = occurrence.days(tz);
    if occurrence.all_day() {
        return match (days.first(), days.last()) {
            (Some(first), Some(last)) if first != last => {
                format!("All day, until {}", last.format("%b %-d"))
            }
            _ => "All day".to_string(),
        };
    }
    let start = occurrence.start.local(tz);
    let Some(end) = occurrence.end.map(|end| end.local(tz)) else {
        return start.format("%H:%M").to_string();
    };
    if end.date() == start.date() {
        format!("{} \u{2013} {}", start.format("%H:%M"), end.format("%H:%M"))
    } else {
        format!(
            "{} \u{2013} {}",
            start.format("%H:%M"),
            end.format("%b %-d, %H:%M")
        )
    }
}

/// The date and time line of an event's detail.
#[must_use]
pub fn detail_label<Tz: TimeZone>(occurrence: &Occurrence, tz: &Tz) -> String {
    if occurrence.is_undated() {
        return "No date yet".to_string();
    }
    let start = occurrence.start.local(tz);
    let day = start.format("%A, %B %-d, %Y");
    match (occurrence.all_day(), occurrence.days(tz).as_slice()) {
        (true, [first, .., last]) => format!(
            "{} to {}, all day",
            first.format("%A, %B %-d"),
            last.format("%A, %B %-d, %Y")
        ),
        (true, _) => format!("{day}, all day"),
        (false, _) => format!("{day}, {}", time_label(occurrence, tz)),
    }
}

/// How an event repeats, in words.
#[must_use]
pub fn recurrence_label(rule: &WaddleRecurrence) -> String {
    let every = |unit: &str, plural: &str| {
        if rule.interval <= 1 {
            format!("Every {unit}")
        } else {
            format!("Every {} {plural}", rule.interval)
        }
    };
    let mut text = match rule.frequency {
        WaddleRecurrenceFrequency::Daily => every("day", "days"),
        WaddleRecurrenceFrequency::Weekly => every("week", "weeks"),
        WaddleRecurrenceFrequency::Monthly => every("month", "months"),
        WaddleRecurrenceFrequency::Yearly => every("year", "years"),
    };
    if !rule.weekdays.is_empty() {
        let names: Vec<&str> = rule.weekdays.iter().map(|day| weekday_name(*day)).collect();
        text.push_str(&format!(" on {}", names.join(", ")));
    }
    if !rule.month_days.is_empty() {
        let days: Vec<String> = rule.month_days.iter().map(ToString::to_string).collect();
        text.push_str(&format!(" on day {}", days.join(", ")));
    }
    match rule.end {
        WaddleRecurrenceEnd::Never => {}
        WaddleRecurrenceEnd::Count { count } => text.push_str(&format!(", {count} times")),
        WaddleRecurrenceEnd::Until { timestamp_ms } => {
            if let Some(until) = DateTime::from_timestamp_millis(timestamp_ms) {
                text.push_str(&format!(", until {}", until.format("%b %-d, %Y UTC")));
            }
        }
    }
    text
}

#[must_use]
pub fn weekday_name(day: WaddleWeekday) -> &'static str {
    match day {
        WaddleWeekday::Sunday => "Sun",
        WaddleWeekday::Monday => "Mon",
        WaddleWeekday::Tuesday => "Tue",
        WaddleWeekday::Wednesday => "Wed",
        WaddleWeekday::Thursday => "Thu",
        WaddleWeekday::Friday => "Fri",
        WaddleWeekday::Saturday => "Sat",
    }
}

#[must_use]
pub fn weekday_of(day: NaiveDate) -> WaddleWeekday {
    match day.weekday() {
        ChronoWeekday::Sun => WaddleWeekday::Sunday,
        ChronoWeekday::Mon => WaddleWeekday::Monday,
        ChronoWeekday::Tue => WaddleWeekday::Tuesday,
        ChronoWeekday::Wed => WaddleWeekday::Wednesday,
        ChronoWeekday::Thu => WaddleWeekday::Thursday,
        ChronoWeekday::Fri => WaddleWeekday::Friday,
        ChronoWeekday::Sat => WaddleWeekday::Saturday,
    }
}

/// An answer to an invitation, as the buttons offer it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rsvp {
    Going,
    Maybe,
    NotGoing,
}

impl Rsvp {
    pub const ALL: [Self; 3] = [Self::Going, Self::Maybe, Self::NotGoing];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Going => "Going",
            Self::Maybe => "Maybe",
            Self::NotGoing => "Not Going",
        }
    }

    #[must_use]
    pub fn to_wire(self) -> WaddleEventParticipation {
        match self {
            Self::Going => WaddleEventParticipation::Accepted,
            Self::Maybe => WaddleEventParticipation::Tentative,
            Self::NotGoing => WaddleEventParticipation::Declined,
        }
    }

    /// `None` for an invitation nobody has answered.
    #[must_use]
    pub fn from_wire(status: WaddleEventParticipation) -> Option<Self> {
        match status {
            WaddleEventParticipation::Accepted => Some(Self::Going),
            WaddleEventParticipation::Tentative => Some(Self::Maybe),
            WaddleEventParticipation::Declined => Some(Self::NotGoing),
            WaddleEventParticipation::NeedsAction => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub going: u32,
    pub maybe: u32,
    pub not_going: u32,
}

impl Counts {
    #[must_use]
    pub fn of(attendees: &[WaddleEventAttendee]) -> Self {
        let mut counts = Self::default();
        for attendee in attendees {
            match Rsvp::from_wire(attendee.status) {
                Some(Rsvp::Going) => counts.going += 1,
                Some(Rsvp::Maybe) => counts.maybe += 1,
                Some(Rsvp::NotGoing) => counts.not_going += 1,
                None => {}
            }
        }
        counts
    }

    #[must_use]
    pub fn summary(self) -> String {
        if self == Self::default() {
            return "No replies yet".to_string();
        }
        format!(
            "{} going, {} maybe, {} not going",
            self.going, self.maybe, self.not_going
        )
    }
}

/// The bare JID an attendee URI names (`xmpp:` is dropped).
#[must_use]
pub fn attendee_jid(uri: &str) -> &str {
    uri.strip_prefix("xmpp:").unwrap_or(uri)
}

/// The account's own answer, if it has given one.
#[must_use]
pub fn my_reply(attendees: &[WaddleEventAttendee], me: &str) -> Option<Rsvp> {
    attendees
        .iter()
        .find(|attendee| attendee_jid(&attendee.uri).eq_ignore_ascii_case(me))
        .and_then(|attendee| Rsvp::from_wire(attendee.status))
}

/// `event` with the account's answer recorded, as the server will fold it in.
pub fn apply_reply(event: &mut WaddleEvent, me: &str, reply: Rsvp) {
    let own = event
        .attendees
        .iter_mut()
        .find(|attendee| attendee_jid(&attendee.uri).eq_ignore_ascii_case(me));
    match own {
        Some(attendee) => attendee.status = reply.to_wire(),
        None => event.attendees.push(WaddleEventAttendee {
            uri: format!("xmpp:{me}"),
            status: reply.to_wire(),
        }),
    }
}

/// Replies, in the order the buttons are, the account's own first.
#[must_use]
pub fn replies(attendees: &[WaddleEventAttendee], me: &str) -> Vec<(String, Rsvp)> {
    let mut found: Vec<(String, Rsvp)> = attendees
        .iter()
        .filter_map(|attendee| {
            Some((
                attendee_jid(&attendee.uri).to_string(),
                Rsvp::from_wire(attendee.status)?,
            ))
        })
        .collect();
    found.sort_by_key(|(jid, reply)| {
        (
            !jid.eq_ignore_ascii_case(me),
            Rsvp::ALL.iter().position(|r| r == reply),
            jid.to_lowercase(),
        )
    });
    found
}

/// The community service hosting the calendar: `community.<account domain>`.
#[must_use]
pub fn community_service(account: &str) -> Option<String> {
    let domain = account.split_once('@')?.1;
    let domain = domain.split('/').next().unwrap_or(domain);
    (!domain.is_empty()).then(|| format!("community.{domain}"))
}

/// How a form's repeat is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    Never,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Repeat {
    pub const ALL: [Self; 5] = [
        Self::Never,
        Self::Daily,
        Self::Weekly,
        Self::Monthly,
        Self::Yearly,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Never => "Does not repeat",
            Self::Daily => "Daily",
            Self::Weekly => "Weekly",
            Self::Monthly => "Monthly",
            Self::Yearly => "Yearly",
        }
    }

    fn frequency(self) -> Option<WaddleRecurrenceFrequency> {
        match self {
            Self::Never => None,
            Self::Daily => Some(WaddleRecurrenceFrequency::Daily),
            Self::Weekly => Some(WaddleRecurrenceFrequency::Weekly),
            Self::Monthly => Some(WaddleRecurrenceFrequency::Monthly),
            Self::Yearly => Some(WaddleRecurrenceFrequency::Yearly),
        }
    }

    fn of(frequency: WaddleRecurrenceFrequency) -> Self {
        match frequency {
            WaddleRecurrenceFrequency::Daily => Self::Daily,
            WaddleRecurrenceFrequency::Weekly => Self::Weekly,
            WaddleRecurrenceFrequency::Monthly => Self::Monthly,
            WaddleRecurrenceFrequency::Yearly => Self::Yearly,
        }
    }
}

/// When a repeating event stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Never,
    /// After this many dates.
    After(u32),
    /// With the last date starting on or before this local day.
    On(NaiveDate),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DraftError {
    #[error("Give the event a title.")]
    NoTitle,
    #[error("The event must end after it starts.")]
    EndBeforeStart,
    #[error("That local time does not exist (the clocks change then). Pick another time.")]
    NoSuchTime,
    #[error("A repeating all-day event can end after a number of dates, not on a date.")]
    AllDayEndsOnDate,
    #[error("The last date of the series is before its first.")]
    UntilBeforeStart,
    #[error("A series needs at least one date and a repeat of at least 1.")]
    BadCount,
}

/// What the event form holds, in local terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    /// Empty to create.
    pub event_id: String,
    pub title: String,
    pub details: String,
    pub location: String,
    pub room: Option<String>,
    pub all_day: bool,
    pub start_date: NaiveDate,
    pub start_time: NaiveTime,
    /// Timed events only: whether an end is set.
    pub has_end: bool,
    /// For an all-day event, the last day (inclusive).
    pub end_date: NaiveDate,
    pub end_time: NaiveTime,
    pub repeat: Repeat,
    pub interval: u32,
    /// Weekly only; empty means the weekday of the start.
    pub weekdays: Vec<WaddleWeekday>,
    pub ending: Ending,
    /// The rule as stored, kept as it is until the repeat is edited here.
    pub stored_rule: Option<WaddleRecurrence>,
    pub rule_edited: bool,
}

impl Draft {
    /// A new event on `day` at `hour`, an hour long.
    #[must_use]
    pub fn new(day: NaiveDate, hour: u32) -> Self {
        let start_time = NaiveTime::from_hms_opt(hour.min(22), 0, 0).unwrap_or(NaiveTime::MIN);
        Self {
            event_id: String::new(),
            title: String::new(),
            details: String::new(),
            location: String::new(),
            room: None,
            all_day: false,
            start_date: day,
            start_time,
            has_end: true,
            end_date: day,
            end_time: start_time + Duration::hours(1),
            repeat: Repeat::Never,
            interval: 1,
            weekdays: Vec::new(),
            ending: Ending::Never,
            stored_rule: None,
            rule_edited: false,
        }
    }

    /// The form for editing a stored event, with the dates on the local clock.
    #[must_use]
    pub fn from_event<Tz: TimeZone>(event: &WaddleEvent, tz: &Tz) -> Self {
        let start = event.start.as_ref().and_then(When::from_wire);
        let end = event.end.as_ref().and_then(When::from_wire);
        let first = start.map_or_else(NaiveDateTime::default, |start| start.local(tz));
        let all_day = start.is_some_and(When::all_day);
        let mut draft = Self::new(first.date(), 9);
        draft.event_id = event.id.clone();
        draft.title = event.summary.clone();
        draft.details = event.description.clone().unwrap_or_default();
        draft.location = event.location.clone().unwrap_or_default();
        draft.room = event.workspace_room.clone();
        draft.all_day = all_day;
        if start.is_some() && !all_day {
            draft.start_time = first.time();
        }
        match end.map(|end| end.local(tz)) {
            Some(end) if all_day => {
                draft.end_date = (end.date() - Duration::days(1)).max(first.date());
            }
            Some(end) => {
                draft.has_end = true;
                draft.end_date = end.date();
                draft.end_time = end.time();
            }
            None => {
                draft.has_end = false;
                draft.end_date = first.date();
                draft.end_time = draft.start_time + Duration::hours(1);
            }
        }
        if let Some(rule) = event.recurrence.as_ref() {
            draft.repeat = Repeat::of(rule.frequency);
            draft.interval = rule.interval.max(1);
            draft.weekdays.clone_from(&rule.weekdays);
            draft.ending = match rule.end {
                WaddleRecurrenceEnd::Never => Ending::Never,
                WaddleRecurrenceEnd::Count { count } => Ending::After(count),
                WaddleRecurrenceEnd::Until { timestamp_ms } => {
                    Ending::On(When::At(timestamp_ms).local_day(tz))
                }
            };
            draft.stored_rule = Some(rule.clone());
        }
        draft
    }

    /// Move the start, taking the end along so the event keeps its length.
    pub fn move_start(&mut self, date: NaiveDate, time: NaiveTime) {
        if self.all_day {
            let days = (self.end_date - self.start_date).max(Duration::zero());
            self.start_date = date;
            self.end_date = date + days;
            return;
        }
        let old_start = self.start_date.and_time(self.start_time);
        let old_end = self.end_date.and_time(self.end_time);
        let span = if old_end > old_start {
            old_end - old_start
        } else {
            Duration::hours(1)
        };
        self.start_date = date;
        self.start_time = time;
        let end = date.and_time(time) + span;
        self.end_date = end.date();
        self.end_time = end.time();
    }

    /// The stored rule says more than the form can: month days, weekdays on
    /// a rule that is not weekly, or an all-day series ending on an instant.
    #[must_use]
    pub fn rule_is_beyond_form(&self) -> bool {
        self.stored_rule.as_ref().is_some_and(|rule| {
            !rule.month_days.is_empty()
                || (!rule.weekdays.is_empty()
                    && rule.frequency != WaddleRecurrenceFrequency::Weekly)
                || (self.all_day && matches!(rule.end, WaddleRecurrenceEnd::Until { .. }))
        })
    }

    /// The record to publish. Fails with what the person must fix.
    pub fn to_event<Tz: TimeZone>(&self, tz: &Tz) -> Result<WaddleEvent, DraftError> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err(DraftError::NoTitle);
        }
        let instant = |day: NaiveDate, time: NaiveTime| {
            tz.from_local_datetime(&day.and_time(time))
                .earliest()
                .map(|at| When::At(at.timestamp_millis()))
                .ok_or(DraftError::NoSuchTime)
        };
        let (start, end) = if self.all_day {
            if self.end_date < self.start_date {
                return Err(DraftError::EndBeforeStart);
            }
            let end = (self.end_date > self.start_date)
                .then(|| When::Day(self.end_date + Duration::days(1)));
            (When::Day(self.start_date), end)
        } else {
            let start = instant(self.start_date, self.start_time)?;
            let end = self
                .has_end
                .then(|| instant(self.end_date, self.end_time))
                .transpose()?;
            if end.is_some_and(|end| end <= start) {
                return Err(DraftError::EndBeforeStart);
            }
            (start, end)
        };
        let recurrence = self.recurrence(start, tz)?;
        let text = |value: &str| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        Ok(WaddleEvent {
            id: self.event_id.clone(),
            uid: String::new(),
            summary: title.to_string(),
            description: text(&self.details),
            location: text(&self.location),
            organizer: None,
            workspace_room: self.room.clone(),
            start: Some(start.to_wire()),
            end: end.map(When::to_wire),
            // Replies, cancelled and changed dates stay as the server has them.
            attendees: Vec::new(),
            recurrence,
            excluded: Vec::new(),
            overrides: Vec::new(),
        })
    }

    fn recurrence<Tz: TimeZone>(
        &self,
        start: When,
        tz: &Tz,
    ) -> Result<Option<WaddleRecurrence>, DraftError> {
        if !self.rule_edited {
            if let Some(rule) = self.stored_rule.clone() {
                return Ok(Some(rule));
            }
        }
        let Some(frequency) = self.repeat.frequency() else {
            return Ok(None);
        };
        if self.interval == 0 {
            return Err(DraftError::BadCount);
        }
        let end = match self.ending {
            Ending::Never => WaddleRecurrenceEnd::Never,
            Ending::After(0) => return Err(DraftError::BadCount),
            Ending::After(count) => WaddleRecurrenceEnd::Count { count },
            Ending::On(_) if self.all_day => return Err(DraftError::AllDayEndsOnDate),
            Ending::On(day) => {
                // The last date may start any time on the chosen day.
                let last = NaiveTime::from_hms_opt(23, 59, 59).unwrap_or(NaiveTime::MIN);
                let until = tz
                    .from_local_datetime(&day.and_time(last))
                    .latest()
                    .map(|at| at.timestamp_millis())
                    .ok_or(DraftError::NoSuchTime)?;
                if When::At(until) < start {
                    return Err(DraftError::UntilBeforeStart);
                }
                WaddleRecurrenceEnd::Until {
                    timestamp_ms: until,
                }
            }
        };
        let weekly = frequency == WaddleRecurrenceFrequency::Weekly;
        Ok(Some(WaddleRecurrence {
            frequency,
            interval: self.interval,
            weekdays: if weekly {
                self.weekdays.clone()
            } else {
                Vec::new()
            },
            month_days: Vec::new(),
            end,
        }))
    }
}

/// One line of an event room's planning space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanningRow {
    pub kind: &'static str,
    pub title: String,
    pub note: Option<String>,
    pub done: bool,
}

/// The room's planning items worth listing: votes and attendance marks are
/// answers to other items, not items to read. Tasks first, then the rest.
#[must_use]
pub fn planning_rows(items: &[WaddlePlanningItem]) -> Vec<PlanningRow> {
    let mut rows: Vec<(u8, PlanningRow)> = items
        .iter()
        .filter(|item| {
            !matches!(
                item.kind,
                WaddlePlanningKind::Vote | WaddlePlanningKind::Attendance
            ) && !item.title.trim().is_empty()
        })
        .map(|item| {
            let (order, kind) = match item.kind {
                WaddlePlanningKind::Task => (0, "Task"),
                WaddlePlanningKind::Decision => (1, "Decision"),
                WaddlePlanningKind::Poll => (2, "Poll"),
                WaddlePlanningKind::Itinerary => (3, "Itinerary"),
                WaddlePlanningKind::Travel => (4, "Travel"),
                WaddlePlanningKind::Expense => (5, "Expense"),
                WaddlePlanningKind::Event => (6, "Event"),
                WaddlePlanningKind::Link => (7, "Link"),
                WaddlePlanningKind::Vote | WaddlePlanningKind::Attendance => (8, ""),
            };
            let note = match item.status {
                Some(WaddlePlanningStatus::Cancelled) => Some("Cancelled".to_string()),
                Some(WaddlePlanningStatus::Confirmed) => Some("Confirmed".to_string()),
                Some(WaddlePlanningStatus::Proposed) => Some("Proposed".to_string()),
                None => item.location.clone().filter(|place| !place.is_empty()),
            };
            (
                order,
                PlanningRow {
                    kind,
                    title: item.title.trim().to_string(),
                    note,
                    done: item.completed,
                },
            )
        })
        .collect();
    rows.sort_by(|(a, left), (b, right)| (a, &left.title).cmp(&(b, &right.title)));
    rows.into_iter().map(|(_, row)| row).collect()
}

/// A failure to show the person, in words.
fn failure(what: &str, error: &WaddleError) -> String {
    match error {
        WaddleError::Stanza { condition, .. }
            if condition == "forbidden" || condition == "not-allowed" =>
        {
            format!("You are not allowed to {what}.")
        }
        WaddleError::NotConnected | WaddleError::Timeout | WaddleError::Transport => {
            format!("Could not {what}: the connection failed. Try again.")
        }
        other => {
            tracing::debug!(?other, "calendar request failed");
            format!("Could not {what}.")
        }
    }
}

impl Session {
    /// The community service hosting the calendar.
    fn community(&self) -> Result<String, String> {
        community_service(&self.me.bare.to_string())
            .ok_or_else(|| "This account has no community calendar.".to_string())
    }

    pub async fn events(&self) -> Result<Vec<WaddleEvent>, String> {
        let (client, service) = (Arc::clone(&self.client), self.community()?);
        runtime::spawn(async move { client.fetch_community_events(service).await })
            .await
            .map_err(|_| "The events could not be loaded.".to_string())?
            .map_err(|error| failure("load the events", &error))
    }

    /// Whether this account may change the calendar (the community owner;
    /// the server re-checks every write).
    pub async fn may_edit_events(&self) -> bool {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.is_community_owner().await })
            .await
            .unwrap_or(false)
    }

    pub async fn publish_event(&self, event: WaddleEvent) -> Result<WaddleEvent, String> {
        let (client, service) = (Arc::clone(&self.client), self.community()?);
        runtime::spawn(async move { client.publish_community_event(service, event).await })
            .await
            .map_err(|_| "The event could not be saved.".to_string())?
            .map_err(|error| failure("save the event", &error))
    }

    pub async fn retract_event(&self, item_id: String) -> Result<(), String> {
        let (client, service) = (Arc::clone(&self.client), self.community()?);
        runtime::spawn(async move { client.retract_community_event(service, item_id).await })
            .await
            .map_err(|_| "The event could not be deleted.".to_string())?
            .map_err(|error| failure("delete the event", &error))
    }

    pub async fn cancel_occurrence(
        &self,
        item_id: String,
        occurrence: When,
    ) -> Result<WaddleEvent, String> {
        let (client, service) = (Arc::clone(&self.client), self.community()?);
        runtime::spawn(async move {
            client
                .cancel_community_event_occurrence(service, item_id, occurrence.to_wire())
                .await
        })
        .await
        .map_err(|_| "The date could not be cancelled.".to_string())?
        .map_err(|error| failure("cancel that date", &error))
    }

    pub async fn rsvp(&self, uid: String, reply: Rsvp) -> Result<(), String> {
        let (client, service) = (Arc::clone(&self.client), self.community()?);
        let me = self.me.bare.to_string();
        runtime::spawn(async move {
            client
                .rsvp_community_event(service, uid, me, reply.to_wire())
                .await
        })
        .await
        .map_err(|_| "Your answer could not be sent.".to_string())?
        .map_err(|error| failure("send your answer", &error))
    }

    /// A room for an event; returns its JID.
    pub async fn create_event_room(&self, name: String) -> Result<String, String> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.create_event_room(name, false).await })
            .await
            .map_err(|_| "The room could not be created.".to_string())?
            .map_err(|error| failure("create the room", &error))
    }

    pub async fn planning_items(&self, room: String) -> Result<Vec<WaddlePlanningItem>, String> {
        let client = Arc::clone(&self.client);
        runtime::spawn(async move { client.event_planning_items(room).await })
            .await
            .map_err(|_| "The planning items could not be loaded.".to_string())?
            .map_err(|error| failure("load the planning items", &error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).expect("offset")
    }

    fn plus(hours: i32) -> FixedOffset {
        FixedOffset::east_opt(hours * 3600).expect("offset")
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("date")
    }

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        day(y, m, d)
            .and_hms_opt(h, min, 0)
            .expect("time")
            .and_utc()
            .timestamp_millis()
    }

    fn event(id: &str, summary: &str) -> WaddleEvent {
        WaddleEvent {
            id: id.to_string(),
            uid: format!("uid-{id}"),
            summary: summary.to_string(),
            description: None,
            location: None,
            organizer: None,
            workspace_room: None,
            start: None,
            end: None,
            attendees: Vec::new(),
            recurrence: None,
            excluded: Vec::new(),
            overrides: Vec::new(),
        }
    }

    fn timed(id: &str, summary: &str, start: i64, end: Option<i64>) -> WaddleEvent {
        WaddleEvent {
            start: Some(WaddleCalendarDate::DateTime {
                timestamp_ms: start,
            }),
            end: end.map(|timestamp_ms| WaddleCalendarDate::DateTime { timestamp_ms }),
            ..event(id, summary)
        }
    }

    fn all_day(id: &str, summary: &str, first: NaiveDate, end: Option<NaiveDate>) -> WaddleEvent {
        WaddleEvent {
            start: Some(WaddleCalendarDate::Date {
                date: first.to_string(),
            }),
            end: end.map(|end| WaddleCalendarDate::Date {
                date: end.to_string(),
            }),
            ..event(id, summary)
        }
    }

    fn weekly(count: u32) -> WaddleRecurrence {
        WaddleRecurrence {
            frequency: WaddleRecurrenceFrequency::Weekly,
            interval: 1,
            weekdays: Vec::new(),
            month_days: Vec::new(),
            end: WaddleRecurrenceEnd::Count { count },
        }
    }

    fn titles(groups: &[DayGroup]) -> Vec<(String, Vec<String>)> {
        groups
            .iter()
            .map(|group| {
                (
                    group.day.to_string(),
                    group.items.iter().map(|item| item.title.clone()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn dates_round_trip_through_the_wire_shape() {
        let all = When::Day(day(2027, 2, 5));
        assert_eq!(When::from_wire(&all.to_wire()), Some(all));
        let instant = When::At(at(2027, 2, 5, 19, 0));
        assert_eq!(When::from_wire(&instant.to_wire()), Some(instant));
        assert_eq!(
            When::from_wire(&WaddleCalendarDate::Date {
                date: "garbage".to_string()
            }),
            None
        );
    }

    #[test]
    fn a_timed_occurrence_lands_on_the_local_day() {
        // 23:30 UTC is already the next day at +2.
        let late = When::At(at(2027, 2, 5, 23, 30));
        assert_eq!(late.local_day(&utc()), day(2027, 2, 5));
        assert_eq!(late.local_day(&plus(2)), day(2027, 2, 6));
        // An all-day date does not move with the zone.
        assert_eq!(
            When::Day(day(2027, 2, 5)).local_day(&plus(-9)),
            day(2027, 2, 5)
        );
    }

    #[test]
    fn the_agenda_groups_by_local_day_with_all_day_first() {
        let events = [
            timed("a", "Standup", at(2027, 2, 5, 9, 0), None),
            all_day("b", "Holiday", day(2027, 2, 5), None),
            timed("c", "Dinner", at(2027, 2, 6, 18, 0), None),
            timed("d", "Past", at(2027, 1, 1, 9, 0), None),
        ];
        let groups = agenda(&events, day(2027, 2, 5), 7, &utc());
        assert_eq!(
            titles(&groups),
            [
                (
                    "2027-02-05".to_string(),
                    vec!["Holiday".to_string(), "Standup".to_string()]
                ),
                ("2027-02-06".to_string(), vec!["Dinner".to_string()]),
            ]
        );
    }

    #[test]
    fn the_agenda_follows_the_clock_of_the_viewer() {
        let events = [timed("a", "Late call", at(2027, 2, 5, 23, 30), None)];
        let groups = agenda(&events, day(2027, 2, 5), 7, &plus(2));
        assert_eq!(groups[0].day, day(2027, 2, 6));
    }

    #[test]
    fn a_repeating_event_expands_into_its_dates() {
        let mut series = timed(
            "w",
            "Weekly",
            at(2027, 2, 5, 19, 0),
            Some(at(2027, 2, 5, 20, 0)),
        );
        series.recurrence = Some(weekly(3));
        let groups = agenda(&[series], day(2027, 2, 1), 28, &utc());
        let days: Vec<String> = groups.iter().map(|g| g.day.to_string()).collect();
        assert_eq!(days, ["2027-02-05", "2027-02-12", "2027-02-19"]);
        assert!(groups[0].items[0].repeating());
        assert_eq!(
            groups[1].items[0].recurrence_id,
            Some(When::At(at(2027, 2, 12, 19, 0)))
        );
    }

    #[test]
    fn a_cancelled_date_is_left_out() {
        let mut series = timed("w", "Weekly", at(2027, 2, 5, 19, 0), None);
        series.recurrence = Some(weekly(3));
        series.excluded = vec![WaddleCalendarDate::DateTime {
            timestamp_ms: at(2027, 2, 12, 19, 0),
        }];
        let groups = agenda(&[series], day(2027, 2, 1), 28, &utc());
        let days: Vec<String> = groups.iter().map(|g| g.day.to_string()).collect();
        assert_eq!(days, ["2027-02-05", "2027-02-19"]);
    }

    #[test]
    fn an_event_still_going_is_listed_on_the_first_day_of_the_window() {
        let events = [
            all_day("r", "Retreat", day(2027, 2, 3), Some(day(2027, 2, 7))),
            all_day("o", "Over", day(2027, 2, 1), Some(day(2027, 2, 3))),
        ];
        let groups = agenda(&events, day(2027, 2, 5), 7, &utc());
        assert_eq!(
            titles(&groups),
            [("2027-02-05".to_string(), vec!["Retreat".to_string()])]
        );
    }

    #[test]
    fn an_all_day_event_covers_the_days_before_its_exclusive_end() {
        let events = [all_day(
            "r",
            "Retreat",
            day(2027, 2, 15),
            Some(day(2027, 2, 18)),
        )];
        let marked = marked_days(&events, 2027, 2, &plus(-5));
        assert_eq!(marked.into_iter().collect::<Vec<_>>(), [15, 16, 17]);
        assert_eq!(on_day(&events, day(2027, 2, 17), &utc()).len(), 1);
        assert!(on_day(&events, day(2027, 2, 18), &utc()).is_empty());
    }

    #[test]
    fn month_marks_stay_inside_the_month() {
        let events = [
            timed(
                "a",
                "Late",
                at(2027, 2, 28, 22, 0),
                Some(at(2027, 3, 1, 2, 0)),
            ),
            timed("b", "Early", at(2027, 3, 1, 9, 0), None),
        ];
        let february = marked_days(&events, 2027, 2, &utc());
        assert_eq!(february.into_iter().collect::<Vec<_>>(), [28]);
        let march = marked_days(&events, 2027, 3, &utc());
        assert_eq!(march.into_iter().collect::<Vec<_>>(), [1]);
        assert!(marked_days(&events, 2027, 13, &utc()).is_empty());
    }

    #[test]
    fn events_without_a_date_are_kept_apart() {
        let events = [event("u", "Someday"), timed("a", "Dated", 0, None)];
        assert!(agenda(&events, day(1970, 1, 1), 3, &utc())
            .iter()
            .all(|group| group.items.iter().all(|item| item.title != "Someday")));
        assert_eq!(undated(&events).len(), 1);
    }

    #[test]
    fn day_headings_name_today_and_tomorrow() {
        let today = day(2027, 2, 5);
        assert_eq!(day_heading(today, today), "Today");
        assert_eq!(day_heading(day(2027, 2, 6), today), "Tomorrow");
        assert_eq!(day_heading(day(2027, 3, 1), today), "Monday, March 1");
        assert_eq!(
            day_heading(day(2028, 1, 3), today),
            "Monday, January 3, 2028"
        );
    }

    #[test]
    fn time_labels_read_on_the_local_clock() {
        let events = [
            timed(
                "a",
                "A",
                at(2027, 2, 5, 19, 0),
                Some(at(2027, 2, 5, 20, 30)),
            ),
            timed("b", "B", at(2027, 2, 5, 22, 0), Some(at(2027, 2, 6, 1, 0))),
            all_day("c", "C", day(2027, 2, 5), None),
            all_day("d", "D", day(2027, 2, 5), Some(day(2027, 2, 8))),
            timed("e", "E", at(2027, 2, 5, 7, 5), None),
        ];
        let found = occurrences_between(&events, day(2027, 2, 5), day(2027, 2, 6), &utc());
        let label = |title: &str| {
            let item = found.iter().find(|item| item.title == title).expect(title);
            time_label(item, &utc())
        };
        assert_eq!(label("A"), "19:00 \u{2013} 20:30");
        assert_eq!(label("B"), "22:00 \u{2013} Feb 6, 01:00");
        assert_eq!(label("C"), "All day");
        assert_eq!(label("D"), "All day, until Feb 7");
        assert_eq!(label("E"), "07:05");
        let a = found.iter().find(|item| item.title == "A").expect("A");
        assert_eq!(time_label(a, &plus(1)), "20:00 \u{2013} 21:30");
        assert_eq!(
            detail_label(a, &utc()),
            "Friday, February 5, 2027, 19:00 \u{2013} 20:30"
        );
    }

    #[test]
    fn recurrences_read_in_words() {
        let mut rule = weekly(4);
        assert_eq!(recurrence_label(&rule), "Every week, 4 times");
        rule.interval = 2;
        rule.weekdays = vec![WaddleWeekday::Monday, WaddleWeekday::Friday];
        rule.end = WaddleRecurrenceEnd::Never;
        assert_eq!(recurrence_label(&rule), "Every 2 weeks on Mon, Fri");
        rule.frequency = WaddleRecurrenceFrequency::Monthly;
        rule.weekdays.clear();
        rule.month_days = vec![1, -1];
        assert_eq!(recurrence_label(&rule), "Every 2 months on day 1, -1");
    }

    fn attendee(jid: &str, status: WaddleEventParticipation) -> WaddleEventAttendee {
        WaddleEventAttendee {
            uri: format!("xmpp:{jid}"),
            status,
        }
    }

    #[test]
    fn replies_are_counted_and_the_own_answer_found() {
        let attendees = [
            attendee("ann@x.test", WaddleEventParticipation::Accepted),
            attendee("bob@x.test", WaddleEventParticipation::Tentative),
            attendee("cy@x.test", WaddleEventParticipation::Declined),
            attendee("di@x.test", WaddleEventParticipation::Accepted),
            attendee("ed@x.test", WaddleEventParticipation::NeedsAction),
        ];
        let counts = Counts::of(&attendees);
        assert_eq!(
            counts,
            Counts {
                going: 2,
                maybe: 1,
                not_going: 1
            }
        );
        assert_eq!(counts.summary(), "2 going, 1 maybe, 1 not going");
        assert_eq!(Counts::default().summary(), "No replies yet");
        assert_eq!(my_reply(&attendees, "bob@x.test"), Some(Rsvp::Maybe));
        assert_eq!(my_reply(&attendees, "ed@x.test"), None);
        assert_eq!(my_reply(&attendees, "zed@x.test"), None);
        let listed = replies(&attendees, "cy@x.test");
        assert_eq!(listed[0], ("cy@x.test".to_string(), Rsvp::NotGoing));
        assert_eq!(listed.len(), 4);
        assert_eq!(listed[1].1, Rsvp::Going);
    }

    #[test]
    fn answers_map_to_the_core_participation() {
        for reply in Rsvp::ALL {
            assert_eq!(Rsvp::from_wire(reply.to_wire()), Some(reply));
        }
        assert!(matches!(
            Rsvp::Going.to_wire(),
            WaddleEventParticipation::Accepted
        ));
        assert!(matches!(
            Rsvp::Maybe.to_wire(),
            WaddleEventParticipation::Tentative
        ));
        assert!(matches!(
            Rsvp::NotGoing.to_wire(),
            WaddleEventParticipation::Declined
        ));
    }

    #[test]
    fn the_calendar_lives_on_the_community_service_of_the_account_domain() {
        assert_eq!(
            community_service("ann@waddle.social"),
            Some("community.waddle.social".to_string())
        );
        assert_eq!(
            community_service("ann@waddle.social/phone"),
            Some("community.waddle.social".to_string())
        );
        assert_eq!(community_service("waddle.social"), None);
    }

    fn filled() -> Draft {
        let mut draft = Draft::new(day(2027, 2, 5), 19);
        draft.title = "  Game night ".to_string();
        draft
    }

    #[test]
    fn a_new_event_is_an_hour_long_at_the_chosen_local_time() {
        let event = filled().to_event(&plus(2)).expect("valid");
        assert_eq!(event.summary, "Game night");
        assert_eq!(event.id, "");
        assert!(matches!(
            event.start,
            Some(WaddleCalendarDate::DateTime { timestamp_ms }) if timestamp_ms == at(2027, 2, 5, 17, 0)
        ));
        assert!(matches!(
            event.end,
            Some(WaddleCalendarDate::DateTime { timestamp_ms }) if timestamp_ms == at(2027, 2, 5, 18, 0)
        ));
        assert!(event.recurrence.is_none());
        assert_eq!(event.description, None);
    }

    #[test]
    fn the_form_refuses_what_the_core_would() {
        let mut draft = filled();
        draft.title = "   ".to_string();
        assert_eq!(draft.to_event(&utc()).err(), Some(DraftError::NoTitle));

        let mut draft = filled();
        draft.end_time = draft.start_time;
        assert_eq!(
            draft.to_event(&utc()).err(),
            Some(DraftError::EndBeforeStart)
        );
        draft.has_end = false;
        assert!(draft.to_event(&utc()).expect("no end").end.is_none());

        let mut draft = filled();
        draft.all_day = true;
        draft.end_date = day(2027, 2, 4);
        assert_eq!(
            draft.to_event(&utc()).err(),
            Some(DraftError::EndBeforeStart)
        );
    }

    #[test]
    fn an_all_day_event_stores_the_day_after_its_last_day() {
        let mut draft = filled();
        draft.all_day = true;
        assert!(draft.to_event(&utc()).expect("one day").end.is_none());
        draft.end_date = day(2027, 2, 7);
        let event = draft.to_event(&plus(-8)).expect("three days");
        assert!(
            matches!(&event.start, Some(WaddleCalendarDate::Date { date }) if date == "2027-02-05")
        );
        assert!(
            matches!(&event.end, Some(WaddleCalendarDate::Date { date }) if date == "2027-02-08")
        );
    }

    #[test]
    fn a_repeat_becomes_a_core_rule() {
        let mut draft = filled();
        draft.repeat = Repeat::Weekly;
        draft.interval = 2;
        draft.weekdays = vec![WaddleWeekday::Friday];
        draft.ending = Ending::After(6);
        draft.rule_edited = true;
        let rule = draft
            .to_event(&utc())
            .expect("valid")
            .recurrence
            .expect("rule");
        assert_eq!(rule.frequency, WaddleRecurrenceFrequency::Weekly);
        assert_eq!(rule.interval, 2);
        assert_eq!(rule.weekdays, [WaddleWeekday::Friday]);
        assert_eq!(rule.end, WaddleRecurrenceEnd::Count { count: 6 });

        // Weekdays belong to weekly rules only.
        draft.repeat = Repeat::Daily;
        let rule = draft
            .to_event(&utc())
            .expect("valid")
            .recurrence
            .expect("rule");
        assert!(rule.weekdays.is_empty());
    }

    #[test]
    fn an_end_date_runs_to_the_end_of_that_local_day() {
        let mut draft = filled();
        draft.repeat = Repeat::Daily;
        draft.ending = Ending::On(day(2027, 2, 7));
        draft.rule_edited = true;
        let rule = draft
            .to_event(&plus(2))
            .expect("valid")
            .recurrence
            .expect("rule");
        // 23:59:59 at +2 is 21:59:59 UTC.
        assert_eq!(
            rule.end,
            WaddleRecurrenceEnd::Until {
                timestamp_ms: at(2027, 2, 7, 21, 59) + 59_000
            }
        );

        draft.ending = Ending::On(day(2027, 2, 4));
        assert_eq!(
            draft.to_event(&utc()).err(),
            Some(DraftError::UntilBeforeStart)
        );
        draft.all_day = true;
        draft.ending = Ending::On(day(2027, 3, 4));
        assert_eq!(
            draft.to_event(&utc()).err(),
            Some(DraftError::AllDayEndsOnDate)
        );
        draft.ending = Ending::After(0);
        assert_eq!(draft.to_event(&utc()).err(), Some(DraftError::BadCount));
    }

    #[test]
    fn editing_reads_the_stored_event_back_on_the_local_clock() {
        let mut stored = timed(
            "e1",
            "Game night",
            at(2027, 2, 5, 17, 0),
            Some(at(2027, 2, 5, 19, 30)),
        );
        stored.description = Some("Bring snacks".to_string());
        stored.workspace_room = Some("event-1@rooms.x.test".to_string());
        stored.recurrence = Some(weekly(5));
        let draft = Draft::from_event(&stored, &plus(2));
        assert_eq!(draft.event_id, "e1");
        assert_eq!(draft.start_date, day(2027, 2, 5));
        assert_eq!(
            draft.start_time,
            NaiveTime::from_hms_opt(19, 0, 0).expect("time")
        );
        assert_eq!(
            draft.end_time,
            NaiveTime::from_hms_opt(21, 30, 0).expect("time")
        );
        assert_eq!(draft.repeat, Repeat::Weekly);
        assert_eq!(draft.ending, Ending::After(5));
        assert!(!draft.rule_is_beyond_form());

        // Saved unchanged, the same rule goes back, so the core keeps it.
        let back = draft.to_event(&plus(2)).expect("valid");
        assert_eq!(back.recurrence, stored.recurrence);
        assert_eq!(back.id, "e1");
        assert_eq!(back.workspace_room.as_deref(), Some("event-1@rooms.x.test"));
        assert_eq!(back.description.as_deref(), Some("Bring snacks"));
    }

    #[test]
    fn a_rule_the_form_cannot_say_is_kept_until_the_repeat_is_edited() {
        let mut stored = all_day("m", "Payday", day(2027, 2, 1), None);
        stored.recurrence = Some(WaddleRecurrence {
            frequency: WaddleRecurrenceFrequency::Monthly,
            interval: 1,
            weekdays: Vec::new(),
            month_days: vec![1, -1],
            end: WaddleRecurrenceEnd::Never,
        });
        let mut draft = Draft::from_event(&stored, &utc());
        assert!(draft.rule_is_beyond_form());
        assert_eq!(
            draft.to_event(&utc()).expect("kept").recurrence,
            stored.recurrence
        );
        draft.rule_edited = true;
        let rule = draft
            .to_event(&utc())
            .expect("rewritten")
            .recurrence
            .expect("rule");
        assert!(rule.month_days.is_empty());
    }

    #[test]
    fn an_all_day_event_reads_back_with_its_inclusive_last_day() {
        let stored = all_day("r", "Retreat", day(2027, 2, 15), Some(day(2027, 2, 18)));
        let draft = Draft::from_event(&stored, &plus(5));
        assert!(draft.all_day);
        assert_eq!(draft.start_date, day(2027, 2, 15));
        assert_eq!(draft.end_date, day(2027, 2, 17));
    }

    fn planning(id: &str, kind: WaddlePlanningKind, title: &str) -> WaddlePlanningItem {
        WaddlePlanningItem {
            id: id.to_string(),
            kind,
            author: None,
            title: title.to_string(),
            description: None,
            url: None,
            category: None,
            location: None,
            timezone: None,
            starts_at: None,
            ends_at: None,
            assignee: None,
            completed: false,
            pinned: false,
            amount_minor: None,
            currency: None,
            participants: Vec::new(),
            options: Vec::new(),
            selections: Vec::new(),
            subject: None,
            attendance: None,
            status: None,
        }
    }

    #[test]
    fn planning_lists_tasks_first_and_skips_votes() {
        let mut done = planning("1", WaddlePlanningKind::Task, "Book venue");
        done.completed = true;
        let mut decision = planning("3", WaddlePlanningKind::Decision, "Where to eat");
        decision.status = Some(WaddlePlanningStatus::Confirmed);
        let items = [
            planning("2", WaddlePlanningKind::Link, "Map"),
            decision,
            planning("4", WaddlePlanningKind::Vote, "Pizza"),
            planning("5", WaddlePlanningKind::Attendance, "ann"),
            done,
            planning("6", WaddlePlanningKind::Task, "  "),
        ];
        let rows = planning_rows(&items);
        let kinds: Vec<&str> = rows.iter().map(|row| row.kind).collect();
        assert_eq!(kinds, ["Task", "Decision", "Link"]);
        assert!(rows[0].done);
        assert_eq!(rows[1].note.as_deref(), Some("Confirmed"));
    }

    #[test]
    fn an_answer_replaces_the_old_one_or_is_added() {
        let mut stored = timed("a", "A", 0, None);
        apply_reply(&mut stored, "ann@x.test", Rsvp::Going);
        apply_reply(&mut stored, "bob@x.test", Rsvp::Maybe);
        apply_reply(&mut stored, "ANN@x.test", Rsvp::NotGoing);
        assert_eq!(stored.attendees.len(), 2);
        assert_eq!(
            my_reply(&stored.attendees, "ann@x.test"),
            Some(Rsvp::NotGoing)
        );
        assert_eq!(my_reply(&stored.attendees, "bob@x.test"), Some(Rsvp::Maybe));
    }

    #[test]
    fn moving_the_start_keeps_the_length() {
        let mut draft = filled();
        draft.end_time = NaiveTime::from_hms_opt(21, 30, 0).expect("time");
        draft.move_start(
            day(2027, 2, 6),
            NaiveTime::from_hms_opt(23, 0, 0).expect("time"),
        );
        assert_eq!(draft.end_date, day(2027, 2, 7));
        assert_eq!(
            draft.end_time,
            NaiveTime::from_hms_opt(1, 30, 0).expect("time")
        );

        let mut draft = filled();
        draft.all_day = true;
        draft.end_date = day(2027, 2, 7);
        draft.move_start(day(2027, 3, 1), NaiveTime::MIN);
        assert_eq!(draft.end_date, day(2027, 3, 3));
    }

    #[test]
    fn an_undated_event_says_so() {
        let occurrence = Occurrence::undated(&event("u", "Someday"));
        assert!(occurrence.is_undated());
        assert_eq!(time_label(&occurrence, &utc()), "No date yet");
        assert_eq!(detail_label(&occurrence, &utc()), "No date yet");
    }
}
