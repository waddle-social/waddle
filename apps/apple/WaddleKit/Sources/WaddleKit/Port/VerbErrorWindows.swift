import Foundation

/// Verbs whose empty answer is ambiguous: the core returns nothing both
/// when there was nothing and when the call failed, reporting the failure
/// only as an out-of-band error.
public enum ErrorScopedVerb: Hashable, Sendable {
    case topology
    case avatar

    /// The verb whose failure a core error `description` reports, from the
    /// prefixes `discover_topology` and `request_avatar` emit; nil when it
    /// names neither.
    public init?(reporting description: String) {
        if description.hasPrefix("discover_topology failed") {
            self = .topology
        } else if description.hasPrefix("request_avatar failed") || description.hasPrefix("Invalid JID for avatar fetch") {
            self = .avatar
        } else {
            return nil
        }
    }
}

/// Tells a port adapter whether an error was reported while a call was in
/// flight. Each call has its own window, so overlapping calls cannot clear
/// each other. An error attributed to one verb reaches only that verb's
/// windows (an avatar lookup failing during a reconnect must not make an
/// empty topology look failed); an unattributed error reaches every
/// window, since it could belong to any of them.
public struct VerbErrorWindows: Sendable {
    public struct Window: Hashable, Sendable {
        fileprivate let id: UInt64
        fileprivate let verb: ErrorScopedVerb
    }

    private var nextID: UInt64 = 0
    /// Open windows and whether an error reached each.
    private var open: [Window: Bool] = [:]

    public init() {}

    public mutating func begin(_ verb: ErrorScopedVerb) -> Window {
        nextID += 1
        let window = Window(id: nextID, verb: verb)
        open[window] = false
        return window
    }

    /// `verb` is who reported the error, nil when unknown.
    public mutating func record(from verb: ErrorScopedVerb?) {
        for window in open.keys where verb == nil || window.verb == verb {
            open[window] = true
        }
    }

    /// Closes `window`; whether an error reached it.
    public mutating func end(_ window: Window) -> Bool {
        open.removeValue(forKey: window) ?? false
    }
}
