/// What a 1:1 call asks of the device beyond the dock: a ring for a call
/// coming in, the ringback tone for one going out.
public enum CallAlert: Hashable, Sendable {
    case ring(IncomingRing)
    case ringback(sid: String)

    public struct IncomingRing: Hashable, Sendable {
        public let sid: String
        public let from: BareJID
        public let isVideo: Bool
        public init(sid: String, from: BareJID, isVideo: Bool) {
            self.sid = sid
            self.from = from
            self.isVideo = isVideo
        }
    }

    /// A call rings until it is answered, declined or gone. Do Not Disturb
    /// silences the ring only: the dock still shows the call, and a call we
    /// placed ourselves is no disturbance.
    ///
    /// Ringback plays only while a device of theirs says it is ringing and
    /// none has picked up, so the tone never claims more than we know.
    public init?(state: CallState, isDoNotDisturb: Bool) {
        switch state {
        case .incoming(let call) where !call.isAccepting && !isDoNotDisturb:
            self = .ring(IncomingRing(sid: call.sid, from: call.from.bare, isVideo: call.media.video))
        case .outgoing(let call) where call.isRinging && call.responder == nil:
            self = .ringback(sid: call.sid)
        case .idle, .incoming, .outgoing, .active, .joiningGroup, .inGroup, .ended:
            return nil
        }
    }
}

/// How an alert reaches the person, given where the app is.
public struct CallRingPlan: Hashable, Sendable {
    /// The tone to loop, if any.
    public let tone: CallTone?
    /// Post the ring as a notification with Accept and Decline.
    public let notifies: Bool

    public init(tone: CallTone?, notifies: Bool) {
        self.tone = tone
        self.notifies = notifies
    }

    /// Where the system rings for the app (an iPhone or iPad, through its
    /// call screen) a ring needs nothing more. Elsewhere it is heard
    /// wherever the app is, and also seen as a notification when the app
    /// is not in front. Ringback is only ever heard.
    public init(alert: CallAlert?, isAppActive: Bool, systemRings: Bool) {
        switch alert {
        case .ring:
            self.init(tone: systemRings ? nil : .ring, notifies: !systemRings && !isAppActive)
        case .ringback:
            self.init(tone: .ringback, notifies: false)
        case nil:
            self.init(tone: nil, notifies: false)
        }
    }
}
