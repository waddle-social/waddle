import SwiftUI
import WaddleKit

/// The account's status (ADR-010) and RFC 6121 status message. The
/// message saves on submit or after a short pause in typing.
struct ProfileStatusSection: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var statusDraft = ""
    @State private var hasLoadedDraft = false

    private static let saveDelay: Duration = .milliseconds(1_200)

    var body: some View {
        Section {
            Picker("Status", selection: choiceBinding) {
                ForEach(ProfileStatusChoice.allCases) { choice in
                    ProfileStatusChoiceLabel(choice: choice, automaticShows: automaticShows)
                        .tag(choice)
                }
            }
            .pickerStyle(.inline)
            .labelsHidden()

            TextField("Status message", text: $statusDraft, prompt: Text("What are you working on?"))
                .submitLabel(.done)
                .onSubmit(saveStatusText)
                .onAppear(perform: loadDraft)
                .onDisappear(perform: saveStatusText)
                .task(id: statusDraft) { await saveAfterPause() }
        } header: {
            Text("Status")
        } footer: {
            Text(ProfileStatusChoice.footer)
        }
    }

    /// The dot on Automatic: what this device's idle time says, which is
    /// what is shown only while the mode is automatic.
    private var automaticShows: Availability {
        session.status.mode == .automatic ? session.status.availability : .available
    }

    private var choiceBinding: Binding<ProfileStatusChoice> {
        Binding(
            get: { ProfileStatusChoice(session.status.mode) },
            set: { choice in
                Task { await session.setPresenceMode(choice.mode) }
            }
        )
    }

    private func loadDraft() {
        guard !hasLoadedDraft else { return }
        statusDraft = session.status.statusText ?? ""
        hasLoadedDraft = true
    }

    private func saveAfterPause() async {
        guard hasLoadedDraft else { return }
        try? await Task.sleep(for: Self.saveDelay)
        guard !Task.isCancelled else { return }
        saveStatusText()
    }

    private func saveStatusText() {
        guard hasLoadedDraft,
              ProfileStatusText.hasChanges(draft: statusDraft, current: session.status.statusText) else { return }
        let text = statusDraft
        Task { await session.setStatusText(text) }
    }
}

/// A presence dot with the choice's name.
private struct ProfileStatusChoiceLabel: View {
    let choice: ProfileStatusChoice
    let automaticShows: Availability

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            PresenceDot(availability: choice.dot(automaticShows: automaticShows))
                .accessibilityHidden(true)
            Text(choice.title)
        }
    }
}
