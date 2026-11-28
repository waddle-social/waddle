import Foundation

/// The four weeks of agenda beginning on the selected local day.
public struct EventsAgendaWindow: Sendable {
    public struct Section: Sendable {
        public let day: CalendarDay
        public let occurrences: [EventOccurrence]
    }

    private let days: Range<CalendarDay>
    private let timeZone: TimeZone

    public init(startingAt day: CalendarDay, timeZone: TimeZone) {
        days = day..<day.advanced(by: 28)
        self.timeZone = timeZone
    }

    /// A single query for both the displayed grid and the selected agenda.
    public func range(including grid: ClosedRange<CalendarDay>) -> Range<Date> {
        let first = min(grid.lowerBound, days.lowerBound)
        let end = max(grid.upperBound.advanced(by: 1), days.upperBound)
        return first.start(in: timeZone)..<end.start(in: timeZone)
    }

    /// Each overlapping occurrence appears once, on its first day in the
    /// agenda. Occurrences on the same day keep their query order.
    public func sections(for occurrences: [EventOccurrence]) -> [Section] {
        var byDay: [CalendarDay: [EventOccurrence]] = [:]
        for occurrence in occurrences {
            let span = occurrence.days(in: timeZone)
            guard span.upperBound >= days.lowerBound, span.lowerBound < days.upperBound else { continue }
            let day = max(span.lowerBound, days.lowerBound)
            byDay[day, default: []].append(occurrence)
        }
        return byDay.sorted { $0.key < $1.key }.map { Section(day: $0.key, occurrences: $0.value) }
    }
}
