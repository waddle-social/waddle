import Foundation

/// What the event listener observed that verb calls consult.
///
/// `discover_topology` answers an empty topology both when discovery
/// failed (reported as an `error` event) and when no session is live, so
/// an empty result alone cannot be trusted.
final class FFISessionSignals: Sendable {
    private struct State: Sendable {
        var isConnected = false
        var topologyFailed = false
    }

    private let state = FFILocked(State())

    var isConnected: Bool {
        state.withLock { $0.isConnected }
    }

    func setConnected(_ connected: Bool) {
        state.withLock { $0.isConnected = connected }
    }

    func beginTopologyDiscovery() {
        state.withLock { $0.topologyFailed = false }
    }

    func recordTopologyFailure() {
        state.withLock { $0.topologyFailed = true }
    }

    /// Whether discovery failed since `beginTopologyDiscovery`; resets.
    func takeTopologyFailure() -> Bool {
        state.withLock { current in
            defer { current.topologyFailed = false }
            return current.topologyFailed
        }
    }
}
