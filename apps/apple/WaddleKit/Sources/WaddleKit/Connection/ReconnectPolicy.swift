import Foundation

/// Exponential backoff with full jitter for stream reconnects: attempt `n`
/// waits a uniformly random time in `[base, min(cap, base · 2ⁿ)]`, so a
/// fleet of clients that dropped together does not reconnect in lockstep.
public struct ReconnectPolicy: Sendable {
    public let base: TimeInterval
    public let cap: TimeInterval

    public init(base: TimeInterval = 1, cap: TimeInterval = 60) {
        self.base = base
        self.cap = cap
    }

    /// Delay before reconnect attempt `attempt` (0-based). `unit` is a
    /// uniform sample in `[0, 1)`, injected so tests are deterministic.
    public func delay(forAttempt attempt: Int, unit: Double) -> TimeInterval {
        let exponent = min(max(attempt, 0), 30)
        let ceiling = min(cap, base * pow(2, Double(exponent)))
        let clamped = min(max(unit, 0), 1)
        return base + (ceiling - base) * clamped
    }
}

/// What the UI shows about the live session.
public enum ConnectionStatus: Equatable, Sendable {
    case signedOut
    case connecting
    case online
    /// Waiting to retry; `retryAt` is when the next attempt starts.
    case offline(retryAt: Date?)
    /// The server rejected the credential; the user must sign in again.
    case authenticationFailed
}
