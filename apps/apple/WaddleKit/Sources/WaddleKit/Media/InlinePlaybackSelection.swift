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
    private enum CallMute {
        case pending, imposed, overridden
    }

    private var liveCallID: String?
    private var callMute = CallMute.pending
    private var userMuted = false

    public init() {}

    public var isMuted: Bool { userMuted || callMute == .imposed }
    public var configuresPlaybackCategory: Bool { liveCallID == nil }

    /// Returns whether a newly live call interrupted playback.
    public mutating func updateCall(_ call: CallState, isPlaying: Bool) -> Bool {
        let callID = call.isLive ? call.sid : nil
        guard callID != liveCallID else { return false }
        liveCallID = callID
        let shouldPause = callID != nil && isPlaying
        callMute = shouldPause ? .imposed : .pending
        return shouldPause
    }

    public mutating func preparePlayback(call: CallState) {
        _ = updateCall(call, isPlaying: false)
        if liveCallID != nil && callMute == .pending { callMute = .imposed }
    }

    public mutating func setMuted(_ muted: Bool) {
        userMuted = muted
        if !muted && liveCallID != nil { callMute = .overridden }
    }
}
