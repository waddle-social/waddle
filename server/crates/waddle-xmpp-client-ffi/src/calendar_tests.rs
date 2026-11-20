//! FFI calendar suite: what a publish stores, what a fetch hands back, and
//! the occurrences of a fetched event. The xCal wire shape is pinned by
//! `waddle_xmpp_core::xcal`, the recurrence walk by
//! `waddle_xmpp_core::xcal_expansion`.

use chrono::{DateTime, TimeZone, Utc};
use waddle_xmpp_core::xcal::{
    Attendee, CalendarDateValue, CalendarItem, Freq, PartStat, Rrule, VEvent, Weekday,
};

use super::{
    apply_input, cancel_occurrence, expand_community_event, fold_rsvps, to_ffi, WaddleCalendarDate,
    WaddleEvent, WaddleRecurrence, WaddleRecurrenceEnd, WaddleRecurrenceFrequency, WaddleWeekday,
};
use crate::WaddleError;

const WEEK_MS: i64 = 7 * 86_400_000;

const FRIDAY_19: i64 = 1_801_854_000_000; // 2027-02-05T19:00:00Z

fn timed(timestamp_ms: i64) -> WaddleCalendarDate {
    WaddleCalendarDate::DateTime { timestamp_ms }
}
fn day(date: &str) -> WaddleCalendarDate {
    WaddleCalendarDate::Date { date: date.into() }
}
fn instant(timestamp_ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(timestamp_ms)
        .single()
        .expect("valid instant")
}
fn stored(event: VEvent) -> CalendarItem {
    CalendarItem::new(event)
}
fn input_for(item: &CalendarItem) -> WaddleEvent {
    to_ffi("trip".into(), item.clone())
}
fn input() -> WaddleEvent {
    input_for(&stored(VEvent::new("trip", "London")))
}
fn weekly() -> WaddleRecurrence {
    WaddleRecurrence {
        frequency: WaddleRecurrenceFrequency::Weekly,
        interval: 1,
        weekdays: Vec::new(),
        month_days: Vec::new(),
        end: WaddleRecurrenceEnd::Never,
    }
}
fn game_night() -> WaddleEvent {
    WaddleEvent {
        start: Some(timed(FRIDAY_19)),
        recurrence: Some(weekly()),
        ..input()
    }
}
fn publish(event: WaddleEvent) -> Result<CalendarItem, WaddleError> {
    apply_input(stored(VEvent::new("trip", "London")), event)
}
fn rsvp(uid: &str, account: &str, status: PartStat) -> (String, CalendarItem) {
    let answer = VEvent::new(uid, "").with_attendees([Attendee::new(
        format!("xmpp:{account}@example.test"),
        status,
    )]);
    (format!("{uid}-rsvp-{account}"), stored(answer))
}

#[test]
fn accepts_undated_planning_but_rejects_invalid_ranges() {
    assert!(publish(input()).is_ok());
    let mut event = input();
    event.start = Some(day("2027-02-12"));
    event.end = Some(day("2027-02-11"));
    assert!(publish(event).is_err());
}

#[test]
fn dates_round_trip_and_mixed_types_are_rejected() {
    let date = CalendarDateValue::try_from(day("2027-02-12")).expect("valid date");
    assert_eq!(
        CalendarDateValue::try_from(WaddleCalendarDate::from(date)).expect("round trip"),
        date
    );
    let mut event = input();
    event.start = Some(WaddleCalendarDate::from(date));
    event.end = Some(timed(1_900_000_000_000));
    assert!(publish(event).is_err());
}

