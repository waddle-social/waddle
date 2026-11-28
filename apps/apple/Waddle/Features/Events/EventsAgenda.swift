import SwiftUI
import WaddleKit

/// What is planned from a day on, a card per day, and the events not given
/// a date yet below them.
struct EventsAgenda<Placeholder: View>: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.eventsCalendar) private var calendar
    /// Occurrences from the calendar query, earliest first.
    let occurrences: [EventOccurrence]
    let from: CalendarDay
    let today: CalendarDay
    let now: Date
    let undated: [CommunityEvent]
    let filter: EventFilter
    let onOpen: (OpenEvent) -> Void
    /// Shown while there are no events at all.
    @ViewBuilder let placeholder: Placeholder

    private var copy: EventsCopy { EventsCopy(calendar: calendar) }

    var body: some View {
        EventReplyAlert(content: agenda)
    }

    private func agenda(_ reply: EventReplyAction) -> some View {
        let days = EventsAgendaWindow(startingAt: from, timeZone: calendar.timeZone).sections(for: occurrences)
        return VStack(alignment: .leading, spacing: Theme.Spacing.l) {
            if session.events.events.isEmpty {
                placeholder
            } else {
                if days.isEmpty {
                    nothingPlanned
                }
                ForEach(days, id: \.day) { group in
                    section(copy.agendaTitle(group.day, today: today)) {
                        ForEach(Array(group.occurrences.enumerated()), id: \.element.id) { index, occurrence in
                            if index > 0 { HuddleDivider() }
                            EventAgendaCard(eventID: occurrence.eventID, replyAction: reply) {
                                EventsAgendaRow(occurrence: occurrence, day: group.day, status: occurrence.status(at: now)) {
                                    onOpen(OpenEvent(eventID: occurrence.eventID, occurrence: occurrence))
                                }
                            }
                        }
                    }
                }
                if !undated.isEmpty {
                    section("No date yet") {
                        ForEach(Array(undated.enumerated()), id: \.element.id) { index, event in
                            if index > 0 { HuddleDivider() }
                            EventAgendaCard(eventID: event.id, replyAction: reply) {
                                EventsUndatedRow(event: event) { onOpen(OpenEvent(eventID: event.id, occurrence: nil)) }
                            }
                        }
                    }
                }
            }
        }
    }

    private var nothingPlanned: some View {
        let day = copy.dayTitle(from)
        let message: LocalizedStringKey = filter == .goingOrMaybe
            ? "Nothing you're going to in the four weeks from \(day)."
            : "Nothing planned for the four weeks from \(day)."
        return Text(message)
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

/// Opens an occurrence from its time, title and location.
private struct EventsAgendaRow: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.eventsCalendar) private var calendar
    let occurrence: EventOccurrence
    let day: CalendarDay
    let status: EventStatus?
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
                    if let status {
                        Text(status.label)
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.liveText)
                    }
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
