import Foundation
import Observation

/// One fetchable overview section: a room's feed or one of its threads, at
/// a given unread state. A fetched section is reused until its count or
/// newest stanza id changes, so a refresh refetches only what moved.
struct UnreadOverviewSection: Hashable, Sendable {
    let room: BareJID
    let threadID: String?
    let unread: Int
    let lastStanzaID: String?
}

/// Observable state of the Activity overview.
@MainActor
@Observable
public final class UnreadOverviewStore {
    public private(set) var groups: [UnreadOverviewGroup] = []
    public private(set) var isLoading = false
    /// At least one refresh finished since the session started.
    public private(set) var hasLoaded = false
    /// Every fetch of the last refresh failed.
    public private(set) var didFail = false

    @ObservationIgnored private var serial = 0
    @ObservationIgnored private(set) var cache: [UnreadOverviewSection: [TimelineItem]] = [:]

    public init() {}

    /// Starts a refresh; results carrying an older serial are dropped.
    func begin() -> Int {
        serial += 1
        isLoading = true
        return serial
    }

    func isCurrent(_ refresh: Int) -> Bool {
        refresh == serial
    }

    func publish(_ groups: [UnreadOverviewGroup], refresh: Int) {
        guard isCurrent(refresh) else { return }
        self.groups = groups
    }

    func cached(_ section: UnreadOverviewSection) -> [TimelineItem]? {
        cache[section]
    }

    func remember(_ messages: [TimelineItem], for section: UnreadOverviewSection, refresh: Int) {
        guard isCurrent(refresh) else { return }
        cache[section] = messages
    }

    /// Ends a refresh and forgets sections no longer shown.
    func finish(refresh: Int, keeping sections: Set<UnreadOverviewSection>, didFail: Bool) {
        guard isCurrent(refresh) else { return }
        cache = cache.filter { sections.contains($0.key) }
        isLoading = false
        hasLoaded = true
        self.didFail = didFail
    }

    /// Sign-out or a fresh session: drop everything, including answers
    /// still in flight.
    func reset() {
        serial += 1
        groups = []
        cache = [:]
        isLoading = false
        hasLoaded = false
        didFail = false
    }
}
