import SwiftUI
import WaddleKit

/// The community's Events: a month to pick a day in, and what is planned
/// from that day on.
struct EventsView: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.eventsCalendar) private var calendar
    @State private var selection: CalendarDay?
    @State private var month: EventsMonth?
    @State private var openEvent: OpenEvent?
    @State private var isAddingEvent = false
    /// Room for the month and the agenda side by side.
    @State private var isWide = false
    /// A fixed clock, for snapshots; the app follows the real one.
    var clock: Date?

    init(clock: Date? = nil, selecting day: CalendarDay? = nil) {
        self.clock = clock
        _selection = State(initialValue: day)
    }

    /// How far the agenda looks ahead of the selected day.
    static let agendaDays = 28

    var body: some View {
        TimelineView(.everyMinute) { timeline in
            content(at: clock ?? timeline.date)
        }
        .background(Theme.Colors.bg)
        .navigationTitle("Calendar")
        .refreshable { await session.refreshEvents() }
        .task(id: session.connection == .online) {
            guard clock == nil, session.connection == .online, isStale else { return }
            await session.refreshEvents()
        }
        .toolbar {
            if session.events.canManage {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        isAddingEvent = true
                    } label: {
                        Label("New event", systemImage: "plus")
                    }
                    .help("New event")
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await session.refreshEvents() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(session.events.isLoading)
                .help("Refresh")
            }
        }
        .sheet(item: $openEvent) { opened in
            EventDetailSheet(opened: opened)
                .carryingCallDock()
        }
        .sheet(isPresented: $isAddingEvent) {
            EventEditorSheet(form: EventForm(on: selectedDay(today: today), now: now, timeZone: calendar.timeZone))
                .carryingCallDock()
        }
    }

    private var now: Date { clock ?? Date() }
    private var today: CalendarDay { CalendarDay(now, in: calendar.timeZone) }

    private func selectedDay(today: CalendarDay) -> CalendarDay {
        selection ?? today
    }

    private func content(at now: Date) -> some View {
        let timeZone = calendar.timeZone
        let today = CalendarDay(now, in: timeZone)
        let selected = selectedDay(today: today)
        let shown = month ?? EventsMonth(containing: selected)
        let grid = shown.days(firstWeekday: calendar.firstWeekday)
        let agendaEnd = selected.advanced(by: Self.agendaDays)
        let from = min(grid.lowerBound, selected).start(in: timeZone)
        let to = max(grid.upperBound.advanced(by: 1), agendaEnd).start(in: timeZone)
        let occurrences = session.eventOccurrences(in: from..<to, timeZone: timeZone)
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
            occurrences: occurrences.filter { $0.days(in: timeZone).upperBound >= selected && $0.days(in: timeZone).lowerBound < agendaEnd },
            from: selected,
            today: today,
            now: now,
            undated: session.events.undated,
            onOpen: { openEvent = $0 }
        ) {
            placeholder
        }
        return ScrollView {
            VStack(alignment: .leading, spacing: Theme.Spacing.l) {
                if session.events.failed {
                    notice("Events didn't load. Refresh to try again.")
                }
                if isWide {
                    HStack(alignment: .top, spacing: Theme.Spacing.l) {
                        monthCard.frame(width: Self.monthWidth)
                        agenda.frame(maxWidth: .infinity, alignment: .topLeading)
                    }
                } else {
                    VStack(alignment: .leading, spacing: Theme.Spacing.xl) {
                        monthCard.frame(maxWidth: Self.compactMonthWidth)
                        agenda
                    }
                }
            }
            .padding(Theme.Spacing.l)
            .frame(maxWidth: Self.maxWidth)
            .frame(maxWidth: .infinity)
        }
        .onGeometryChange(for: Bool.self) { $0.size.width >= Self.wideWidth } action: { isWide = $0 }
    }

    private func select(_ day: CalendarDay) {
        selection = day
        month = EventsMonth(containing: day)
    }

    /// Coming back within a minute shows what is there; after that, or
    /// after a reconnect that cleared it, it reloads.
    private var isStale: Bool {
        session.events.loadedAt.map { Date().timeIntervalSince($0) > 60 } ?? true
    }

    private static var monthWidth: CGFloat { 340 }
    private static var compactMonthWidth: CGFloat { 480 }
    /// The month, the gap and an agenda at least 320pt wide, with margins.
    private static var wideWidth: CGFloat { 720 }
    private static var maxWidth: CGFloat { 1040 }

    private func notice(_ message: String) -> some View {
        Label(message, systemImage: "exclamationmark.triangle")
            .huddleText(Theme.Typography.caption)
            .foregroundStyle(Theme.Colors.warningText)
    }

    /// Shown in place of the agenda while there are no events to list.
    @ViewBuilder
    private var placeholder: some View {
        let events = session.events
        if events.events.isEmpty, !events.failed {
            if events.hasLoaded {
                EmptyStateView(
                    title: "No events yet",
                    message: events.canManage
                        ? "Plan the first one for your community with New event."
                        : "When something is planned for your community, it shows up here.",
                    symbol: "calendar"
                )
            } else if session.connection == .online {
                ProgressView()
                    .frame(maxWidth: .infinity)
                    .padding(Theme.Spacing.xl)
            } else {
                EmptyStateView(
                    title: "You're offline",
                    message: "Events load when you're connected again.",
                    symbol: "wifi.slash"
                )
            }
        }
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

extension EnvironmentValues {
    /// The Gregorian calendar the Events screens draw dates in, with the
    /// person's first weekday, time zone and locale.
    var eventsCalendar: Calendar {
        var gregorian = Calendar(identifier: .gregorian)
        gregorian.firstWeekday = calendar.firstWeekday
        gregorian.minimumDaysInFirstWeek = calendar.minimumDaysInFirstWeek
        gregorian.timeZone = timeZone
        gregorian.locale = locale
        return gregorian
    }
}
