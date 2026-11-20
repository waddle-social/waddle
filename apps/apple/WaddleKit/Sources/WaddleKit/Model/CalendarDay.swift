import Foundation

/// A day of the week, numbered as `Calendar` numbers them (Sunday is 1).
public enum Weekday: Int, CaseIterable, Comparable, Hashable, Sendable {
    case sunday = 1, monday, tuesday, wednesday, thursday, friday, saturday

    /// The weekday `days` later (or earlier, when negative).
    public func shifted(by days: Int) -> Weekday {
        Weekday(rawValue: ((rawValue - 1 + days) % 7 + 7) % 7 + 1)!
    }

    public static func < (lhs: Weekday, rhs: Weekday) -> Bool {
        lhs.rawValue < rhs.rawValue
    }
}

/// A date on the Gregorian calendar with no time and no time zone: an
/// all-day event's date, or a day as the person sees it on their clock.
public struct CalendarDay: Hashable, Sendable, Strideable, CustomStringConvertible {
    public let year: Int
    public let month: Int
    public let day: Int

    public init(year: Int, month: Int, day: Int) {
        self.year = year
        self.month = month
        self.day = day
    }

    /// `YYYY-MM-DD`; nil for anything else or a date that does not exist.
    public init?(iso: String) {
        let parts = iso.split(separator: "-", omittingEmptySubsequences: false)
        guard parts.count == 3, parts[0].count == 4, parts[1].count == 2, parts[2].count == 2,
              let year = Int(parts[0]), let month = Int(parts[1]), let day = Int(parts[2])
        else { return nil }
        guard (1...12).contains(month), (1...Self.length(ofMonth: month, in: year)).contains(day) else { return nil }
        self.init(year: year, month: month, day: day)
    }

    private static func length(ofMonth month: Int, in year: Int) -> Int {
        switch month {
        case 2: year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) ? 29 : 28
        case 4, 6, 9, 11: 30
        default: 31
        }
    }

    /// The day `date` falls on in `timeZone`.
    public init(_ date: Date, in timeZone: TimeZone) {
        let parts = Self.gregorian(timeZone).dateComponents([.year, .month, .day], from: date)
        self.init(year: parts.year!, month: parts.month!, day: parts.day!)
    }

    public var iso: String {
        String(format: "%04d-%02d-%02d", year, month, day)
    }

    public var description: String { iso }

    /// The first moment of this day in `timeZone`: midnight, or the first
    /// hour after it on a day a clock change skips midnight.
    public func start(in timeZone: TimeZone) -> Date {
        let calendar = Self.gregorian(timeZone)
        let noon = calendar.date(from: DateComponents(year: year, month: month, day: day, hour: 12))!
        return calendar.startOfDay(for: noon)
    }

    public var weekday: Weekday {
        // 1970-01-01 was a Thursday.
        Weekday.thursday.shifted(by: ordinal)
    }

    // MARK: Strideable, in days

    public func distance(to other: CalendarDay) -> Int {
        other.ordinal - ordinal
    }

    public func advanced(by days: Int) -> CalendarDay {
        CalendarDay(ordinal: ordinal + days)
    }

    /// Days since 1970-01-01, by the civil-calendar arithmetic of Howard
    /// Hinnant's `days_from_civil`, so no time zone or clock change enters.
    private var ordinal: Int {
        let shiftedYear = month <= 2 ? year - 1 : year
        let era = (shiftedYear >= 0 ? shiftedYear : shiftedYear - 399) / 400
        let yearOfEra = shiftedYear - era * 400
        let dayOfYear = (153 * (month + (month > 2 ? -3 : 9)) + 2) / 5 + day - 1
        let dayOfEra = yearOfEra * 365 + yearOfEra / 4 - yearOfEra / 100 + dayOfYear
        return era * 146_097 + dayOfEra - 719_468
    }

    private init(ordinal: Int) {
        let shifted = ordinal + 719_468
        let era = (shifted >= 0 ? shifted : shifted - 146_096) / 146_097
        let dayOfEra = shifted - era * 146_097
        let yearOfEra = (dayOfEra - dayOfEra / 1460 + dayOfEra / 36524 - dayOfEra / 146_096) / 365
        let dayOfYear = dayOfEra - (365 * yearOfEra + yearOfEra / 4 - yearOfEra / 100)
        let monthIndex = (5 * dayOfYear + 2) / 153
        let day = dayOfYear - (153 * monthIndex + 2) / 5 + 1
        let month = monthIndex < 10 ? monthIndex + 3 : monthIndex - 9
        let year = yearOfEra + era * 400 + (month <= 2 ? 1 : 0)
        self.init(year: year, month: month, day: day)
    }

    static func gregorian(_ timeZone: TimeZone) -> Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        return calendar
    }
}

/// When an event starts or ends: a whole day, or an instant.
public enum CalendarMoment: Hashable, Sendable {
    /// All day. An end on this kind is exclusive, as in iCalendar.
    case day(CalendarDay)
    case instant(Date)

    public var isAllDay: Bool {
        if case .day = self { true } else { false }
    }

    /// The first moment of it in `timeZone`.
    public func date(in timeZone: TimeZone) -> Date {
        switch self {
        case let .day(day): day.start(in: timeZone)
        case let .instant(date): date
        }
    }

    /// The day it falls on in `timeZone`; an all-day date is that date
    /// everywhere.
    public func day(in timeZone: TimeZone) -> CalendarDay {
        switch self {
        case let .day(day): day
        case let .instant(date): CalendarDay(date, in: timeZone)
        }
    }

    /// Whether this is later than `other` of the same kind; false across kinds.
    func isAfter(_ other: CalendarMoment) -> Bool {
        switch (self, other) {
        case let (.day(this), .day(that)): this > that
        case let (.instant(this), .instant(that)): this > that
        default: false
        }
    }

    /// How many days the day in `timeZone` is ahead of the day in UTC:
    /// what moves a weekday between the two. Zero for an all-day date.
    func dayOffset(in timeZone: TimeZone) -> Int {
        guard case let .instant(date) = self else { return 0 }
        return CalendarDay(date, in: TimeZone(identifier: "UTC")!).distance(to: CalendarDay(date, in: timeZone))
    }
}
