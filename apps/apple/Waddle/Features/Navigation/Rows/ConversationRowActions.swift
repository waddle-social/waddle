import SwiftUI
import WaddleKit

extension View {
    /// Context menu and swipe actions shared by every conversation row.
    func conversationRowActions(_ conversation: ConversationID, style: ConversationLinkStyle) -> some View {
        modifier(ConversationRowActions(conversation: conversation, style: style))
    }
}

/// Mark as read, notification mode and details for one conversation.
struct ConversationRowActions: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let conversation: ConversationID
    let style: ConversationLinkStyle

    func body(content: Content) -> some View {
        content
            .contextMenu {
                markReadButton
                NotifyModeMenu(conversation: conversation)
                Divider()
                Button {
                    ConversationDetailsOpener.open(conversation, style: style, navigation: navigation)
                } label: {
                    Label("Open details", systemImage: "info.circle")
                }
            }
            .swipeActions(edge: .leading, allowsFullSwipe: true) {
                markReadButton
                    .tint(.blue)
            }
            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                muteButton
                    .tint(.indigo)
            }
    }

    private var markReadButton: some View {
        Button {
            let session = self.session
            let conversation = self.conversation
            Task { await session.markDisplayed(conversation) }
        } label: {
            Label("Mark as read", systemImage: "envelope.open")
        }
        .disabled(!hasUnread)
    }

    private var muteButton: some View {
        let current = session.directory.notifyMode(for: conversation)
        let next = NotifyModeLabels.toggledMute(current, isRoom: conversation.isRoom)
        return Button {
            NotifyModeSetter.set(next, for: conversation, in: session)
        } label: {
            if next == .never {
                Label("Mute", systemImage: "bell.slash")
            } else {
                Label("Unmute", systemImage: "bell")
            }
        }
    }

    private var hasUnread: Bool {
        session.unread.count(for: conversation) > 0 || session.unread.mentions.contains(conversation)
    }
}

/// XEP-0492 mode picker as a submenu with a checkmark on the current mode.
struct NotifyModeMenu: View {
    @Environment(SessionCoordinator.self) private var session
    let conversation: ConversationID

    var body: some View {
        let current = session.directory.notifyMode(for: conversation)
        Menu {
            ForEach(NotifyModeLabels.ordered, id: \.self) { mode in
                Button {
                    NotifyModeSetter.set(mode, for: conversation, in: session)
                } label: {
                    if mode == current {
                        Label(NotifyModeLabels.title(mode), systemImage: "checkmark")
                    } else {
                        Text(NotifyModeLabels.title(mode))
                    }
                }
            }
        } label: {
            Label("Notifications", systemImage: NotifyModeLabels.symbol(current))
        }
    }
}

/// Fire-and-forget mode change for list rows; the coordinator applies it
/// optimistically and rolls back on failure, which the row then shows.
@MainActor
enum NotifyModeSetter {
    static func set(_ mode: NotifyMode, for conversation: ConversationID, in session: SessionCoordinator) {
        Task { try? await session.setNotifyMode(mode, for: conversation) }
    }
}

/// "Open details" from a row: the inspector beside the conversation on
/// iPad/Mac, a pushed details screen on iPhone.
@MainActor
enum ConversationDetailsOpener {
    static func open(_ conversation: ConversationID, style: ConversationLinkStyle, navigation: NavigationModel) {
        switch style {
        case .phone:
            navigation.open(conversation)
            navigation.showDetails(of: conversation, usesInspector: false)
        case .sidebar:
            navigation.open(conversation)
            navigation.inspector = .details
        }
    }
}
