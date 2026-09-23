import Foundation

/// Where a tapped APNs push leads. The Push Service sends no sender or
/// body, only the routing object it also gives Web Push: the notification
/// class, the conversation JID, and the push node the device registered,
/// which names the account on an install with several.
public struct PushRoute: Hashable, Sendable {
    public let node: String
    public let conversation: ConversationID

    /// Builds the route from the payload's `waddle` object. Returns nil for
    /// a payload this version does not understand, so the tap just opens
    /// the app.
    public init?(version: Int?, node: String?, conversation: String?, notificationClass: String?) {
        guard version == 1,
              let node, !node.isEmpty,
              let jid = conversation.flatMap(BareJID.init(parsing:)),
              let kind = notificationClass.flatMap(Self.kind(ofClass:))
        else { return nil }
        self.node = node
        self.conversation = ConversationID(jid: jid, kind: kind)
    }

    /// The server's notification classes: DMs, mentions in either, and
    /// room-wide notices.
    private static func kind(ofClass value: String) -> ConversationKind? {
        switch value {
        case "dm", "dm_mention":
            return .direct
        case "personal_mention", "channel_mention", "active_channel_mention", "notify_all":
            return .room
        default:
            return nil
        }
    }
}
