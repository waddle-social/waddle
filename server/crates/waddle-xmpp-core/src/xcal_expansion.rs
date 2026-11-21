//! Occurrences of a calendar item (RFC 5545 §3.3.10, §3.8.5).
//!
//! A repeating event is stored once, as a rule. Every client has to agree
//! on which instants that rule names, because a cancelled occurrence
//! (EXDATE) and a changed one (RECURRENCE-ID) are addressed by the start
//! the occurrence would have had. This module is that one answer.
//!
//! The calendar carries no time zone, so the rule is walked in UTC: a
//! weekly event keeps its UTC time of day across a daylight-saving change.
//!
//! ## What a rule means here
//!
//! | FREQ    | occurrences in each period                                   |
//! |---------|--------------------------------------------------------------|
//! | DAILY   | the day                                                      |
//! | WEEKLY  | BYDAY, else the weekday of DTSTART; weeks start on Monday     |
//! | MONTHLY | BYMONTHDAY, else every BYDAY weekday, else the day of DTSTART |
//! | YEARLY  | BYMONTHDAY in every month, else every BYDAY weekday, else the |
//! |         | month and day of DTSTART                                     |
//!
//! A day a month does not have (the 31st in April, 29 February outside a
//! leap year) is skipped, not moved. BYDAY and BYMONTHDAY that do not pick
//! the days of a period only narrow them. COUNT counts cancelled
//! occurrences; UNTIL includes an occurrence that starts on it. Ordinal
//! BYDAY ("the second Tuesday") is not modelled by [`Rrule`].

use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveTime, TimeDelta, Utc};

use crate::xcal::{CalendarDateValue, CalendarItem, Freq, Rrule, RruleEnd, VEvent, Weekday};

/// Periods walked before giving up on a rule: 130 years of a daily event.
const MAX_PERIODS: u32 = 48_000;

/// One concrete occurrence of a calendar item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// The start the rule gives this occurrence: what EXDATE and
    /// RECURRENCE-ID name it by. `None` for an event that does not repeat.
    pub recurrence_id: Option<CalendarDateValue>,
    /// When the occurrence starts, after any change to it alone.
    pub start: CalendarDateValue,
    /// When the occurrence ends, if the event has an end.
    pub end: Option<CalendarDateValue>,
    /// Title of the occurrence.
    pub summary: String,
    /// Description of the occurrence.
    pub description: Option<String>,
    /// Location of the occurrence.
    pub location: Option<String>,
}

