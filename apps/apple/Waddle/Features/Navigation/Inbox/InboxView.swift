import SwiftUI
import WaddleKit

/// Everything unread in your rooms, one card per room with its messages
/// and thread replies, newest first: the native counterpart of the web
/// client's Inbox.
struct InboxView: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    /// How this Inbox opens a room: pushed on the phone tab's stack, or
    /// selected in the split view.
    let style: ConversationLinkStyle

    var body: some View {
        let overview = session.unreadOverview
        ScrollView {
            LazyVStack(spacing: Theme.Spacing.l) {
                ForEach(overview.groups) { group in
                    InboxRoomCard(
                        group: group,
                        onOpenRoom: { open(group.conversation) },
                        onOpenThread: { open($0) }
                    )
                }
            }
            .padding(Theme.Spacing.l)
            .frame(maxWidth: Self.readableWidth)
            .frame(maxWidth: .infinity)
        }
        .background(Theme.Colors.bg)
        .navigationTitle("Inbox")
        .overlay { placeholder(overview) }
        .refreshable {
            await session.refreshInbox()
            await session.refreshUnreadOverview()
        }
        // Re-runs when a count changes; the short sleep coalesces a burst
        // of inbox pushes into one refresh, since a newer key cancels it.
        .task(id: InboxRefreshKey(session: session)) {
            try? await Task.sleep(for: .milliseconds(300))
            guard !Task.isCancelled else { return }
            await session.refreshUnreadOverview()
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    markAllRead()
                } label: {
                    Label("Mark all as read", systemImage: "checkmark.circle")
                }
                .disabled(overview.groups.isEmpty)
                .help("Mark all as read")
            }
        }
    }

    /// Web `sizes.read`: a comfortable line length on a wide window.
    private static var readableWidth: CGFloat { 720 }

    @ViewBuilder
    private func placeholder(_ overview: UnreadOverviewStore) -> some View {
        if overview.groups.isEmpty {
            if overview.isLoading, !overview.hasLoaded {
                ProgressView()
            } else {
                EmptyStateView(
                    title: "You're all caught up",
                    message: "Unread messages and replies from your rooms show up here.",
                    symbol: "tray"
                )
            }
        }
    }

    private func open(_ conversation: ConversationID) {
        switch style {
        case .phone:
            navigation.inboxPath.append(.conversation(conversation))
        case .sidebar:
            navigation.open(conversation)
        }
    }

    private func open(_ thread: ThreadKey) {
        switch style {
        case .phone:
            navigation.inboxPath.append(.thread(.room(thread.room), rootID: thread.threadID))
        case .sidebar:
            navigation.open(.room(thread.room))
            navigation.openThread(thread.threadID, in: .room(thread.room), usesInspector: true)
        }
    }

    private func markAllRead() {
        let session = self.session
        Task {
            await session.markOverviewRead()
            await session.refreshUnreadOverview()
        }
    }
}
