import Foundation
import Observation

/// Pinned messages per room (`urn:waddle:pin:0`). A snapshot seed never
/// overwrites a live pin event that arrived while the fetch was in flight.
@MainActor
@Observable
public final class PinStore {
    public private(set) var pins: [BareJID: [PinEntry]] = [:]

    @ObservationIgnored private var version: [BareJID: Int] = [:]

    public init() {}

    public func pins(in room: BareJID) -> [PinEntry] {
        pins[room] ?? []
    }

    public func isPinned(_ stanzaID: String, in room: BareJID) -> Bool {
        pins[room]?.contains { $0.targetStanzaID == stanzaID } == true
    }

    /// Captures the event version before a snapshot fetch starts.
    public func versionBeforeFetch(_ room: BareJID) -> Int {
        version[room] ?? 0
    }

    /// Seeds from a snapshot unless live events arrived since `fetchedAt`.
    public func seed(_ entries: [PinEntry], in room: BareJID, fetchedAt: Int) {
        guard (version[room] ?? 0) == fetchedAt else { return }
        pins[room] = entries
    }

    public func apply(_ event: PinEvent, in room: BareJID, at date: Date = Date()) {
        version[room, default: 0] += 1
        var list = pins[room] ?? []
        list.removeAll { $0.targetStanzaID == event.targetStanzaID }
        if event.action == .pinned {
            let preview = event.preview ?? PinPreview(author: nil, authorNick: nil, text: "", messageTimestamp: nil)
            list.insert(PinEntry(targetStanzaID: event.targetStanzaID, pinner: event.by, pinnedAt: date, preview: preview), at: 0)
        }
        pins[room] = list
    }

    public func clear() {
        pins.removeAll()
        version.removeAll()
    }
}
