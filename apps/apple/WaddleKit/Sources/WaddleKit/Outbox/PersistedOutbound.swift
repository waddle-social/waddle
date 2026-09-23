import Foundation

/// One unconfirmed own send as the outbox keeps it across app launches.
public struct PersistedOutbound: Hashable, Sendable, Codable {
    public enum State: String, Hashable, Sendable, Codable {
        /// Queued, in flight, or written but not yet confirmed by the
        /// server: re-sent under the same client id on the next ready
        /// session.
        case pending
        /// The send failed; it waits for the user to retry or discard it.
        case failed
    }

    public let message: OutboundMessage
    /// When the message was composed: the local echo's display time and
    /// its order among restored echoes.
    public let createdAt: Date
    public let state: State

    public init(message: OutboundMessage, createdAt: Date, state: State) {
        self.message = message
        self.createdAt = createdAt
        self.state = state
    }
}
