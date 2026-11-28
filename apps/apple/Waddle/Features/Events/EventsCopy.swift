import Foundation
import WaddleKit

/// Words and dates the Events screens share. Dates follow the calendar,
/// time zone and locale the screen is shown with.
struct EventsCopy {
    let calendar: Calendar

    var timeZone: TimeZone { calendar.timeZone }
    private var locale: Locale { calendar.locale ?? .current }

    static func label(for reply: EventReply) -> String {
        switch reply {
        case .going: "Going"
        case .maybe: "Maybe"
        case .declined: "Can't go"
        }
    }

    static func count(_ count: Int, replying reply: EventReply) -> String {
        switch reply {
        case .going: "\(count) going"
        case .maybe: "\(count) maybe"
        case .declined: "\(count) can't go"
        }
    }

    // MARK: Dates

    /// "Friday 5 February".
    func dayTitle(_ day: CalendarDay) -> String {
        day.start(in: timeZone).formatted(style.weekday(.wide).day().month(.wide))
    }

    /// "Today", "Tomorrow", or the day.
    func agendaTitle(_ day: CalendarDay, today: CalendarDay) -> String {
        switch today.distance(to: day) {
        case 0: "Today · \(dayTitle(day))"
        case 1: "Tomorrow · \(dayTitle(day))"
        default: dayTitle(day)
        }
    }

    /// "February 2027".
    func monthTitle(year: Int, month: Int) -> String {
        CalendarDay(year: year, month: month, day: 1).start(in: timeZone).formatted(style.month(.wide).year())
    }

    /// "7:00 PM" or "19:00", depending on the locale.
    func time(_ date: Date) -> String {
        date.formatted(EventTimeLabel.formatStyle(calendar: calendar, locale: locale))
    }

    /// "Friday 5 February 2027".
    func longDay(_ day: CalendarDay) -> String {
        day.start(in: timeZone).formatted(style.weekday(.wide).day().month(.wide).year())
    }

    /// "5 Feb 2027".
    func shortDay(_ day: CalendarDay) -> String {
        day.start(in: timeZone).formatted(style.day().month(.abbreviated).year())
    }

    /// The date line and the time line of an occurrence: "Friday 5 February
    /// 2027" and "19:00 – 22:00", "19:00" or "All day".
    func when(_ occurrence: EventOccurrence) -> (date: String, time: String) {
        let days = occurrence.days(in: timeZone)
        let date = days.count == 1
            ? longDay(days.lowerBound)
            : "\(shortDay(days.lowerBound)) – \(shortDay(days.upperBound))"
        guard case let .instant(start) = occurrence.start else { return (date, "All day") }
        guard case let .instant(end)? = occurrence.end else { return (date, time(start)) }
        return (date, "\(time(start)) – \(time(end))")
    }

    /// The time an agenda row shows on `day`.
    func rowTimes(_ occurrence: EventOccurrence, on day: CalendarDay) -> (start: String, end: String?) {
        guard case let .instant(start) = occurrence.start else { return ("All day", nil) }
        let begins = CalendarDay(start, in: timeZone) == day ? time(start) : "Earlier"
        guard case let .instant(end)? = occurrence.end else { return (begins, nil) }
        return (begins, CalendarDay(end, in: timeZone) == day ? time(end) : "Later")
    }

    // MARK: Repeats

    /// "Every 2 weeks on Monday and Friday, 10 times".
    func repeatSummary(_ rule: EventRecurrence, startingAt start: CalendarMoment) -> String {
        var summary = cadence(rule)
        switch rule.frequency {
        case .weekly:
            let days = orderedWeekdays(rule.localWeekdays(startingAt: start, in: timeZone)).map(weekdayName)
            summary += " on \(list(days))"
        case .monthly where !rule.monthDays.isEmpty:
            summary += " on the \(list(rule.monthDays.map(monthDayName)))"
        default:
            break
        }
        switch rule.end {
        case .never: break
        case let .count(count): summary += count == 1 ? ", once" : ", \(count) times"
        case let .until(last): summary += ", until \(shortDay(CalendarDay(last, in: timeZone)))"
        }
        return summary
    }

    private func cadence(_ rule: EventRecurrence) -> String {
        let unit = switch rule.frequency {
        case .daily: ("day", "days")
        case .weekly: ("week", "weeks")
        case .monthly: ("month", "months")
        case .yearly: ("year", "years")
        }
        return rule.interval <= 1 ? "Every \(unit.0)" : "Every \(rule.interval) \(unit.1)"
    }

    /// "week" or "weeks", after "Every N" in the editor.
    static func unit(of repeats: EventForm.Repeat, interval: Int) -> String {
        let one = interval == 1
        return switch repeats {
        case .never: ""
        case .daily: one ? "day" : "days"
        case .weekly: one ? "week" : "weeks"
        case .monthly: one ? "month" : "months"
        case .yearly: one ? "year" : "years"
        }
    }

    /// The week as the person's calendar starts it.
    func orderedWeekdays(_ days: some Collection<Weekday>) -> [Weekday] {
        let first = calendar.firstWeekday
        return days.sorted { ($0.rawValue - first + 7) % 7 < ($1.rawValue - first + 7) % 7 }
    }

    var week: [Weekday] {
        orderedWeekdays(Weekday.allCases)
    }

    func weekdayName(_ day: Weekday) -> String {
        calendar.standaloneWeekdaySymbols[day.rawValue - 1]
    }

    /// "Mon", for the month grid's header.
    func shortWeekdayName(_ day: Weekday) -> String {
        calendar.shortStandaloneWeekdaySymbols[day.rawValue - 1]
    }

    /// "M", for the editor's weekday chips.
    func initial(of day: Weekday) -> String {
        calendar.veryShortStandaloneWeekdaySymbols[day.rawValue - 1]
    }

    private func monthDayName(_ day: Int) -> String {
        if day == -1 { return "last day" }
        if day < 0 { return "\(ordinal(-day)) last day" }
        return "\(ordinal(day))"
    }

    private func ordinal(_ number: Int) -> String {
        let suffix = switch (number % 10, number % 100) {
        case (_, 11...13): "th"
        case (1, _): "st"
        case (2, _): "nd"
        case (3, _): "rd"
        default: "th"
        }
        return "\(number)\(suffix)"
    }

    private func list(_ items: [String]) -> String {
        items.formatted(.list(type: .and).locale(locale))
    }

    private var style: Date.FormatStyle {
        Date.FormatStyle(locale: locale, calendar: calendar, timeZone: timeZone)
    }
}
