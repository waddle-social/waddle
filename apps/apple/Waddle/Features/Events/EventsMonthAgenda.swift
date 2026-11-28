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
        let window = EventsAgendaWindow(startingAt: selected, timeZone: timeZone)
        let occurrences = session.eventOccurrences(in: window.range(including: grid), timeZone: timeZone, filter: filter)
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
            occurrences: occurrences,
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
