import SwiftUI
import WaddleKit

/// Creates or edits an event. Save stays off until the server would take
/// what is filled in; a failed save keeps the sheet open and says why.
struct EventEditorSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var form: EventForm
    @State private var isSaving = false
    @State private var errorMessage: String?
    @State private var saving: Task<Void, Never>?
    private let onSaved: (() -> Void)?

    /// Snapshots hand in an error to show.
    init(form: EventForm, errorMessage: String? = nil, onSaved: (() -> Void)? = nil) {
        _form = State(initialValue: form)
        _errorMessage = State(initialValue: errorMessage)
        self.onSaved = onSaved
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Spacing.l) {
                    TextField("Event name", text: $form.title)
                        .textFieldStyle(.plain)
                        .huddleText(Theme.Typography.title)
                        .foregroundStyle(Theme.Colors.fg)
                        .padding(Theme.Spacing.l)
                        .huddleSurface(.card, radius: Theme.Radius.card)
                        .accessibilityLabel(Text("Event name"))
                    EventWhenCard(form: $form)
                    if form.hasDate {
                        EventRepeatCard(form: $form)
                    }
                    place
                    if let errorMessage {
                        Label(errorMessage, systemImage: "exclamationmark.triangle")
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.dangerText)
                    }
                }
                .disabled(isSaving)
                .padding(Theme.Spacing.l)
            }
            .background(Theme.Colors.bg)
            .navigationTitle(form.eventID == nil ? "New event" : "Edit event")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        saving?.cancel()
                        dismiss()
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(isSaving ? "Saving…" : "Save", action: save)
                        .disabled(form.draft() == nil || isSaving)
                }
            }
            .interactiveDismissDisabled(isSaving)
        }
        #if os(macOS)
        .frame(width: 460, height: 720)
        #endif
    }

    private var place: some View {
        VStack(spacing: 0) {
            Label {
                TextField("Where", text: $form.location)
                    .textFieldStyle(.plain)
                    .huddleText(Theme.Typography.input)
                    .foregroundStyle(Theme.Colors.fg)
            } icon: {
                Image(systemName: "mappin")
                    .foregroundStyle(Theme.Colors.fgSoft)
            }
            .padding(Theme.Spacing.l)
            HuddleDivider()
            TextField("What's it about?", text: $form.details, axis: .vertical)
                .textFieldStyle(.plain)
                .huddleText(Theme.Typography.input)
                .foregroundStyle(Theme.Colors.fg)
                .lineLimit(3...8)
                .padding(Theme.Spacing.l)
                .accessibilityLabel(Text("Description"))
        }
        .huddleSurface(.card, radius: Theme.Radius.card)
        .environment(\.huddleGround, Theme.Colors.surface)
    }

    private func save() {
        guard let draft = form.draft(), !isSaving else { return }
        isSaving = true
        errorMessage = nil
        saving = Task {
            do {
                try await session.saveEvent(draft)
                onSaved?()
                dismiss()
            } catch where Task.isCancelled {
                // The sheet was closed while saving; nobody is left to tell.
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't save the event. Try again.")
            }
            isSaving = false
        }
    }
}
