import Foundation
import Testing
import WaddleKit

struct EventStatusTests {
    private let start = Date(timeIntervalSince1970: 1_800_000_000)

    @Test func theTwoHourWindowIncludesItsFirstMoment() {
        let event = timed(endingAt: .instant(start.addingTimeInterval(90 * 60)))

        #expect(event.status(at: start.addingTimeInterval(-7201)) == nil)
        #expect(event.status(at: start.addingTimeInterval(-7200))?.label == "Starting soon")
        #expect(event.status(at: start.addingTimeInterval(-1)) == .startingSoon)
    }

    @Test func aTimedEventIsHappeningFromItsStartUntilItsEnd() {
        let end = start.addingTimeInterval(90 * 60)
        let event = timed(endingAt: .instant(end))

        #expect(event.status(at: start)?.label == "Happening now")
        #expect(event.status(at: end.addingTimeInterval(-1)) == .happeningNow)
        #expect(event.status(at: end) == nil)
    }

    @Test(arguments: [nil, .day(CalendarDay(year: 2027, month: 2, day: 1))] as [CalendarMoment?])
    func aTimedEventWithoutATimedEndUsesOneHour(end: CalendarMoment?) {
        let event = timed(endingAt: end)

        #expect(event.status(at: start.addingTimeInterval(-7200)) == .startingSoon)
        #expect(event.status(at: start.addingTimeInterval(3599)) == .happeningNow)
        #expect(event.status(at: start.addingTimeInterval(3600)) == nil)
    }

    @Test func anAllDayEventHasNoStatusLabel() {
        let day = CalendarDay(year: 2027, month: 2, day: 1)
        let event = EventOccurrence(
            eventID: "offsite", recurrenceStart: nil,
            start: .day(day), end: .day(day.advanced(by: 1)), title: "Offsite"
        )

        #expect(event.status(at: day.start(in: .gmt)) == nil)
    }

    private func timed(endingAt end: CalendarMoment?) -> EventOccurrence {
        EventOccurrence(eventID: "game-night", recurrenceStart: nil, start: .instant(start), end: end, title: "Game night")
    }
}
