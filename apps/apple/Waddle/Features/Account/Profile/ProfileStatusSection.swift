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
            ForEach(ProfileStatusChoice.allCases) { choice in
                ProfileStatusChoiceRow(
                    choice: choice,
                    automaticShows: automaticShows,
                    isSelected: ProfileStatusChoice(session.status.presenceMode) == choice
                ) {
                    Task { await session.setPresenceMode(choice.mode) }
                }
            }

            TextField("Status message", text: $statusDraft, prompt: Text("What are you working on?"))
                .huddleText(Theme.Typography.body)
                .submitLabel(.done)
                .onSubmit(saveStatusText)
                .onAppear(perform: loadDraft)
                .onDisappear(perform: saveStatusText)
                .task(id: statusDraft) { await saveAfterPause() }
        } header: {
            FormSectionHeader("Status")
        } footer: {
            FormSectionFooter(ProfileStatusChoice.footer)
        }
    }

    /// The dot on Automatic: what this device's idle time says, which is
    /// what is shown only while the mode is automatic.
    private var automaticShows: Availability {
        session.status.presenceMode == .automatic ? session.status.availability : .available
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

/// A presence dot with the choice's name, and a check on the one in use.
private struct ProfileStatusChoiceRow: View {
    let choice: ProfileStatusChoice
    let automaticShows: Availability
    let isSelected: Bool
    let select: () -> Void

    var body: some View {
        Button(action: select) {
            HStack(spacing: Theme.Spacing.s) {
                PresenceDot(availability: choice.dot(automaticShows: automaticShows))
                    .accessibilityHidden(true)
                Text(choice.title)
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fg)
                Spacer(minLength: Theme.Spacing.s)
                if isSelected {
                    Image(systemName: "checkmark")
                        .huddleText(Theme.Typography.label)
                        .foregroundStyle(Theme.Colors.action)
                        .accessibilityHidden(true)
                }
            }
            .frame(maxWidth: .infinity, minHeight: Theme.Size.formRow, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}
