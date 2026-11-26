/// When a full-screen call's header and controls show (web
/// `revealImmersiveChrome`): they fade out once the pointer has rested,
/// and a move, a key or a tap brings them back.
///
/// Times are seconds on any steady clock. A move only notes when it was;
/// whoever waits asks `check` when the rest is up, so a pointer crossing
/// the stage changes nothing anyone draws.
public struct CallChromeFade: Equatable, Sendable {
    /// How long the pointer rests before they go, as on the web.
    public static let rest: Double = 2

    public private(set) var isShown = true
    private var lastStir: Double = 0

    public init() { /* shown, nothing stirred yet */ }

    /// The pointer moved, a key went down, or something on the stage was used.
    public mutating func stirred(at seconds: Double) {
        lastStir = seconds
        isShown = true
    }

    /// Asked when a wait is over: hides them if nothing has stirred for a
    /// rest and nothing holds them.
    ///
    /// - Parameter isHeld: something keeps them on screen, a menu or panel
    ///   that is open or the keyboard being among them. It is asked about
    ///   again after a full rest, as the web does.
    /// - Returns: how long to wait before asking again; `nil` once they
    ///   are hidden, when there is nothing to wait for until the next stir.
    public mutating func check(at seconds: Double, isHeld: Bool) -> Double? {
        let remaining = lastStir + Self.rest - seconds
        if remaining > 0 { return remaining }
        if isHeld { return Self.rest }
        isShown = false
        return nil
    }
}
