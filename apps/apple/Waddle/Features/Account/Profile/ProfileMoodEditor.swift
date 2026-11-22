import SwiftUI
import WaddleKit

/// Pick an XEP-0107 mood and an optional note, or clear it.
struct ProfileMoodEditor: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var selection: String?
    @State private var note: String
    @State private var save = ProfileSave()
    private let current: UserMood?

    init(current: UserMood?) {
        self.current = current
        _selection = State(initialValue: current?.value)
        _note = State(initialValue: current?.text ?? "")
    }

    var body: some View {
        Form {
            Section {
                ProfileChoiceGrid(choices: ProfileMoodCatalog.options(including: current?.value), selection: $selection)
            } header: {
                FormSectionHeader("How are you feeling?")
            }
            Section {
                TextField("Note", text: $note, prompt: Text("Add a note (optional)"))
                    .submitLabel(.done)
                    .onSubmit(publish)
            } footer: {
                FormSectionFooter("Your mood is shared with your contacts.")
            }
            ProfileSaveError(save: save)
            if current != nil {
                Section {
                    FormActionButton(title: "Clear mood", symbol: "xmark.circle", isDestructive: true, action: clear)
                        .disabled(save.isSaving)
                }
            }
        }
        .huddleForm()
        .navigationTitle("Mood")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ProfileSaveButton(save: save, isDisabled: pendingMood == nil, action: publish)
        }
    }

    private var pendingMood: UserMood? {
        selection.flatMap { ProfileMoodCatalog.mood(value: $0, note: note) }
    }

    private func publish() {
        guard let mood = pendingMood else { return }
        save.run({ try await session.setMood(mood) }, then: { dismiss() })
    }

    private func clear() {
        save.run({ try await session.setMood(nil) }, then: { dismiss() })
    }
}