#[test]
fn an_edit_that_leaves_the_start_keeps_recurrence_attendees_and_changed_occurrences() {
    let master = VEvent::new("trip", "London")
        .with_dtstart(instant(FRIDAY_19))
        .with_rrule(Rrule::new(Freq::Weekly))
        .with_attendees([Attendee::new("xmpp:alex@example.test", PartStat::Accepted)]);
    let moved_occurrence =
        VEvent::new("trip", "London, late").with_recurrence_id(instant(FRIDAY_19 + WEEK_MS));
    let original = stored(master).add_override(moved_occurrence);
    let mut event = input_for(&original);
    event.summary = "London maintainers".into();

    let changed = apply_input(original.clone(), event).expect("valid edit");

    assert_eq!(changed.master.summary, "London maintainers");
    assert_eq!(changed.master.rrule, original.master.rrule);
    assert_eq!(changed.master.attendees, original.master.attendees);
    assert_eq!(changed.overrides, original.overrides);
}

#[test]
fn a_recurrence_is_written_as_the_rule_the_caller_gave() {
    let mut event = game_night();
    event.recurrence = Some(WaddleRecurrence {
        frequency: WaddleRecurrenceFrequency::Weekly,
        interval: 2,
        weekdays: vec![WaddleWeekday::Monday, WaddleWeekday::Friday],
        month_days: Vec::new(),
        end: WaddleRecurrenceEnd::Count { count: 10 },
    });

    let item = publish(event).expect("valid recurrence");

    assert_eq!(
        item.master.rrule,
        Some(
            Rrule::new(Freq::Weekly)
                .with_interval(2)
                .with_by_day([Weekday::Monday, Weekday::Friday])
                .with_count(10)
        )
    );
}

#[test]
fn a_stored_recurrence_reads_back_as_it_was_written() {
    let until = FRIDAY_19 + 30 * 86_400_000;
    let rule = WaddleRecurrence {
        frequency: WaddleRecurrenceFrequency::Monthly,
        interval: 3,
        weekdays: Vec::new(),
        month_days: vec![1, -1],
        end: WaddleRecurrenceEnd::Until {
            timestamp_ms: until,
        },
    };
    let mut event = game_night();
    event.recurrence = Some(rule.clone());

    let read = input_for(&publish(event).expect("valid recurrence"));

    assert_eq!(read.recurrence, Some(rule));
}

#[test]
fn a_rule_without_an_interval_on_the_wire_repeats_every_time() {
    let item = stored(VEvent::new("trip", "London").with_rrule(Rrule::new(Freq::Daily)));
    assert_eq!(
        input_for(&item).recurrence.map(|rule| rule.interval),
        Some(1)
    );
}

#[test]
fn removing_the_recurrence_makes_the_event_single() {
    let original = publish(game_night()).expect("valid recurrence");
    let mut event = input_for(&original);
    event.recurrence = None;

    let single = apply_input(original, event).expect("valid edit");

    assert_eq!(single.master.rrule, None);
}

#[test]
fn rules_no_reader_can_follow_are_rejected() {
    let with = |change: fn(&mut WaddleRecurrence)| {
        let mut rule = weekly();
        change(&mut rule);
        WaddleEvent {
            recurrence: Some(rule),
            ..game_night()
        }
    };
    assert!(publish(with(|rule| rule.interval = 0)).is_err());
    assert!(publish(with(
        |rule| rule.end = WaddleRecurrenceEnd::Count { count: 0 }
    ))
    .is_err());
    let monthly = WaddleRecurrenceFrequency::Monthly;
    assert!(publish(with(|rule| rule.month_days = vec![5])).is_err());
    assert!(publish(with(|rule| {
        rule.frequency = WaddleRecurrenceFrequency::Monthly;
        rule.weekdays = vec![WaddleWeekday::Friday];
    }))
    .is_err());
    for day in [0, 32, -32] {
        let mut event = game_night();
        event.recurrence = Some(WaddleRecurrence {
            frequency: monthly,
            month_days: vec![day],
            ..weekly()
        });
        assert!(publish(event).is_err(), "month day {day}");
    }
    assert!(publish(with(|rule| rule.end = WaddleRecurrenceEnd::Until {
        timestamp_ms: FRIDAY_19 - 1
    }))
    .is_err());
    assert!(publish(with(|rule| rule.weekdays = vec![WaddleWeekday::Friday])).is_ok());
    assert!(publish(with(|rule| {
        rule.frequency = WaddleRecurrenceFrequency::Monthly;
        rule.month_days = vec![-31, 31];
    }))
    .is_ok());
    let undated = WaddleEvent {
        start: None,
        ..game_night()
    };
    assert!(publish(undated).is_err());
}

