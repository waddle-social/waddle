import SwiftUI
import WaddleKit

/// Everything unread in your rooms, grouped by room and thread, newest
/// first: the native counterpart of the web client's unread view.
struct ActivityView: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation

    init() {}

    var body: some View {
        let overview = session.unreadOverview
        List {
            ForEach(overview.groups) { group in
                Section {
                    ActivityGroupRow(group: group) { open(group.conversation) }
                    ForEach(group.messages) { item in
                        ActivityMessageRow(item: item) { open(group.conversation) }
                    }
                    ForEach(group.threads) { thread in
                        ActivityThreadRow(thread: thread) { open(thread.key) }
                        ForEach(thread.messages) { item in
                            ActivityMessageRow(item: item, isThreadReply: true) { open(thread.key) }
                        }
                    }
                    if group.isIncomplete {
                        Label("Some messages couldn't be loaded. Pull to retry.", systemImage: "exclamationmark.triangle")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .navigationTitle("Activity")
        .overlay { placeholder(overview) }
        .refreshable {
            await session.refreshInbox()
            await session.refreshUnreadOverview()
        }
        // Re-runs when a count changes; the short sleep coalesces a burst
        // of inbox pushes into one refresh, since a newer key cancels it.
        .task(id: ActivityRefreshKey(session: session)) {
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

    @ViewBuilder
    private func placeholder(_ overview: UnreadOverviewStore) -> some View {
        if overview.groups.isEmpty {
            if overview.isLoading, !overview.hasLoaded {
                ProgressView()
            } else {
                ContentUnavailableView(
                    "You're all caught up",
                    systemImage: "checkmark.circle",
                    description: Text("Unread messages and threads from your rooms show up here.")
                )
            }
        }
    }

    /// Activity is a phone tab, so rooms and threads push onto its stack.
    private func open(_ conversation: ConversationID) {
        navigation.activityPath.append(.conversation(conversation))
    }

    private func open(_ thread: ThreadKey) {
        navigation.activityPath.append(.thread(.room(thread.room), rootID: thread.threadID))
    }

    private func markAllRead() {
        let session = self.session
        Task {
            await session.markOverviewRead()
            await session.refreshUnreadOverview()
        }
    }
}
