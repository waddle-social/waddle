import Foundation
import WaddleKit

extension FFIInbound {
    /// XEP-0492 settings read from both carriers: XEP-0402 bookmarks
    /// (rooms) and the Waddle DM-bookmark node (1:1).
    static func notifySettings(rooms: [WaddleBookmarkItem], direct: [WaddleDmBookmarkItem]) -> NotifySettings {
        var settings = NotifySettings()
        for item in rooms {
            guard let room = bareJID(item.jid) else { continue }
            record(.room(room), mode: item.notifyMode, optIn: item.richPayloadOptIn, into: &settings)
        }
        for item in direct {
            guard let peer = bareJID(item.jid) else { continue }
            record(.direct(peer), mode: item.notifyMode, optIn: item.richPayloadOptIn, into: &settings)
        }
        return settings
    }

    static func notifyMode(_ mode: WaddleNotifyMode) -> NotifyMode {
        switch mode {
        case .always: return .always
        case .onMention: return .onMention
        case .never: return .never
        }
    }

    private static func record(
        _ conversation: ConversationID,
        mode: WaddleNotifyMode?,
        optIn: Bool,
        into settings: inout NotifySettings
    ) {
        if optIn {
            settings.showingMessagesInPush.insert(conversation)
        }
        if let mode {
            settings.modes[conversation] = notifyMode(mode)
        }
    }
}
