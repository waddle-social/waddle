import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

// Expected values are written out by hand from a calendar, never worked
// out the way the code does it.

private let oslo = TimeZone(identifier: "Europe/Oslo")!
private let losAngeles = TimeZone(identifier: "America/Los_Angeles")!
private let utc = TimeZone(identifier: "UTC")!

/// An instant written as ISO 8601 with its offset.
private func at(_ iso: String) -> Date {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime]
    return formatter.date(from: iso)!
}

private func day(_ year: Int, _ month: Int, _ day: Int) -> CalendarDay {
    CalendarDay(year: year, month: month, day: day)
}

private func form(
    start: String,
    end: String? = nil,
    allDay: Bool = false,
    repeats: EventForm.Repeat = .never,
    timeZone: TimeZone = oslo
) -> EventForm {
    var form = EventForm(on: day(2027, 2, 1), now: at("2027-02-01T09:00:00Z"), timeZone: timeZone)
    form.title = "Game night"
    form.start = at(start)
    form.lastDay = at(start)
    if let end {
        if allDay {
            form.lastDay = at(end)
        } else {
            form.duration = .custom
            form.customEnd = at(end)
        }
    }
    form.isAllDay = allDay
    form.repeats = repeats
    return form
}

private func occurrence(_ start: CalendarMoment, _ end: CalendarMoment?) -> EventOccurrence {
    EventOccurrence(eventID: "e", recurrenceStart: nil, start: start, end: end, title: "Game night")
}

@Suite("Events in local time")
struct EventsTimeTests {
    // MARK: Days

    @Test func aDayReadsAndWritesItsISOForm() {
        #expect(CalendarDay(iso: "2027-02-05") == day(2027, 2, 5))
        #expect(day(2027, 2, 5).iso == "2027-02-05")
        #expect(CalendarDay(iso: "2027-02-30") == nil)
        #expect(CalendarDay(iso: "2027-02-29") == nil)
        #expect(CalendarDay(iso: "2028-02-29") == day(2028, 2, 29))
        #expect(CalendarDay(iso: "5 Feb") == nil)
    }

    @Test func daysStepAcrossMonthsAndYears() {
        #expect(day(2027, 2, 28).advanced(by: 1) == day(2027, 3, 1))
        #expect(day(2028, 2, 28).advanced(by: 1) == day(2028, 2, 29))
        #expect(day(2027, 12, 31).advanced(by: 1) == day(2028, 1, 1))
        #expect(day(2027, 3, 1).distance(to: day(2027, 2, 1)) == -28)
    }

    @Test func aDayKnowsItsWeekday() {
        #expect(day(2027, 2, 5).weekday == .friday)
        #expect(day(2027, 2, 7).weekday == .sunday)
        #expect(day(2027, 2, 8).weekday == .monday)
    }

    @Test func theLocalDayOfAnInstantFollowsTheTimeZone() {
        let late = at("2027-02-07T23:30:00Z")
        #expect(CalendarDay(late, in: oslo) == day(2027, 2, 8))
        #expect(CalendarDay(late, in: utc) == day(2027, 2, 7))
        #expect(day(2027, 2, 8).start(in: oslo) == at("2027-02-07T23:00:00Z"))
    }

    // MARK: Picks to a draft

    @Test func mondaysAtHalfPastMidnightInOsloRepeatOnSundaysInUTC() throws {
        var picks = form(start: "2027-02-08T00:30:00+01:00", end: "2027-02-08T02:00:00+01:00", repeats: .weekly)
        picks.weekdays = [.monday]

        let draft = try #require(picks.draft())

        #expect(draft.start == .instant(at("2027-02-07T23:30:00Z")))
        #expect(draft.recurrence == EventRecurrence(frequency: .weekly, weekdays: [.sunday]))
    }

    @Test func everyWeekdayPickedMovesBackADayInUTC() throws {
        var picks = form(start: "2027-02-08T00:30:00+01:00", repeats: .weekly)
        picks.weekdays = [.monday, .friday, .sunday]

        let draft = try #require(picks.draft())

        #expect(draft.recurrence?.weekdays == [.sunday, .thursday, .saturday])
    }

    @Test func fridayEveningsInLosAngelesRepeatOnSaturdaysInUTC() throws {
        var picks = form(start: "2027-02-05T18:00:00-08:00", repeats: .weekly, timeZone: losAngeles)
        picks.weekdays = [.friday]

        let draft = try #require(picks.draft())

        #expect(draft.recurrence?.weekdays == [.saturday])
    }

