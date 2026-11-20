import SwiftUI
import WaddleKit

/// What is planned from a day on, a card per day, and the events not given
/// a date yet below them.
struct EventsAgenda<Placeholder: View>: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.eventsCalendar) private var calendar
    /// Each occurrence that runs on `from` or later, earliest first.
    let occurrences: [EventOccurrence]
    let from: CalendarDay
    let today: CalendarDay
    let undated: [CommunityEvent]
    let onOpen: (OpenEvent) -> Void
    /// Shown while there are no events at all.
    @ViewBuilder let placeholder: Placeholder

    private var copy: EventsCopy { EventsCopy(calendar: calendar) }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.l) {
            if session.events.events.isEmpty {
                placeholder
            } else {
                if occurrences.isEmpty {
                    nothingPlanned
                }
                ForEach(days, id: \.day) { group in
                    section(copy.agendaTitle(group.day, today: today)) {
                        ForEach(Array(group.occurrences.enumerated()), id: \.element.id) { index, occurrence in
                            if index > 0 { HuddleDivider() }
                            EventsAgendaRow(occurrence: occurrence, day: group.day) {
                                onOpen(OpenEvent(eventID: occurrence.eventID, occurrence: occurrence))
                            }
                        }
                    }
                }
                if !undated.isEmpty {
                    section("No date yet") {
                        ForEach(Array(undated.enumerated()), id: \.element.id) { index, event in
                            if index > 0 { HuddleDivider() }
                            EventsUndatedRow(event: event) { onOpen(OpenEvent(eventID: event.id, occurrence: nil)) }
                        }
                    }
                }
            }
        }
    }

    /// Each occurrence once: under its first day from `from` on.
    private var days: [(day: CalendarDay, occurrences: [EventOccurrence])] {
        let grouped = Dictionary(grouping: occurrences) { max($0.days(in: calendar.timeZone).lowerBound, from) }
        return grouped.keys.sorted().map { ($0, grouped[$0] ?? []) }
    }

    private var nothingPlanned: some View {
        Text("Nothing planned for the four weeks from \(copy.dayTitle(from)).")
            .huddleText(Theme.Typography.body)
            .foregroundStyle(Theme.Colors.fgMuted)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Theme.Spacing.l)
            .huddleSurface(.card, radius: Theme.Radius.card)
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            Text(title)
                .huddleText(Theme.Typography.eyebrow)
                .foregroundStyle(Theme.Colors.fgMuted)
                .padding(.horizontal, Theme.Spacing.xs)
                .accessibilityAddTraits(.isHeader)
            VStack(spacing: 0) { content() }
                .huddleSurface(.card, radius: Theme.Radius.card)
                .environment(\.huddleGround, Theme.Colors.surface)
        }
    }
}

/// One occurrence in the agenda: when, what, where, and the user's reply.
private struct EventsAgendaRow: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.eventsCalendar) private var calendar
    let occurrence: EventOccurrence
    let day: CalendarDay
    let onOpen: () -> Void

    var body: some View {
        let times = EventsCopy(calendar: calendar).rowTimes(occurrence, on: day)
        let event = session.events.event(occurrence.eventID)
        Button(action: onOpen) {
            HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.m) {
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    Text(times.start)
                        .huddleText(Theme.Typography.label)
                        .foregroundStyle(Theme.Colors.fg)
                    if let end = times.end {
                        Text(end)
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.fgSoft)
                    }
                }
                .monospacedDigit()
                .frame(width: 58, alignment: .leading)
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    HStack(spacing: Theme.Spacing.xs) {
                        Text(occurrence.title)
                            .huddleText(Theme.Typography.author)
                            .foregroundStyle(Theme.Colors.fg)
                            .lineLimit(2)
                        if event?.recurrence != nil {
                            Image(systemName: "repeat")
                                .font(.system(size: 11, weight: .semibold))
                                .foregroundStyle(Theme.Colors.fgSoft)
                                .accessibilityLabel(Text("Repeats"))
                        }
                    }
                    if let location = occurrence.location {
                        Label(location, systemImage: "mappin")
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.fgMuted)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: Theme.Spacing.s)
                if let reply = session.events.myReply(to: occurrence.eventID) {
                    HuddleTag(EventsCopy.label(for: reply))
                }
            }
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.vertical, Theme.Spacing.m)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
    }
}

/// An event not given a date yet.
private struct EventsUndatedRow: View {
    let event: CommunityEvent
    let onOpen: () -> Void

    var body: some View {
        Button(action: onOpen) {
            label
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(.isButton)
    }

    private var label: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
            Text(event.title)
                .huddleText(Theme.Typography.author)
                .foregroundStyle(Theme.Colors.fg)
            if let location = event.location {
                Label(location, systemImage: "mappin")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, Theme.Spacing.l)
        .padding(.vertical, Theme.Spacing.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

/// An event opened from the agenda: one date of it, or the event itself
/// when it has no date yet.
struct OpenEvent: Identifiable, Hashable {
    let eventID: String
    let occurrence: EventOccurrence?

    var id: String { occurrence?.id ?? eventID }
}
