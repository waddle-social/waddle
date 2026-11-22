import SwiftUI
import WaddleKit

/// Say what you are listening to (XEP-0118), or stop sharing it.
struct ProfileTuneEditor: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var tune: UserTune
    @State private var save = ProfileSave()
    private let current: UserTune?

    init(current: UserTune?) {
        self.current = current
        _tune = State(initialValue: current ?? UserTune())
    }

    var body: some View {
        Form {
            Section {
                TextField("Title", text: $tune.title)
                TextField("Artist", text: $tune.artist)
                TextField("Album", text: $tune.album)
                ProfileLinkField(text: $tune.link)
            } header: {
                FormSectionHeader("What are you listening to?")
            } footer: {
                FormSectionFooter(footer)
            }
            ProfileSaveError(save: save)
            if current != nil {
                Section {
                    FormActionButton(title: "Stop sharing", symbol: "xmark.circle", isDestructive: true, action: clear)
                        .disabled(save.isSaving)
                }
            }
        }
        .huddleForm()
        .navigationTitle("Listening to")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ProfileSaveButton(save: save, isDisabled: tune.isEmpty || !tune.hasUsableLink, action: publish)
        }
    }

    private var footer: String {
        tune.hasUsableLink
            ? "What you are listening to is shared with your contacts."
            : "Use a full link, like https://… or spotify:track:…"
    }

    private func publish() {
        let tune = tune
        save.run({ try await session.setTune(tune) }, then: { dismiss() })
    }

    private func clear() {
        save.run({ try await session.setTune(nil) }, then: { dismiss() })
    }
}