    @Test func aWeeklyRuleAlwaysNamesTheStartsWeekday() throws {
        let picks = form(start: "2027-02-10T10:00:00+01:00", repeats: .weekly)

        let draft = try #require(picks.draft())

        #expect(draft.recurrence?.weekdays == [.wednesday])
        #expect(picks.shownWeekdays() == [.wednesday])
    }

    @Test func aDailyRuleNamesNoWeekdays() throws {
        let draft = try #require(form(start: "2027-02-10T10:00:00+01:00", repeats: .daily).draft())

        #expect(draft.recurrence == EventRecurrence(frequency: .daily))
    }

    @Test func aOneDayEventEndsTheNextDay() throws {
        let draft = try #require(form(start: "2027-02-05T12:00:00+01:00", allDay: true).draft())

        #expect(draft.start == .day(day(2027, 2, 5)))
        #expect(draft.end == .day(day(2027, 2, 6)))
    }

    @Test func theLastDayPickedIsIncludedInAnAllDayEvent() throws {
        let picks = form(start: "2027-02-05T12:00:00+01:00", end: "2027-02-07T08:00:00+01:00", allDay: true)

        let draft = try #require(picks.draft())

        #expect(draft.end == .day(day(2027, 2, 8)))
    }

    @Test func anAllDayEventIsTheDayPickedWhateverTheHour() throws {
        // 00:30 in Oslo is still the evening before in UTC.
        let draft = try #require(form(start: "2027-02-05T00:30:00+01:00", allDay: true).draft())

        #expect(draft.start == .day(day(2027, 2, 5)))
    }

    @Test func anAllDayEventCannotEndBeforeItStarts() {
        let picks = form(start: "2027-02-05T12:00:00+01:00", end: "2027-02-04T12:00:00+01:00", allDay: true)

        #expect(picks.draft() == nil)
    }

    @Test func anAllDaySeriesKeepsItsWeekdayInEveryTimeZone() throws {
        let picks = form(start: "2027-02-08T12:00:00+01:00", allDay: true, repeats: .weekly)

        #expect(try #require(picks.draft()).recurrence?.weekdays == [.monday])
    }

    @Test func aTimedEventWithoutAnEndIsAnInstant() throws {
        let draft = try #require(EventDraft(title: "Game night", start: .instant(at("2027-02-05T19:00:00+01:00"))))

        #expect(draft.start == .instant(at("2027-02-05T18:00:00Z")))
        #expect(draft.end == nil)
    }

    @Test func aTimedEventMustEndAfterItStarts() {
        let picks = form(start: "2027-02-05T19:00:00+01:00", end: "2027-02-05T19:00:00+01:00")

        #expect(picks.draft() == nil)
    }

    @Test func endingOnADateRunsToTheEndOfThatLocalDay() throws {
        var winter = form(start: "2027-02-05T19:00:00+01:00", repeats: .weekly)
        winter.ending = .on
        winter.until = at("2027-03-01T09:00:00+01:00")
        var summer = winter
        summer.until = at("2027-07-01T09:00:00+02:00")

        #expect(try #require(winter.draft()).recurrence?.end == .until(at("2027-03-01T22:59:59Z")))
        #expect(try #require(summer.draft()).recurrence?.end == .until(at("2027-07-01T21:59:59Z")))
    }

    @Test func endingAfterSomeTimesCountsThem() throws {
        var picks = form(start: "2027-02-05T19:00:00+01:00", repeats: .monthly)
        picks.ending = .after
        picks.count = 6
        picks.interval = 2

        let draft = try #require(picks.draft())

        #expect(draft.recurrence == EventRecurrence(frequency: .monthly, interval: 2, end: .count(6)))
    }

    @Test func anAllDaySeriesCannotEndOnADate() {
        var picks = form(start: "2027-02-05T19:00:00+01:00", repeats: .weekly)
        picks.ending = .on
        picks.isAllDay = true

        #expect(picks.ending == .never)
        #expect(!picks.offersEndDate)
    }

    @Test func movingTheStartKeepsTheNumberOfAllDayDates() {
        var picks = form(start: "2027-02-05T19:00:00+01:00", end: "2027-02-07T21:30:00+01:00", allDay: true)

        picks.moveStart(to: at("2027-02-06T10:00:00+01:00"))

        #expect(picks.start == at("2027-02-06T10:00:00+01:00"))
        #expect(CalendarDay(picks.lastDay, in: oslo) == day(2027, 2, 8))
    }

