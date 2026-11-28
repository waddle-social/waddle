import Foundation
import Testing
@testable import WaddleKit

struct EventsWeekTests {
    private let oslo = TimeZone(identifier: "Europe/Oslo")!
    private let locale = Locale(identifier: "en_GB")

    @Test func theSevenDaysStartTodayAndCrossTheClockChange() {
        let week = EventsWeek(now: at("2027-03-27T10:00:00+01:00"), timeZone: oslo)
        let last = timed("last day", "2027-04-02T23:59:00+02:00")
        let outside = timed("eighth day", "2027-04-03T00:00:00+02:00")

        #expect(week.range == at("2027-03-27T00:00:00+01:00")..<at("2027-04-03T00:00:00+02:00"))
        let sections = week.sections(for: [outside, last], locale: locale)
        #expect(sections.map(\.day.iso) == ["2027-04-02"])
        #expect(sections.flatMap(\.entries).map(\.occurrence.eventID) == ["last day"])
    }

    @Test func upcomingAndOngoingEventsAppearOnEveryDayTheyOverlap() {
        let week = EventsWeek(now: at("2027-03-27T10:00:00+01:00"), timeZone: oslo)
        let overnight = timed("overnight", "2027-03-26T22:00:00+01:00", "2027-03-28T03:30:00+02:00")
        let ended = timed("ended", "2027-03-27T09:00:00+01:00", "2027-03-27T10:00:00+01:00")
        let midnight = timed("midnight", "2027-03-27T20:00:00+01:00", "2027-03-28T00:00:00+01:00")
        let offsite = allDay("offsite", from: "2027-03-26", to: "2027-03-29")

        let sections = week.sections(for: [midnight, ended, overnight, offsite], locale: locale)

        #expect(sections.map(\.day.iso) == ["2027-03-27", "2027-03-28"])
        #expect(sections.map { $0.entries.map(\.occurrence.eventID) } == [
            ["offsite", "overnight", "midnight"], ["offsite", "overnight"],
        ])
    }

    @Test func eventsStartingBeforeTheDaySortByEndThenTitle() {
        let week = EventsWeek(now: at("2027-03-27T04:00:00+01:00"), timeZone: oslo)
        let laterEnd = timed("D", "2027-03-26T19:00:00+01:00", "2027-03-27T12:00:00+01:00")
        let earlierEnd = timed("B", "2027-03-26T21:00:00+01:00", "2027-03-27T10:00:00+01:00")
        let sameEnd = timed("A", "2027-03-26T22:00:00+01:00", "2027-03-27T10:00:00+01:00")
        let startsAfterMidnight = timed("C", "2027-03-27T03:00:00+01:00", "2027-03-27T08:00:00+01:00")

        let sections = week.sections(for: [laterEnd, earlierEnd, sameEnd, startsAfterMidnight], locale: locale)

        #expect(sections.flatMap(\.entries).map(\.occurrence.title) == ["A", "B", "D", "C"])
    }

    @Test func aTimedEventWithNoEndIsListedForItsOneHourLiveWindow() {
        let start = at("2027-03-27T10:00:00+01:00")
        let event = timed("check-in", "2027-03-27T10:00:00+01:00")
        let ongoing = EventsWeek(now: start.addingTimeInterval(3599), timeZone: oslo)
        let finished = EventsWeek(now: start.addingTimeInterval(3600), timeZone: oslo)

        #expect(ongoing.sections(for: [event], locale: locale).first?.entries.first?.status == .happeningNow)
        #expect(finished.sections(for: [event], locale: locale).isEmpty)
    }

    @Test func anEventWithNoEndCanCarryOverFromYesterday() {
        let week = EventsWeek(now: at("2027-03-27T00:15:00+01:00"), timeZone: oslo)
        let event = timed("check-in", "2027-03-26T23:30:00+01:00")
        let queried = [event].filter { $0.overlaps(week.range, in: oslo) }

        let sections = week.sections(for: queried, locale: locale)

        #expect(sections.map(\.day.iso) == ["2027-03-27"])
        #expect(sections.first?.entries.first?.timeLabel == "Happening now · continues")
    }

    @Test func dayHeadingsAndCardTimesDescribeWhatIsVisibleOnThatDay() {
        let week = EventsWeek(now: at("2027-03-27T10:00:00+01:00"), timeZone: oslo)
        let overnight = timed("overnight", "2027-03-26T22:00:00+01:00", "2027-03-28T03:30:00+02:00")
        let offsite = allDay("offsite", from: "2027-03-27")
        let soon = timed("soon", "2027-03-27T11:00:00+01:00")

        let sections = week.sections(for: [overnight, offsite, soon], locale: locale)

        #expect(sections.map(\.heading) == ["Saturday 27 Mar", "Sunday 28 Mar"])
        #expect(sections.map { $0.entries.map(\.timeLabel) } == [
            ["All day", "Happening now · continues", "Starting soon · 11:00"],
            ["Happening now · continues until 03:30"],
        ])
    }

    @Test func aTwelveHourLocaleUsesAMAndPMOnCards() {
        let week = EventsWeek(now: at("2027-03-27T10:00:00+01:00"), timeZone: oslo)
        let overnight = timed("overnight", "2027-03-27T14:00:00+01:00", "2027-03-28T03:30:00+02:00")

        let labels = week.sections(for: [overnight], locale: Locale(identifier: "en_US"))
            .flatMap(\.entries).map { $0.timeLabel.split(whereSeparator: \.isWhitespace).joined(separator: " ") }

        #expect(labels == ["2:00 PM", "continues until 3:30 AM"])
    }

    @Test func theEmptyWeekCopyExplainsTheFilterOrWhatToDoNext() {
        let filtered = EventsWeek.emptyCopy(for: .goingOrMaybe, canManage: false)
        let owner = EventsWeek.emptyCopy(for: .all, canManage: true)
        let member = EventsWeek.emptyCopy(for: .all, canManage: false)

        #expect(filtered.title == "Nothing you're going to this week.")
        #expect(filtered.message == "Say yes to something, or schedule one yourself.")
        #expect(owner.title == "Nothing scheduled this week.")
        #expect(owner.message == "Schedule one to get the community together.")
        #expect(member.title == "Nothing scheduled this week.")
        #expect(member.message == "Check back later.")
    }

    private func timed(_ title: String, _ start: String, _ end: String? = nil) -> EventOccurrence {
        EventOccurrence(eventID: title, recurrenceStart: nil, start: .instant(at(start)),
                        end: end.map { .instant(at($0)) }, title: title)
    }

    private func allDay(_ title: String, from start: String, to end: String? = nil) -> EventOccurrence {
        EventOccurrence(eventID: title, recurrenceStart: nil, start: .day(CalendarDay(iso: start)!),
                        end: end.map { .day(CalendarDay(iso: $0)!) }, title: title)
    }

    private func at(_ iso: String) -> Date {
        ISO8601DateFormatter().date(from: iso)!
    }
}
