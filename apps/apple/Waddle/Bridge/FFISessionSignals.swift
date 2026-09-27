import Foundation

/// What the event listener observed that verb calls consult.
///
/// Some verbs (`discover_topology`, `request_avatar`) answer an empty
/// value both when they failed (reported as an `error` event) and when
/// there was genuinely nothing, so an empty result alone cannot be
/// trusted. Any error emitted while such a call is in flight counts, so no
/// diagnostic wording is parsed; an unrelated error only makes the caller
/// treat an empty answer as a failure. Each call has its own window, so
/// overlapping calls cannot clear each other.
final class FFISessionSignals: Sendable {
    struct ErrorWindow: Hashable, Sendable {
        fileprivate let value: UInt64
    }

    private struct State: Sendable {
        var isConnected = false
        var nextWindow: UInt64 = 0
        /// Open windows and whether an error arrived during each.
        var windows: [UInt64: Bool] = [:]
    }

    private let state = FFILocked(State())

    var isConnected: Bool {
        state.withLock { $0.isConnected }
    }

    func setConnected(_ connected: Bool) {
        state.withLock { $0.isConnected = connected }
    }

    func beginErrorWindow() -> ErrorWindow {
        state.withLock { current in
            current.nextWindow += 1
            current.windows[current.nextWindow] = false
            return ErrorWindow(value: current.nextWindow)
        }
    }

    func recordError() {
        state.withLock { current in
            for key in Array(current.windows.keys) {
                current.windows[key] = true
            }
        }
    }

    /// Ends `token`'s window; whether an error was emitted during it.
    func endErrorWindow(_ token: ErrorWindow) -> Bool {
        state.withLock { current in
            current.windows.removeValue(forKey: token.value) ?? false
        }
    }
}
