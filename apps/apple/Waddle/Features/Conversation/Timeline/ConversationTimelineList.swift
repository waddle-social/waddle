import SwiftUI
import WaddleKit

/// The scrolling feed: newest at the bottom, older pages load at the top,
/// and new messages keep the view pinned only when it already sits at the
/// bottom.
struct ConversationTimelineList: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(MessageActionModel.self) private var actions
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    let conversation: ConversationID
    let header: ConversationHeaderText
    let unreadAnchorID: String?

    @State private var isNearBottom = true
    @State private var unseenCount = 0

    private static let bottomID = "timeline-bottom"

    var body: some View {
        let timeline = session.timelines.timeline(for: conversation)
        let entries = TimelineFeedLayout.entries(
            for: timeline.feedItems,
            unreadAnchorID: unreadAnchorID,
            groupingWindow: Theme.groupingWindow,
            replyCount: { timeline.replyCount(for: $0) },
            replyParent: { timeline.item(withID: $0) }
        )
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    TimelineHistoryHeader(
                        state: session.history.state(of: conversation),
                        header: header,
                        onLoadOlder: loadOlder
                    )
                    ForEach(entries) { entry in
                        MessageRow(entry: entry)
                            .id(entry.id)
                    }
                    bottomSentinel
                }
                .frame(maxWidth: Theme.Size.readableWidth)
                .frame(maxWidth: .infinity)
                .padding(.bottom, Theme.Spacing.s)
            }
            .defaultScrollAnchor(.bottom)
            .overlay(alignment: .bottom) {
                if !isNearBottom {
                    JumpToLatestPill(unseenCount: unseenCount) {
                        scrollToBottom(proxy, animated: true)
                    }
                    .transition(.opacity.combined(with: .move(edge: .bottom)))
                }
            }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: isNearBottom)
            .onChange(of: entries.last?.id) { oldLast, newLast in
                followNewest(oldLast: oldLast, newLast: newLast, entries: entries, proxy: proxy)
            }
            .onChange(of: entries.first?.id) { oldFirst, _ in
                keepPlaceAfterPrepend(oldFirst: oldFirst, entries: entries, proxy: proxy)
            }
            .onChange(of: unreadAnchorID, initial: true) { _, anchor in
                guard let anchor else { return }
                proxy.scrollTo(anchor, anchor: .top)
            }
            .onChange(of: actions.scrollRequest, initial: true) { _, target in
                guard let target else { return }
                // Cleared on the next turn, so a page prepended in this same
                // update sees the request and leaves the scroll to it.
                Task { @MainActor in actions.scrollRequest = nil }
                withAnimation(reduceMotion ? nil : .easeInOut) {
                    proxy.scrollTo(target, anchor: .center)
                }
                actions.highlight(target)
            }
        }
    }

    /// A 1pt marker after the last row: while it is laid out, the reader
    /// is at (or within a screen of) the bottom.
    private var bottomSentinel: some View {
        Color.clear
            .frame(height: 1)
            .id(Self.bottomID)
            .onAppear {
                isNearBottom = true
                unseenCount = 0
            }
            .onDisappear {
                isNearBottom = false
            }
    }

    private func followNewest(oldLast: String?, newLast: String?, entries: [TimelineFeedEntry], proxy: ScrollViewProxy) {
        guard let newLast, newLast != oldLast else { return }
        let sentByMe = entries.last?.item.isMine == true
        if isNearBottom || sentByMe {
            scrollToBottom(proxy, animated: oldLast != nil)
        } else {
            unseenCount += TimelineUnreadAnchor.arrivals(after: oldLast, in: entries.map(\.item))
        }
    }

    /// Older rows were inserted above: put the previously first row back
    /// at the top so the reader's place does not jump.
    private func keepPlaceAfterPrepend(oldFirst: String?, entries: [TimelineFeedEntry], proxy: ScrollViewProxy) {
        guard actions.scrollRequest == nil,
              let oldFirst, entries.first?.id != oldFirst, entries.contains(where: { $0.id == oldFirst })
        else { return }
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            proxy.scrollTo(oldFirst, anchor: .top)
        }
    }

    private func scrollToBottom(_ proxy: ScrollViewProxy, animated: Bool) {
        unseenCount = 0
        if animated, !reduceMotion {
            withAnimation(.easeOut(duration: 0.25)) {
                proxy.scrollTo(Self.bottomID, anchor: .bottom)
            }
        } else {
            proxy.scrollTo(Self.bottomID, anchor: .bottom)
        }
    }

    private func loadOlder() {
        let state = session.history.state(of: conversation)
        // A failed load retries through here too ("Try again").
        guard !state.isLoading, state.failed || (state.hasLoadedLatest && state.hasMoreOlder) else { return }
        Task { await session.loadOlder(conversation) }
    }
}
