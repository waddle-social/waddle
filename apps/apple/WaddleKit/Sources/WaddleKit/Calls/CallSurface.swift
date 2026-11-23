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
