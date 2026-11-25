/// The line under the title of the sidebar's current-call card (web
/// `CurrentCallPanel`): how the call is doing, what kind it is, and who is in it.
public enum CallCardSummary {
    public typealias Status = CallSessionStatus

    public enum Kind: Equatable, Sendable {
        case direct(CallMedia)
        /// A room's call with `count` people in it, ourselves included.
        case room(count: Int)
    }

    public static func line(status: Status, kind: Kind) -> String {
        let state = switch status {
        case .connecting: "Connecting…"
        case .connected: "Connected"
        case .reconnecting: "Reconnecting…"
        }
        let (type, people) = switch kind {
        case .direct(let media): (media.video ? "Video call" : "Voice call", "1:1")
        // We are in it, whatever the room has heard so far.
        case .room(let count): ("Group call", "\(max(count, 1)) in call")
        }
        return [state, type, people].joined(separator: " · ")
    }
}
