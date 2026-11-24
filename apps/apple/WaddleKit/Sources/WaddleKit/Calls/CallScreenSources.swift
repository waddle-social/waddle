import Foundation

/// Something on this Mac that can be shared in a call: a whole screen or one window.
public struct CallScreenSource: Equatable, Identifiable, Sendable {
    public enum Kind: Equatable, Sendable {
        case screen, window
    }

    /// Stable while the screen or window exists: its kind and system number.
    public let id: String
    public let kind: Kind
    /// "Entire screen", "Screen 2", or the window's title.
    public let title: String
    /// The screen's size, or the application the window belongs to.
    public let detail: String
}

/// What the system reports can be captured, before it is sorted and named.
public struct CallScreenCandidates: Equatable, Sendable {
    public struct Screen: Equatable, Sendable {
        public let number: UInt32
        public let width: Int
        public let height: Int
        public let isMain: Bool

        public init(number: UInt32, width: Int, height: Int, isMain: Bool) {
            self.number = number
            self.width = width
            self.height = height
            self.isMain = isMain
        }
    }

    public struct Window: Equatable, Sendable {
        public let number: UInt32
        public let title: String?
        public let application: String?
        public let bundleID: String?
        /// 0 for an ordinary window; menus, the Dock and overlays are above it.
        public let layer: Int
        public let isOnScreen: Bool
        public let width: Double
        public let height: Double

        public init(
            number: UInt32, title: String?, application: String?, bundleID: String?, layer: Int = 0,
            isOnScreen: Bool = true, width: Double = 800, height: Double = 600
        ) {
            self.number = number
            self.title = title
            self.application = application
            self.bundleID = bundleID
            self.layer = layer
            self.isOnScreen = isOnScreen
            self.width = width
            self.height = height
        }
    }

    public var screens: [Screen]
    public var windows: [Window]

    public init(screens: [Screen] = [], windows: [Window] = []) {
        self.screens = screens
        self.windows = windows
    }
}

/// The list a person picks from to share their screen.
public struct CallScreenSources: Equatable, Sendable {
    public let screens: [CallScreenSource]
    public let windows: [CallScreenSource]

    public var isEmpty: Bool { screens.isEmpty && windows.isEmpty }

    /// The smallest window worth offering: anything less is a palette or a stray panel.
    static let minimumWindowSide = 120.0

    /// The main screen first, then the others in the system's order. The
    /// windows are the ordinary ones on screen that have a title, by
    /// application and then title. Our own windows are left out: sharing
    /// the call's window into the call shows a hall of mirrors.
    public init(_ candidates: CallScreenCandidates, ownBundleID: String?) {
        let ordered = candidates.screens.filter(\.isMain) + candidates.screens.filter { !$0.isMain }
        screens = ordered.enumerated().map { index, screen in
            CallScreenSource(
                id: "screen:\(screen.number)", kind: .screen,
                title: ordered.count == 1 ? "Entire screen" : "Screen \(index + 1)",
                detail: "\(screen.width) × \(screen.height)"
            )
        }
        windows = candidates.windows
            .filter { window in
                window.layer == 0 && window.isOnScreen
                    && window.width >= Self.minimumWindowSide && window.height >= Self.minimumWindowSide
                    && !(window.title ?? "").trimmingCharacters(in: .whitespaces).isEmpty
                    && (ownBundleID == nil || window.bundleID != ownBundleID)
            }
            .map { window in
                CallScreenSource(
                    id: "window:\(window.number)", kind: .window,
                    title: (window.title ?? "").trimmingCharacters(in: .whitespaces),
                    detail: window.application ?? ""
                )
            }
            .sorted { lhs, rhs in
                let byApplication = lhs.detail.localizedCaseInsensitiveCompare(rhs.detail)
                if byApplication != .orderedSame { return byApplication == .orderedAscending }
                let byTitle = lhs.title.localizedCaseInsensitiveCompare(rhs.title)
                // The system number last, so two windows of one name keep their places.
                return byTitle == .orderedSame ? lhs.id < rhs.id : byTitle == .orderedAscending
            }
    }

    /// The system number behind a source's `id`, for the capturer.
    public static func number(of id: String, kind: CallScreenSource.Kind) -> UInt32? {
        let prefix = kind == .screen ? "screen:" : "window:"
        return id.hasPrefix(prefix) ? UInt32(id.dropFirst(prefix.count)) : nil
    }
}