    @Test func movingAWeeklySeriesMovesItsWeekday() throws {
        var picks = form(start: "2027-02-08T19:00:00+01:00", repeats: .weekly)
        picks.duration = .twoHours
        picks.weekdays = [.monday, .friday]

        picks.moveStart(to: at("2027-02-09T19:00:00+01:00"))

        #expect(picks.shownWeekdays() == [.tuesday, .friday])
        #expect(try #require(picks.draft()?.recurrence).weekdays == [.tuesday, .friday])
    }

    @Test func aSeriesCanAlwaysEndOnADateOnceItsStartMoves() throws {
        var picks = form(start: "2027-02-05T19:00:00+01:00", repeats: .weekly)
        picks.duration = .twoHours

        picks.moveStart(to: at("2027-06-04T19:00:00+02:00"))
        picks.ending = .on

        #expect(picks.until == at("2027-07-04T19:00:00+02:00"))
        #expect(picks.draft() != nil)
    }

    @Test func editingAnEventFarAheadOffersAnEndDateAfterItsStart() throws {
        let event = fetched(nil, start: .instant(at("2027-09-03T17:00:00Z")), end: .instant(at("2027-09-03T19:00:00Z")))
        var picks = EventForm(editing: event, now: at("2027-02-01T09:00:00Z"), timeZone: oslo)
        picks.repeats = .weekly
        picks.ending = .on

        #expect(picks.until == at("2027-10-03T17:00:00Z"))
        #expect(picks.draft() != nil)
    }

    @Test func anAllDayEventGivenATimeUsesTheDefaultDuration() throws {
        let event = fetched(nil, start: .day(day(2027, 2, 5)), end: .day(day(2027, 2, 6)))
        var picks = EventForm(editing: event, now: at("2027-02-01T09:00:00Z"), timeZone: oslo)

        picks.isAllDay = false

        let draft = try #require(picks.draft())
        #expect(draft.start == .instant(at("2027-02-05T00:00:00+01:00")))
        #expect(draft.end == .instant(at("2027-02-05T00:30:00+01:00")))
    }

    @Test func aNewEventStartsAtTheNextFullHourToday() {
        let picks = EventForm(on: day(2027, 2, 5), now: at("2027-02-05T14:20:00+01:00"), timeZone: oslo)

        #expect(picks.start == at("2027-02-05T15:00:00+01:00"))
    }

    @Test func aNewEventOnAnotherDayStartsInTheMorning() {
        let picks = EventForm(on: day(2027, 2, 9), now: at("2027-02-05T14:20:00+01:00"), timeZone: oslo)

        #expect(picks.start == at("2027-02-09T10:00:00+01:00"))
    }

    // MARK: Editing what was fetched

    private func fetched(
        _ recurrence: EventRecurrence?,
        start: CalendarMoment,
        end: CalendarMoment? = nil,
        cancelled: [CalendarMoment] = []
    ) -> CommunityEvent {
        CommunityEvent(
            id: "e1",
            uid: "e1",
            title: "Game night",
            location: "Rink",
            workspaceRoom: bare("lounge@muc.waddle.test"),
            start: start,
            end: end,
            recurrence: recurrence,
            cancelled: cancelled
        )
    }

    @Test func aRuleLeftAloneIsHandedBackAsItWasFetched() throws {
        let rule = EventRecurrence(frequency: .monthly, monthDays: [15, 1], end: .until(at("2027-06-01T10:00:00Z")))
        let event = fetched(rule, start: .instant(at("2027-02-01T18:00:00Z")))
        var picks = EventForm(editing: event, timeZone: oslo)
        picks.title = "Board game night"

        let draft = try #require(picks.draft())

        #expect(draft.recurrence == rule)
        #expect(draft.eventID == "e1")
        #expect(draft.workspaceRoom == bare("lounge@muc.waddle.test"))
        #expect(draft.location == "Rink")
    }

    @Test func aChangedRuleIsWrittenFromThePicks() throws {
        let rule = EventRecurrence(frequency: .weekly, weekdays: [.sunday])
        let event = fetched(rule, start: .instant(at("2027-02-07T23:30:00Z")))
        var picks = EventForm(editing: event, timeZone: oslo)
        #expect(picks.weekdays == [.monday])

        picks.weekdays = [.monday, .tuesday]
        let draft = try #require(picks.draft())

        #expect(draft.recurrence == EventRecurrence(frequency: .weekly, weekdays: [.sunday, .monday]))
    }

