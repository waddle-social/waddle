import SwiftUI
import WaddleKit

/// Where a conversation row lives, which decides how it navigates.
enum ConversationLinkStyle {
    /// iPad/Mac sidebar: the row selects the conversation.
    case sidebar
    /// iPhone stacks: the row pushes a `Route`.
    case phone
}

/// A row that opens `conversation` the way its list navigates.
struct ConversationLink<Content: View>: View {
    @Environment(NavigationModel.self) private var navigation
    let conversation: ConversationID
    let style: ConversationLinkStyle
    @ViewBuilder let label: () -> Content

    var body: some View {
        switch style {
        case .sidebar:
            let isSelected = navigation.selection == conversation
            Button {
                navigation.selection = conversation
            } label: {
                NavigationRowChrome(isSelected: isSelected) { label() }
            }
            .buttonStyle(.plain)
            .navigationListRow()
            .accessibilityAddTraits(isSelected ? .isSelected : [])
        case .phone:
            NavigationRowChrome { label() }
                // The link sits behind the row so the list draws no chevron.
                .background {
                    NavigationLink(value: Route.conversation(conversation)) { EmptyView() }
                        .opacity(0)
                }
                .navigationListRow()
        }
    }
}

/// Unread, mention and mute state of one row.
struct ConversationAttention: Equatable {
    let unread: Int
    let isMention: Bool
    let isMuted: Bool

    var isUnread: Bool { unread > 0 || isMention }

    /// Muted and read: the row steps back.
    var isDimmed: Bool { isMuted && !isUnread }

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
