import Foundation
import Observation

/// XEP-0313 paging state per conversation.
public struct HistoryState: Hashable, Sendable {
    public var hasLoadedLatest = false
    public var isLoading = false
    public var hasMoreOlder = true
    /// RSM `<first/>` of the oldest page loaded; the next `before` cursor.
    public var olderCursor: String?
    public var failed = false

    public init() {}
}

@MainActor
@Observable
public final class HistoryStore {
    public private(set) var states: [ConversationID: HistoryState] = [:]

    public init() {}

    public func state(of conversation: ConversationID) -> HistoryState {
        states[conversation] ?? HistoryState()
    }

    /// Marks a load as started; returns false if one is already running.
    func begin(_ conversation: ConversationID) -> Bool {
        var state = self.state(of: conversation)
        guard !state.isLoading else { return false }
        state.isLoading = true
        state.failed = false
        states[conversation] = state
        return true
    }

    func finish(_ conversation: ConversationID, page: ArchivePage, wasLatest: Bool) {
        var state = self.state(of: conversation)
        state.isLoading = false
        if wasLatest {
            state.hasLoadedLatest = true
            // A refresh of the newest page only moves the older cursor on
            // the first load; later refreshes keep the deeper paging state.
            if state.olderCursor == nil {
                state.olderCursor = page.first
                state.hasMoreOlder = !page.isComplete && page.first != nil
            }
        } else {
            let advanced = page.first != nil && page.first != state.olderCursor
            state.olderCursor = page.first ?? state.olderCursor
            state.hasMoreOlder = !page.isComplete && advanced
        }
        states[conversation] = state
    }

    func fail(_ conversation: ConversationID) {
        var state = self.state(of: conversation)
        state.isLoading = false
        state.failed = true
        states[conversation] = state
    }

    /// Forces a newest-page refetch on next open, keeping paging cursors.
    func markAllStale() {
        for key in Array(states.keys) {
            states[key]?.hasLoadedLatest = false
        }
    }

    public func clear() {
        states.removeAll()
    }
}

/// Our own read position per conversation: the id the last XEP-0333/0490
/// dispatch named. Dedupes dispatches and anchors sibling-device cursors.
@MainActor
final class ReadCursorStore {
    private var cursors: [ConversationID: String] = [:]

    func cursor(_ conversation: ConversationID) -> String? {
        cursors[conversation]
    }

    /// Advances only if the cursor still equals `expected`, so a stale
    /// dispatch never regresses a newer sibling-device cursor.
    @discardableResult
    func advance(_ conversation: ConversationID, from expected: String?, to next: String) -> Bool {
        guard cursors[conversation] == expected else { return false }
        cursors[conversation] = next
        return true
    }

    func clear() {
        cursors.removeAll()
    }
}