    @Test func editingAnAllDayEventShowsItsLastDay() {
        let event = fetched(nil, start: .day(day(2027, 2, 5)), end: .day(day(2027, 2, 8)))

        let picks = EventForm(editing: event, timeZone: oslo)

        #expect(picks.isAllDay)
        #expect(CalendarDay(picks.start, in: oslo) == day(2027, 2, 5))
        #expect(CalendarDay(picks.lastDay, in: oslo) == day(2027, 2, 7))
    }

    @Test func editingASeriesThatEndsOnADateShowsThatLocalDay() {
        let rule = EventRecurrence(frequency: .daily, end: .until(at("2027-03-01T22:59:59Z")))
        let picks = EventForm(editing: fetched(rule, start: .instant(at("2027-02-01T18:00:00Z"))), timeZone: oslo)

        #expect(picks.ending == .on)
        #expect(CalendarDay(picks.until, in: oslo) == day(2027, 3, 1))
    }

    @Test func movingTheStartOfASeriesWithCancelledDatesIsPointedOut() {
        let event = fetched(
            EventRecurrence(frequency: .daily),
            start: .instant(at("2027-02-01T18:00:00Z")),
            cancelled: [.instant(at("2027-02-03T18:00:00Z"))]
        )
        var picks = EventForm(editing: event, timeZone: oslo)
        #expect(!picks.bringsBackCancelledDates())

        picks.moveStart(to: at("2027-02-01T20:00:00+01:00"))

        #expect(picks.bringsBackCancelledDates())
    }

    // MARK: A rule as the person sees it

    @Test func aUTCRuleShowsTheLocalWeekdays() {
        let rule = EventRecurrence(frequency: .weekly, weekdays: [.sunday, .thursday])
        let start = CalendarMoment.instant(at("2027-02-07T23:30:00Z"))

        #expect(rule.localWeekdays(startingAt: start, in: oslo) == [.monday, .friday])
        #expect(rule.localWeekdays(startingAt: start, in: utc) == [.sunday, .thursday])
        #expect(rule.localWeekdays(startingAt: .instant(at("2027-02-06T02:00:00Z")), in: losAngeles) == [.wednesday, .saturday])
    }

    @Test func aWeeklyRuleWithoutWeekdaysShowsTheStartsWeekday() {
        let rule = EventRecurrence(frequency: .weekly)

        #expect(rule.localWeekdays(startingAt: .instant(at("2027-02-07T23:30:00Z")), in: oslo) == [.monday])
        #expect(rule.localWeekdays(startingAt: .day(day(2027, 2, 7)), in: oslo) == [.sunday])
    }

    // MARK: Which days an occurrence covers

    @Test func aTimedEventCoversTheLocalDaysItRunsOn() {
        let late = occurrence(.instant(at("2027-02-05T22:00:00+01:00")), .instant(at("2027-02-06T02:00:00+01:00")))
        let atMidnight = occurrence(.instant(at("2027-02-05T22:00:00+01:00")), .instant(at("2027-02-06T00:00:00+01:00")))
        let instant = occurrence(.instant(at("2027-02-05T23:30:00Z")), nil)

        #expect(Array(late.days(in: oslo)) == [day(2027, 2, 5), day(2027, 2, 6)])
        #expect(Array(late.days(in: utc)) == [day(2027, 2, 5), day(2027, 2, 6)])
        #expect(Array(atMidnight.days(in: oslo)) == [day(2027, 2, 5)])
        #expect(Array(instant.days(in: oslo)) == [day(2027, 2, 6)])
        #expect(Array(instant.days(in: utc)) == [day(2027, 2, 5)])
    }

    @Test func anAllDayEventCoversItsDatesWhateverTheTimeZone() {
        let threeDays = occurrence(.day(day(2027, 2, 5)), .day(day(2027, 2, 8)))
        let oneDay = occurrence(.day(day(2027, 2, 5)), nil)

        #expect(Array(threeDays.days(in: oslo)) == [day(2027, 2, 5), day(2027, 2, 6), day(2027, 2, 7)])
        #expect(Array(threeDays.days(in: losAngeles)) == [day(2027, 2, 5), day(2027, 2, 6), day(2027, 2, 7)])
        #expect(Array(oneDay.days(in: losAngeles)) == [day(2027, 2, 5)])
    }

