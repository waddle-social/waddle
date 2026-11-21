import SwiftUI
import WaddleKit

/// Pick an XEP-0108 activity, a more exact one under it and an optional
/// note, or clear it.
struct ProfileActivityEditor: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var general: String?
    @State private var specific: String?
    @State private var note: String
    @State private var save = ProfileSave()
    private let current: UserActivity?

    init(current: UserActivity?) {
        self.current = current
        _general = State(initialValue: current?.general)
        _specific = State(initialValue: current?.specific)
        _note = State(initialValue: current?.text ?? "")
    }

    var body: some View {
        Form {
            Section("What are you doing?") {
                ProfileChoiceGrid(choices: ProfileActivityCatalog.choices(including: current?.general), selection: $general)
            }
            Section {
                if !specifics.isEmpty {
                    Picker("More exactly", selection: $specific) {
                        Text("Not set").tag(String?.none)
                        ForEach(specifics, id: \.self) { value in
                            Text(ProfileActivityCatalog.specificTitle(for: value)).tag(String?.some(value))
                        }
                    }
                }
                TextField("Note", text: $note, prompt: Text("Add a note (optional)"))
                    .submitLabel(.done)
                    .onSubmit(publish)
            } footer: {
                Text("Your activity is shared with your contacts.")
            }
            ProfileSaveError(save: save)
            if current != nil {
                Section {
                    Button("Clear activity", role: .destructive, action: clear)
                        .disabled(save.isSaving)
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Activity")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ProfileSaveButton(save: save, isDisabled: pendingActivity == nil, action: publish)
        }
        .onChange(of: general) { specific = specifics.contains(specific ?? "") ? specific : nil }
    }

    private var specifics: [String] {
        ProfileActivityCatalog.specifics(of: general, including: current)
    }

    private var pendingActivity: UserActivity? {
        general.flatMap { ProfileActivityCatalog.activity(general: $0, specific: specific, note: note) }
    }

    private func publish() {
        guard let activity = pendingActivity else { return }
        save.run({ try await session.setActivity(activity) }, then: { dismiss() })
    }

    private func clear() {
        save.run({ try await session.setActivity(nil) }, then: { dismiss() })
    }
}
