import SwiftUI
import WaddleKit

/// Writes a post at the top of the Feed.
struct FeedComposerCard: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var text = ""
    @State private var isPosting = false
    @State private var errorMessage: String?
    @FocusState private var isFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            HStack(alignment: .center, spacing: Theme.Spacing.m) {
                JIDAvatar(jid: session.account.jid)
                TextField("Share something with everyone", text: $text, axis: .vertical)
                    .textFieldStyle(.plain)
                    .huddleText(Theme.Typography.input)
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1...8)
                    .focused($isFocused)
                    .disabled(isPosting)
                    .frame(minHeight: Theme.Size.avatar)
                    .accessibilityLabel(Text("New post"))
                Button(isPosting ? "Posting…" : "Post", action: post)
                    .buttonStyle(.huddle(.primary, size: .small))
                    .disabled(draft == nil || isPosting)
                    .keyboardShortcut(.return, modifiers: .command)
            }
            if let errorMessage {
                Label(errorMessage, systemImage: "exclamationmark.triangle")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.dangerText)
            }
        }
        .padding(Theme.Spacing.l)
        .huddleSurface(.card, radius: Theme.Radius.card)
        .environment(\.huddleGround, Theme.Colors.surface)
    }

    private var draft: FeedPostDraft? {
        FeedPostDraft(body: text)
    }

    private func post() {
        guard let draft, !isPosting else { return }
        isPosting = true
        errorMessage = nil
        Task {
            do {
                try await session.postToFeed(draft)
                text = ""
                isFocused = false
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't post. Try again.")
            }
            isPosting = false
        }
    }
}
