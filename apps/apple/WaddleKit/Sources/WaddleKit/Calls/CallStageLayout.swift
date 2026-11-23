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
    }

    public init(viewMode: ViewMode = .gallery, pinnedTileID: CallStageTile.ID? = nil, isSelfViewHidden: Bool = false) {
        self.viewMode = viewMode
        self.pinnedTileID = pinnedTileID
        self.isSelfViewHidden = isSelfViewHidden
    }

    /// Remember arrival order so a redraw or reordered roster cannot change
    /// which ongoing share is featured. A new arrival takes the stage first.
    fileprivate mutating func newestRemoteScreen(in tiles: [CallStageTile]) -> CallStageTile? {
        var active: [ScreenKey: CallStageTile] = [:]
        var seen = Set(seenRemoteScreens)
        var firstNew: CallStageTile?
        for tile in tiles where tile.source == .screen && !tile.isSelf && tile.hasVideo {
            let key = ScreenKey(tileID: tile.id, publicationID: tile.screenPublicationID)
            active[key] = tile
            if seen.insert(key).inserted {
                seenRemoteScreens.append(key)
                if firstNew == nil { firstNew = tile }
            }
        }
        return firstNew ?? seenRemoteScreens.reversed().lazy.compactMap { active[$0] }.first
    }
}

public enum CallStageLayout: Equatable, Sendable {
    case grid(columns: Int, rows: Int, tiles: [CallStageTile], hiddenCount: Int)
    case spotlight(main: CallStageTile, strip: [CallStageTile])

    /// Reconcile the pin and share history with the current visible tiles.
    /// The cell cap applies to the gallery; the spotlight strip scrolls.
    public static func make(
        tiles: [CallStageTile], presentation: inout CallStagePresentation,
        promoted: String? = nil, size: CGSize, maxTiles: Int = .max
    ) -> Self {
        let selfView = presentation.isSelfViewHidden ? [] : tiles.filter(\.isSelfCamera)
        let ordered = selfView + tiles.filter { !$0.isSelfCamera }
        let pinned = ordered.first { $0.id == presentation.pinnedTileID }
        presentation.pinnedTileID = pinned?.id
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
