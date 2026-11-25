/// The keys of a call's stage (web `call-shortcuts.ts`): one bare letter
/// each, with no modifier, so they are for when nobody is typing.
/// Escape, which steps the stage down a size, is the stage's own.
public enum CallShortcut: CaseIterable, Equatable, Sendable {
    case microphone, camera, screenShare, hand, stepUp

    public var key: Character {
        switch self {
        case .microphone: "m"
        case .camera: "v"
        case .screenShare: "s"
        case .hand: "r"
        case .stepUp: "f"
        }
    }

    /// What the key does, for a list of shortcuts.
    public var title: String {
        switch self {
        case .microphone: "Mute or unmute"
        case .camera: "Turn camera on or off"
        case .screenShare: "Share or stop sharing your screen"
        case .hand: "Raise or lower your hand"
        case .stepUp: "Make the call bigger"
        }
    }

    public func action(size: CallSurface.Size) -> CallStageAction {
        switch self {
        case .microphone: .toggleMicrophone
        case .camera: .toggleCamera
        case .screenShare: .toggleScreenShare
        case .hand: .toggleHand
        case .stepUp: .setSize(size.steppedUp)
        }
    }

    /// The keys that do something on a stage as it is now; the others are
    /// left alone, for whatever else may want them.
    public static func available(for controls: CallControls) -> [CallShortcut] {
        allCases.filter { shortcut in
            switch shortcut {
            case .microphone, .camera: controls.devices.hasMedia
            case .screenShare: controls.devices.hasMedia && controls.devices.isSharingScreen != nil
            case .hand: controls.isHandRaised != nil
            // Full screen is as big as a call gets.
            case .stepUp: controls.size != .immersive
            }
        }
    }
}

/// A key held down repeats, and a repeat must not toggle anything again
/// (web: `input.repeat`). For a keyboard that does not say which presses
/// are repeats, an iPad's: a press of the same key that follows the last
/// one this closely is taken for one. A Mac's keyboard says, and is believed.
public struct CallShortcutRepeats: Equatable, Sendable {
    /// Longer than the longest wait before a held key starts repeating.
    public static let window: Double = 0.7

    private var last: CallShortcut?
    private var lastAt: Double = -.infinity

    public init() { /* nothing pressed yet */ }

    /// Whether a press at `seconds`, on any steady clock, is one of its own.
    /// A repeat extends the wait, so a key held for a minute is one press.
    public mutating func isFreshPress(of shortcut: CallShortcut, at seconds: Double) -> Bool {
        let isRepeat = shortcut == last && seconds - lastAt < Self.window
        last = shortcut
        lastAt = seconds
        return !isRepeat
    }
}

/// Holding Space to talk (web `installCallShortcuts`): the microphone is
/// open for as long as the key is down, and closed again when it comes up.
///
/// Only a microphone that was closed is opened this way. Someone already
/// talking who rests a thumb on Space is not muted when they lift it.
public struct CallPushToTalk: Equatable, Sendable {
    /// What a press of Space is.
    public struct Press: Equatable, Sendable {
        /// The system repeating a key held down.
        public var isRepeat = false
        /// Command, Control or Option is down: a chord, somebody else's.
        public var hasModifiers = false
        /// The keyboard is somewhere of its own: a field being typed in, or
        /// a button or list that Space presses or scrolls.
        public var keyboardIsElsewhere = false
        /// Media has connected: there is a microphone to open.
        public var hasMedia = true
        public var isMicrophoneOn = false

        public init(
            isRepeat: Bool = false, hasModifiers: Bool = false, keyboardIsElsewhere: Bool = false,
            hasMedia: Bool = true, isMicrophoneOn: Bool = false
        ) {
            self.isRepeat = isRepeat
            self.hasModifiers = hasModifiers
            self.keyboardIsElsewhere = keyboardIsElsewhere
            self.hasMedia = hasMedia
            self.isMicrophoneOn = isMicrophoneOn
        }
    }

    public enum Outcome: Equatable, Sendable {
        /// Not ours: the key goes on to whatever else wants it.
        case passOn
        /// Ours, and nothing to do: a repeat of the key we are holding.
        case swallow
        case openMicrophone
        case closeMicrophone
    }

    /// The microphone is open because Space is down.
    public private(set) var isHolding = false

    public init() { /* nothing held */ }

    public mutating func spaceWentDown(_ press: Press) -> Outcome {
        // Ours for as long as it is held, wherever the keyboard has gone since.
        if isHolding { return .swallow }
        guard !press.hasModifiers, !press.keyboardIsElsewhere, !press.isRepeat,
              press.hasMedia, !press.isMicrophoneOn else { return .passOn }
        isHolding = true
        return .openMicrophone
    }

    /// The key came up. A microphone we opened is always closed, whatever
    /// has happened to the keyboard meanwhile: it must not be left open.
    public mutating func spaceCameUp() -> Outcome {
        release() ? .closeMicrophone : .passOn
    }

    /// The window lost the keyboard, or the stage went away, with the key
    /// still down: its coming up will not be seen here. True when the
    /// microphone is to be closed.
    public mutating func release() -> Bool {
        guard isHolding else { return false }
        isHolding = false
        return true
    }
}
