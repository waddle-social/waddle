import Foundation

/// A call going on in a room we are in and are not part of, for the
/// sidebar's "Live now" list (web `CallActivityDock`).
public struct LiveCall: Identifiable, Equatable, Sendable {
    /// What choosing the call does.
    public enum Action: Equatable, Sendable {
        /// Joins the call and opens its room.
        case join
        /// Opens its room only: we are in another call, or offline.
        case open

        public var title: String {
            switch self {
            case .join: "Join"
            case .open: "Open"
            }
        }
    }

    public let room: BareJID
    /// The room's name.
    public let title: String
    /// Who is in it, one row a person.
    public let people: [GroupCallRosterRow]
    public let hasVideo: Bool
    public let action: Action

    public var id: BareJID { room }

    public init(room: BareJID, title: String, people: [GroupCallRosterRow], hasVideo: Bool, action: Action) {
        self.room = room
        self.title = title
        self.people = people
        self.hasVideo = hasVideo
        self.action = action
    }

    /// "Group call · 3 people"
    public var meta: String {
        "\(hasVideo ? "Group video call" : "Group call") · \(headcount)"
    }

    /// "3 people": what is left of `meta` where the whole of it does not fit.
    public var headcount: String {
        people.count == 1 ? "1 person" : "\(people.count) people"
    }

    /// "Ada, Linus +2": the first names, for whoever cannot see the faces.
    public func preview(naming limit: Int = 2) -> String {
        let names = people.map(\.nick)
        guard names.count > limit else { return names.joined(separator: ", ") }
        return "\(names.prefix(limit).joined(separator: ", ")) +\(names.count - limit)"
    }

    /// The whole card in a sentence.
    public var spoken: String {
        "\(action.title) \(title), \(meta), \(preview()) in call"
    }
}

public enum LiveCalls {
    /// A room with a call, as the session knows it.
    public struct Room: Equatable, Sendable {
        public let room: BareJID
        public let title: String
        public let people: [GroupCallRosterRow]
        public let hasVideo: Bool
        /// A call can be joined there now: we are online, in the room, and in no other call.
        public let canJoin: Bool

        public init(room: BareJID, title: String, people: [GroupCallRosterRow], hasVideo: Bool, canJoin: Bool) {
            self.room = room
            self.title = title
            self.people = people
            self.hasVideo = hasVideo
            self.canJoin = canJoin
        }
    }

    /// The calls to list while the call slot is in `state`: every room's
    /// call but our own, which has its own card, with those we can walk
    /// into first and then by name.
    public static func list(_ rooms: [Room], state: CallState) -> [LiveCall] {
        rooms
            .filter { !$0.people.isEmpty && !state.isInGroupCall(in: $0.room) }
            .map {
                LiveCall(
                    room: $0.room, title: $0.title, people: $0.people, hasVideo: $0.hasVideo,
                    action: $0.canJoin ? .join : .open
                )
            }
            .sorted { left, right in
                if left.action != right.action { return left.action == .join }
                let order = left.title.localizedStandardCompare(right.title)
                if order != .orderedSame { return order == .orderedAscending }
                return left.room.description < right.room.description
            }
    }

    /// "Live now · 2"
    public static func heading(count: Int) -> String { "Live now · \(count)" }

    /// What is said when the list changes (web: a polite live region).
    public static func announcement(count: Int) -> String {
        switch count {
        case 0: "No other active calls."
        case 1: "1 other active call."
        default: "\(count) other active calls."
        }
    }
}
