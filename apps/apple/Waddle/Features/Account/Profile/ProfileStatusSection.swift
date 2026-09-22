import SwiftUI
import WaddleKit

/// RFC 6121 availability and status message. The message saves on submit
/// or after a short pause in typing.
struct ProfileStatusSection: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var statusDraft = ""
    @State private var hasLoadedDraft = false

    private static let saveDelay: Duration = .milliseconds(1_200)

    var body: some View {
        Section {
            Picker("Availability", selection: availabilityBinding) {
                ForEach(ProfileAvailabilityOption.allCases) { option in
                    ProfileAvailabilityLabel(option: option)
                        .tag(option)
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
            Text("Everyone who can see your presence sees this.")
        }
    }

    private var availabilityBinding: Binding<ProfileAvailabilityOption> {
        Binding(
            get: { ProfileAvailabilityOption(session.status.availability) },
            set: { option in
                let text = statusDraft
                Task { await session.setAvailability(option.availability, statusText: text) }
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
        let availability = session.status.availability
        let text = statusDraft
        Task { await session.setAvailability(availability, statusText: text) }
    }
}

/// A presence dot with the option's name.
private struct ProfileAvailabilityLabel: View {
    let option: ProfileAvailabilityOption

    var body: some View {
        HStack(spacing: Theme.Spacing.s) {
            PresenceDot(availability: option.availability)
                .accessibilityHidden(true)
            Text(option.title)
        }
    }
}
