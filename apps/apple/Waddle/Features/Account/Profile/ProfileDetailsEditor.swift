import SwiftUI
import WaddleKit

/// Edit the account's details (XEP-0292): name, nickname, pronouns, bio
/// and a link. The fields appear once the current details have loaded, so
/// a save never overwrites details this device has not seen.
struct ProfileDetailsEditor: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var details: ProfileDetails?
    @State private var loadFailed = false
    @State private var save = ProfileSave()

    var body: some View {
        Form {
            if let details = Binding($details) {
                ProfileDetailsFields(details: details)
                ProfileSaveError(save: save)
            } else if loadFailed {
                Section {
                    Text("Your details didn't load.")
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(Theme.Colors.fgMuted)
                    FormActionButton(title: "Try again", symbol: "arrow.clockwise") { Task { await load() } }
                }
            } else {
                Section {
                    ProgressView()
                        .frame(maxWidth: .infinity)
                }
            }
        }
        .huddleForm()
        .navigationTitle("Your details")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ProfileSaveButton(save: save, isDisabled: !canSave, action: publish)
        }
        // Not again when the screen comes back: that would replace what
        // has been typed.
        .task { if details == nil { await load() } }
    }

    private var canSave: Bool {
        guard let details else { return false }
        return details.hasUsableLink && details.cleaned != session.status.details
    }

    private func load() async {
        loadFailed = false
        do {
            details = try await session.loadOwnDetails()
        } catch {
            loadFailed = true
        }
    }

    private func publish() {
        guard let details else { return }
        save.run({ try await session.saveDetails(details) }, then: { dismiss() })
    }
}

private struct ProfileDetailsFields: View {
    @Binding var details: ProfileDetails

    var body: some View {
        Section {
            TextField("Name", text: $details.fullName)
                .textContentType(.name)
            TextField("Nickname", text: $details.nickname)
            TextField("Pronouns", text: $details.pronouns, prompt: Text("she/her, he/him, they/them…"))
                .autocorrectionDisabled()
        }
        Section {
            TextField("Bio", text: $details.bio, prompt: Text("A few words about you"), axis: .vertical)
                .lineLimit(3...8)
                .labelsHidden()
        } header: {
            FormSectionHeader("Bio")
        }
        Section {
            ProfileLinkField(text: $details.link)
        } footer: {
            FormSectionFooter(details.hasUsableLink
                ? "People see these details when they open your profile."
                : "Use a full link, like https://example.org")
        }
    }
}