#[test]
fn an_all_day_series_cannot_end_on_an_instant() {
    let mut event = game_night();
    event.start = Some(day("2027-02-05"));
    assert!(publish(event.clone()).is_ok());
    event.recurrence = Some(WaddleRecurrence {
        end: WaddleRecurrenceEnd::Until {
            timestamp_ms: FRIDAY_19 + 86_400_000,
        },
        ..weekly()
    });
    assert!(publish(event).is_err());
}

#[test]
fn cancelling_an_occurrence_excludes_its_start_once() {
    let original = publish(game_night()).expect("valid recurrence");
    let second = CalendarDateValue::DateTime(instant(FRIDAY_19 + WEEK_MS));
    let third = CalendarDateValue::DateTime(instant(FRIDAY_19 + 2 * WEEK_MS));

    let item = [third, second, third]
        .into_iter()
        .try_fold(original, cancel_occurrence)
        .expect("occurrences of the series");

    assert_eq!(item.master.exdates, vec![second, third]);
    assert_eq!(input_for(&item).excluded.len(), 2);
}

#[test]
fn only_an_occurrence_of_a_repeating_event_can_be_cancelled() {
    let mut single = game_night();
    single.recurrence = None;
    let single = publish(single).expect("valid event");
    let friday = CalendarDateValue::DateTime(instant(FRIDAY_19));
    assert!(matches!(
        cancel_occurrence(single, friday),
        Err(WaddleError::InvalidArgument)
    ));

    let series = publish(game_night()).expect("valid recurrence");
    let all_day = CalendarDateValue::try_from(day("2027-02-12")).expect("valid date");
    assert!(cancel_occurrence(series, all_day).is_err());
}

#[test]
fn an_edit_cannot_restore_or_add_cancelled_occurrences() {
    let series = publish(game_night()).expect("valid recurrence");
    let second = CalendarDateValue::DateTime(instant(FRIDAY_19 + WEEK_MS));
    let stale = input_for(&series);
    let cancelled = cancel_occurrence(series, second).expect("an occurrence");
    let mut invented = stale.clone();
    invented.excluded = vec![timed(FRIDAY_19 + 3 * WEEK_MS)];

    for edit in [stale, invented] {
        let item = apply_input(cancelled.clone(), edit).expect("valid edit");
        assert_eq!(item.master.exdates, vec![second]);
    }
}

#[test]
fn moving_a_series_drops_its_cancelled_and_changed_occurrences() {
    let second = instant(FRIDAY_19 + WEEK_MS);
    let master = VEvent::new("trip", "London")
        .with_dtstart(instant(FRIDAY_19))
        .with_rrule(Rrule::new(Freq::Weekly))
        .add_exdate(second);
    let original =
        stored(master).add_override(VEvent::new("trip", "Late").with_recurrence_id(second));
    let mut event = input_for(&original);
    event.start = Some(timed(FRIDAY_19 + 86_400_000));

    let moved = apply_input(original, event).expect("valid edit");

    assert!(moved.master.exdates.is_empty());
    assert!(moved.overrides.is_empty());
    assert!(moved.master.rrule.is_some());
}

#[test]
fn a_changed_occurrence_is_read_with_only_the_fields_it_changes() {
    let second = instant(FRIDAY_19 + WEEK_MS);
    let master = VEvent::new("trip", "London")
        .with_dtstart(instant(FRIDAY_19))
        .with_rrule(Rrule::new(Freq::Weekly));
    let changed = VEvent::new("trip", "")
        .with_recurrence_id(second)
        .with_location("Room 2");

    let read = input_for(&stored(master).add_override(changed));

    let [occurrence] = read.overrides.as_slice() else {
        panic!("expected one changed occurrence, got {:?}", read.overrides);
    };
    assert!(matches!(
        occurrence.recurrence_id,
        WaddleCalendarDate::DateTime { timestamp_ms } if timestamp_ms == second.timestamp_millis()
    ));
    assert_eq!(occurrence.summary, None);
    assert_eq!(occurrence.location.as_deref(), Some("Room 2"));
    assert!(occurrence.start.is_none());
}

