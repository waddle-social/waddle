import SwiftUI
import WaddleKit

/// The scrolling feed. In the chat order the newest message is at the
/// bottom and older pages load at the top; in the social order the newest
/// is at the top and older pages load at the bottom. Either way, new
/// messages keep the view pinned to the live edge only when it already
/// sits there.
struct ConversationTimelineList: View {
    @Environment(AppState.self) private var app

    let conversation: ConversationID
    let header: ConversationHeaderText
    let unreadAnchorID: String?

    var body: some View {
        // A new list per order, so its scroll state starts over.
        let newestFirst = app.preferences.messageOrder.isNewestFirst
        TimelineScroller(
            conversation: conversation,
            header: header,
            unreadAnchorID: unreadAnchorID,
            newestFirst: newestFirst
        )
        .id(newestFirst)
    }
}

private struct TimelineScroller: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(MessageActionModel.self) private var actions
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    let conversation: ConversationID
    let header: ConversationHeaderText
    let unreadAnchorID: String?
    let newestFirst: Bool

    @State private var isAtLiveEdge = true
    @State private var unseenCount = 0
    /// The row at the top of the viewport. The social order inserts new
    /// rows above it, so it is put back there to keep the reader's place.
    @State private var topRowID: String?

    private static let liveEdgeID = "timeline-live-edge"

    var body: some View {
        let timeline = session.timelines.timeline(for: conversation)
        let items = timeline.feedItems
        let entries = TimelineFeedLayout.entries(
            for: items,
            unreadAnchorID: unreadAnchorID,
            groupingWindow: Theme.groupingWindow,
            newestFirst: newestFirst,
            replyCount: { timeline.replyCount(for: $0) },
            replyParent: { timeline.item(withID: $0) }
        )
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if newestFirst {
                        liveEdgeSentinel
                        rows(entries)
                        historyHeader
                    } else {
                        historyHeader
                        rows(entries)
                        liveEdgeSentinel
                    }
                }
                .scrollTargetLayout()
                .frame(maxWidth: Theme.Size.readableWidth)
                .frame(maxWidth: .infinity)
                .padding(newestFirst ? .top : .bottom, Theme.Spacing.s)
            }
            .defaultScrollAnchor(newestFirst ? UnitPoint.top : UnitPoint.bottom)
            .scrollDismissesKeyboard(.interactively)
            .modifier(TopRowTracking(isActive: newestFirst, topRowID: $topRowID))
            .overlay(alignment: newestFirst ? .top : .bottom) {
                if !isAtLiveEdge {
                    JumpToLatestPill(unseenCount: unseenCount, edge: newestFirst ? .top : .bottom) {
                        scrollToLiveEdge(proxy, animated: true)
                    }
                    .transition(.opacity.combined(with: .move(edge: newestFirst ? .top : .bottom)))
                }
            }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: isAtLiveEdge)
            .onChange(of: items.last?.id) { oldNewest, newNewest in
                followNewest(oldNewest: oldNewest, newNewest: newNewest, items: items, proxy: proxy)
            }
            .onChange(of: items.first?.id) { oldOldest, _ in
                keepPlaceAfterOlderPage(oldOldest: oldOldest, items: items, proxy: proxy)
            }
            .onChange(of: unreadAnchorID, initial: true) { _, anchor in
                // The social order opens on the newest message, with the
                // unread rows right under it; only the chat order has to
                // scroll back to where they begin.
                guard !newestFirst, let anchor else { return }
                proxy.scrollTo(anchor, anchor: .top)
            }
            .onChange(of: actions.scrollRequest, initial: true) { _, target in
                guard let target else { return }
                // Cleared on the next turn, so a page added in this same
                // update sees the request and leaves the scroll to it.
                Task { @MainActor in actions.scrollRequest = nil }
                withAnimation(reduceMotion ? nil : .easeInOut) {
                    proxy.scrollTo(target, anchor: .center)
                }
                actions.highlight(target)
            }
        }
    }

    private func rows(_ entries: [TimelineFeedEntry]) -> some View {
        ForEach(entries) { entry in
            MessageRow(entry: entry)
                .id(entry.id)
        }
    }

    private var historyHeader: some View {
        TimelineHistoryHeader(
            state: session.history.state(of: conversation),
            header: header,
            onLoadOlder: loadOlder
        )
    }

    /// A 1pt marker at the newest end: while it is laid out, the reader
    /// is at (or within a screen of) the live edge.
    private var liveEdgeSentinel: some View {
        Color.clear
            .frame(height: 1)
            .id(Self.liveEdgeID)
            .onAppear {
                isAtLiveEdge = true
                unseenCount = 0
            }
            .onDisappear {
                isAtLiveEdge = false
            }
    }

    private func followNewest(oldNewest: String?, newNewest: String?, items: [TimelineItem], proxy: ScrollViewProxy) {
        guard let newNewest, newNewest != oldNewest else { return }
        let sentByMe = items.last?.isMine == true
        if isAtLiveEdge || sentByMe {
            scrollToLiveEdge(proxy, animated: oldNewest != nil)
            return
        }
        unseenCount += TimelineUnreadAnchor.arrivals(after: oldNewest, in: items)
        // New rows went in above the reader: put the row they were
        // reading back at the top.
        if newestFirst, let topRowID, topRowID != Self.liveEdgeID {
            restore(topRowID, proxy: proxy)
        }
    }

    /// Chat order: older rows were inserted above, so put the previously
    /// first row back at the top. The social order adds them below the
    /// reader, where nothing moves.
    private func keepPlaceAfterOlderPage(oldOldest: String?, items: [TimelineItem], proxy: ScrollViewProxy) {
        guard !newestFirst, actions.scrollRequest == nil,
              let oldOldest, items.first?.id != oldOldest, items.contains(where: { $0.id == oldOldest })
        else { return }
        restore(oldOldest, proxy: proxy)
    }

    private func restore(_ id: String, proxy: ScrollViewProxy) {
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            proxy.scrollTo(id, anchor: .top)
        }
    }

    private func scrollToLiveEdge(_ proxy: ScrollViewProxy, animated: Bool) {
        unseenCount = 0
        let anchor: UnitPoint = newestFirst ? .top : .bottom
        if animated, !reduceMotion {
            withAnimation(.easeOut(duration: 0.25)) {
                proxy.scrollTo(Self.liveEdgeID, anchor: anchor)
            }
        } else {
            proxy.scrollTo(Self.liveEdgeID, anchor: anchor)
        }
    }

    private func loadOlder() {
        let state = session.history.state(of: conversation)
        // A failed load retries through here too ("Try again").
        guard !state.isLoading, state.failed || (state.hasLoadedLatest && state.hasMoreOlder) else { return }
        Task { await session.loadOlder(conversation) }
    }
}

/// Tracks the row at the top of the viewport, in the social order only;
/// the chat order keeps its place with its own prepend handling.
private struct TopRowTracking: ViewModifier {
    let isActive: Bool
    @Binding var topRowID: String?

    func body(content: Content) -> some View {
        if isActive {
            content.scrollPosition(id: $topRowID, anchor: .top)
        } else {
            content
        }
    }
}
