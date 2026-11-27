/// The player that owns playback, including while its system controls are paused.
public struct InlinePlaybackSelection<Owner: Equatable> {
    public private(set) var current: Owner?

    public init() {}

    /// Returns the previous owner so its playback can be stopped before the new owner plays.
    public mutating func start(_ owner: Owner) -> Owner? {
        guard current != owner else { return nil }
        let previous = current
        current = owner
        return previous
    }

    /// A late dismissal from a displaced row cannot release the new owner.
    public mutating func stop(_ owner: Owner) {
        if current == owner { current = nil }
    }
}

/// The inline player's audio policy while a call may own the audio session.
public struct InlinePlaybackSound {
    private let callIsLive: Bool

    public init(call: CallState) {
        callIsLive = call.isLive
    }

    public var startsMuted: Bool { callIsLive }
    public var configuresPlaybackCategory: Bool { !callIsLive }

    public func pausesPlayback(wasCallLive: Bool, isPlaying: Bool) -> Bool {
        !wasCallLive && callIsLive && isPlaying
    }
}
