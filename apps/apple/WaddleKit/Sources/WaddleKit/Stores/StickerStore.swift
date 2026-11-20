import Foundation
import Observation

/// The account's own sticker packs.
@MainActor
@Observable
public final class StickerStore {
    public enum Phase: Hashable, Sendable {
        /// Not asked for since the session started.
        case idle
        case loading
        case loaded
        /// The last load failed; `packs` holds what was shown before.
        case failed
    }

    public private(set) var packs: [StickerPack] = []
    public private(set) var phase: Phase = .idle

    /// Counts loads and changes, so an answer asked for before a pack was
    /// added or removed here does not undo that.
    @ObservationIgnored private var serial = 0

    public init() {}

    func begin() -> Int {
        serial += 1
        phase = .loading
        return serial
    }

    func finish(_ load: Int, with packs: [StickerPack]?) {
        guard load == serial else { return }
        if let packs {
            self.packs = packs
        }
        phase = packs == nil ? .failed : .loaded
    }

    func add(_ pack: StickerPack) {
        settle()
        packs.removeAll { $0.id == pack.id }
        packs.append(pack)
    }

    func remove(_ packID: String) {
        settle()
        packs.removeAll { $0.id == packID }
    }

    func reset() {
        serial += 1
        packs = []
        phase = .idle
    }

    /// A change made here outdates any load still under way.
    private func settle() {
        serial += 1
        phase = .loaded
    }
}
