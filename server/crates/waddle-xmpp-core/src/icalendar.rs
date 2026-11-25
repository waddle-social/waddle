//! RFC 5545 text export of published xCal events.

use chrono::{DateTime, Utc};

use crate::xcal::{CalendarDateValue, CalendarItem, Rrule, RruleEnd, VEvent};

const ICS_PRODUCT_ID: &str = "-//Waddle//Community xCal Events//EN";
const MAX_CONTENT_LINE_BYTES: usize = 75;

/// Encodes complete master events with their recurrence exceptions for external calendar clients.
pub fn calendar_text<'a>(
    items: impl IntoIterator<Item = (&'a CalendarItem, DateTime<Utc>)>,
) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        property("PRODID", ICS_PRODUCT_ID),
        "CALSCALE:GREGORIAN".to_string(),
    ];

    for (item, published_at) in items {
        lines.extend(event_lines(&item.master, None, published_at));
        for override_event in &item.overrides {
            lines.extend(event_lines(
                override_event,
                Some(&item.master),
                published_at,
            ));
        }
    }

    lines.push("END:VCALENDAR".to_string());
    format!(
        "{}\r\n",
        lines
            .iter()
            .map(|line| fold_content_line(line))
            .collect::<Vec<_>>()
            .join("\r\n")
    )
}

fn event_lines(
    event: &VEvent,
    master: Option<&VEvent>,
    published_at: DateTime<Utc>,
) -> Vec<String> {
    let Some(dtstart) = event.dtstart.or(event.recurrence_id) else {
        return Vec::new();
    };
    let dtstamp = event
        .dtstamp
        .or_else(|| master.and_then(|event| event.dtstamp))
        .unwrap_or(published_at);
    let dtend = event
        .dtend
        .or_else(|| master.and_then(|master| inherited_end(master, dtstart)));
    let summary = if event.summary.is_empty() {
        master.map(|event| event.summary.as_str()).unwrap_or("")
    } else {
        event.summary.as_str()
    };
    let description = event
        .description
        .as_deref()
        .or_else(|| master.and_then(|event| event.description.as_deref()));
    let location = event
        .location
        .as_deref()
        .or_else(|| master.and_then(|event| event.location.as_deref()));
    let organizer = event
        .organizer
        .as_deref()
        .or_else(|| master.and_then(|event| event.organizer.as_deref()));

    let mut lines = vec![
        "BEGIN:VEVENT".to_string(),
        property("UID", &event.uid),
        date_time_property("DTSTAMP", dtstamp),
        calendar_date_property("DTSTART", dtstart),
    ];
    if let Some(dtend) = dtend {
        lines.push(calendar_date_property("DTEND", dtend));
    }
    if let Some(recurrence_id) = event.recurrence_id {
        lines.push(calendar_date_property("RECURRENCE-ID", recurrence_id));
    }
    lines.push(property("SUMMARY", summary));
    if let Some(description) = description {
        lines.push(property("DESCRIPTION", description));
    }
    if let Some(location) = location {
        lines.push(property("LOCATION", location));
    }
    if let Some(organizer) = organizer {
        lines.push(cal_address_property("ORGANIZER", organizer));
    }
    if let Some(rrule) = event.rrule.as_ref() {
        lines.push(rrule_property(rrule));
    }
    if !event.exdates.is_empty() {
        let mut exdates = event.exdates.clone();
        exdates.sort();
        lines.push(exdate_property(exdates));
    }
    lines.push("END:VEVENT".to_string());
    lines
}

fn inherited_end(master: &VEvent, dtstart: CalendarDateValue) -> Option<CalendarDateValue> {
    let master_start = master.dtstart?;
    let master_end = master.dtend?;
    match (master_start, master_end, dtstart) {
        (
            CalendarDateValue::DateTime(master_start),
            CalendarDateValue::DateTime(master_end),
            CalendarDateValue::DateTime(dtstart),
        ) => {
            let duration = master_end.signed_duration_since(master_start);
            if duration.num_milliseconds() < 0 {
                return None;
            }
            dtstart
                .checked_add_signed(duration)
                .map(CalendarDateValue::DateTime)
        }
        (
            CalendarDateValue::Date(master_start),
            CalendarDateValue::Date(master_end),
            CalendarDateValue::Date(dtstart),
        ) => {
            let duration = master_end.signed_duration_since(master_start);
            if duration.num_days() < 0 {
                return None;
            }
            dtstart
                .checked_add_signed(duration)
                .map(CalendarDateValue::Date)
        }
        _ => None,
    }
}

fn property(name: &str, value: &str) -> String {
    format!("{name}:{}", escape_text(value))
}

fn cal_address_property(name: &str, value: &str) -> String {
    let mut line = String::with_capacity(name.len() + value.len() + 1);
    line.push_str(name);
    line.push(':');
    for ch in value.chars() {
        if ch != '\r' && ch != '\n' {
            line.push(ch);
        }
    }
    line
}

fn date_time_property(name: &str, value: DateTime<Utc>) -> String {
    format!("{name}:{}", format_utc_date_time(value))
}

fn calendar_date_property(name: &str, value: CalendarDateValue) -> String {
    match value {
        CalendarDateValue::Date(date) => {
            format!("{name};VALUE=DATE:{}", date.format("%Y%m%d"))
        }
        CalendarDateValue::DateTime(date_time) => {
            format!("{name}:{}", format_utc_date_time(date_time))
        }
    }
}

fn exdate_property(values: Vec<CalendarDateValue>) -> String {
    let all_day = values
        .first()
        .is_some_and(|value| matches!(value, CalendarDateValue::Date(_)));
    let encoded = values
        .into_iter()
        .map(|value| match value {
            CalendarDateValue::Date(date) => date.format("%Y%m%d").to_string(),
            CalendarDateValue::DateTime(date_time) => format_utc_date_time(date_time),
        })
        .collect::<Vec<_>>()
        .join(",");
    if all_day {
        format!("EXDATE;VALUE=DATE:{encoded}")
    } else {
        format!("EXDATE:{encoded}")
    }
}

fn rrule_property(rrule: &Rrule) -> String {
    let mut parts = vec![format!("FREQ={}", rrule.freq.as_str())];
    if let Some(interval) = rrule.interval {
        parts.push(format!("INTERVAL={interval}"));
    }
    if !rrule.by_day.is_empty() {
        parts.push(format!(
            "BYDAY={}",
            rrule
                .by_day
                .iter()
                .map(|day| day.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if !rrule.by_month_day.is_empty() {
        parts.push(format!(
            "BYMONTHDAY={}",
            rrule
                .by_month_day
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    match rrule.end {
        Some(RruleEnd::Count(count)) => parts.push(format!("COUNT={count}")),
        Some(RruleEnd::Until(until)) => {
            parts.push(format!("UNTIL={}", format_utc_date_time(until)))
        }
        None => {}
    }
    format!("RRULE:{}", parts.join(";"))
}

fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            _ => out.push(ch),
        }
    }
    out
}

fn format_utc_date_time(value: DateTime<Utc>) -> String {
    value.format("%Y%m%dT%H%M%SZ").to_string()
}

fn fold_content_line(line: &str) -> String {
    let mut out = String::new();
    let mut current_bytes = 0;
    for ch in line.chars() {
        let char_bytes = ch.len_utf8();
        if current_bytes > 0 && current_bytes + char_bytes > MAX_CONTENT_LINE_BYTES {
            out.push_str("\r\n ");
            current_bytes = 1;
        }
        out.push(ch);
        current_bytes += char_bytes;
    }
    out
}
