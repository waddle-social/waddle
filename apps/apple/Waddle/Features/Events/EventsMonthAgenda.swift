import SwiftUI
import WaddleKit

/// The month grid and the four weeks of agenda from its selected day.
struct EventsMonthAgenda<Placeholder: View>: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.eventsCalendar) private var calendar
    @Binding var month: EventsMonth?
    @Binding var selection: CalendarDay?
    let filter: EventFilter
    let today: CalendarDay
    let now: Date
    let isWide: Bool
    let onOpen: (OpenEvent) -> Void
    @ViewBuilder let placeholder: Placeholder

    var body: some View {
        let timeZone = calendar.timeZone
        let selected = selection ?? today
        let shown = month ?? EventsMonth(containing: selected)
        let grid = shown.days(firstWeekday: calendar.firstWeekday)
        let agendaEnd = selected.advanced(by: 28)
        let from = min(grid.lowerBound, selected).start(in: timeZone)
        let to = max(grid.upperBound.advanced(by: 1), agendaEnd).start(in: timeZone)
        let occurrences = session.eventOccurrences(in: from..<to, timeZone: timeZone, filter: filter)
        let monthCard = EventsMonthCard(
            month: shown,
            days: grid,
            today: today,
            selection: selected,
            occurrences: occurrences,
            onSelect: select,
            onMove: { month = shown.advanced(by: $0) },
            onToday: { select(today) }
        )
        let agenda = EventsAgenda(
            occurrences: occurrences.filter {
                let days = $0.days(in: timeZone)
                return days.upperBound >= selected && days.lowerBound < agendaEnd
            },
            from: selected,
            today: today,
            now: now,
            undated: filter.apply(to: session.events.undated, for: session.account.jid),
            filter: filter,
            onOpen: onOpen
        ) {
            placeholder
        }
        if isWide {
            HStack(alignment: .top, spacing: Theme.Spacing.l) {
                monthCard.frame(width: 340)
                agenda.frame(maxWidth: .infinity, alignment: .topLeading)
            }
        } else {
            VStack(alignment: .leading, spacing: Theme.Spacing.xl) {
                monthCard.frame(maxWidth: 480)
                agenda
            }
        }
    }

    private func select(_ day: CalendarDay) {
        selection = day
        month = EventsMonth(containing: day)
    }
}

/// A month of the Gregorian calendar.
struct EventsMonth: Hashable {
    let year: Int
    let month: Int

    init(year: Int, month: Int) {
        self.year = year
        self.month = month
    }

    init(containing day: CalendarDay) {
        self.init(year: day.year, month: day.month)
    }

    var first: CalendarDay { CalendarDay(year: year, month: month, day: 1) }

    func advanced(by months: Int) -> EventsMonth {
        let index = year * 12 + (month - 1) + months
        return EventsMonth(year: index / 12, month: index % 12 + 1)
    }

    /// The weeks that hold this month's days, from the calendar's first
    /// weekday: whole rows, so they begin and end in the months beside it.
    func days(firstWeekday: Int) -> ClosedRange<CalendarDay> {
        let leading = (first.weekday.rawValue - firstWeekday + 7) % 7
        let length = first.distance(to: advanced(by: 1).first)
        let weeks = (leading + length + 6) / 7
        let start = first.advanced(by: -leading)
        return start...start.advanced(by: weeks * 7 - 1)
    }
}
