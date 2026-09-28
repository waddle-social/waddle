import Foundation
import Observation

/// XEP-0084 avatars keyed by bare JID.
///
/// Lookups are lazy (a row asks for its JID when it renders), shared per
/// JID, and at most `maxConcurrentLookups` run at once. An avatar is
/// revalidated after `revalidateAfter`, passing the id it holds so an
/// unchanged avatar transfers no data (XEP-0084 §4.2); a miss or failure
/// is retried after `retryAfter`. `markAllStale()` (a new session) and
/// `avatarChanged(_:id:)` (a PEP notification) force the next lookup.
@MainActor
@Observable
public final class AvatarStore {
    public typealias Lookup = @MainActor (_ jid: BareJID, _ knownID: String?) async -> AvatarFetch

    public static let maxConcurrentLookups = 4
    public static let revalidateAfter: TimeInterval = 45 * 60
    public static let retryAfter: TimeInterval = 10 * 60

    public private(set) var images: [BareJID: AvatarImage] = [:]
    /// Changes when every avatar became stale, so rows on screen ask again.
    public private(set) var generation = 0

    /// Runs one lookup; set by the session.
    @ObservationIgnored public var fetch: Lookup?

    private struct Entry {
        /// The XEP-0084 id of the held image, when known.
        var id: String?
        var dueAt: Date
        var isStale = false
    }

    /// What a lookup started under, to tell whether its answer still applies.
    private struct Ticket {
        let session: Int
        let generation: Int
        let change: Int
    }

    @ObservationIgnored private var entries: [BareJID: Entry] = [:]
    @ObservationIgnored private var queue: [BareJID] = []
    @ObservationIgnored private var inFlight: [BareJID: Ticket] = [:]
    /// Bumped per JID by events and local writes that supersede a lookup.
    @ObservationIgnored private var changes: [BareJID: Int] = [:]
    @ObservationIgnored private var session = 0
    @ObservationIgnored private let now: () -> Date

    public init(now: @escaping () -> Date = Date.init) {
        self.now = now
    }

    public func image(for jid: BareJID) -> AvatarImage? {
        images[jid]
    }

    /// A row showing `jid` rendered: look it up if nothing current is held.
    public func request(_ jid: BareJID) {
        guard isDue(jid) else { return }
        queue.append(jid)
        drain()
    }

    /// A XEP-0084 metadata notification: `id` is the newly advertised
    /// avatar, nil when the peer disabled it. JIDs never shown are left to
    /// their first render.
    public func avatarChanged(_ jid: BareJID, id: String?) {
        guard entries[jid] != nil || inFlight[jid] != nil || queue.contains(jid) else { return }
        guard let id else {
            changes[jid, default: 0] += 1
            queue.removeAll { $0 == jid }
            images[jid] = nil
            entries[jid] = Entry(id: nil, dueAt: now().addingTimeInterval(Self.retryAfter))
            return
        }
        if inFlight[jid] == nil, images[jid] != nil, entries[jid]?.id == id, entries[jid]?.isStale == false {
            return
        }
        changes[jid, default: 0] += 1
        entries[jid]?.isStale = true
        // A lookup in flight may predate the change; `finish` asks again.
        guard inFlight[jid] == nil else { return }
        request(jid)
    }

    /// A new session: whatever is held may be out of date.
    public func markAllStale() {
        generation += 1
        for jid in entries.keys {
            entries[jid]?.isStale = true
        }
    }

    /// Records an avatar known locally (our own, just published or removed),
    /// with its item id so revalidation transfers no data while unchanged.
    public func set(_ jid: BareJID, image: AvatarImage?) {
        changes[jid, default: 0] += 1
        queue.removeAll { $0 == jid }
        images[jid] = image
        let wait = image == nil ? Self.retryAfter : Self.revalidateAfter
        entries[jid] = Entry(id: image?.itemID, dueAt: now().addingTimeInterval(wait))
    }

    public func clear() {
        session += 1
        generation += 1
        images.removeAll()
        entries.removeAll()
        queue.removeAll()
        inFlight.removeAll()
        changes.removeAll()
    }

    private func isDue(_ jid: BareJID) -> Bool {
        guard inFlight[jid] == nil, !queue.contains(jid) else { return false }
        guard let entry = entries[jid] else { return true }
        return entry.isStale || now() >= entry.dueAt
    }

    private func drain() {
        guard let fetch else { return }
        while inFlight.count < Self.maxConcurrentLookups, !queue.isEmpty {
            let jid = queue.removeFirst()
            let ticket = Ticket(session: session, generation: generation, change: changes[jid, default: 0])
            inFlight[jid] = ticket
            let knownID = images[jid] == nil ? nil : entries[jid]?.id
            Task { [weak self] in
                let result = await fetch(jid, knownID)
                self?.finish(jid, result, ticket: ticket)
            }
        }
    }

    private func finish(_ jid: BareJID, _ result: AvatarFetch, ticket: Ticket) {
        guard ticket.session == session else { return }
        inFlight[jid] = nil
        defer { drain() }
        guard ticket.change == changes[jid, default: 0] else {
            // Superseded while in flight: a change with a new id asks again.
            if isDue(jid) { queue.append(jid) }
            return
        }
        let current = entries[jid]
        switch result {
        case let .published(id, image):
            images[jid] = image
            entries[jid] = Entry(id: id, dueAt: now().addingTimeInterval(Self.revalidateAfter))
        case .unchanged where images[jid] != nil:
            entries[jid] = Entry(id: current?.id, dueAt: now().addingTimeInterval(Self.revalidateAfter))
        case .unchanged, .absent:
            images[jid] = nil
            entries[jid] = Entry(id: nil, dueAt: now().addingTimeInterval(Self.retryAfter))
        case .failed:
            // Keep what is shown; try again sooner.
            entries[jid] = Entry(id: current?.id, dueAt: now().addingTimeInterval(Self.retryAfter))
        }
        if ticket.generation != generation {
            // Started before the reconnect: its row's own request after the
            // reconnect was absorbed by this lookup, so ask again now.
            entries[jid]?.isStale = true
            queue.append(jid)
        }
    }
}
