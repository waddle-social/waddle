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
    /// A trim moved `olderCursor` back while a page was loading; that page
    /// must not move it past the rows the rewind refetches.
    var rewoundDuringLoad = false

    public init() {}
}

@MainActor
@Observable
public final class HistoryStore {
    public private(set) var states: [ConversationID: HistoryState] = [:]
    /// Bumped by `clear()`, so a long-running pager can tell its paging
    /// state was thrown away underneath it.
    @ObservationIgnored private(set) var generation = 0
    @ObservationIgnored private var idleWaiters: [ConversationID: [UUID: CheckedContinuation<Void, Never>]] = [:]

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
        if state.rewoundDuringLoad {
            state.rewoundDuringLoad = false
            state.hasLoadedLatest = state.hasLoadedLatest || wasLatest
        } else if wasLatest {
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
        resumeIdleWaiters(conversation)
    }

    func fail(_ conversation: ConversationID) {
        var state = self.state(of: conversation)
        state.isLoading = false
        state.rewoundDuringLoad = false
        state.failed = true
        states[conversation] = state
        resumeIdleWaiters(conversation)
    }

    /// Returns once no load of `conversation` is running, or at once when
    /// the calling task is cancelled.
    func waitUntilIdle(_ conversation: ConversationID) async {
        let id = UUID()
        await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                guard state(of: conversation).isLoading, !Task.isCancelled else {
                    continuation.resume()
                    return
                }
                idleWaiters[conversation, default: [:]][id] = continuation
            }
        } onCancel: {
            Task { @MainActor in self.resumeIdleWaiter(id, of: conversation) }
        }
    }

    private func resumeIdleWaiter(_ id: UUID, of conversation: ConversationID) {
        idleWaiters[conversation]?.removeValue(forKey: id)?.resume()
    }

    private func resumeIdleWaiters(_ conversation: ConversationID) {
        let waiters = idleWaiters.removeValue(forKey: conversation) ?? [:]
        waiters.values.forEach { $0.resume() }
    }

    /// The timeline dropped archived rows: page older from the oldest row
    /// still loaded, or reload the newest page when no loaded row has an
    /// archive id. A page already in flight keeps the rewound cursor.
    func rewind(_ conversation: ConversationID, toOlderCursor cursor: String?) {
        guard var state = states[conversation] else { return }
        if let cursor {
            state.olderCursor = cursor
            state.hasMoreOlder = true
            state.rewoundDuringLoad = state.isLoading
        } else {
            let isLoading = state.isLoading
            state = HistoryState()
            state.isLoading = isLoading
        }
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
        generation += 1
        for conversation in Array(idleWaiters.keys) {
            resumeIdleWaiters(conversation)
        }
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
