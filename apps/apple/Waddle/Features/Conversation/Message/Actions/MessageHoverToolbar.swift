import SwiftUI
import WaddleKit

extension EnvironmentValues {
    /// The row (by presentation id) whose hover toolbar shows without a
    /// pointer over it, for snapshots.
    @Entry var messageToolbarRow: String?
}

/// The toolbar that floats at a message row's top trailing corner while
/// the pointer is over the row (web `MessageHoverToolbar`): quick
/// reactions, add reaction, reply, thread, pin, then edit and delete for
/// one's own message. What it offers comes from `MessageToolbar`.
struct MessageHoverToolbar: View {
    /// Web's five quick reactions.
    static var quickReactions: [String] { Array(Theme.quickReactions.prefix(5)) }

    let actions: [MessageToolbarAction]
    let perform: (MessageToolbarAction) -> Void
    @ScaledMetric(relativeTo: .body) private var glyphSize: CGFloat = 13

    var body: some View {
        HStack(spacing: Theme.Spacing.xxs) {
            ForEach(Array(actions.enumerated()), id: \.element) { index, action in
                if action.isOwn, index > 0, !actions[index - 1].isOwn {
                    Rectangle()
                        .fill(Theme.Colors.border)
                        .frame(width: WaddleTokens.BorderWidth.hair, height: Theme.Spacing.l + Theme.Spacing.xs)
                        .padding(.horizontal, Theme.Spacing.xxs)
                        .accessibilityHidden(true)
                }
                Button {
                    perform(action)
                } label: {
                    glyph(action)
                }
                .buttonStyle(MessageToolbarButtonStyle(isDanger: action == .delete))
                .help(Self.title(action))
                .accessibilityLabel(Text(Self.accessibilityLabel(action)))
            }
        }
        .padding(Theme.Spacing.xxs)
        .huddleSurface(.raised, radius: Theme.Radius.medium)
    }

    @ViewBuilder
    private func glyph(_ action: MessageToolbarAction) -> some View {
        if case let .react(emoji) = action {
            Text(emoji).huddleText(Theme.Typography.body)
        } else {
            Image(systemName: Self.symbol(action))
                .font(.system(size: glyphSize, weight: .semibold))
        }
    }

    static func symbol(_ action: MessageToolbarAction) -> String {
        switch action {
        case .react: ""
        case .addReaction: "face.smiling"
        case .reply: "arrowshape.turn.up.left"
        case .thread: "bubble.left.and.bubble.right"
        case let .pin(isPinned): isPinned ? "pin.slash" : "pin"
        case .edit: "pencil"
        case .delete: "trash"
        }
    }

    /// The tooltip, as on web.
    static func title(_ action: MessageToolbarAction) -> String {
        switch action {
        case let .react(emoji): "React with \(emoji)"
        case .addReaction: "Add reaction"
        case .reply: "Reply"
        case let .thread(opens): opens ? "Open thread" : "Reply in thread"
        case let .pin(isPinned): isPinned ? "Unpin message" : "Pin message"
        case .edit: "Edit message"
        case .delete: "Delete message"
        }
    }

    static func accessibilityLabel(_ action: MessageToolbarAction) -> String {
        switch action {
        case let .react(emoji): "React to message with \(emoji)"
        case .reply: "Reply to message"
        default: title(action)
        }
    }
}

private extension MessageToolbarAction {
    var isOwn: Bool { self == .edit || self == .delete }
}

/// A 28pt glyph button: a tint on hover and press, red for delete.
private struct MessageToolbarButtonStyle: ButtonStyle {
    let isDanger: Bool

    func makeBody(configuration: Configuration) -> some View {
        MessageToolbarButton(configuration: configuration, isDanger: isDanger)
    }
}

private struct MessageToolbarButton: View {
    let configuration: ButtonStyleConfiguration
    let isDanger: Bool
    @State private var isHovering = false

    var body: some View {
        let active = isHovering || configuration.isPressed
        configuration.label
            .foregroundStyle(isDanger && active ? Theme.Colors.dangerText : Theme.Colors.fgMuted)
            .frame(width: WaddleTokens.ControlHeight.xs, height: WaddleTokens.ControlHeight.xs)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.small, style: .continuous)
                    .fill(active ? (isDanger ? Theme.Colors.dangerSubtle : Theme.Colors.surface2) : .clear)
            )
            .contentShape(Rectangle())
            .onHover { isHovering = $0 }
    }
}