#[test]
fn rsvps_are_folded_into_the_event_they_answer() {
    let event = VEvent::new("trip", "London").with_attendees([Attendee::new(
        "xmpp:alex@example.test",
        PartStat::NeedsAction,
    )]);
    let items = vec![
        rsvp("trip", "alex", PartStat::Accepted),
        ("trip".to_string(), stored(event)),
        rsvp("trip", "sam", PartStat::Declined),
        rsvp("gone", "sam", PartStat::Accepted),
    ];

    let folded = fold_rsvps(items);

    let [(id, trip)] = folded.as_slice() else {
        panic!("expected only the event, got {folded:?}");
    };
    assert_eq!(id, "trip");
    assert_eq!(
        trip.master.attendees,
        vec![
            Attendee::new("xmpp:alex@example.test", PartStat::Accepted),
            Attendee::new("xmpp:sam@example.test", PartStat::Declined),
        ]
    );
}

#[test]
fn an_event_whose_id_looks_like_an_rsvp_stays_an_event() {
    let event = VEvent::new("book-rsvp-club", "Book club")
        .with_attendees([Attendee::new("xmpp:alex@example.test", PartStat::Accepted)]);

    let folded = fold_rsvps(vec![("book-rsvp-club".to_string(), stored(event))]);

    assert_eq!(folded.len(), 1);
}

#[test]
fn a_fetched_event_expands_to_its_occurrences() {
    let second = instant(FRIDAY_19 + WEEK_MS);
    let master = VEvent::new("trip", "London")
        .with_dtstart(instant(FRIDAY_19))
        .with_dtend(instant(FRIDAY_19 + 3_600_000))
        .with_rrule(Rrule::new(Freq::Weekly).with_count(4))
        .add_exdate(instant(FRIDAY_19 + 2 * WEEK_MS));
    let changed = VEvent::new("trip", "London, late")
        .with_recurrence_id(second)
        .with_dtstart(instant(FRIDAY_19 + WEEK_MS + 3_600_000));
    let fetched = input_for(&stored(master).add_override(changed));

    let found =
        expand_community_event(fetched, FRIDAY_19, FRIDAY_19 + 10 * WEEK_MS).expect("valid event");

    let seen: Vec<_> = found
        .iter()
        .map(|occurrence| match occurrence.start {
            WaddleCalendarDate::DateTime { timestamp_ms } => (
                occurrence.summary.as_str(),
                (timestamp_ms - FRIDAY_19) / 3_600_000,
            ),
            WaddleCalendarDate::Date { .. } => panic!("a timed event has timed occurrences"),
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("London", 0),
            ("London, late", 7 * 24 + 1),
            ("London", 3 * 7 * 24)
        ]
    );
    assert!(matches!(
        found[1].recurrence_id,
        Some(WaddleCalendarDate::DateTime { timestamp_ms }) if timestamp_ms == FRIDAY_19 + WEEK_MS
    ));
    assert!(matches!(
        found[1].end,
        Some(WaddleCalendarDate::DateTime { timestamp_ms })
            if timestamp_ms == FRIDAY_19 + WEEK_MS + 2 * 3_600_000
    ));
}

#[test]
fn an_edit_keeps_a_rule_this_client_would_not_write() {
    let first_fridays = Rrule::new(Freq::Monthly).with_by_day([Weekday::Friday]);
    let original = stored(
        VEvent::new("trip", "London")
            .with_dtstart(instant(FRIDAY_19))
            .with_rrule(first_fridays.clone()),
    );
    let mut event = input_for(&original);
    event.summary = "London maintainers".into();

    let changed = apply_input(original, event).expect("the rule is unchanged");

    assert_eq!(changed.master.rrule, Some(first_fridays));
}
