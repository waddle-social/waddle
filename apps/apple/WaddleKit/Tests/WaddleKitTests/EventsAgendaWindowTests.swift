import Foundation
import Testing
@testable import WaddleKit

struct EventsAgendaWindowTests {
    private let oslo = TimeZone(identifier: "Europe/Oslo")!
    private let selected = CalendarDay(year: 2027, month: 3, day: 27)

    @Test func queryCoversTheGridAndFourLocalWeeksAcrossAClockChange() {
        let agenda = EventsAgendaWindow(startingAt: selected, timeZone: oslo)
        let march = EventsMonth(containing: selected)
        let april = march.advanced(by: 1)

        #expect(agenda.range(including: march.days(firstWeekday: Weekday.monday.rawValue))
            == at("2027-03-01T00:00:00+01:00")..<at("2027-04-24T00:00:00+02:00"))
        #expect(agenda.range(including: april.days(firstWeekday: Weekday.monday.rawValue))
            == at("2027-03-27T00:00:00+01:00")..<at("2027-05-03T00:00:00+02:00"))
    }

    @Test func agendaPlacesEachOverlapOnceUnderItsFirstVisibleDay() {
        let agenda = EventsAgendaWindow(startingAt: selected, timeZone: oslo)
        let endedAtMidnight = timed("ended", "2027-03-26T20:00:00+01:00", "2027-03-27T00:00:00+01:00")
        let continued = timed("continued", "2027-03-26T22:00:00+01:00", "2027-03-28T03:30:00+02:00")
        let onSelectedDay = timed("selected", "2027-03-27T10:00:00+01:00")
        let last = timed("last", "2027-04-23T23:59:00+02:00")
        let outside = timed("outside", "2027-04-24T00:00:00+02:00")

        let sections = agenda.sections(for: [endedAtMidnight, continued, onSelectedDay, last, outside])

        #expect(sections.map(\.day.iso) == ["2027-03-27", "2027-04-23"])
        #expect(sections.map { $0.occurrences.map(\.eventID) } == [["continued", "selected"], ["last"]])
    }

    @Test func anAllDayEndDateDoesNotReachTheAgendaStart() {
        let agenda = EventsAgendaWindow(startingAt: selected, timeZone: oslo)
        let yesterday = CalendarDay(year: 2027, month: 3, day: 26)
        let ended = EventOccurrence(eventID: "ended", recurrenceStart: nil, start: .day(yesterday),
                                    end: .day(selected), title: "Ended")
        let continues = EventOccurrence(eventID: "continues", recurrenceStart: nil, start: .day(yesterday),
                                        end: .day(CalendarDay(year: 2027, month: 3, day: 28)), title: "Continues")

        let sections = agenda.sections(for: [ended, continues])

        #expect(sections.map(\.day) == [selected])
        #expect(sections.flatMap(\.occurrences).map(\.eventID) == ["continues"])
    }

    private func timed(_ id: String, _ start: String, _ end: String? = nil) -> EventOccurrence {
        EventOccurrence(eventID: id, recurrenceStart: nil, start: .instant(at(start)),
                        end: end.map { .instant(at($0)) }, title: id)
    }

    private func at(_ iso: String) -> Date {
        ISO8601DateFormatter().date(from: iso)!
    }
}
