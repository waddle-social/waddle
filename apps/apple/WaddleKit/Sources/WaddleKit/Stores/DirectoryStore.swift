import Foundation
import Observation

/// A 1:1 conversation in the DM list.
public struct DirectConversation: Hashable, Sendable, Identifiable {
    public var id: BareJID { peer }
    public let peer: BareJID
    public var lastActivity: Date
    public var preview: String?

    public init(peer: BareJID, lastActivity: Date, preview: String?) {
        self.peer = peer
        self.lastActivity = lastActivity
        self.preview = preview
    }

    public var conversation: ConversationID { .direct(peer) }
    public var displayName: String { peer.localpart ?? peer.domain }
}

/// Spaces, channels, group DMs, the DM list and per-conversation notify
/// modes.
@MainActor
@Observable
public final class DirectoryStore {
    public private(set) var spaces: [Space] = []
    /// Space channels, ordered by position then name.
    public private(set) var channels: [Channel] = []
    public private(set) var groupDMs: [Channel] = []
    /// DMs, most recent first.
    public private(set) var directConversations: [DirectConversation] = []
    public private(set) var notifyModes: [ConversationID: NotifyMode] = [:]
    private var showingMessagesInPush: Set<ConversationID> = []
    public private(set) var hasLoadedTopology = false

    @ObservationIgnored private var roomJIDs: Set<BareJID> = []

    public init() {}

    public func isRoom(_ jid: BareJID) -> Bool {
        roomJIDs.contains(jid)
    }

    public func channel(for jid: BareJID) -> Channel? {
        channels.first { $0.roomJID == jid } ?? groupDMs.first { $0.roomJID == jid }
    }

    public func channels(in spaceID: String?) -> [Channel] {
        guard let spaceID else { return channels }
        return channels.filter { $0.spaceID == spaceID }
    }

    public func title(for conversation: ConversationID) -> String {
        switch conversation.kind {
        case .room:
            return channel(for: conversation.jid)?.name ?? conversation.jid.localpart ?? conversation.jid.description
        case .direct:
            return conversation.jid.localpart ?? conversation.jid.domain
        }
    }

    /// The stored mode, or the default for the conversation.
    public func notifyMode(for conversation: ConversationID) -> NotifyMode {
        notifyModes[conversation] ?? defaultNotifyMode(for: conversation)
    }

    /// Every message in a 1:1, group chat or members-only room.
    /// Rooms whose access is open or unknown default to mentions.
    func defaultNotifyMode(for conversation: ConversationID) -> NotifyMode {
        guard conversation.isRoom else { return .always }
        guard let channel = channel(for: conversation.jid) else { return .onMention }
        return channel.isGroupDM || channel.membersOnly == true ? .always : .onMention
    }

    /// Whether push notifications for the conversation show who wrote and
    /// what. Off unless the account turned it on.
    public func showsMessagesInPush(for conversation: ConversationID) -> Bool {
        showingMessagesInPush.contains(conversation)
    }

    public func apply(_ topology: Topology) {
        spaces = topology.spaces
        let previous = Dictionary(
            (channels + groupDMs).map { ($0.roomJID, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        let sorted = topology.channels.map { channel in
            channel.withKnownMembersOnly(channel.membersOnly ?? previous[channel.roomJID]?.membersOnly)
        }.sorted(by: Self.listOrder)
        channels = sorted.filter { !$0.isGroupDM }
        groupDMs = sorted.filter(\.isGroupDM)
        roomJIDs = Set(topology.channels.map(\.roomJID))
        hasLoadedTopology = true
    }

    private static func listOrder(_ first: Channel, _ second: Channel) -> Bool {
        (first.position, first.name.lowercased()) < (second.position, second.name.lowercased())
    }

    /// Adds or replaces a room, ordered by position then name.
    public func upsert(_ channel: Channel) {
        roomJIDs.insert(channel.roomJID)
        if channel.isGroupDM {
            groupDMs.removeAll { $0.roomJID == channel.roomJID }
            groupDMs.append(channel)
            groupDMs.sort(by: Self.listOrder)
        } else {
            channels.removeAll { $0.roomJID == channel.roomJID }
            channels.append(channel)
            channels.sort(by: Self.listOrder)
        }
    }

    /// Removes a room and its notification preference.
    public func remove(_ room: BareJID) {
        channels.removeAll { $0.roomJID == room }
        groupDMs.removeAll { $0.roomJID == room }
        roomJIDs.remove(room)
        notifyModes.removeValue(forKey: .room(room))
        showingMessagesInPush.remove(.room(room))
    }

    /// Records 1:1 activity, moving the peer to the top of the DM list.
    public func touchDirect(_ peer: BareJID, at date: Date, preview: String?) {
        if let index = directConversations.firstIndex(where: { $0.peer == peer }) {
            var existing = directConversations[index]
            guard date >= existing.lastActivity || preview == nil else { return }
            existing.lastActivity = max(existing.lastActivity, date)
            if let preview { existing.preview = preview }
            directConversations.remove(at: index)
            directConversations.insert(existing, at: insertionIndex(for: existing.lastActivity))
        } else {
            let created = DirectConversation(peer: peer, lastActivity: date, preview: preview)
            directConversations.insert(created, at: insertionIndex(for: date))
        }
    }

    public func setNotifyMode(_ mode: NotifyMode, for conversation: ConversationID) {
        notifyModes[conversation] = mode
    }

    /// The mode as the account stores it. A nil mode uses the conversation's default.
    public func setStoredNotifyMode(_ mode: NotifyMode?, for conversation: ConversationID) {
        notifyModes[conversation] = mode
    }

    public func setShowsMessagesInPush(_ shows: Bool, for conversation: ConversationID) {
        if shows {
            showingMessagesInPush.insert(conversation)
        } else {
            showingMessagesInPush.remove(conversation)
        }
    }

    public func replaceNotifySettings(_ settings: NotifySettings) {
        notifyModes = settings.modes
        showingMessagesInPush = settings.showingMessagesInPush
    }

    public func clear() {
        spaces = []
        channels = []
        groupDMs = []
        directConversations = []
        notifyModes = [:]
        showingMessagesInPush = []
        roomJIDs = []
        hasLoadedTopology = false
    }

    private func insertionIndex(for date: Date) -> Int {
        directConversations.firstIndex { $0.lastActivity < date } ?? directConversations.count
    }
}
