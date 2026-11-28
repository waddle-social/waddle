import Foundation

/// A snapshot of the room directory ordered by what's happening in each room.
public struct BrowseRooms: Sendable {
    public struct Row: Identifiable, Sendable {
        public enum Action: Equatable, Sendable {
            case open
            case joinRoom
            case joinCall

            public var title: String {
                switch self {
                case .open: "Open"
                case .joinRoom, .joinCall: "Join"
                }
            }

            public func accessibilityLabel(roomName: String) -> String {
                switch self {
                case .open: "Open \(roomName)"
                case .joinRoom: "Join \(roomName)"
                case .joinCall: "Join the huddle in \(roomName)"
                }
            }
        }

        public let channel: Channel
        public let action: Action
        public let people: [GroupCallRosterRow]
        public let unread: Int
        public let mentionsMe: Bool
        public let metadata: String
        public let preview: String?
        fileprivate let lastUpdated: Int64?

        public var id: BareJID { channel.roomJID }
        public var isLive: Bool { !people.isEmpty }
    }

    public let rows: [Row]
    public let isLoading: Bool
    public let liveCount: Int

    public var countText: String {
        let rooms = rows.count == 1 ? "1 room" : "\(rows.count) rooms"
        return liveCount > 0 ? "\(rooms) · \(liveCount) live" : rooms
    }

    @MainActor
    public init(session: SessionCoordinator) {
        let activity = session.inbox.latestRoomEntries()
        let directory = session.directory
        rows = (directory.channels + directory.leftRooms).map { channel in
            Self.row(channel, activity: activity[channel.roomJID], session: session)
        }.sorted(by: Self.precedes)
        isLoading = !directory.hasLoadedTopology
        liveCount = rows.count(where: \.isLive)
    }

    @MainActor
    private static func row(_ channel: Channel, activity: InboxEntry?, session: SessionCoordinator) -> Row {
        let room = channel.roomJID
        let joined = !channel.hasLeft && session.presence.joinedRooms.contains(room)
        let call = session.calls.groupPresence
        let participants = channel.hasLeft ? [] : call.participants[room] ?? []
        let people = GroupCallRoster.rows(presence: participants, live: [], speaking: [], own: session.account.jid)
        let unread = channel.hasLeft ? 0 : session.unread.count(for: channel.conversation)
        let mentionsMe = unread > 0 && session.unread.mentions.contains(channel.conversation)
        let here = joined ? session.presence.occupants[room]?.values.count(where: { $0.availability.isOnline }) : nil
        let latest = channel.hasLeft ? nil : activity
        let preview = (latest?.preview ?? latest?.threadTitle)?.trimmingCharacters(in: .whitespacesAndNewlines)
        let metadata: String
        if !people.isEmpty {
            metadata = "\(call.media[room]?.video == true ? "Video" : "Voice") huddle · \(people.count) in"
        } else if let here {
            metadata = "Room · \(here) here"
        } else if mentionsMe {
            metadata = "Room · mention"
        } else if unread > 0 {
            metadata = "Room · \(unread) unread"
        } else {
            metadata = "Room · quiet"
        }
        let action: Row.Action = if !people.isEmpty {
            session.canJoinGroupCall(in: room) ? .joinCall : .open
        } else {
            !channel.hasLeft && (channel.autojoin || joined) ? .open : .joinRoom
        }
        return Row(
            channel: channel,
            action: action,
            people: people,
            unread: unread,
            mentionsMe: mentionsMe,
            metadata: metadata,
            preview: preview.flatMap { $0.isEmpty ? nil : $0 },
            lastUpdated: latest?.lastUpdated
        )
    }

    private static func precedes(_ lhs: Row, _ rhs: Row) -> Bool {
        if lhs.people.count != rhs.people.count { return lhs.people.count > rhs.people.count }
        if lhs.mentionsMe != rhs.mentionsMe { return lhs.mentionsMe }
        if lhs.unread != rhs.unread { return lhs.unread > rhs.unread }
        if lhs.lastUpdated != rhs.lastUpdated { return (lhs.lastUpdated ?? .min) > (rhs.lastUpdated ?? .min) }
        let order = lhs.channel.name.compare(rhs.channel.name, options: [.caseInsensitive, .diacriticInsensitive], locale: .current)
        if order != .orderedSame { return order == .orderedAscending }
        return lhs.id.description < rhs.id.description
    }
}
