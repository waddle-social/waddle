import Foundation

/// Day separator titles: Today, Yesterday, the weekday within the last
/// week, else the full date (with the year only when it differs).
enum TimelineDayLabel {
    static func title(for day: Date, now: Date = Date(), calendar: Calendar = .current) -> String {
        if calendar.isDate(day, inSameDayAs: now) {
            return "Today"
        }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now),
           calendar.isDate(day, inSameDayAs: yesterday) {
            return "Yesterday"
        }
        let daysAgo = calendar.dateComponents(
            [.day],
            from: calendar.startOfDay(for: day),
            to: calendar.startOfDay(for: now)
        ).day ?? Int.max
        if daysAgo > 1, daysAgo < 7 {
            return format(day, template: "EEEE", calendar: calendar)
        }
        let sameYear = calendar.component(.year, from: day) == calendar.component(.year, from: now)
        return format(day, template: sameYear ? "EEEEMMMMd" : "EEEEMMMMdyyyy", calendar: calendar)
    }

    private static func format(_ date: Date, template: String, calendar: Calendar) -> String {
        let formatter = DateFormatter()
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        formatter.locale = calendar.locale ?? Locale.current
        formatter.setLocalizedDateFormatFromTemplate(template)
        return formatter.string(from: date)
    }
}
