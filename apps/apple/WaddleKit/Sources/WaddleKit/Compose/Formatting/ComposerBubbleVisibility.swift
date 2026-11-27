/// Visibility for one editor. A temporary blocker hides the bubble without
/// discarding its selection. Typing and Escape invalidate that selection.
public struct ComposerBubbleVisibility {
    public struct Suppression: OptionSet, Sendable {
        public let rawValue: UInt8

        public init(rawValue: UInt8) { self.rawValue = rawValue }

        public static let linkPrompt = Self(rawValue: 1 << 0)
        public static let formattingBar = Self(rawValue: 1 << 1)
        public static let suggestions = Self(rawValue: 1 << 2)
    }

    public enum Event: Sendable {
        case selectionChanged(ComposerScalarRange?)
        case mouseDown
        case mouseUp
        case pauseElapsed
        case typed
        case markedTextChanged(Bool)
        case focusChanged(Bool)
        case windowKeyChanged(Bool)
        case suppressionChanged(Suppression)
        case formatApplied(ComposerScalarRange?)
        case escape
    }

    private enum Phase {
        case idle, waiting, settled, dismissed
    }

    public private(set) var suppression: Suppression = []
    private var phase: Phase = .idle
    private var selection: ComposerScalarRange?
    private var isTrackingMouse = false
    private var hasMarkedText = false
    private var isFocused = false
    private var isWindowKey = false

    public init() {}

    public var pendingSelection: ComposerScalarRange? {
        phase == .waiting && !isTrackingMouse ? selection : nil
    }

    public var visibleSelection: ComposerScalarRange? {
        guard phase == .settled, !isTrackingMouse, !hasMarkedText, isFocused, isWindowKey, suppression.isEmpty
        else { return nil }
        return selection
    }

    public mutating func reduce(_ event: Event) {
        switch event {
        case let .selectionChanged(range):
            let next = Self.nonempty(range)
            guard selection != next else { return }
            selection = next
            phase = next == nil ? .idle : .waiting
        case .mouseDown:
            isTrackingMouse = true
            if phase != .dismissed { phase = .idle }
        case .mouseUp:
            isTrackingMouse = false
            if phase != .dismissed { phase = .settled }
        case .pauseElapsed:
            if pendingSelection != nil { phase = .settled }
        case .typed:
            if phase != .dismissed { phase = .idle }
        case .escape:
            phase = .dismissed
        case let .markedTextChanged(marked):
            hasMarkedText = marked
        case let .focusChanged(focused):
            isFocused = focused
        case let .windowKeyChanged(key):
            isWindowKey = key
        case let .suppressionChanged(active):
            suppression = active
        case let .formatApplied(range):
            selection = Self.nonempty(range)
            if phase != .dismissed { phase = .settled }
        }
    }

    private static func nonempty(_ range: ComposerScalarRange?) -> ComposerScalarRange? {
        range.flatMap { $0.isEmpty ? nil : $0 }
    }
}
