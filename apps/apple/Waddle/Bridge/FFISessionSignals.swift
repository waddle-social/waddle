import Foundation

/// What the event listener observed that verb calls consult.
///
/// `discover_topology` answers an empty topology both when discovery
/// failed (reported as an `error` event) and when no session is live, so
/// an empty result alone cannot be trusted. Any error emitted while a
/// discovery is in flight counts, so no diagnostic wording is parsed.
final class FFISessionSignals: Sendable {
    private struct State: Sendable {
        var isConnected = false
        var isDiscovering = false
        var sawErrorWhileDiscovering = false
    }

    private let state = FFILocked(State())

    var isConnected: Bool {
        state.withLock { $0.isConnected }
    }

    func setConnected(_ connected: Bool) {
        state.withLock { $0.isConnected = connected }
    }

    func beginTopologyDiscovery() {
        state.withLock {
            $0.isDiscovering = true
            $0.sawErrorWhileDiscovering = false
        }
    }

    func recordError() {
        state.withLock { current in
            if current.isDiscovering {
                current.sawErrorWhileDiscovering = true
            }
        }
    }

    /// Ends the discovery window; whether an error was emitted during it.
    func endTopologyDiscovery() -> Bool {
        state.withLock { current in
            defer {
                current.isDiscovering = false
                current.sawErrorWhileDiscovering = false
            }
            return current.sawErrorWhileDiscovering
        }
    }
}
