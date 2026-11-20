import SwiftUI
import WaddleKit

/// One event: when and where, the user's reply and everyone else's, and
/// for owners, Edit and Cancel.
struct EventDetailSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @Environment(\.eventsCalendar) private var calendar
    let opened: OpenEvent
    @State private var isEditing = false
    @State private var isConfirmingCancel = false
    @State private var isCancelling = false
    @State private var errorMessage: String?

    var body: some View {
        NavigationStack {
            Group {
                if let event = session.events.event(opened.eventID) {
                    content(event)
                } else {
                    EmptyStateView(
                        title: "No longer on the calendar",
                        message: "This event was cancelled.",
                        symbol: "calendar.badge.minus"
                    )
                }
            }
            .background(Theme.Colors.bg)
            .navigationTitle("Event")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        #if os(macOS)
        .frame(width: 440, height: 680)
        #endif
        .sheet(isPresented: $isEditing) {
            if let event = session.events.event(opened.eventID) {
                EventEditorSheet(form: EventForm(editing: event, timeZone: calendar.timeZone)) { dismiss() }
                    .carryingCallDock()
            }
        }
    }

    private func content(_ event: CommunityEvent) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.Spacing.l) {
                EventFacts(event: event, occurrence: opened.occurrence)
                if let details = opened.occurrence?.details ?? event.details {
                    Text(details)
                        .huddleText(Theme.Typography.message)
                        .foregroundStyle(Theme.Colors.fg)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                EventReplyCard(event: event)
                if session.events.canManage {
                    ownerActions(event)
                }
            }
            .padding(Theme.Spacing.l)
        }
    }

    private func ownerActions(_ event: CommunityEvent) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            HStack(spacing: Theme.Spacing.s) {
                Button("Edit") { isEditing = true }
                    .buttonStyle(.huddle())
                Button(isCancelling ? "Cancelling…" : "Cancel event") { isConfirmingCancel = true }
                    .buttonStyle(.huddle(.danger))
                    .disabled(isCancelling)
            }
            if let errorMessage {
                Label(errorMessage, systemImage: "exclamationmark.triangle")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.dangerText)
            }
        }
        .confirmationDialog("Cancel \(event.title)?", isPresented: $isConfirmingCancel, titleVisibility: .visible) {
            if event.recurrence != nil, let occurrence = opened.occurrence, occurrence.recurrenceStart != nil {
                Button("This event only", role: .destructive) {
                    cancel { try await session.cancelOccurrence(occurrence) }
                }
                Button("All events in the series", role: .destructive) {
                    cancel { try await session.cancelEvent(event.id) }
                }
            } else {
                Button("Cancel event", role: .destructive) {
                    cancel { try await session.cancelEvent(event.id) }
                }
            }
            Button("Keep it", role: .cancel) {
                // Closing the dialog is all it does.
            }
        } message: {
            Text("It's taken off everyone's calendar.")
        }
    }

    private func cancel(_ action: @escaping () async throws -> Void) {
        isCancelling = true
        errorMessage = nil
        Task {
            do {
                try await action()
                dismiss()
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't cancel the event. Try again.")
            }
            isCancelling = false
        }
    }
}

/// The title, when, how it repeats, where, and who organised it.
private struct EventFacts: View {
    @Environment(\.eventsCalendar) private var calendar
    let event: CommunityEvent
    let occurrence: EventOccurrence?

    var body: some View {
        let copy = EventsCopy(calendar: calendar)
        VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            Text(occurrence?.title ?? event.title)
                .huddleText(Theme.Typography.title)
                .foregroundStyle(Theme.Colors.fg)
                .accessibilityAddTraits(.isHeader)
            VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                if let occurrence {
                    let when = copy.when(occurrence)
                    fact("calendar", when.date, detail: when.time)
                } else {
                    fact("calendar", "No date yet")
                }
                if let rule = event.recurrence, let start = event.start {
                    fact("repeat", copy.repeatSummary(rule, startingAt: start))
                }
                if let location = occurrence?.location ?? event.location {
                    fact("mappin", location)
                }
                if let organizer = event.organizer {
                    fact("person", "Organised by \(CommunityCopy.name(of: organizer))")
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func fact(_ symbol: String, _ text: String, detail: String? = nil) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.m) {
            Image(systemName: symbol)
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(Theme.Colors.fgSoft)
                .frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(text)
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fg)
                if let detail {
                    Text(detail)
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
            }
        }
        .accessibilityElement(children: .combine)
    }
}
