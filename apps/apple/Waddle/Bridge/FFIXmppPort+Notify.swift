import Foundation
import WaddleKit

/// XEP-0492 notification settings over the two bookmark carriers.
extension FFIXmppPort {
    func setNotifyMode(_ mode: NotifyMode, for conversation: ConversationID) async throws {
        let optIn = richPayloadOptIns.withLock { $0[conversation] ?? false }
        let jid = conversation.jid.description
        let ffiMode = FFIOutbound.notifyMode(mode)
        if conversation.isRoom {
            let outcome = try await mappingPortErrors {
                try await client.setRoomNotificationMode(roomJid: jid, mode: ffiMode, name: nil, richPayloadOptIn: optIn)
            }
            try apply(outcome, for: conversation)
        } else {
            let outcome = try await mappingPortErrors {
                try await client.setDmNotificationMode(dmJid: jid, mode: ffiMode, richPayloadOptIn: optIn)
            }
            try apply(outcome, for: conversation)
        }
    }

    func fetchNotifyModes() async throws -> [ConversationID: NotifyMode] {
        let rooms = try await mappingPortErrors { try await client.fetchUserBookmarks() }
        let direct = try await mappingPortErrors { try await client.fetchDmBookmarks() }
        let snapshot = FFIInbound.notifySettings(rooms: rooms, direct: direct)
        richPayloadOptIns.withLock { $0 = snapshot.richPayloadOptIns }
        return snapshot.modes
    }

    private func apply(_ outcome: WaddleSetRoomNotificationModeOutcome, for conversation: ConversationID) throws {
        switch outcome {
        case let .ok(item):
            richPayloadOptIns.withLock { $0[conversation] = item.richPayloadOptIn }
        case .nodeConfigMismatch, .error:
            throw PortError.rejected
        }
    }

    /// `removed` is success: the DM went back to the XEP-0492 §3 default,
    /// so the sparse DM node dropped its override item.
    private func apply(_ outcome: WaddleSetDmNotificationModeOutcome, for conversation: ConversationID) throws {
        switch outcome {
        case let .ok(item):
            richPayloadOptIns.withLock { $0[conversation] = item.richPayloadOptIn }
        case .removed:
            richPayloadOptIns.withLock { $0[conversation] = nil }
        case .nodeConfigMismatch, .error:
            throw PortError.rejected
        }
    }
}
