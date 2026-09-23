import Foundation

/// What the event listener observed that verb calls consult.
///
/// `discover_topology` answers an empty topology both when discovery
/// failed (reported as an `error` event) and when no session is live, so
/// an empty result alone cannot be trusted. Any error emitted while a
/// discovery is in flight counts, so no diagnostic wording is parsed.
/// Each discovery has its own window, so overlapping calls (a reconnect
/// while an earlier discovery is still running) cannot clear each other.
final class FFISessionSignals: Sendable {
    struct DiscoveryToken: Hashable, Sendable {
        fileprivate let value: UInt64
    }

    private struct State: Sendable {
        var isConnected = false
        var nextDiscovery: UInt64 = 0
        /// In-flight discoveries and whether an error arrived during each.
        var discoveries: [UInt64: Bool] = [:]
    }

    private let state = FFILocked(State())

    var isConnected: Bool {
        state.withLock { $0.isConnected }
    }

    func setConnected(_ connected: Bool) {
        state.withLock { $0.isConnected = connected }
    }

    func beginTopologyDiscovery() -> DiscoveryToken {
        state.withLock { current in
            current.nextDiscovery += 1
            current.discoveries[current.nextDiscovery] = false
            return DiscoveryToken(value: current.nextDiscovery)
        }
    }

    func recordError() {
        state.withLock { current in
            for key in Array(current.discoveries.keys) {
                current.discoveries[key] = true
            }
        }
    }

    /// Ends `token`'s window; whether an error was emitted during it.
    func endTopologyDiscovery(_ token: DiscoveryToken) -> Bool {
        state.withLock { current in
            current.discoveries.removeValue(forKey: token.value) ?? false
        }
    }
}
