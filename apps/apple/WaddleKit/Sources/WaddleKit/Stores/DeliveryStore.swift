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
/// outcome arrives. A XEP-0198 ack is proof the server has the stanza, so
/// it wins over a transport failure (the core may have re-sent it); only an
/// error bounce from the recipient overrides an ack.
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

    /// Settles a send. An ack or failure that arrived while the send was
    /// suspended (recorded in `states`, or early before `began`) is kept:
    /// the outcome never downgrades it. Early markers are consumed so a
    /// retry under the same id starts clean.
    public func outcome(_ outcome: SendOutcome, for clientID: String) {
        let current = states[clientID]
        let ackedEarly = consume(clientID, from: &earlyAcks)
        let failedEarly = consume(clientID, from: &earlyFailures)
        let acknowledged = current == .acknowledged || ackedEarly
        switch outcome {
        case .sent:
            if acknowledged {
                states[clientID] = .acknowledged
            } else if current == .failed || failedEarly {
                states[clientID] = .failed
            } else {
                states[clientID] = .sent
            }
        case .notConnected, .transportError:
            // The server acknowledged it, so it is not re-sent.
            states[clientID] = acknowledged ? .acknowledged : .queued
        case .rejected:
            states[clientID] = .failed
        }
    }

    public func acknowledged(_ clientID: String) {
        guard states[clientID] != nil else {
            remember(clientID, in: &earlyAcks)
            return
        }
        states[clientID] = .acknowledged
    }

    public func failed(_ clientID: String) {
        guard let current = states[clientID] else {
            remember(clientID, in: &earlyFailures)
            return
        }
        if current != .acknowledged {
            states[clientID] = .failed
        }
    }

    /// The recipient returned an error for the stanza.
    public func bounced(_ clientID: String) {
        guard states[clientID] != nil else { return }
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

    private func consume(_ id: String, from list: inout [String]) -> Bool {
        guard let index = list.firstIndex(of: id) else { return false }
        list.remove(at: index)
        return true
    }

    private func remember(_ id: String, in list: inout [String]) {
        list.removeAll { $0 == id }
        list.append(id)
        if list.count > cap {
            list.removeFirst(list.count - cap)
        }
    }
}
