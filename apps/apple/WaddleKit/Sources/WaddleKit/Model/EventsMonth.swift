/// A month of the Gregorian calendar.
public struct EventsMonth: Hashable, Sendable {
    public let year: Int
    public let month: Int

    private init(year: Int, month: Int) {
        self.year = year
        self.month = month
    }

    public init(containing day: CalendarDay) {
        self.init(year: day.year, month: day.month)
    }

    private var first: CalendarDay { CalendarDay(year: year, month: month, day: 1) }

    public func advanced(by months: Int) -> EventsMonth {
        let index = year * 12 + (month - 1) + months
        return EventsMonth(year: index / 12, month: index % 12 + 1)
    }

    /// Whole grid rows, including the days in neighboring months.
    public func days(firstWeekday: Int) -> ClosedRange<CalendarDay> {
        let leading = (first.weekday.rawValue - firstWeekday + 7) % 7
        let length = first.distance(to: advanced(by: 1).first)
        let weeks = (leading + length + 6) / 7
        let start = first.advanced(by: -leading)
        return start...start.advanced(by: weeks * 7 - 1)
    }
}
