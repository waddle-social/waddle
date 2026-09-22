import Foundation
import WaddleKit

/// XEP-0492 settings read from both carriers: XEP-0402 bookmarks (rooms)
/// and the Waddle DM-bookmark node (1:1).
struct NotifySettingsSnapshot: Equatable, Sendable {
    /// Only conversations with an explicit fallback mode.
    var modes: [ConversationID: NotifyMode]
    /// Waddle rich push-summary opt-in per bookmarked conversation, kept
    /// so a mode change repeats it instead of clearing it.
    var richPayloadOptIns: [ConversationID: Bool]
}

extension FFIInbound {
    static func notifySettings(rooms: [WaddleBookmarkItem], direct: [WaddleDmBookmarkItem]) -> NotifySettingsSnapshot {
        var snapshot = NotifySettingsSnapshot(modes: [:], richPayloadOptIns: [:])
        for item in rooms {
            guard let room = bareJID(item.jid) else { continue }
            record(.room(room), mode: item.notifyMode, optIn: item.richPayloadOptIn, into: &snapshot)
        }
        for item in direct {
            guard let peer = bareJID(item.jid) else { continue }
            record(.direct(peer), mode: item.notifyMode, optIn: item.richPayloadOptIn, into: &snapshot)
        }
        return snapshot
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
        into snapshot: inout NotifySettingsSnapshot
    ) {
        snapshot.richPayloadOptIns[conversation] = optIn
        if let mode {
            snapshot.modes[conversation] = notifyMode(mode)
        }
    }
}
