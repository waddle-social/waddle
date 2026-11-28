import Foundation

/// The rolling seven local dates beginning today. Occurrences are supplied by
/// the same calendar query as the month view, after the core expands series.
public struct EventsWeek: Sendable {
    public struct Section: Sendable {
        public let day: CalendarDay
        public let heading: String
        public let entries: [Entry]
    }

    public struct Entry: Sendable {
        public let occurrence: EventOccurrence
        public let timeLabel: String
        public let status: EventStatus?
    }

    /// The occurrence query can include late starts from yesterday that
    /// still fall within the one-hour window.
    public let range: Range<Date>
    private let days: ClosedRange<CalendarDay>
    private let now: Date
    private let timeZone: TimeZone

    public init(now: Date, timeZone: TimeZone) {
        let today = CalendarDay(now, in: timeZone)
        days = today...today.advanced(by: 6)
        let earliestOngoingStart = now.addingTimeInterval(-EventOccurrence.missingTimedEndDuration)
        let lowerBound = min(today.start(in: timeZone), earliestOngoingStart)
        range = lowerBound..<today.advanced(by: 7).start(in: timeZone)
        self.now = now
        self.timeZone = timeZone
    }

    public static func emptyCopy(for filter: EventFilter, canManage: Bool) -> (title: String, message: String) {
        if filter == .goingOrMaybe {
            return ("Nothing you're going to this week.", "Say yes to something, or schedule one yourself.")
        }
        return (
            "Nothing scheduled this week.",
            canManage ? "Schedule one to get the community together." : "Check back later."
        )
    }

    /// Future or ongoing occurrences, once on each local day they run on.
    /// Empty days are skipped.
    public func sections(for occurrences: [EventOccurrence], locale: Locale) -> [Section] {
        let remaining = now..<range.upperBound
        var byDay: [CalendarDay: [EventOccurrence]] = [:]
        for occurrence in occurrences {
            let end = occurrence.displayEnd
            guard occurrence.overlaps(remaining, in: timeZone, endingAt: end) else { continue }
            let span = occurrence.days(in: timeZone, endingAt: end)
            let first = max(span.lowerBound, days.lowerBound)
            let last = min(span.upperBound, days.upperBound)
            guard first <= last else { continue }
            for day in first...last {
                byDay[day, default: []].append(occurrence)
            }
        }

        let calendar = CalendarDay.gregorian(timeZone)
        let headingStyle = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: timeZone)
            .weekday(.wide).month(.abbreviated).day()
        let time = EventTimeLabel.formatStyle(calendar: calendar, locale: locale)

        return byDay.sorted { $0.key < $1.key }.map { day, occurrences in
            let midnight = day.start(in: timeZone)
            let entries = occurrences.sorted {
                comesBefore($0, $1, on: midnight, locale: locale)
            }.map { occurrence in
                let label = timeLabel(for: occurrence, on: day, using: time)
                let status = occurrence.status(at: now)
                return Entry(
                    occurrence: occurrence,
                    timeLabel: status.map { "\($0.label) · \(label)" } ?? label,
                    status: status
                )
            }
            return Section(day: day, heading: midnight.formatted(headingStyle), entries: entries)
        }
    }

    private func comesBefore(_ lhs: EventOccurrence, _ rhs: EventOccurrence, on dayStart: Date, locale: Locale) -> Bool {
        if lhs.isAllDay != rhs.isAllDay { return lhs.isAllDay }
        let left = orderingTimes(lhs, on: dayStart)
        let right = orderingTimes(rhs, on: dayStart)
        if left != right { return left < right }
        let titleOrder = lhs.title.compare(rhs.title, locale: locale)
        if titleOrder != .orderedSame { return titleOrder == .orderedAscending }
        return lhs.id < rhs.id
    }

    private func orderingTimes(_ occurrence: EventOccurrence, on dayStart: Date) -> (Date, Date) {
        let start = occurrence.start.date(in: timeZone)
        let end: Date
        if let finish = occurrence.displayEnd?.date(in: timeZone), finish > start {
            end = finish
        } else if case let .day(day) = occurrence.start {
            end = day.advanced(by: 1).start(in: timeZone)
        } else {
            end = start.addingTimeInterval(0.001)
        }
        return (max(start, dayStart), end)
    }

    private func timeLabel(for occurrence: EventOccurrence, on day: CalendarDay, using time: Date.FormatStyle) -> String {
        guard case let .instant(start) = occurrence.start else { return "All day" }
        if CalendarDay(start, in: timeZone) == day { return start.formatted(time) }
        if case let .instant(end)? = occurrence.end, CalendarDay(end, in: timeZone) == day {
            return "continues until \(end.formatted(time))"
        }
        return "continues"
    }
}
