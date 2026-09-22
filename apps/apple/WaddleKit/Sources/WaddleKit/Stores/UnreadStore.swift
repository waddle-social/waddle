import Foundation
import Observation

/// Per-conversation unread badges. Live messages that are not our own and
/// arrive while the conversation is not on screen increment; opening the
/// conversation clears. The server inbox absolute-sets counts, except for
/// the conversation on screen, whose badge the local read path owns.
@MainActor
@Observable
public final class UnreadStore {
    public private(set) var counts: [ConversationID: Int] = [:]
    /// Conversations with an unread mention of the account.
    public private(set) var mentions: Set<ConversationID> = []
    public private(set) var activeConversation: ConversationID?

    public init() {}

    public func count(for conversation: ConversationID) -> Int {
        counts[conversation] ?? 0
    }

    public var totalDirect: Int {
        counts.filter { $0.key.kind == .direct }.values.reduce(0, +)
    }

    public var total: Int {
        counts.values.reduce(0, +)
    }

    public func setActive(_ conversation: ConversationID?) {
        activeConversation = conversation
        if let conversation {
            clear(conversation)
        }
    }

    /// Clears the active marker only if it still names `conversation`, so a
    /// late disappear callback cannot clobber the screen that replaced it.
    public func clearActive(ifMatches conversation: ConversationID) {
        if activeConversation == conversation {
            activeConversation = nil
        }
    }

    public func liveMessage(in conversation: ConversationID, isMine: Bool, mentionsMe: Bool) {
        guard !isMine, conversation != activeConversation else { return }
        counts[conversation, default: 0] += 1
        if mentionsMe {
            mentions.insert(conversation)
        }
    }

    public func clear(_ conversation: ConversationID) {
        counts[conversation] = nil
        mentions.remove(conversation)
    }

    /// Absolute set from an authoritative source (inbox, read sync), unless
    /// the conversation is on screen.
    public func set(_ count: Int, for conversation: ConversationID) {
        guard conversation != activeConversation else { return }
        if count <= 0 {
            clear(conversation)
        } else {
            counts[conversation] = count
        }
    }

    public func clearAll() {
        counts.removeAll()
        mentions.removeAll()
    }
}
