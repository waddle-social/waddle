import Foundation
import WaddleKit

extension FFIXmppPort {
    func send(_ message: OutboundMessage) async -> SendOutcome {
        let options = FFIOutbound.sendOptions(message.options, stanzaID: message.clientID)
        let target = message.conversation.jid.description
        let outcome: WaddleSendMessageOutcome
        if message.conversation.isRoom {
            outcome = await client.sendGroupchatMessage(roomJid: target, body: message.body, options: options)
        } else {
            outcome = await client.sendChatMessage(peerJid: target, body: message.body, options: options)
        }
        return FFIOutbound.sendOutcome(outcome)
    }

    /// XEP-0308. The core assigns the correction its own stanza id.
    func sendCorrection(of targetID: String, body: String, in conversation: ConversationID, options: OutboundOptions) async -> SendOutcome {
        let outcome = await client.sendCorrection(
            peerJid: conversation.jid.description,
            targetId: targetID,
            newBody: body,
            isMuc: conversation.isRoom,
            options: FFIOutbound.sendOptions(options, stanzaID: nil)
        )
        return FFIOutbound.sendOutcome(outcome)
    }

    func sendReaction(to targetID: String, emojis: [String], in conversation: ConversationID) async -> Bool {
        await client.sendReaction(
            targetJid: conversation.jid.description,
            targetStanzaId: targetID,
            emojis: emojis,
            isMuc: conversation.isRoom
        )
    }

    func sendRetraction(of targetID: String, in conversation: ConversationID) async -> Bool {
        await client.sendRetraction(peerJid: conversation.jid.description, targetStanzaId: targetID, isMuc: conversation.isRoom)
    }

    func sendModeration(of targetID: String, in room: BareJID, reason: String?) async -> Bool {
        await client.sendModeration(roomJid: room.description, targetStanzaId: targetID, reason: reason)
    }

    func sendChatState(_ state: ChatState, in conversation: ConversationID) async -> Bool {
        await client.sendChatState(
            peerJid: conversation.jid.description,
            state: FFIOutbound.chatState(state),
            isMuc: conversation.isRoom
        )
    }

    func setPinned(_ pinned: Bool, targetID: String, in conversation: ConversationID) async -> Bool {
        let jid = conversation.jid.description
        switch (conversation.isRoom, pinned) {
        case (true, true): return await client.pinMessage(roomJid: jid, targetStanzaId: targetID)
        case (true, false): return await client.unpinMessage(roomJid: jid, targetStanzaId: targetID)
        case (false, true): return await client.pinDirectMessage(peerJid: jid, targetStanzaId: targetID)
        case (false, false): return await client.unpinDirectMessage(peerJid: jid, targetStanzaId: targetID)
        }
    }

    func fetchPins(in room: BareJID) async throws -> [PinEntry] {
        let entries = try await mappingPortErrors { try await client.fetchRoomPins(roomJid: room.description) }
        return entries.map(FFIInbound.pinEntry)
    }
}
