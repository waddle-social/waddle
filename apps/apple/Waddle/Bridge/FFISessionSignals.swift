import Foundation
import WaddleKit

/// What the event listener observed that verb calls consult.
///
/// Some verbs (`discover_topology`, `request_avatar`) answer an empty
/// value both when they failed (reported as an `error` event) and when
/// there was genuinely nothing, so an empty result alone cannot be
/// trusted. An error is attributed to its verb by the core's
/// `"<verb> failed"` prefix; anything else counts against every open
/// window. See `VerbErrorWindows`.
final class FFISessionSignals: Sendable {
    private struct State: Sendable {
        var isConnected = false
        var windows = VerbErrorWindows()
    }

    private let state = FFILocked(State())

    var isConnected: Bool {
        state.withLock { $0.isConnected }
    }

    func setConnected(_ connected: Bool) {
        state.withLock { $0.isConnected = connected }
    }

    func beginErrorWindow(_ verb: ErrorScopedVerb) -> VerbErrorWindows.Window {
        state.withLock { $0.windows.begin(verb) }
    }

    func recordError(_ description: String) {
        let verb = ErrorScopedVerb(reporting: description)
        state.withLock { $0.windows.record(from: verb) }
    }

    /// Ends `window`; whether an error reached it.
    func endErrorWindow(_ window: VerbErrorWindows.Window) -> Bool {
        state.withLock { $0.windows.end(window) }
    }
}
