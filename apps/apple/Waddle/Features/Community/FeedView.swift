import SwiftUI
import WaddleKit

/// The community's Feed: stories along the top, a box to post in, and what
/// people shared, newest first.
struct FeedView: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @State private var openStory: Story?
    @State private var isAddingStory = false
    /// A fixed clock, for snapshots; the app follows the real one.
    var clock: Date?
    /// On the phone, where Events has no tab of its own, the toolbar opens it.
    var opensEvents = false

    var body: some View {
        TimelineView(.everyMinute) { timeline in
            content(at: clock ?? timeline.date)
        }
        .background(Theme.Colors.bg)
        .navigationTitle("Feed")
        .refreshable { await session.refreshCommunity() }
        .task(id: session.connection == .online) {
            guard clock == nil, session.connection == .online, isStale else { return }
            await session.refreshCommunity()
        }
        .toolbar {
            if opensEvents {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        navigation.feedPath = [.events]
                    } label: {
                        Label("Events", systemImage: "calendar")
                    }
                    .help("Events")
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await session.refreshCommunity() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(session.community.isLoading)
                .help("Refresh")
            }
        }
        .sheet(isPresented: $isAddingStory) {
            StoryComposerSheet()
                .carryingCallDock()
        }
        .storyViewer($openStory)
    }

    private func content(at now: Date) -> some View {
        let community = session.community
        let stories = community.activeStories(at: now)
        return ScrollView {
            LazyVStack(alignment: .leading, spacing: Theme.Spacing.l) {
                if community.storiesFailed {
                    notice("Stories didn't load. Refresh to try again.")
                }
                StoryRail(stories: stories, now: now, onAdd: { isAddingStory = true }, onOpen: { openStory = $0 })
                FeedComposerCard()
                if community.feedFailed {
                    notice("The feed didn't load. Refresh to try again.")
                }
                ForEach(community.posts) { post in
                    FeedPostCard(post: post, now: now)
                }
                placeholder(community)
            }
            .padding(Theme.Spacing.l)
            .frame(maxWidth: Self.readableWidth)
            .frame(maxWidth: .infinity)
        }
    }

    /// Coming back to the Feed within a minute shows what is there;
    /// after that, or after a reconnect that cleared it, it reloads.
    private var isStale: Bool {
        session.community.loadedAt.map { Date().timeIntervalSince($0) > 60 } ?? true
    }

    /// Web `sizes.read`: a comfortable line length on a wide window.
    private static var readableWidth: CGFloat { 720 }

    private func notice(_ message: String) -> some View {
        Label(message, systemImage: "exclamationmark.triangle")
            .huddleText(Theme.Typography.caption)
            .foregroundStyle(Theme.Colors.warningText)
    }

    @ViewBuilder
    private func placeholder(_ community: CommunityStore) -> some View {
        if community.posts.isEmpty, !community.feedFailed {
            if community.hasLoaded {
                EmptyStateView(
                    title: "Nothing here yet",
                    message: "Posts from people in your community show up here. Be the first to share something.",
                    symbol: "newspaper"
                )
            } else if session.connection == .online {
                ProgressView()
                    .frame(maxWidth: .infinity)
                    .padding(Theme.Spacing.xl)
            } else {
                EmptyStateView(
                    title: "You're offline",
                    message: "The Feed loads when you're connected again.",
                    symbol: "wifi.slash"
                )
            }
        }
    }
}