    @Test func daysThatAreNot24HoursLongAreCoveredOnce() {
        // Oslo skips 02:00-03:00 on 28 March 2027 and repeats it on 31 October.
        let spring = occurrence(.instant(at("2027-03-28T00:30:00+01:00")), .instant(at("2027-03-28T03:30:00+02:00")))
        let autumn = occurrence(.instant(at("2027-10-31T00:00:00+02:00")), .instant(at("2027-11-01T00:00:00+01:00")))
        let across = occurrence(.instant(at("2027-10-30T12:00:00+02:00")), .instant(at("2027-11-01T12:00:00+01:00")))

        #expect(Array(spring.days(in: oslo)) == [day(2027, 3, 28)])
        #expect(Array(autumn.days(in: oslo)) == [day(2027, 10, 31)])
        #expect(Array(across.days(in: oslo)) == [day(2027, 10, 30), day(2027, 10, 31), day(2027, 11, 1)])
    }

    @Test func anAllDayEventOverlapsOnlyTheLocalDaysItCovers() {
        let fifth = occurrence(.day(day(2027, 2, 5)), .day(day(2027, 2, 6)))
        let sixthInOslo = at("2027-02-06T00:00:00+01:00")..<at("2027-02-07T00:00:00+01:00")
        let fifthInOslo = at("2027-02-05T00:00:00+01:00")..<at("2027-02-06T00:00:00+01:00")

        #expect(!fifth.overlaps(sixthInOslo, in: oslo))
        #expect(fifth.overlaps(fifthInOslo, in: oslo))
    }

    @Test func anInstantOverlapsARangeThatStartsAtIt() {
        let instant = occurrence(.instant(at("2027-02-05T10:00:00Z")), nil)

        #expect(instant.overlaps(at("2027-02-05T10:00:00Z")..<at("2027-02-06T10:00:00Z"), in: oslo))
        #expect(!instant.overlaps(at("2027-02-04T10:00:00Z")..<at("2027-02-05T10:00:00Z"), in: oslo))
    }

    // MARK: What the server refuses

    @Test func draftsTheServerWouldRefuseAreNotDrafts() {
        let start = CalendarMoment.instant(at("2027-02-05T18:00:00Z"))
        let weekly = EventRecurrence(frequency: .weekly, weekdays: [.friday])

        #expect(EventDraft(title: "  ", start: start) == nil)
        #expect(EventDraft(title: "Game night", start: nil, end: start) == nil)
        #expect(EventDraft(title: "Game night", start: start, end: .day(day(2027, 2, 6))) == nil)
        #expect(EventDraft(title: "Game night", start: nil, recurrence: weekly) == nil)
        #expect(EventDraft(title: "Game night", start: start, recurrence: EventRecurrence(frequency: .weekly, interval: 0)) == nil)
        #expect(EventDraft(title: "Game night", start: start, recurrence: EventRecurrence(frequency: .daily, end: .count(0))) == nil)
        #expect(EventDraft(title: "Game night", start: start, recurrence: EventRecurrence(frequency: .daily, weekdays: [.friday])) == nil)
        #expect(EventDraft(title: "Game night", start: start, recurrence: EventRecurrence(frequency: .weekly, monthDays: [5])) == nil)
        #expect(EventDraft(title: "Game night", start: start, recurrence: EventRecurrence(frequency: .monthly, monthDays: [32])) == nil)
        #expect(EventDraft(title: "Game night", start: start, recurrence: EventRecurrence(frequency: .daily, end: .until(at("2027-02-05T17:59:59Z")))) == nil)
        #expect(EventDraft(title: "Game night", start: .day(day(2027, 2, 5)), recurrence: EventRecurrence(frequency: .daily, end: .until(at("2027-03-05T00:00:00Z")))) == nil)
    }

    @Test func aDraftTrimsWhatWasTyped() throws {
        let draft = try #require(EventDraft(
            title: "  Game night ",
            details: " \n ",
            location: " Rink ",
            start: .instant(at("2027-02-05T18:00:00Z")),
            recurrence: EventRecurrence(frequency: .daily, end: .until(at("2027-02-05T18:00:00Z")))
        ))

        #expect(draft.title == "Game night")
        #expect(draft.details == nil)
        #expect(draft.location == "Rink")
    }

    @Test func anUndatedEventIsADraft() {
        #expect(EventDraft(title: "Spring trip") != nil)
    }
}
