import Foundation

public enum CallVideoSource: Hashable, Sendable {
    case camera, screen
}

public struct CallStageTile: Equatable, Identifiable, Sendable {
    public let identity: String
    public let source: CallVideoSource
    public let label: String
    public let isSelf: Bool
    public let isMuted: Bool
    public let isHandRaised: Bool
    public let isSpeaking: Bool
    public let hasVideo: Bool
    /// A new publication is a new share, even when it reuses the tile.
    public let screenPublicationID: String?

    public struct ID: Hashable, Sendable {
        public let identity: String
        public let source: CallVideoSource
    }

    /// The full identity distinguishes devices; the source distinguishes their tiles.
    public var id: ID { ID(identity: identity, source: source) }
    /// Display names and badges belong to the account behind the device.
    public var bareJID: BareJID? { JID(parsing: identity)?.bare }

    fileprivate var isSelfCamera: Bool { isSelf && source == .camera }

    public init(
        identity: String, source: CallVideoSource, label: String,
        isSelf: Bool = false, isMuted: Bool = false, isHandRaised: Bool = false,
        isSpeaking: Bool = false, hasVideo: Bool = false, screenPublicationID: String? = nil
    ) {
        self.identity = identity
        self.source = source
        self.label = label
        self.isSelf = isSelf
        // The camera tile represents the person; the screen tile is their content.
        self.isMuted = source == .camera && isMuted
        self.isHandRaised = source == .camera && isHandRaised
        self.isSpeaking = source == .camera && isSpeaking
        self.hasVideo = hasVideo
        self.screenPublicationID = screenPublicationID
    }
}

public struct CallStagePresentation: Equatable, Sendable {
    public enum ViewMode: Equatable, Sendable {
        case gallery, speaker
    }

    public var viewMode: ViewMode
    public var pinnedTileID: CallStageTile.ID?
    public var isSelfViewHidden: Bool
    private var seenRemoteScreens: [ScreenKey] = []

    private struct ScreenKey: Hashable, Sendable {
        let tileID: CallStageTile.ID
        let publicationID: String?

        init?(_ tile: CallStageTile) {
            guard tile.source == .screen, !tile.isSelf, tile.hasVideo else { return nil }
            tileID = tile.id
            publicationID = tile.screenPublicationID
        }
    }

    public init(viewMode: ViewMode = .gallery, pinnedTileID: CallStageTile.ID? = nil, isSelfViewHidden: Bool = false) {
        self.viewMode = viewMode
        self.pinnedTileID = pinnedTileID
        self.isSelfViewHidden = isSelfViewHidden
    }

    /// Record share arrivals and clear a pin when its tile leaves or self-view hides it.
    /// Call with the full roster when tiles or self-view change. Skip when the roster
    /// is unavailable, such as during a reconnect, so a temporary gap keeps the pin.
    public mutating func reconcile(with tiles: [CallStageTile]) {
        if let pinnedTileID, !tiles.contains(where: { $0.id == pinnedTileID && shows($0) }) {
            self.pinnedTileID = nil
        }
        var seen = Set(seenRemoteScreens)
        for tile in tiles {
            guard let key = ScreenKey(tile), seen.insert(key).inserted else { continue }
            seenRemoteScreens.append(key)
        }
    }

    fileprivate func shows(_ tile: CallStageTile) -> Bool {
        !isSelfViewHidden || !tile.isSelfCamera
    }

    fileprivate func newestRemoteScreen(in tiles: [CallStageTile]) -> CallStageTile? {
        var active: [ScreenKey: CallStageTile] = [:]
        for tile in tiles {
            if let key = ScreenKey(tile) { active[key] = tile }
        }
        return seenRemoteScreens.reversed().lazy.compactMap { active[$0] }.first
    }
}

public enum CallStageLayout: Equatable, Sendable {
    case grid(columns: Int, rows: Int, tiles: [CallStageTile], hiddenCount: Int)
    case spotlight(main: CallStageTile, strip: [CallStageTile])

    /// Lay out the available tiles using the pin and recorded share order.
    /// The cell cap applies to the gallery; the spotlight strip scrolls.
    public static func make(
        tiles: [CallStageTile], presentation: CallStagePresentation,
        promoted: String? = nil, size: CGSize, maxTiles: Int = .max
    ) -> Self {
        let selfView = tiles.filter { $0.isSelfCamera && presentation.shows($0) }
        let ordered = selfView + tiles.filter { !$0.isSelfCamera }
        let pinned = ordered.first { $0.id == presentation.pinnedTileID }
        let main = presentation.newestRemoteScreen(in: ordered)
            ?? pinned
            ?? (presentation.viewMode == .speaker ? speakerTile(in: ordered, promoted: promoted) : nil)
        if let main {
            return .spotlight(main: main, strip: ordered.filter { $0.id != main.id })
        }
        return gallery(ordered, size: size, maxTiles: maxTiles)
    }

    private static func speakerTile(in tiles: [CallStageTile], promoted: String?) -> CallStageTile? {
        let cameras = tiles.filter { $0.source == .camera }
        return cameras.first { $0.identity == promoted } ?? cameras.first { !$0.isSelf } ?? cameras.first
    }

    private static func gallery(_ tiles: [CallStageTile], size: CGSize, maxTiles: Int) -> Self {
        let limit = max(1, maxTiles)
        let grid = CallGrid.make(tileCount: min(tiles.count, limit), size: size)
        let capacity = min(limit, grid.capacity)
        var visible = tiles.prefix(capacity)
        var hiddenCount = 0
        if capacity > 1, tiles.count > capacity {
            let withOverflow = visible.dropLast()
            // A person with any tile on stage is already visible.
            hiddenCount = Set(tiles.map(\.identity)).subtracting(withOverflow.map(\.identity)).count
            if hiddenCount > 0 { visible = withOverflow }
        }
        return .grid(columns: grid.columns, rows: grid.rows, tiles: Array(visible), hiddenCount: hiddenCount)
    }
}
