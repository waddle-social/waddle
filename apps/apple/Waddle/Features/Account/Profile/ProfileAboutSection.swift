import SwiftUI
import WaddleKit

/// Your details (XEP-0292) edited in place, as Android `profile` ("About
/// you") and web `settings` (Profile) have them: name, nickname,
/// pronouns, link and bio, then Revert and Save profile. The fields show
/// once the current details have loaded, so a save never overwrites
/// details this device has not seen.
struct ProfileAboutSection: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var draft: ProfileDetails?
    @State private var loadFailed = false
    @State private var save = ProfileSave()

    var body: some View {
        Section {
            if let draft = Binding($draft) {
                ProfileDetailsFields(details: draft)
                actions(for: draft.wrappedValue)
            } else if loadFailed {
                Text("Your details didn't load.")
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fgMuted)
                FormActionButton(title: "Try again", symbol: "arrow.clockwise") { Task { await load() } }
            } else {
                ProgressView()
                    .frame(maxWidth: .infinity)
            }
        } header: {
            FormSectionHeader("About you")
        } footer: {
            if let draft, !draft.hasUsableLink {
                FormSectionFooter("Use a full link, like https://example.org")
            } else {
                FormSectionFooter("People see these details when they open your profile.")
            }
        }
        // Not again when the profile comes back: that would replace what
        // has been typed.
        .task { if draft == nil { await load() } }
    }

    /// Where the details stand, then Revert and Save profile.
    private func actions(for details: ProfileDetails) -> some View {
        let hasChanges = details.cleaned != session.status.details
        return HStack(spacing: Theme.Spacing.s) {
            Text(save.errorMessage ?? (hasChanges ? "Not saved yet." : "Published."))
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(save.errorMessage == nil ? Theme.Colors.fgMuted : Theme.Colors.dangerText)
            Spacer(minLength: Theme.Spacing.s)
            Button("Revert") {
                self.draft = session.status.details
            }
            .buttonStyle(.huddle(size: .small))
            .disabled(!hasChanges || save.isSaving)
            Button {
                publish(details)
            } label: {
                if save.isSaving {
                    ProgressView().controlSize(.small)
                } else {
                    Text("Save profile")
                }
            }
            .buttonStyle(.huddle(.primary, size: .small))
            .disabled(!hasChanges || !details.hasUsableLink || save.isSaving)
        }
        .padding(.vertical, Theme.Spacing.xs)
    }

    private func load() async {
        loadFailed = false
        do {
            draft = try await session.loadOwnDetails()
        } catch {
            loadFailed = true
        }
    }

    private func publish(_ details: ProfileDetails) {
        save.run({ try await session.saveDetails(details) }, then: {
            // Tidy the draft only if nothing was typed while saving;
            // otherwise keep those edits, which then read "Not saved yet."
            if draft == details { draft = details.cleaned }
        })
    }
}

/// The detail fields, one row each.
private struct ProfileDetailsFields: View {
    @Binding var details: ProfileDetails

    var body: some View {
        Group {
            TextField("Full name", text: $details.fullName)
                .textContentType(.name)
            TextField("Nickname", text: $details.nickname)
            TextField("Pronouns", text: $details.pronouns, prompt: Text("she/her, he/him, they/them…"))
                .autocorrectionDisabled()
            ProfileLinkField(text: $details.link)
            TextField("Bio", text: $details.bio, prompt: Text("A few words about you"), axis: .vertical)
                .lineLimit(3...8)
        }
        .huddleText(Theme.Typography.body)
    }
}
