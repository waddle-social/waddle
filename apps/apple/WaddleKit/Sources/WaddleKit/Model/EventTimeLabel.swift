import Foundation

/// The clock format shared by calendar cards and event details.
public enum EventTimeLabel {
    public static func formatStyle(calendar: Calendar, locale: Locale) -> Date.FormatStyle {
        Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
            .hour().minute()
    }
}
