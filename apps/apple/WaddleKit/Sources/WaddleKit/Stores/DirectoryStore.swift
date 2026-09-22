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

    public func notifyMode(for conversation: ConversationID) -> NotifyMode {
        notifyModes[conversation] ?? (conversation.isRoom ? .onMention : .always)
    }

    public func apply(_ topology: Topology) {
        spaces = topology.spaces
        let sorted = topology.channels.sorted {
            ($0.position, $0.name.lowercased()) < ($1.position, $1.name.lowercased())
        }
        channels = sorted.filter { !$0.isGroupDM }
        groupDMs = sorted.filter(\.isGroupDM)
        roomJIDs = Set(topology.channels.map(\.roomJID))
        hasLoadedTopology = true
    }

    /// Adds a room created or joined in this session before the next
    /// topology refresh lists it.
    public func upsert(_ channel: Channel) {
        roomJIDs.insert(channel.roomJID)
        if channel.isGroupDM {
            groupDMs.removeAll { $0.roomJID == channel.roomJID }
            groupDMs.append(channel)
        } else {
            channels.removeAll { $0.roomJID == channel.roomJID }
            channels.append(channel)
        }
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

    public func replaceNotifyModes(_ modes: [ConversationID: NotifyMode]) {
        notifyModes = modes
    }

    public func clear() {
        spaces = []
        channels = []
        groupDMs = []
        directConversations = []
        notifyModes = [:]
        roomJIDs = []
        hasLoadedTopology = false
    }

    private func insertionIndex(for date: Date) -> Int {
        directConversations.firstIndex { $0.lastActivity < date } ?? directConversations.count
    }
}
