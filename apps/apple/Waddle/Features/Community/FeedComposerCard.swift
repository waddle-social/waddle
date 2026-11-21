import SwiftUI
import WaddleKit

/// Writes a post at the top of the Feed, with a link if you add one.
struct FeedComposerCard: View {
    @Environment(SessionCoordinator.self) private var session
    @State private var text: String
    @State private var link: String
    @State private var hasLink: Bool
    @State private var isPosting = false
    @State private var errorMessage: String?
    @FocusState private var focus: Field?

    private enum Field {
        case text
        case link
    }

    /// Snapshots hand in a post already written; `link` opens the link
    /// field with it.
    init(text: String = "", link: String? = nil) {
        _text = State(initialValue: text)
        _link = State(initialValue: link ?? "")
        _hasLink = State(initialValue: link != nil)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            HStack(alignment: .center, spacing: Theme.Spacing.m) {
                JIDAvatar(jid: session.account.jid)
                TextField("Share something with everyone", text: $text, axis: .vertical)
                    .textFieldStyle(.plain)
                    .huddleText(Theme.Typography.input)
                    .foregroundStyle(Theme.Colors.fg)
                    .lineLimit(1...8)
                    .focused($focus, equals: .text)
                    .disabled(isPosting)
                    .frame(minHeight: Theme.Size.avatar)
                    .accessibilityLabel(Text("New post"))
                linkToggle
                Button(isPosting ? "Posting…" : "Post", action: post)
                    .buttonStyle(.huddle(.primary, size: .small))
                    .disabled(draft == nil || isPosting)
                    .keyboardShortcut(.return, modifiers: .command)
            }
            if hasLink {
                linkField
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

    private var linkToggle: some View {
        HuddleIconButton(hasLink ? "Remove the link" : "Add a link", symbol: "link") {
            hasLink.toggle()
            if hasLink {
                focus = .link
            } else {
                link = ""
            }
        }
        .foregroundStyle(hasLink ? Theme.Colors.actionText : Theme.Colors.fgMuted)
        .disabled(isPosting)
    }

    /// Lined up with the text above it.
    private var linkField: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            TextField("Link, like waddle.social/news", text: $link)
                .textFieldStyle(.plain)
                .huddleText(Theme.Typography.input)
                .foregroundStyle(Theme.Colors.fg)
                .autocorrectionDisabled()
                #if os(iOS)
                .textInputAutocapitalization(.never)
                .keyboardType(.URL)
                #endif
                .focused($focus, equals: .link)
                .disabled(isPosting)
                .padding(.horizontal, Theme.Spacing.m)
                .frame(minHeight: WaddleTokens.ControlHeight.md)
                .background(Theme.Colors.surface2, in: RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous))
                .accessibilityLabel(Text("Link"))
            if linkIsInvalid {
                Text("That doesn't look like a web address.")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.dangerText)
            }
        }
        .padding(.leading, Theme.Size.avatar + Theme.Spacing.m)
    }

    private var linkIsInvalid: Bool {
        FeedPostDraft.link(from: link) == .invalid
    }

    private var draft: FeedPostDraft? {
        FeedPostDraft(body: text, link: hasLink ? link : "")
    }

    private func post() {
        guard let draft, !isPosting else { return }
        isPosting = true
        errorMessage = nil
        Task {
            do {
                try await session.postToFeed(draft)
                text = ""
                link = ""
                hasLink = false
                focus = nil
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't post. Try again.")
            }
            isPosting = false
        }
    }
}
