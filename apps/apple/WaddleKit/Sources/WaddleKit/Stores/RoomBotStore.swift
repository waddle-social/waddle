import Foundation
import Observation

/// The bots each room declares, as the server last listed them.
@MainActor
@Observable
public final class RoomBotStore {
    /// Room → its declared bots, sorted by display name.
    public private(set) var bots: [BareJID: [RoomBot]] = [:]

    @ObservationIgnored private var issued = 0
    /// Room → the newest refresh begun; an older answer is dropped.
    @ObservationIgnored private var latest: [BareJID: Int] = [:]

    public init() {}

    public func bots(in room: BareJID) -> [RoomBot] {
        bots[room] ?? []
    }

    /// Whether `jid` is one of the bots `room` declares.
    public func isDeclared(_ jid: BareJID, in room: BareJID) -> Bool {
        bots[room]?.contains { $0.jid == jid } ?? false
    }

    /// Starts a refresh of `room`. Answers can arrive out of order, so
    /// only the newest refresh may `replace`.
    func beginRefresh(in room: BareJID) -> Int {
        issued += 1
        latest[room] = issued
        return issued
    }

    func replace(_ listed: [RoomBot], in room: BareJID, ticket: Int) {
        guard latest[room] == ticket else { return }
        bots[room] = listed.sorted { $0.displayName.localizedCaseInsensitiveCompare($1.displayName) == .orderedAscending }
    }

    public func clear() {
        bots.removeAll()
        latest.removeAll()
    }
}
