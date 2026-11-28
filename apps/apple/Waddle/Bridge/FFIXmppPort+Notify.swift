import Foundation
import WaddleKit

/// XEP-0492 notification settings over the two bookmark carriers.
extension FFIXmppPort {
    func setNotifyMode(_ mode: NotifyMode, showsMessagesInPush: Bool, for conversation: ConversationID) async throws {
        let jid = conversation.jid.description
        let ffiMode = FFIOutbound.notifyMode(mode)
        if conversation.isRoom {
            let outcome = try await mappingPortErrors {
                try await client.setRoomNotificationMode(roomJid: jid, mode: ffiMode, name: nil, richPayloadOptIn: showsMessagesInPush)
            }
            switch outcome {
            case .ok: break
            case .nodeConfigMismatch: throw PortError.notificationSettingsStorageMismatch
            case .error: throw PortError.rejected
            }
        } else {
            let outcome = try await mappingPortErrors {
                try await client.setDmNotificationMode(dmJid: jid, mode: ffiMode, richPayloadOptIn: showsMessagesInPush)
            }
            // `removed` is success: the DM went back to the XEP-0492 §3
            // default, so the sparse DM node dropped its override item.
            switch outcome {
            case .ok, .removed: break
            case .nodeConfigMismatch: throw PortError.notificationSettingsStorageMismatch
            case .error: throw PortError.rejected
            }
        }
    }

    func setRoomMembership(_ membership: RoomMembership, of room: BareJID, name: String?) async throws {
        let ffiMembership: WaddleRoomMembership
        switch membership {
        case .left: ffiMembership = .left
        case let .joined(notifyMode): ffiMembership = .joined(notifyMode: FFIOutbound.notifyMode(notifyMode))
        }
        let outcome = try await mappingPortErrors {
            try await client.setRoomMembership(roomJid: room.description, membership: ffiMembership, name: name)
        }
        guard case .ok = outcome else { throw PortError.rejected }
    }

    func fetchNotifySettings() async throws -> NotifySettings {
        async let rooms = mappingPortErrors { try await client.fetchUserBookmarks() }
        async let direct = mappingPortErrors { try await client.fetchDmBookmarks() }
        return try await FFIInbound.notifySettings(rooms: rooms, direct: direct)
    }
}
