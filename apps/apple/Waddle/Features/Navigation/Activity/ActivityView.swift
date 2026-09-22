import SwiftUI
import WaddleKit

/// Mentions and unread conversations, newest first.
struct ActivityView: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation

    init() {}

    var body: some View {
        let feed = currentFeed
        List {
            if !feed.mentions.isEmpty {
                Section("Mentions") {
                    ForEach(feed.mentions) { entry in
                        ActivityRow(entry: entry) { open(entry.conversation) }
                    }
                }
            }
            if !feed.unread.isEmpty {
                Section("Unread") {
                    ForEach(feed.unread) { entry in
                        ActivityRow(entry: entry) { open(entry.conversation) }
                    }
                }
            }
        }
        .navigationTitle("Activity")
        .overlay {
            if feed.isEmpty {
                ContentUnavailableView(
                    "You're all caught up",
                    systemImage: "checkmark.circle",
                    description: Text("Mentions and unread messages show up here.")
                )
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    markAllRead(feed.conversations)
                } label: {
                    Label("Mark all as read", systemImage: "checkmark.circle")
                }
                .disabled(feed.isEmpty)
                .help("Mark all as read")
            }
        }
    }

    private var currentFeed: ActivityFeed {
        ActivityFeed.build(
            counts: session.unread.counts,
            mentions: session.unread.mentions,
            recency: { recency(of: $0) },
            title: { session.directory.title(for: $0) }
        )
    }

    private func recency(of conversation: ConversationID) -> Date? {
        if let item = session.timelines.timeline(for: conversation).lastContentItem {
            return item.sentAt
        }
        guard !conversation.isRoom else { return nil }
        return session.directory.directConversations.first { $0.peer == conversation.jid }?.lastActivity
    }

    /// On the phone Activity tab the conversation pushes onto that tab's
    /// stack; anywhere else it opens through the shared navigation.
    private func open(_ conversation: ConversationID) {
        if navigation.tab == .activity {
            navigation.activityPath.append(.conversation(conversation))
        } else {
            navigation.open(conversation)
        }
    }

    private func markAllRead(_ conversations: [ConversationID]) {
        let session = self.session
        Task {
            for conversation in conversations {
                await session.markDisplayed(conversation)
            }
        }
    }
}