/// The occurrences of `item` that overlap `from..to`, earliest first.
///
/// An all-day occurrence is placed at midnight UTC of its date; a caller
/// that shows dates in another zone should widen the range by a day.
/// An item without a start has no occurrences.
#[must_use]
pub fn expand_occurrences(
    item: &CalendarItem,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<Occurrence> {
    let master = &item.master;
    let Some(start) = master.dtstart else {
        return Vec::new();
    };
    let in_range = |occurrence: &Occurrence| overlaps(occurrence, from, to);
    let Some(rule) = &master.rrule else {
        let single = occurrence_of(master, None, start);
        return Some(single).filter(in_range).into_iter().collect();
    };
    // A changed occurrence may have been moved into the range from after it.
    let horizon = item
        .overrides
        .iter()
        .filter_map(|changed| changed.recurrence_id)
        .map(instant)
        .fold(to, DateTime::max);
    let mut occurrences: Vec<Occurrence> = rule_starts(rule, start, horizon)
        .into_iter()
        .filter(|candidate| !master.exdates.contains(candidate))
        .map(|candidate| {
            let changed = item
                .overrides
                .iter()
                .find(|changed| changed.recurrence_id == Some(candidate));
            match changed {
                Some(changed) => changed_occurrence(master, changed, candidate),
                None => occurrence_of(master, Some(candidate), candidate),
            }
        })
        .filter(in_range)
        .collect();
    occurrences.sort_by_key(|occurrence| instant(occurrence.start));
    occurrences
}

/// Every start the rule names from `start` up to `horizon`, in order,
/// cancelled ones included.
fn rule_starts(
    rule: &Rrule,
    start: CalendarDateValue,
    horizon: DateTime<Utc>,
) -> Vec<CalendarDateValue> {
    let (first_day, time) = match start {
        CalendarDateValue::Date(date) => (date, None),
        CalendarDateValue::DateTime(at) => (at.date_naive(), Some(at.time())),
    };
    let (count, until) = match rule.end {
        Some(RruleEnd::Count(count)) => (Some(count as usize), None),
        Some(RruleEnd::Until(until)) => (None, Some(until)),
        None => (None, None),
    };
    let last = until.map_or(horizon, |until| until.min(horizon));
    let mut starts = Vec::new();
    for period in 0..MAX_PERIODS {
        let Some(days) = period_days(rule, first_day, period) else {
            break;
        };
        if days.opens > last.date_naive() {
            break;
        }
        for day in days.picked {
            let candidate = at_time(day, time);
            if day < first_day {
                continue;
            }
            if instant(candidate) > last || count.is_some_and(|count| starts.len() >= count) {
                return starts;
            }
            starts.push(candidate);
        }
    }
    starts
}

/// The days a rule picks in one of its periods.
struct PeriodDays {
    /// First day of the period, picked or not.
    opens: NaiveDate,
    /// The picked days, in order.
    picked: Vec<NaiveDate>,
}

/// The `period`th day, week, month or year of the rule, counted from the
/// one holding `first_day`. `None` once the calendar runs out.
fn period_days(rule: &Rrule, first_day: NaiveDate, period: u32) -> Option<PeriodDays> {
    let step = period.checked_mul(rule.interval.unwrap_or(1).max(1))?;
    let (opens, mut picked) = match rule.freq {
        Freq::Daily => {
            let day = first_day.checked_add_days(Days::new(u64::from(step)))?;
            (day, vec![day])
        }
        Freq::Weekly => {
            let monday = first_day
                .checked_sub_days(Days::new(u64::from(days_from_monday(first_day))))?
                .checked_add_days(Days::new(u64::from(step) * 7))?;
            let week = (0..7).filter_map(|offset| monday.checked_add_days(Days::new(offset)));
            let picked = if rule.by_day.is_empty() {
                week.filter(|day| day.weekday() == first_day.weekday())
                    .collect()
            } else {
                week.collect()
            };
            (monday, picked)
        }
        Freq::Monthly => {
            let months = first_day.year().checked_mul(12)? + first_day.month0() as i32;
            let months = months.checked_add(i32::try_from(step).ok()?)?;
            let (year, month) = (months.div_euclid(12), months.rem_euclid(12) as u32 + 1);
            let opens = NaiveDate::from_ymd_opt(year, month, 1)?;
            let month_days = opens.iter_days().take_while(|day| day.month() == month);
            let picked = if rule.by_month_day.is_empty() && rule.by_day.is_empty() {
                month_days
                    .filter(|day| day.day() == first_day.day())
                    .collect()
            } else {
                month_days.collect()
            };
            (opens, picked)
        }
        Freq::Yearly => {
            let year = first_day.year().checked_add(i32::try_from(step).ok()?)?;
            let opens = NaiveDate::from_ymd_opt(year, 1, 1)?;
            let picked = if rule.by_month_day.is_empty() && rule.by_day.is_empty() {
                NaiveDate::from_ymd_opt(year, first_day.month(), first_day.day())
                    .into_iter()
                    .collect()
            } else {
                opens
                    .iter_days()
                    .take_while(|day| day.year() == year)
                    .collect()
            };
            (opens, picked)
        }
    };
    picked.retain(|day| on_weekday(rule, *day) && on_month_day(rule, *day));
    Some(PeriodDays { opens, picked })
}

fn on_weekday(rule: &Rrule, day: NaiveDate) -> bool {
    rule.by_day.is_empty()
        || rule
            .by_day
            .iter()
            .any(|weekday| chrono_weekday(*weekday) == day.weekday())
}

fn on_month_day(rule: &Rrule, day: NaiveDate) -> bool {
    let from_start = day.day() as i32;
    // -1 is the last day of the month.
    let from_end = from_start - days_in_month(day) - 1;
    rule.by_month_day.is_empty()
        || rule
            .by_month_day
            .iter()
            .any(|month_day| *month_day == from_start || *month_day == from_end)
}

fn days_in_month(day: NaiveDate) -> i32 {
    let next_month = if day.month() == 12 {
        NaiveDate::from_ymd_opt(day.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(day.year(), day.month() + 1, 1)
    };
    next_month
        .and_then(|next| next.pred_opt())
        .map_or(31, |last| last.day() as i32)
}

fn days_from_monday(day: NaiveDate) -> u32 {
    day.weekday().num_days_from_monday()
}

fn chrono_weekday(weekday: Weekday) -> chrono::Weekday {
    match weekday {
        Weekday::Sunday => chrono::Weekday::Sun,
        Weekday::Monday => chrono::Weekday::Mon,
        Weekday::Tuesday => chrono::Weekday::Tue,
        Weekday::Wednesday => chrono::Weekday::Wed,
        Weekday::Thursday => chrono::Weekday::Thu,
        Weekday::Friday => chrono::Weekday::Fri,
        Weekday::Saturday => chrono::Weekday::Sat,
    }
}

fn at_time(day: NaiveDate, time: Option<NaiveTime>) -> CalendarDateValue {
    match time {
        Some(time) => CalendarDateValue::DateTime(day.and_time(time).and_utc()),
        None => CalendarDateValue::Date(day),
    }
}

/// Where a calendar value sits on the UTC timeline; a date is its midnight.
fn instant(value: CalendarDateValue) -> DateTime<Utc> {
    match value {
        CalendarDateValue::DateTime(at) => at,
        CalendarDateValue::Date(date) => date.and_time(NaiveTime::MIN).and_utc(),
    }
}

/// `start` moved by the length of the event, when it has one.
fn end_after(master: &VEvent, start: CalendarDateValue) -> Option<CalendarDateValue> {
    let length = instant(master.dtend?) - instant(master.dtstart?);
    match start {
        CalendarDateValue::DateTime(at) => at.checked_add_signed(length).map(Into::into),
        CalendarDateValue::Date(date) => date
            .checked_add_signed(TimeDelta::days(length.num_days()))
            .map(Into::into),
    }
}

fn occurrence_of(
    master: &VEvent,
    recurrence_id: Option<CalendarDateValue>,
    start: CalendarDateValue,
) -> Occurrence {
    Occurrence {
        recurrence_id,
        start,
        end: end_after(master, start),
        summary: master.summary.clone(),
        description: master.description.clone(),
        location: master.location.clone(),
    }
}

/// The occurrence at `recurrence_id` with the fields `changed` replaces.
fn changed_occurrence(
    master: &VEvent,
    changed: &VEvent,
    recurrence_id: CalendarDateValue,
) -> Occurrence {
    let start = changed.dtstart.unwrap_or(recurrence_id);
    let kept = occurrence_of(master, Some(recurrence_id), start);
    Occurrence {
        end: changed.dtend.or(kept.end),
        summary: Some(changed.summary.clone())
            .filter(|summary| !summary.is_empty())
            .unwrap_or(kept.summary),
        description: changed.description.clone().or(kept.description),
        location: changed.location.clone().or(kept.location),
        ..kept
    }
}

fn overlaps(occurrence: &Occurrence, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
    let start = instant(occurrence.start);
    let end = match (occurrence.end, occurrence.start) {
        (Some(end), _) => instant(end),
        (None, CalendarDateValue::Date(_)) => start + TimeDelta::days(1),
        (None, CalendarDateValue::DateTime(_)) => start,
    };
    start < to && (end > from || start >= from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, 0, 0)
            .single()
            .expect("valid instant")
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
    }

    /// 2027-02-05 is a Friday.
    fn friday_evening(rule: Rrule) -> CalendarItem {
        CalendarItem::new(
            VEvent::new("night", "Game night")
                .with_dtstart(at(2027, 2, 5, 19))
                .with_dtend(at(2027, 2, 5, 22))
                .with_rrule(rule),
        )
    }

    fn starts(item: &CalendarItem, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<String> {
        expand_occurrences(item, from, to)
            .into_iter()
            .map(|occurrence| match occurrence.start {
                CalendarDateValue::DateTime(at) => at.format("%Y-%m-%d %H:%M").to_string(),
                CalendarDateValue::Date(date) => date.to_string(),
            })
            .collect()
    }

    fn february(item: &CalendarItem) -> Vec<String> {
        starts(item, at(2027, 2, 1, 0), at(2027, 3, 1, 0))
    }

    #[test]
    fn an_event_that_does_not_repeat_occurs_once() {
        let item = CalendarItem::new(
            VEvent::new("launch", "Launch")
                .with_dtstart(at(2027, 2, 5, 19))
                .with_dtend(at(2027, 2, 5, 22)),
        );

        let found = expand_occurrences(&item, at(2027, 2, 1, 0), at(2027, 3, 1, 0));

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].recurrence_id, None);
        assert_eq!(found[0].end, Some(at(2027, 2, 5, 22).into()));
        assert_eq!(starts(&item, at(2027, 3, 1, 0), at(2027, 4, 1, 0)), [""; 0]);
    }

    #[test]
    fn an_event_without_a_start_never_occurs() {
        let item = CalendarItem::new(VEvent::new("idea", "Some day"));
        assert!(expand_occurrences(&item, at(2000, 1, 1, 0), at(2100, 1, 1, 0)).is_empty());
    }

    #[test]
    fn an_event_still_running_when_the_range_opens_is_in_it() {
        let item = CalendarItem::new(
            VEvent::new("camp", "Camp")
                .with_dtstart(at(2027, 1, 30, 12))
                .with_dtend(at(2027, 2, 2, 12)),
        );
        assert_eq!(february(&item), ["2027-01-30 12:00"]);
    }

    #[test]
    fn a_weekly_event_keeps_its_weekday_and_length() {
        let item = friday_evening(Rrule::new(Freq::Weekly));

        let found = expand_occurrences(&item, at(2027, 2, 1, 0), at(2027, 3, 1, 0));

        assert_eq!(
            february(&item),
            [
                "2027-02-05 19:00",
                "2027-02-12 19:00",
                "2027-02-19 19:00",
                "2027-02-26 19:00"
            ]
        );
        assert_eq!(
            found[1].recurrence_id,
            Some(at(2027, 2, 12, 19).into()),
            "an occurrence is named by the start the rule gives it"
        );
        assert_eq!(found[1].end, Some(at(2027, 2, 12, 22).into()));
    }

    #[test]
    fn a_range_far_from_the_start_only_holds_what_falls_in_it() {
        let item = friday_evening(Rrule::new(Freq::Daily));
        assert_eq!(
            starts(&item, at(2031, 6, 1, 0), at(2031, 6, 3, 0)),
            ["2031-06-01 19:00", "2031-06-02 19:00"]
        );
    }

    #[test]
    fn an_interval_skips_periods() {
        let item = friday_evening(Rrule::new(Freq::Daily).with_interval(10));
        assert_eq!(
            february(&item),
            ["2027-02-05 19:00", "2027-02-15 19:00", "2027-02-25 19:00"]
        );
    }

    #[test]
    fn a_week_can_hold_several_days_and_every_other_week_keeps_them_all() {
        let rule = Rrule::new(Freq::Weekly)
            .with_interval(2)
            .with_by_day([Weekday::Monday, Weekday::Friday]);
        // The first week only has its Friday: Monday the 1st is before the start.
        assert_eq!(
            february(&friday_evening(rule)),
            ["2027-02-05 19:00", "2027-02-15 19:00", "2027-02-19 19:00"]
        );
    }

    #[test]
    fn sunday_belongs_to_the_week_that_started_on_monday() {
        let rule = Rrule::new(Freq::Weekly)
            .with_interval(2)
            .with_by_day([Weekday::Sunday]);
        assert_eq!(
            february(&friday_evening(rule)),
            ["2027-02-07 19:00", "2027-02-21 19:00"]
        );
    }

    #[test]
    fn count_ends_the_series_and_counts_cancelled_occurrences() {
        let mut item = friday_evening(Rrule::new(Freq::Weekly).with_count(3));
        item.master = item.master.add_exdate(at(2027, 2, 12, 19));
        assert_eq!(february(&item), ["2027-02-05 19:00", "2027-02-19 19:00"]);
    }

    #[test]
    fn until_includes_an_occurrence_that_starts_on_it() {
        let on_the_hour = friday_evening(Rrule::new(Freq::Weekly).with_until(at(2027, 2, 19, 19)));
        let an_hour_short =
            friday_evening(Rrule::new(Freq::Weekly).with_until(at(2027, 2, 19, 18)));
        assert_eq!(february(&on_the_hour).len(), 3);
        assert_eq!(february(&an_hour_short).len(), 2);
    }

    #[test]
    fn a_monthly_event_skips_months_without_its_day() {
        let item = CalendarItem::new(
            VEvent::new("rent", "Rent")
                .with_dtstart(at(2027, 1, 31, 9))
                .with_rrule(Rrule::new(Freq::Monthly)),
        );
        assert_eq!(
            starts(&item, at(2027, 1, 1, 0), at(2027, 6, 1, 0)),
            ["2027-01-31 09:00", "2027-03-31 09:00", "2027-05-31 09:00"]
        );
    }

    #[test]
    fn month_days_count_from_either_end() {
        let item = CalendarItem::new(
            VEvent::new("books", "Close the books")
                .with_dtstart(at(2027, 1, 1, 9))
                .with_rrule(Rrule::new(Freq::Monthly).with_by_month_day([1, -1])),
        );
        assert_eq!(
            starts(&item, at(2027, 1, 1, 0), at(2027, 3, 1, 0)),
            [
                "2027-01-01 09:00",
                "2027-01-31 09:00",
                "2027-02-01 09:00",
                "2027-02-28 09:00"
            ]
        );
    }

    #[test]
    fn a_monthly_rule_with_weekdays_takes_each_of_them() {
        let item = friday_evening(Rrule::new(Freq::Monthly).with_by_day([Weekday::Friday]));
        assert_eq!(february(&item).len(), 4);
    }

    #[test]
    fn a_yearly_rule_with_weekdays_or_month_days_takes_each_of_them() {
        let fridays = friday_evening(Rrule::new(Freq::Yearly).with_by_day([Weekday::Friday]));
        let fifths = friday_evening(Rrule::new(Freq::Yearly).with_by_month_day([5]));
        assert_eq!(february(&fridays).len(), 4);
        assert_eq!(
            starts(&fifths, at(2027, 1, 1, 0), at(2027, 5, 1, 0)),
            ["2027-02-05 19:00", "2027-03-05 19:00", "2027-04-05 19:00"]
        );
    }

    #[test]
    fn a_leap_day_event_happens_in_leap_years_only() {
        let item = CalendarItem::new(
            VEvent::new("leap", "Leap day")
                .with_dtstart_date(date(2028, 2, 29))
                .with_rrule(Rrule::new(Freq::Yearly)),
        );
        assert_eq!(
            starts(&item, at(2028, 1, 1, 0), at(2033, 1, 1, 0)),
            ["2028-02-29", "2032-02-29"]
        );
    }

    #[test]
    fn an_all_day_series_keeps_dates_and_its_length_in_days() {
        let item = CalendarItem::new(
            VEvent::new("retreat", "Retreat")
                .with_dtstart_date(date(2027, 2, 5))
                .with_dtend_date(date(2027, 2, 7))
                .with_rrule(Rrule::new(Freq::Weekly).with_count(2)),
        );

        let found = expand_occurrences(&item, at(2027, 2, 1, 0), at(2027, 3, 1, 0));

        assert_eq!(found.len(), 2);
        assert_eq!(found[1].start, date(2027, 2, 12).into());
        assert_eq!(found[1].end, Some(date(2027, 2, 14).into()));
    }

    #[test]
    fn a_cancelled_occurrence_is_left_out() {
        let mut item = friday_evening(Rrule::new(Freq::Weekly));
        item.master = item.master.add_exdate(at(2027, 2, 12, 19));
        assert_eq!(
            february(&item),
            ["2027-02-05 19:00", "2027-02-19 19:00", "2027-02-26 19:00"]
        );
    }

    #[test]
    fn a_changed_occurrence_replaces_only_what_it_states() {
        let changed = VEvent::new("night", "")
            .with_recurrence_id(at(2027, 2, 12, 19))
            .with_dtstart(at(2027, 2, 13, 18))
            .with_location("Room 2");
        let item = friday_evening(Rrule::new(Freq::Weekly)).add_override(changed);

        let found = expand_occurrences(&item, at(2027, 2, 1, 0), at(2027, 3, 1, 0));

        let moved = &found[1];
        assert_eq!(moved.recurrence_id, Some(at(2027, 2, 12, 19).into()));
        assert_eq!(moved.start, at(2027, 2, 13, 18).into());
        assert_eq!(moved.end, Some(at(2027, 2, 13, 21).into()));
        assert_eq!(moved.summary, "Game night");
        assert_eq!(moved.location.as_deref(), Some("Room 2"));
        assert_eq!(found.len(), 4);
    }

    #[test]
    fn an_occurrence_moved_across_the_edge_of_the_range_follows_its_new_start() {
        let pulled_in = VEvent::new("night", "")
            .with_recurrence_id(at(2027, 3, 5, 19))
            .with_dtstart(at(2027, 2, 27, 19));
        let pushed_out = VEvent::new("night", "")
            .with_recurrence_id(at(2027, 2, 5, 19))
            .with_dtstart(at(2027, 1, 30, 19));
        let item = friday_evening(Rrule::new(Freq::Weekly))
            .add_override(pulled_in)
            .add_override(pushed_out);
        assert_eq!(
            february(&item),
            [
                "2027-02-12 19:00",
                "2027-02-19 19:00",
                "2027-02-26 19:00",
                "2027-02-27 19:00"
            ]
        );
    }

    #[test]
    fn a_rule_that_never_matches_ends() {
        let no_such_day = friday_evening(Rrule::new(Freq::Daily).with_by_month_day([32]));
        let never_reached = friday_evening(
            Rrule::new(Freq::Yearly)
                .with_interval(u32::MAX)
                .with_count(2),
        );
        assert!(starts(&no_such_day, at(2027, 1, 1, 0), at(2030, 1, 1, 0)).is_empty());
        assert_eq!(
            starts(&never_reached, at(2027, 1, 1, 0), at(2100, 1, 1, 0)),
            ["2027-02-05 19:00"]
        );
    }

    #[test]
    fn an_interval_of_zero_is_read_as_one() {
        let item = friday_evening(Rrule::new(Freq::Weekly).with_interval(0));
        assert_eq!(february(&item).len(), 4);
    }
}
