import Foundation
import UserNotifications
import WaddleKit

/// Local notifications for incoming messages, with inline reply and
/// mark-read actions, grouped per conversation.
@MainActor
final class NotificationController: NSObject {
    var onOpenConversation: ((ConversationID) -> Void)?
    var onReply: ((ConversationID, String) async -> Void)?
    var onMarkRead: ((ConversationID) async -> Void)?
    var showsPreviews = true

    /// Whether the app is frontmost. Set by the scene phase.
    var isAppActive = true

    private let center = UNUserNotificationCenter.current()
    private var hasRequestedAuthorization = false

    private enum Identifier {
        static let category = "waddle.message"
        static let reply = "waddle.reply"
        static let markRead = "waddle.mark-read"
        static let conversationKey = "conversation"
        static let kindKey = "kind"
    }

    override init() {
        super.init()
        center.delegate = self
        let reply = UNTextInputNotificationAction(
            identifier: Identifier.reply,
            title: "Reply",
            options: [],
            textInputButtonTitle: "Send",
            textInputPlaceholder: "Message"
        )
        let markRead = UNNotificationAction(identifier: Identifier.markRead, title: "Mark as Read", options: [])
        center.setNotificationCategories([
            UNNotificationCategory(identifier: Identifier.category, actions: [reply, markRead], intentIdentifiers: [], options: [])
        ])
    }

    func requestAuthorizationIfNeeded() {
        guard !hasRequestedAuthorization else { return }
        hasRequestedAuthorization = true
        Task {
            _ = try? await center.requestAuthorization(options: [.alert, .badge, .sound])
        }
    }

    /// Posts `alert` unless the user is already looking at it.
    func post(_ alert: IncomingAlert, isVisible: Bool) {
        guard !isVisible else { return }
        let content = UNMutableNotificationContent()
        if alert.conversation.isRoom {
            content.title = "#\(alert.conversationTitle)"
            content.subtitle = alert.senderName
        } else {
            content.title = alert.senderName
        }
        content.body = showsPreviews ? alert.body : (alert.mentionsMe ? "Mentioned you" : "New message")
        content.sound = .default
        content.threadIdentifier = alert.conversation.description
        content.categoryIdentifier = Identifier.category
        content.userInfo = [
            Identifier.conversationKey: alert.conversation.jid.description,
            Identifier.kindKey: alert.conversation.isRoom ? "room" : "direct",
        ]
        if #available(iOS 15.0, macOS 12.0, *) {
            content.interruptionLevel = alert.mentionsMe ? .timeSensitive : .active
        }
        let request = UNNotificationRequest(identifier: "\(alert.conversation)#\(alert.messageID)", content: content, trigger: nil)
        center.add(request)
    }

    /// Removes delivered notifications for a conversation the user read.
    func clear(_ conversation: ConversationID) {
        center.getDeliveredNotifications { notifications in
            let identifiers = notifications
                .filter { $0.request.content.threadIdentifier == conversation.description }
                .map(\.request.identifier)
            UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: identifiers)
        }
    }

    func clearAll() {
        center.removeAllDeliveredNotifications()
        setBadge(0)
    }

    func setBadge(_ count: Int) {
        center.setBadgeCount(count)
    }

    nonisolated private static func conversation(from userInfo: [AnyHashable: Any]) -> ConversationID? {
        guard let raw = userInfo[Identifier.conversationKey] as? String,
              let jid = BareJID(parsing: raw)
        else { return nil }
        return (userInfo[Identifier.kindKey] as? String) == "room" ? .room(jid) : .direct(jid)
    }
}

extension NotificationController: UNUserNotificationCenterDelegate {
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound, .list]
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        // Parse before hopping: the userInfo dictionary is not Sendable.
        guard let conversation = Self.conversation(from: response.notification.request.content.userInfo) else { return }
        let action = response.actionIdentifier
        let text = (response as? UNTextInputNotificationResponse)?.userText
        await MainActor.run {
            switch action {
            case Identifier.reply:
                guard let text, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
                Task { await self.onReply?(conversation, text) }
            case Identifier.markRead:
                Task { await self.onMarkRead?(conversation) }
                self.clear(conversation)
            default:
                self.onOpenConversation?(conversation)
            }
        }
    }
}
