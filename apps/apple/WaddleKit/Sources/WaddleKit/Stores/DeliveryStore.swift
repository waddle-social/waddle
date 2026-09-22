import Foundation
import Observation

/// Delivery state of an own send, keyed by its client stanza id.
public enum DeliveryState: Hashable, Sendable {
    /// Handed to the core; not yet written.
    case sending
    /// Offline; will be re-sent when the session is ready.
    case queued
    /// Written to the stream.
    case sent
    /// XEP-0198 acknowledged by the server.
    case acknowledged
    case failed
}

/// Tracks the lifecycle of own sends. Ack and failure events can beat the
/// send's own continuation, so both are remembered (bounded) until the
/// outcome arrives; failure wins over an ack.
@MainActor
@Observable
public final class DeliveryStore {
    public private(set) var states: [String: DeliveryState] = [:]

    @ObservationIgnored private var earlyAcks: [String] = []
    @ObservationIgnored private var earlyFailures: [String] = []
    @ObservationIgnored private let cap = 256

    public init() {}

    public func state(of clientID: String) -> DeliveryState? {
        states[clientID]
    }

    public func began(_ clientID: String) {
        states[clientID] = .sending
    }

    public func queued(_ clientID: String) {
        states[clientID] = .queued
    }

    public func outcome(_ outcome: SendOutcome, for clientID: String) {
        switch outcome {
        case .sent:
            if earlyFailures.contains(clientID) {
                states[clientID] = .failed
            } else if earlyAcks.contains(clientID) {
                states[clientID] = .acknowledged
            } else {
                states[clientID] = .sent
            }
        case .notConnected, .transportError:
            states[clientID] = .queued
        case .rejected:
            states[clientID] = .failed
        }
    }

    public func acknowledged(_ clientID: String) {
        guard let current = states[clientID] else {
            remember(clientID, in: &earlyAcks)
            return
        }
        if current != .failed {
            states[clientID] = .acknowledged
        }
    }

    public func failed(_ clientID: String) {
        guard states[clientID] != nil else {
            remember(clientID, in: &earlyFailures)
            return
        }
        states[clientID] = .failed
    }

    public func forget(_ clientID: String) {
        states[clientID] = nil
    }

    public func clear() {
        states.removeAll()
        earlyAcks.removeAll()
        earlyFailures.removeAll()
    }

    private func remember(_ id: String, in list: inout [String]) {
        list.removeAll { $0 == id }
        list.append(id)
        if list.count > cap {
            list.removeFirst(list.count - cap)
        }
    }
}
