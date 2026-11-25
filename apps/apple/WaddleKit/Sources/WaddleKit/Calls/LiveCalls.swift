import Foundation

/// A call going on that this device is not part of, for the sidebar's
/// "Live now" list (web `CallActivityDock`): a room's call we have not
/// joined, or a 1:1 call that another of our devices is in.
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

    public enum Kind: Equatable, Sendable {
        case room
        /// A 1:1 call another of our devices is in. Two with one person are told apart by `sid`.
        case elsewhere(sid: String)
    }

    /// Where the call is: its room, or the conversation with the other person.
    public let conversation: ConversationID
    public let kind: Kind
    /// The room's name, or the other person's.
    public let title: String
    /// Who is in it, one row a person; the other person, for a call of two.
    public let people: [GroupCallRosterRow]
    /// `nil` when it is not known, as for a call answered elsewhere before we came online.
    public let hasVideo: Bool?
    public let action: Action

    public var id: String {
        switch kind {
        case .room: conversation.description
        case .elsewhere(let sid): "elsewhere:\(sid)"
        }
    }

    public init(room: BareJID, title: String, people: [GroupCallRosterRow], hasVideo: Bool, action: Action) {
        conversation = .room(room)
        kind = .room
        self.title = title
        self.people = people
        self.hasVideo = hasVideo
        self.action = action
    }

    /// A call of two that another of our devices is in. This device can open
    /// the conversation; the call itself is the other device's.
    public init(elsewhere call: CallElsewhere, name: String) {
        conversation = .direct(call.peer)
        kind = .elsewhere(sid: call.sid)
        title = name
        people = [GroupCallRosterRow(nick: name, jid: call.peer)]
        hasVideo = call.media?.video
        action = .open
    }

    /// "Group call · 3 people", "Voice call · On another device"
    public var meta: String { "\(kindLabel(capitalized: true)) · \(headcount)" }

    /// "3 people", "On another device": what is left of `meta` where the whole of it does not fit.
    public var headcount: String {
        switch kind {
        case .room: people.count == 1 ? "1 person" : "\(people.count) people"
        case .elsewhere: "On another device"
        }
    }

    private func kindLabel(capitalized: Bool) -> String {
        let label = switch (kind, hasVideo) {
        case (.room, true): "group video call"
        case (.room, _): "group call"
        case (.elsewhere, true): "video call"
        case (.elsewhere, false): "voice call"
        case (.elsewhere, nil): "call"
        }
        return capitalized ? label.prefix(1).uppercased() + label.dropFirst() : label
    }

    /// "You, Ada and 2 more": who is there, in words, for whoever cannot see
    /// the faces. Our own other device in the call is "You", as on the room's banner.
    public func preview(naming limit: Int = 2) -> String {
        let names = people.map { $0.isSelf ? "You" : $0.nick }
        guard names.count > limit else { return names.formatted(.list(type: .and)) }
        return "\(names.prefix(limit).joined(separator: ", ")) and \(names.count - limit) more"
    }

    /// The whole card in a sentence, with nothing in it that is read out as a symbol.
    public var spoken: String {
        switch kind {
        case .room: "\(action.title) \(title), \(kindLabel(capitalized: false)), \(headcount): \(preview())"
        case .elsewhere: "\(action.title) \(title), \(kindLabel(capitalized: false)), live on another of your devices"
        }
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
    ///
    /// - Parameter elsewhere: The calls of two our other devices are in, each with the other person's name.
    public static func list(
        _ rooms: [Room], elsewhere: [(call: CallElsewhere, name: String)] = [], state: CallState, now: Date = Date()
    ) -> [LiveCall] {
        let inRooms = rooms
            .filter { !$0.people.isEmpty && !state.isInGroupCall(in: $0.room) }
            .map {
                LiveCall(
                    room: $0.room, title: $0.title, people: $0.people, hasVideo: $0.hasVideo,
                    action: $0.canJoin ? .join : .open
                )
            }
        // A call this device has since taken up itself is its own, with its own
        // card. One whose ring merely ended here is still live where it was answered.
        let onOtherDevices = elsewhere
            .filter { $0.call.isShown(at: now) && !(state.isLive && state.sid == $0.call.sid) }
            .map { LiveCall(elsewhere: $0.call, name: $0.name) }
        return (inRooms + onOtherDevices).sorted { left, right in
            if left.action != right.action { return left.action == .join }
            let order = left.title.localizedStandardCompare(right.title)
            if order != .orderedSame { return order == .orderedAscending }
            return left.id < right.id
        }
    }

    /// When the list next changes by the clock alone: the first of `calls`
    /// still shown at `now` to reach the end of its day. Nothing else tells
    /// a list that sits untouched that one of its cards has run out.
    public static func nextExpiry(of calls: [CallElsewhere], after now: Date) -> Date? {
        calls.filter { $0.isShown(at: now) }.map(\.shownUntil).min()
    }

    /// "Live now · 2"
    public static func heading(count: Int) -> String { "Live now · \(count)" }

    /// What is said when a call starts in one of our rooms (web: a polite
    /// live region). "Other" only while we are in a call ourselves.
    public static func announcement(count: Int, besidesOurs: Bool) -> String {
        let other = besidesOurs ? "other " : ""
        return count == 1 ? "1 \(other)active call." : "\(count) \(other)active calls."
    }

    /// Whether a change in the list is news worth saying: a call has started
    /// somewhere. Not a call ending, not our own joining or leaving one,
    /// which moves a call out of or into the list and is said by the call
    /// itself, and not the rooms coming back one by one after a reconnect.
    public static func isNews(before: Change, after: Change) -> Bool {
        after.count > before.count && before.ownCall == after.ownCall && before.isOnline && after.isOnline
    }

    /// The list at one moment, as far as `isNews` cares.
    public struct Change: Equatable, Sendable {
        public let count: Int
        /// The call our slot holds.
        public let ownCall: String?
        public let isOnline: Bool

        public init(count: Int, ownCall: String?, isOnline: Bool) {
            self.count = count
            self.ownCall = ownCall
            self.isOnline = isOnline
        }
    }
}
