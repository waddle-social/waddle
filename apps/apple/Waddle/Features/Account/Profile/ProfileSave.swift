import SwiftUI

/// One save at a time for a profile editor: busy while it runs, then
/// either done or holding the reason it failed.
@MainActor
@Observable
final class ProfileSave {
    private(set) var isSaving = false
    private(set) var errorMessage: String?

    func run(_ work: @escaping () async throws -> Void, then done: @escaping () -> Void) {
        guard !isSaving else { return }
        isSaving = true
        errorMessage = nil
        Task {
            do {
                try await work()
                isSaving = false
                done()
            } catch {
                isSaving = false
                errorMessage = error.localizedDescription
            }
        }
    }
}

/// Why the last save failed, when it did.
struct ProfileSaveError: View {
    let save: ProfileSave

    var body: some View {
        if let message = save.errorMessage {
            Section {
                Text(message)
                    .font(.footnote)
                    .foregroundStyle(.red)
            }
        }
    }
}

/// The editor's Save button, a spinner while saving.
struct ProfileSaveButton: ToolbarContent {
    let save: ProfileSave
    let isDisabled: Bool
    let action: () -> Void

    var body: some ToolbarContent {
        ToolbarItem(placement: .confirmationAction) {
            if save.isSaving {
                ProgressView().controlSize(.small)
            } else {
                Button("Save", action: action)
                    .disabled(isDisabled)
            }
        }
    }
}
