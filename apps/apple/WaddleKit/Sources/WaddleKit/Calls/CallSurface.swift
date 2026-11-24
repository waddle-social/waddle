import Foundation

public enum CallSurface: Equatable, Sendable {
    case dock, sidebarCard, split, expanded, fullscreen

    public enum Size: String, Sendable {
        case split, expanded, immersive

        public static let defaultSize: Self = .split

        /// Immersive hides the app chrome and requires an explicit choice.
        public static func restored(from stored: String?) -> Self {
            stored == expanded.rawValue ? .expanded : defaultSize
        }

        public var steppedUp: Self { self == .expanded ? .immersive : .expanded }
        public var steppedDown: Self { self == .immersive ? .expanded : .split }
        public var afterFullscreenExit: Self { self == .immersive ? .expanded : self }
    }

    public enum Platform: Equatable, Sendable {
        case phone, regularWidth
    }

    /// An established call can take over only its own conversation.
    public static func placement(
        isInSession: Bool, conversationIsOpen: Bool, platform: Platform, size: Size
    ) -> Self {
        guard platform == .regularWidth, isInSession else { return .dock }
        guard conversationIsOpen else { return .sidebarCard }
        switch size {
        case .split: return .split
        case .expanded: return .expanded
        case .immersive: return .fullscreen
        }
    }

    public static let defaultSplitPercent = 50.0

    /// Leave enough room for both the call and the conversation.
    public static func clampSplitPercent(_ percent: Double) -> Double {
        percent.isFinite ? min(75, max(25, percent)) : defaultSplitPercent
    }
}

// MARK: - Who draws the call

public extension CallSurface {
    /// What a call dock draws while this surface has the call.
    enum DockContent: Equatable, Sendable {
        /// The whole call, video included.
        case call
        /// The controls alone: something else on screen draws the video.
        case controls
        case hidden
    }

    /// The call has a stage of its own. Expanded and full screen draw as the split until they are built.
    var showsStage: Bool {
        switch self {
        case .split, .expanded, .fullscreen: true
        case .dock, .sidebarCard: false
        }
    }

    /// One surface draws a call's video and one set of controls is in reach:
    /// the shell's dock yields to a stage, and a sheet's dock, which covers
    /// whatever has the video, keeps only the controls of a call in session.
    func dockContent(isInSession: Bool, isInSheet: Bool) -> DockContent {
        if isInSheet { return isInSession ? .controls : .call }
        return showsStage ? .hidden : .call
    }

    /// Where the stage stands in the pane it shares with its conversation.
    enum SplitArrangement: Equatable, Sendable {
        case beside(width: CGFloat)
        case above(height: CGFloat)
    }

    /// The web huddle column.
    static let stageWidth: CGFloat = 400
    /// The narrowest conversation the stage may stand beside.
    static let minimumConversationWidth: CGFloat = 480
    /// The shortest stage worth having: its header (62), one row of tiles
    /// at the height the grid gives a row before it adds a second (180) on
    /// the tile area's padding (18), and one row of controls (72).
    static let minimumStageHeight: CGFloat = 332

    /// Beside the conversation while that leaves it room to read; otherwise
    /// above it, sharing the height. `nil` when the pane is too short for
    /// either: a strip of a stage is worse than the dock.
    static func splitArrangement(in pane: CGSize) -> SplitArrangement? {
        let arrangement: SplitArrangement = pane.width - stageWidth >= minimumConversationWidth
            ? .beside(width: stageWidth)
            : .above(height: pane.height * defaultSplitPercent / 100)
        let height = switch arrangement {
        case .beside: pane.height
        case .above(let height): height
        }
        return height >= minimumStageHeight ? arrangement : nil
    }
}

public extension CallSurface.Platform {
    /// A phone keeps every call in the dock however it is held: a large
    /// one is regular width in landscape and still has no room for a stage.
    /// Anything else squeezed to a compact size is treated as one.
    init(isPhone: Bool, isCompactWidth: Bool, isCompactHeight: Bool) {
        self = isPhone || isCompactWidth || isCompactHeight ? .phone : .regularWidth
    }
}

public extension CallSurface {
    /// Where the slot's call shows while `selection` is the open conversation.
    ///
    /// - Parameter pane: the pane the conversation and a stage would share,
    ///   once it has been laid out. A call whose stage it cannot hold stays in the dock.
    static func placement(
        of state: CallState, open selection: ConversationID?, platform: Platform, pane: CGSize?,
        size: Size = .defaultSize
    ) -> Self {
        let conversation = state.sessionConversation
        let surface = placement(
            isInSession: conversation != nil, conversationIsOpen: conversation != nil && conversation == selection,
            platform: platform, size: size
        )
        if surface.showsStage, let pane, splitArrangement(in: pane) == nil { return .dock }
        return surface
    }
}

public extension CallState {
    /// The conversation an established call belongs to: the only one it can take a stage beside.
    var sessionConversation: ConversationID? {
        switch self {
        case .active(let call): .direct(call.peer.bare)
        case .inGroup(let call): .room(call.room)
        case .idle, .incoming, .outgoing, .joiningGroup, .ended: nil
        }
    }
}
