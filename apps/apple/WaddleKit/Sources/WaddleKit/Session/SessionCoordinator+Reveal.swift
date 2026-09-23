import Foundation

/// How looking for a message in a conversation's history ended.
public enum RevealOutcome: Hashable, Sendable {
    /// The row is in the timeline under this `TimelineItem.id`.
    case found(itemID: String)
    /// The archive's start was reached without it.
    case notFound
    /// The page budget ran out, or the search was cancelled, first.
    case gaveUp
    /// A page failed to load, or the history was reset meanwhile.
    case failed
}

extension SessionCoordinator {
    /// Pages XEP-0313 history back until `messageID` (any id the row
    /// answers to, as `ConversationTimeline.item(withID:)` resolves it) is
    /// in the timeline: the newest page first if it is not loaded, then
    /// older pages. Shares the paging state with scroll-triggered loads, so
    /// it never runs a second request beside one already in flight; it
    /// waits for that one instead, and only its own requests count against
    /// `pageBudget`. The default pages at most three fifths of the
    /// timeline's live-trim capacity, so a revealed row survives the live
    /// messages that follow instead of being trimmed from under the reader.
    public func reveal(messageID: String, in conversation: ConversationID, pageBudget: Int? = nil) async -> RevealOutcome {
        let generation = history.generation
        var pagesLeft = pageBudget ?? defaultRevealPageBudget
        while true {
            if let item = timelines.timeline(for: conversation).item(withID: messageID) {
                return .found(itemID: item.id)
            }
            guard history.generation == generation else { return .failed }
            guard !Task.isCancelled else { return .gaveUp }
            let state = history.state(of: conversation)
            if state.isLoading {
                await history.waitUntilIdle(conversation)
                continue
            }
            if state.hasLoadedLatest, !state.hasMoreOlder {
                return .notFound
            }
            guard pagesLeft > 0 else { return .gaveUp }
            pagesLeft -= 1
            guard await loadPageForReveal(conversation, state: state) else { return .failed }
        }
    }

    /// Loads the next page toward the archive's start; false when it
    /// failed.
    private func loadPageForReveal(_ conversation: ConversationID, state: HistoryState) async -> Bool {
        if state.hasLoadedLatest {
            await loadOlder(conversation)
        } else {
            await loadLatest(conversation)
        }
        return !history.state(of: conversation).failed
    }

    var defaultRevealPageBudget: Int {
        max(1, timelines.capacity * 3 / 5 / Self.historyPageSize)
    }
}
