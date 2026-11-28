import SwiftUI
import WaddleKit

/// The community calendar, with a rolling seven-day list or a month and its agenda.
struct EventsView: View {
    enum Mode: Hashable {
        case week, month
    }

    @Environment(SessionCoordinator.self) private var session
    @Environment(\.eventsCalendar) private var calendar
    @State private var selection: CalendarDay?
    @State private var filter: EventFilter
    @State private var mode: Mode
    @State private var month: EventsMonth?
    @State private var openEvent: OpenEvent?
    @State private var isAddingEvent = false
    /// Room for the month and the agenda side by side.
    @State private var isWide = false
    /// A fixed clock, for snapshots; the app follows the real one.
    var clock: Date?

    init(
        clock: Date? = nil,
        selecting day: CalendarDay? = nil,
        filtering filter: EventFilter = .all,
        showing mode: Mode = .month
    ) {
        self.clock = clock
        _selection = State(initialValue: day)
        _filter = State(initialValue: filter)
        _mode = State(initialValue: mode)
    }

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
            let day = mode == .week ? today : selection ?? today
            EventEditorSheet(form: EventForm(on: day, now: now, timeZone: calendar.timeZone))
                .carryingCallDock()
        }
    }

    private var now: Date { clock ?? Date() }
    private var today: CalendarDay { CalendarDay(now, in: calendar.timeZone) }

    private func content(at now: Date) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.Spacing.l) {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Theme.Spacing.s) {
                        modeControl
                        EventsFilterChip(filter: $filter)
                    }
                    VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                        modeControl
                        EventsFilterChip(filter: $filter)
                    }
                }
                if session.events.failed {
                    notice("Events didn't load. Refresh to try again.")
                }
                switch mode {
                case .week:
                    EventsWeekView(now: now, filter: filter, onOpen: { openEvent = $0 }, onAdd: { isAddingEvent = true }) {
                        initialState
                    }
                case .month:
                    EventsMonthAgenda(
                        month: $month, selection: $selection, filter: filter,
                        today: CalendarDay(now, in: calendar.timeZone), now: now, isWide: isWide,
                        onOpen: { openEvent = $0 }
                    ) {
                        monthPlaceholder
                    }
                }
            }
            .padding(Theme.Spacing.l)
            .frame(maxWidth: Self.maxWidth)
            .frame(maxWidth: .infinity)
        }
        .onGeometryChange(for: Bool.self) { $0.size.width >= Self.wideWidth } action: { isWide = $0 }
    }

    private var modeControl: some View {
        HuddleSegmentedControl(selection: $mode, segments: [
            .init(value: .week, title: "7 days", systemImage: "list.bullet"),
            .init(value: .month, title: "Month", systemImage: "calendar"),
        ])
        .accessibilityLabel(Text("Calendar view"))
    }

    /// Coming back within a minute shows what is there; after that, or
    /// after a reconnect that cleared it, it reloads.
    private var isStale: Bool {
        session.events.loadedAt.map { Date().timeIntervalSince($0) > 60 } ?? true
    }

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
    private var monthPlaceholder: some View {
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
            } else {
                initialState
            }
        }
    }

    @ViewBuilder
    private var initialState: some View {
        if session.connection == .online {
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
