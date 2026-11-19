/// Closed-set call outcomes contain no peer or session identifiers.
public struct CallLifecycleReport: Hashable, Sendable {
    public enum Kind: Hashable, Sendable { case direct, group }
    public enum SetupOutcome: Hashable, Sendable { case proposed, accepted, declined, timeout, failed }
    public enum EndReason: Hashable, Sendable { case hangup, peerLeft, error, reconnectExhausted }
    public enum DurationBucket: Hashable, Sendable { case none, underOneMinute, oneToTenMinutes, tenToSixtyMinutes, overSixtyMinutes }
    public enum ReconnectBucket: Hashable, Sendable { case none, once, multiple }

    public let kind: Kind
    public let setupOutcome: SetupOutcome
    public let endReason: EndReason
    public let duration: DurationBucket
    public let reconnects: ReconnectBucket

    public init(kind: Kind, setupOutcome: SetupOutcome, endReason: EndReason, duration: DurationBucket, reconnects: ReconnectBucket) {
        self.kind = kind
        self.setupOutcome = setupOutcome
        self.endReason = endReason
        self.duration = duration
        self.reconnects = reconnects
    }
}

public enum CallLifecycleReportResult: Hashable, Sendable {
    case accepted, unsupported, rateLimited, failed
}
