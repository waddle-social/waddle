import SwiftUI
import WaddleKit

/// Where a conversation row lives, which decides how it navigates.
enum ConversationLinkStyle {
    /// iPad/Mac sidebar: the link value drives `List(selection:)`.
    case sidebar
    /// iPhone stacks: the link pushes a `Route`.
    case phone
}

/// A row that opens `conversation` the way its list navigates.
struct ConversationLink<Content: View>: View {
    let conversation: ConversationID
    let style: ConversationLinkStyle
    @ViewBuilder let label: () -> Content

    var body: some View {
        switch style {
        case .sidebar:
            NavigationLink(value: conversation) {
                label()
            }
        case .phone:
            NavigationLink(value: Route.conversation(conversation)) {
                label()
            }
        }
    }
}

/// Unread, mention and mute state of one row.
struct ConversationAttention: Equatable {
    let unread: Int
    let isMention: Bool
    let isMuted: Bool

    var isUnread: Bool { unread > 0 || isMention }

    init(unread: Int, isMention: Bool, isMuted: Bool) {
        self.unread = unread
        self.isMention = isMention
        self.isMuted = isMuted
    }

    @MainActor
    init(session: SessionCoordinator, conversation: ConversationID) {
        self.init(
            unread: session.unread.count(for: conversation),
            isMention: session.unread.mentions.contains(conversation),
            isMuted: session.directory.notifyMode(for: conversation) == .never
        )
    }

    func accessibilityLabel(title: String) -> String {
        RowAccessibility.label(title: title, unread: unread, isMention: isMention, isMuted: isMuted)
    }
}
