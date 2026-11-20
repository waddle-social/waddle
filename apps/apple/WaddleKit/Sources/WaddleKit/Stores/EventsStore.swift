import Foundation
import Observation

/// Observable state of the community calendar.
@MainActor
@Observable
public final class EventsStore {
    /// In the server's order; views ask for occurrences instead.
    public private(set) var events: [CommunityEvent] = []
    public private(set) var isLoading = false
    /// At least one refresh finished since the session started.
    public private(set) var hasLoaded = false
    /// When the last refresh finished.
    public private(set) var loadedAt: Date?
    /// The last refresh could not fetch the events.
    public private(set) var failed = false
    /// The account may create, edit and cancel events.
    public private(set) var canManage = false

    @ObservationIgnored private let account: BareJID
    @ObservationIgnored private var serial = 0
    /// Events saved (or, for nil, removed) here, with the refresh number at
    /// the time: a refresh that began earlier does not know of the change.
    @ObservationIgnored private var changedHere: [(id: String, event: CommunityEvent?, during: Int)] = []
    /// Counts this device's reply changes per event, so a slower answer
    /// does not undo a newer change.
    @ObservationIgnored private var replyEdits: [String: Int] = [:]
    /// The reply being sent per event; a refresh keeps showing it.
    @ObservationIgnored private var unsentReplies: [String: (reply: EventReply, writes: Int)] = [:]
    /// What the server last agreed to, for events with a reply being sent.
    @ObservationIgnored private var confirmedReplies: [String: EventReply?] = [:]
    /// Replies the server took, with the refresh number at the time: a
    /// refresh that began earlier was answered without them.
    @ObservationIgnored private var repliesSentHere: [(id: String, reply: EventReply, during: Int)] = []
    /// The newest reply write of each event; the next one waits for it.
    @ObservationIgnored private var replyWrites: [String: Task<Void, any Error>] = [:]

    public init(account: BareJID) {
        self.account = account
    }

    public func event(_ id: String) -> CommunityEvent? {
        events.first { $0.id == id }
    }

    /// The signed-in user's reply, as shown.
    public func myReply(to eventID: String) -> EventReply? {
        event(eventID)?.reply(of: account)
    }

    /// Events not given a date yet, by title.
    public var undated: [CommunityEvent] {
        events.filter { $0.start == nil }.sorted { ($0.title, $0.id) < ($1.title, $1.id) }
    }

    // MARK: Refresh

    /// Starts a refresh; results carrying an older serial are dropped.
    func begin() -> Int {
        serial += 1
        isLoading = true
        return serial
    }

    func isCurrent(_ refresh: Int) -> Bool {
        refresh == serial
    }

    /// The answer of refresh `refresh`. Changes made here after it began
    /// are kept, and so is a reply still being sent.
    func publish(events fetched: [CommunityEvent], refresh: Int) {
        changedHere.removeAll { $0.during < refresh }
        var list = fetched
        for change in changedHere {
            replace(change.id, with: change.event, in: &list)
        }
        repliesSentHere.removeAll { $0.during < refresh }
        events = list.map { event in
            let sent = repliesSentHere.last { $0.id == event.id }?.reply
            return (unsentReplies[event.id]?.reply ?? sent).map { event.replying($0, by: account) } ?? event
        }
    }

    func finish(failed: Bool, canManage: Bool, at now: Date = Date()) {
        isLoading = false
        hasLoaded = true
        loadedAt = now
        self.failed = failed
        self.canManage = canManage
    }

    /// Ends a refresh that could not ask anything.
    func cancel(_ refresh: Int) {
        guard isCurrent(refresh) else { return }
        isLoading = false
    }

    // MARK: Changes made here

    /// The event as the server stored it, in place of the one shown.
    func save(_ event: CommunityEvent) {
        let shown = unsentReplies[event.id].map { event.replying($0.reply, by: account) } ?? event
        replace(event.id, with: shown, in: &events)
        // As the server stored it: a reply still being sent may be refused,
        // and a refresh must not bring it back.
        changedHere.append((event.id, event, serial))
    }

    func remove(_ eventID: String) {
        replace(eventID, with: nil, in: &events)
        changedHere.append((eventID, nil, serial))
    }

    private func replace(_ id: String, with event: CommunityEvent?, in list: inout [CommunityEvent]) {
        if let index = list.firstIndex(where: { $0.id == id }) {
            if let event { list[index] = event } else { list.remove(at: index) }
        } else if let event {
            list.append(event)
        }
    }

    /// A reply made on this device, shown before it is sent; returns its
    /// edit number. Pair with `sent` or `undo`.
    func edit(_ reply: EventReply, of eventID: String) -> Int {
        let edit = (replyEdits[eventID] ?? 0) + 1
        replyEdits[eventID] = edit
        if unsentReplies[eventID] == nil {
            confirmedReplies[eventID] = .some(myReply(to: eventID))
        }
        unsentReplies[eventID] = (reply, (unsentReplies[eventID]?.writes ?? 0) + 1)
        show(reply, on: eventID)
        return edit
    }

    /// The server took `reply`.
    func sent(_ reply: EventReply, of eventID: String) {
        confirmedReplies[eventID] = .some(reply)
        repliesSentHere.append((eventID, reply, serial))
        settle(eventID)
    }

    /// The server refused edit `edit`: back to what it last agreed to,
    /// unless a newer reply is on its way.
    func undo(_ edit: Int, of eventID: String) {
        let confirmed = confirmedReplies[eventID]
        settle(eventID)
        guard replyEdits[eventID] == edit, let confirmed else { return }
        show(confirmed, on: eventID)
    }

    private func settle(_ eventID: String) {
        guard let unsent = unsentReplies[eventID] else { return }
        if unsent.writes > 1 {
            unsentReplies[eventID] = (unsent.reply, unsent.writes - 1)
        } else {
            unsentReplies[eventID] = nil
            confirmedReplies[eventID] = nil
            replyWrites[eventID] = nil
        }
    }

    private func show(_ reply: EventReply?, on eventID: String) {
        guard let index = events.firstIndex(where: { $0.id == eventID }) else { return }
        events[index] = events[index].replying(reply, by: account)
    }

    /// Runs `write` after the event's earlier reply writes, so the server
    /// ends up with the last reply chosen.
    func afterEarlierReplyWrites(
        of eventID: String,
        _ write: @escaping @Sendable () async throws -> Void
    ) -> Task<Void, any Error> {
        let earlier = replyWrites[eventID]
        let task = Task {
            _ = await earlier?.result
            try await write()
        }
        replyWrites[eventID] = task
        return task
    }

    /// Sign-out or a fresh session: drop everything, including answers
    /// still in flight.
    func reset() {
        serial += 1
        events = []
        changedHere = []
        replyEdits = [:]
        unsentReplies = [:]
        repliesSentHere = []
        confirmedReplies = [:]
        replyWrites = [:]
        loadedAt = nil
        isLoading = false
        hasLoaded = false
        failed = false
        canManage = false
    }
}
