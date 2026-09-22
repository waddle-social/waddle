import Foundation
import WaddleKit

// MARK: - ArchivePort

extension FFIXmppPort {
    func fetchHistory(of conversation: ConversationID, before cursor: String?, max: Int) async -> ArchivePage {
        let jid = conversation.jid.description
        let limit = UInt32(clamping: max)
        let page = conversation.isRoom
            ? await client.fetchRoomHistory(roomJid: jid, maxMessages: limit, beforeId: cursor)
            : await client.fetchDmHistory(peerJid: jid, maxMessages: limit, beforeId: cursor)
        return FFIInbound.archivePage(page)
    }

    func searchHistory(of conversation: ConversationID, query: String, max: Int) async -> ArchivePage {
        let jid = conversation.jid.description
        let limit = UInt32(clamping: max)
        let page = conversation.isRoom
            ? await client.searchRoomHistory(roomJid: jid, query: query, maxMessages: limit)
            : await client.searchDmHistory(peerJid: jid, query: query, maxMessages: limit)
        return FFIInbound.archivePage(page)
    }
}

// MARK: - ReadStatePort

extension FFIXmppPort {
    func sendDisplayed(stanzaID: String, in conversation: ConversationID) async -> Bool {
        await client.sendDisplayed(peerJid: conversation.jid.description, stanzaId: stanzaID, isMuc: conversation.isRoom)
    }

    func publishDisplayedCursor(_ cursor: DisplayedCursor) async -> Bool {
        await client.publishMdsDisplayed(
            chatJid: cursor.conversation.description,
            stanzaId: cursor.stanzaID,
            stanzaIdBy: cursor.stanzaIDBy.description
        )
    }

    func supportsDisplayedCursorPublish() async -> Bool {
        await client.supportsMdsPublishOptions()
    }

    func fetchDisplayedCursors() async throws -> [DisplayedCursor] {
        let entries = try await mappingPortErrors { try await client.fetchMdsDisplayed() }
        return entries.compactMap(FFIInbound.displayedCursor)
    }

    func subscribeDisplayedCursors() async -> Bool {
        await client.subscribeMdsDisplayed()
    }

    /// The cheap hydration shape: every conversation, no embedded bodies.
    func fetchInbox() async throws -> [InboxEntry] {
        let result = try await mappingPortErrors { try await client.fetchInbox(onlyUnread: false, noMessages: true) }
        return result.conversations.compactMap(FFIInbound.inboxEntry)
    }

    func markInboxRead(partner: BareJID, threadID: String?) async throws {
        try await mappingPortErrors { try await client.markInboxRead(partnerJid: partner.description, threadId: threadID) }
    }
}
