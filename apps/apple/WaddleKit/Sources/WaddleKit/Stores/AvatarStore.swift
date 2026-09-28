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
    public static let maxCachedBytes = 4 * 1024 * 1024
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
    /// JIDs with held bytes, least recently used first.
    @ObservationIgnored private var useOrder: [BareJID] = []
    @ObservationIgnored private var queue: [BareJID] = []
    @ObservationIgnored private var inFlight: [BareJID: Ticket] = [:]
    /// Bumped per JID by events and local writes that supersede a lookup.
    @ObservationIgnored private var changes: [BareJID: Int] = [:]
    @ObservationIgnored private var session = 0
    @ObservationIgnored private let now: () -> Date
    @ObservationIgnored private let cacheBudget: Int

    public init(now: @escaping () -> Date = Date.init, maxCachedBytes: Int = AvatarStore.maxCachedBytes) {
        self.now = now
        self.cacheBudget = max(0, maxCachedBytes)
    }

    public func image(for jid: BareJID) -> AvatarImage? {
        guard let image = images[jid] else { return nil }
        markUsed(jid)
        return image
    }

    /// A row showing `jid` rendered: look it up if nothing current is held.
    public func request(_ jid: BareJID) {
        if images[jid] != nil { markUsed(jid) }
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
            useOrder.removeAll { $0 == jid }
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

    /// Records an avatar known locally (our own, just published or removed).
    /// `id` is its XEP-0084 item id, so revalidation transfers no data
    /// while it is unchanged.
    public func set(_ jid: BareJID, image: AvatarImage?, id: String?) {
        changes[jid, default: 0] += 1
        queue.removeAll { $0 == jid }
        if let image {
            remember(image, id: id, for: jid)
        } else {
            images[jid] = nil
            useOrder.removeAll { $0 == jid }
            entries[jid] = Entry(id: nil, dueAt: now().addingTimeInterval(Self.retryAfter))
        }
    }

    public func clear() {
        session += 1
        generation += 1
        images.removeAll()
        entries.removeAll()
        useOrder.removeAll()
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
            remember(image, id: id, for: jid)
        case .unchanged where images[jid] != nil:
            entries[jid] = Entry(id: current?.id, dueAt: now().addingTimeInterval(Self.revalidateAfter))
        case .unchanged:
            // Its bytes were evicted during the known-id lookup. An id-only
            // answer cannot restore them, so the next render fetches data.
            entries[jid] = nil
        case .absent:
            images[jid] = nil
            useOrder.removeAll { $0 == jid }
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

    private func markUsed(_ jid: BareJID) {
        useOrder.removeAll { $0 == jid }
        useOrder.append(jid)
    }

    /// Keep only encoded bytes within the session budget. Evicted JIDs lose
    /// their known id too; an id-only revalidation cannot redraw the image.
    private func remember(_ image: AvatarImage, id: String?, for jid: BareJID) {
        images[jid] = nil
        useOrder.removeAll { $0 == jid }
        guard image.data.count <= cacheBudget else {
            entries[jid] = Entry(id: nil, dueAt: now().addingTimeInterval(Self.retryAfter))
            return
        }
        var heldBytes = images.values.reduce(0) { $0 + $1.data.count }
        while heldBytes + image.data.count > cacheBudget, !useOrder.isEmpty {
            let evicted = useOrder.removeFirst()
            if let old = images.removeValue(forKey: evicted) {
                heldBytes -= old.data.count
            }
            entries[evicted] = nil
        }
        images[jid] = image
        entries[jid] = Entry(id: id, dueAt: now().addingTimeInterval(Self.revalidateAfter))
        markUsed(jid)
    }
}
