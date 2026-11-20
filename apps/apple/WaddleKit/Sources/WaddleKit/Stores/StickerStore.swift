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

    /// Counts loads; only the newest one's answer is taken.
    @ObservationIgnored private var serial = 0
    /// Packs added and removed here while a load is under way: its answer
    /// may have been written before them.
    @ObservationIgnored private var addedWhileLoading: [StickerPack] = []
    @ObservationIgnored private var removedWhileLoading: Set<String> = []

    public init() {}

    func begin() -> Int {
        serial += 1
        phase = .loading
        addedWhileLoading = []
        removedWhileLoading = []
        return serial
    }

    func finish(_ load: Int, with fetched: [StickerPack]?) {
        guard load == serial else { return }
        if let fetched {
            let kept = fetched.filter { !removedWhileLoading.contains($0.id) }
            let known = Set(kept.map(\.id))
            packs = kept + addedWhileLoading.filter { !known.contains($0.id) }
        }
        phase = fetched == nil ? .failed : .loaded
    }

    func add(_ pack: StickerPack) {
        packs.removeAll { $0.id == pack.id }
        packs.append(pack)
        guard phase == .loading else { return }
        removedWhileLoading.remove(pack.id)
        addedWhileLoading.append(pack)
    }

    func remove(_ packID: String) {
        packs.removeAll { $0.id == packID }
        guard phase == .loading else { return }
        addedWhileLoading.removeAll { $0.id == packID }
        removedWhileLoading.insert(packID)
    }

    func reset() {
        serial += 1
        packs = []
        phase = .idle
    }
}
