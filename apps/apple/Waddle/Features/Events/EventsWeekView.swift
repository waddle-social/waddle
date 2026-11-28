import SwiftUI
import WaddleKit

/// The web calendar's rolling "7 days" list.
struct EventsWeekView<InitialState: View>: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.eventsCalendar) private var calendar
    @State private var replyError: String?
    let now: Date
    let filter: EventFilter
    let onOpen: (OpenEvent) -> Void
    let onAdd: () -> Void
    @ViewBuilder let initialState: InitialState

    var body: some View {
        let week = EventsWeek(now: now, timeZone: calendar.timeZone)
        let occurrences = session.eventOccurrences(in: week.range, timeZone: calendar.timeZone, filter: filter)
        let sections = week.sections(for: occurrences, locale: calendar.locale ?? .current)
        VStack(alignment: .leading, spacing: Theme.Spacing.l) {
            if sections.isEmpty {
                if session.events.isLoading || !session.events.hasLoaded {
                    initialState
                } else if !session.events.failed {
                    emptyWeek
                }
            }
            ForEach(sections, id: \.day) { section in
                VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                    Text(section.heading)
                        .huddleText(Theme.Typography.eyebrow)
                        .foregroundStyle(Theme.Colors.fgMuted)
                        .accessibilityAddTraits(.isHeader)
                    ForEach(section.entries, id: \.occurrence.id) { entry in
                        card(entry)
                    }
                }
            }
        }
        .alert("Couldn't send your answer", isPresented: replyErrorBinding) {
            Button("OK", role: .cancel) { replyError = nil }
        } message: {
            Text(replyError ?? "")
        }
    }

    private var replyErrorBinding: Binding<Bool> {
        Binding(get: { replyError != nil }, set: { if !$0 { replyError = nil } })
    }

    private func card(_ entry: EventsWeek.Entry) -> some View {
        let occurrence = entry.occurrence
        return HuddleCard(isLive: entry.status != nil) {
            EventAgendaCard(eventID: occurrence.eventID, insetsFooter: false, onReplyError: { replyError = $0 }) {
                EventsWeekCardHeader(entry: entry) {
                    onOpen(OpenEvent(eventID: occurrence.eventID, occurrence: occurrence))
                }
            }
        }
        .environment(\.huddleGround, Theme.Colors.surface)
    }

    private var emptyWeek: some View {
        let canManage = session.events.canManage
        let copy = EventsWeek.emptyCopy(for: filter, canManage: canManage)
        return VStack(spacing: Theme.Spacing.s) {
            EmptyStateView(title: copy.title, message: copy.message, symbol: "calendar")
            if canManage {
                Button("New event", action: onAdd)
                    .buttonStyle(.huddle(.primary))
            }
        }
    }
}

/// The time, title and details open the same occurrence as in the agenda.
private struct EventsWeekCardHeader: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.eventsCalendar) private var calendar
    let entry: EventsWeek.Entry
    let onOpen: () -> Void

    var body: some View {
        let occurrence = entry.occurrence
        let event = session.events.event(occurrence.eventID)
        let meta = [occurrence.location, event?.organizer.map { "Hosted by \(CommunityCopy.name(of: $0))" }]
            .compactMap { $0 }.joined(separator: " · ")
        Button(action: onOpen) {
            VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
                Text(entry.timeLabel)
                    .huddleText(Theme.Typography.eyebrow)
                    .foregroundStyle(entry.status == nil ? Theme.Colors.actionText : Theme.Colors.liveText)
                Text(occurrence.title)
                    .huddleText(Theme.Typography.subtitle)
                    .foregroundStyle(Theme.Colors.fg)
                if !meta.isEmpty {
                    HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.xs) {
                        if occurrence.location != nil {
                            Image(systemName: "mappin")
                                .accessibilityHidden(true)
                        }
                        Text(meta)
                    }
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                }
                if let details = occurrence.details {
                    Text(details)
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(Theme.Colors.fg)
                }
                if let rule = event?.recurrence, let start = event?.start {
                    Label(EventsCopy(calendar: calendar).repeatSummary(rule, startingAt: start), systemImage: "repeat")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.actionText)
                }
            }
            .multilineTextAlignment(.leading)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.bottom, Theme.Spacing.s)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}
