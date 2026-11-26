import Foundation

public enum CallSurface: Equatable, Sendable {
    case dock, sidebarCard, split, expanded, fullscreen
    /// A phone's stage: over everything, once the person has opened it from the dock.
    case cover

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

    /// An established call can take over only its own conversation. On a
    /// phone it stays in the dock until the person opens it, and then
    /// covers the screen whatever conversation is open.
    public static func placement(
        isInSession: Bool, conversationIsOpen: Bool, platform: Platform, size: Size, isOpened: Bool = false
    ) -> Self {
        guard isInSession else { return .dock }
        guard platform == .regularWidth else { return isOpened ? .cover : .dock }
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

    /// The call has a stage of its own in the pane of its conversation.
    var showsStage: Bool {
        switch self {
        case .split, .expanded, .fullscreen: true
        case .dock, .sidebarCard, .cover: false
        }
    }

    /// One surface draws a call's video and one set of controls is in reach:
    /// the shell's dock yields to a stage and to the sidebar's card, and a
    /// sheet's dock, which covers them, keeps only the controls of a call in session.
    func dockContent(isInSession: Bool, isInSheet: Bool) -> DockContent {
        if isInSheet { return isInSession ? .controls : .call }
        return self == .dock ? .call : .hidden
    }

    /// Where the stage stands in the pane it shares with its conversation.
    enum SplitArrangement: Equatable, Sendable {
        case beside(width: CGFloat)
        case above(height: CGFloat)
        /// The whole pane: the conversation waits behind an expanded or full-screen stage.
        case filling
    }

    /// The web huddle column, and the narrowest a stage beside a conversation gets.
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
    ///
    /// - Parameter percent: the share of the pane the person dragged the
    ///   stage to, along whichever axis the two share; `nil` until they do.
    ///   It sizes a stage and never costs a pane the one it could hold.
    static func splitArrangement(in pane: CGSize, percent: Double? = nil) -> SplitArrangement? {
        let share = percent.map { clampSplitPercent($0) / 100 }
        if pane.width - stageWidth >= minimumConversationWidth {
            guard pane.height >= minimumStageHeight else { return nil }
            let width = share.map { min(max(pane.width * $0, stageWidth), pane.width - minimumConversationWidth) }
            return .beside(width: width ?? stageWidth)
        }
        guard pane.height * defaultSplitPercent / 100 >= minimumStageHeight else { return nil }
        return .above(height: max(pane.height * (share ?? defaultSplitPercent / 100), minimumStageHeight))
    }

    /// The share of the pane a stage `extent` long takes of the `paneExtent` it is dragged along.
    static func splitPercent(stage extent: CGFloat, of paneExtent: CGFloat) -> Double {
        paneExtent > 0 ? clampSplitPercent(extent / paneExtent * 100) : defaultSplitPercent
    }

    /// Where a stage of `size` stands in `pane`, or `nil` when the pane cannot hold one.
    static func stageArrangement(for size: Size, in pane: CGSize, splitPercent: Double?) -> SplitArrangement? {
        switch size {
        case .split: splitArrangement(in: pane, percent: splitPercent)
        case .expanded, .immersive: pane.height >= minimumStageHeight ? .filling : nil
        }
    }
}

public extension CallSurface.Platform {
    /// A phone has no pane to share with a stage however it is held: a large
    /// one is regular width in landscape and still has no room for one. Its
    /// call is in the dock, or covers the screen (`cover`).
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
    /// - Parameter isSidebarShown: the sidebar is on screen to hold the card
    ///   of a call whose conversation is not open; otherwise the dock keeps it.
    static func placement(
        of state: CallState, open selection: ConversationID?, platform: Platform, pane: CGSize?,
        size: Size = .defaultSize, isOpened: Bool = false, isSidebarShown: Bool = true
    ) -> Self {
        let conversation = state.sessionConversation
        let surface = placement(
            isInSession: conversation != nil, conversationIsOpen: conversation != nil && conversation == selection,
            platform: platform, size: size, isOpened: isOpened
        )
        if surface.showsStage, let pane, stageArrangement(for: size, in: pane, splitPercent: nil) == nil { return .dock }
        if surface == .sidebarCard, !isSidebarShown { return .dock }
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
