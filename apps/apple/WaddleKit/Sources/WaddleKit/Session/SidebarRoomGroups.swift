import Foundation

/// The rooms and unread discussions shown by the split sidebar, in directory order.
public struct SidebarRoomGroups: Sendable {
    public struct Group: Identifiable, Sendable {
        public enum ID: Hashable, Sendable {
            case space(String)
            case standalone
        }

        public let space: Space?
        public let rooms: [Room]
        public let activity: Activity

        public var id: ID { space.map { .space($0.id) } ?? .standalone }
        public var title: String { space?.name ?? "Standalone rooms" }
    }

    public struct Room: Identifiable, Sendable {
        public let channel: Channel
        public let huddleParticipantCount: Int
        public let threads: [Thread]

        public var id: BareJID { channel.roomJID }
    }

    public struct Thread: Identifiable, Sendable {
        public let key: ThreadKey
        public let title: String
        public let unread: Int

        public var id: ThreadKey { key }
    }

    public struct Activity: Sendable {
        public enum Badge: Equatable, Sendable {
            case mentions(Int)
            case unread(Int)
        }

        public fileprivate(set) var unread = 0
        /// Apple retains a mention flag for each room, so count rooms that mention us.
        public fileprivate(set) var mentionedRoomCount = 0
        public fileprivate(set) var huddleParticipantCount = 0

        public var badge: Badge? {
            if mentionedRoomCount > 0 { return .mentions(mentionedRoomCount) }
            if unread > 0 { return .unread(unread) }
            return nil
        }
    }

    public let groups: [Group]

    // Discovery puts otherwise ungrouped rooms under its synthetic "standalone" space.
    private static let standaloneSpaceID = "standalone"

    /// The group to reveal when a conversation outside the sidebar is selected.
    @MainActor
    public static func groupID(for conversation: ConversationID, in directory: DirectoryStore) -> Group.ID? {
        guard let room = directory.channels.first(where: { $0.conversation == conversation }) else { return nil }
        return groupID(for: room) { id in directory.spaces.contains { $0.id == id } }
    }

    private static func groupID(for channel: Channel, hasSpace: (String) -> Bool) -> Group.ID {
        guard let id = channel.spaceID, id != standaloneSpaceID, hasSpace(id) else { return .standalone }
        return .space(id)
    }

    @MainActor
    public init(session: SessionCoordinator) {
        let spaces = session.directory.spaces.filter { $0.id != Self.standaloneSpaceID }
        let spaceIDs = Set(spaces.map(\.id))
        let inboxThreads = Dictionary(uniqueKeysWithValues: session.overviewCandidates().map { ($0.room, $0.threads) })
        let participants = session.calls.groupPresence.participants
        var rooms: [Group.ID: [Room]] = [:]
        var activity: [Group.ID: Activity] = [:]
        for channel in session.directory.channels {
            let group = Self.groupID(for: channel, hasSpace: spaceIDs.contains)
            let people =
                participants[channel.roomJID].map {
                    GroupCallRoster.rows(presence: $0, live: [], speaking: [], own: session.account.jid).count
                } ?? 0
            let threads = (inboxThreads[channel.roomJID] ?? []).map {
                Thread(key: $0.key, title: $0.title, unread: $0.unread)
            }
            rooms[group, default: []].append(Room(channel: channel, huddleParticipantCount: people, threads: threads))
            // A reply can count in both badges. Threads can stay unread after the room is read.
            let threadUnread = threads.reduce(0) { $0 + $1.unread }
            let roomUnread = session.unread.count(for: channel.conversation)
            activity[group, default: Activity()].unread += max(roomUnread, threadUnread)
            if session.unread.mentions.contains(channel.conversation) {
                activity[group, default: Activity()].mentionedRoomCount += 1
            }
            activity[group, default: Activity()].huddleParticipantCount += people
        }
        var groups = spaces.map { space in
            let id = Group.ID.space(space.id)
            return Group(space: space, rooms: rooms[id] ?? [], activity: activity[id] ?? Activity())
        }
        if let standalone = rooms[.standalone] {
            groups.append(Group(space: nil, rooms: standalone, activity: activity[.standalone] ?? Activity()))
        }
        self.groups = groups
    }
}
