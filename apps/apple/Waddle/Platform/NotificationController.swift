import Foundation
import UserNotifications
import WaddleKit

/// Local notifications for incoming messages, with inline reply and
/// mark-read actions, grouped per conversation.
@MainActor
final class NotificationController: NSObject {
    /// Each callback names the account the notification was posted for.
    var onOpenConversation: ((BareJID, ConversationID) -> Void)?
    var onReply: ((BareJID, ConversationID, String) async -> Void)?
    var onMarkRead: ((BareJID, ConversationID) async -> Void)?
    var showsPreviews = true

    /// Whether the app is frontmost. Set by the scene phase.
    var isAppActive = true
    /// Only a live session for the push's account can replace its alert with
    /// the richer local XMPP notification.
    var hasLiveSession: ((BareJID) -> Bool)?
    var onAuthorizationResult: ((Bool) -> Void)?

    private let center = UNUserNotificationCenter.current()
    private var hasRequestedAuthorization = false

    private enum Identifier {
        static let category = "waddle.message"
        static let reply = "waddle.reply"
        static let markRead = "waddle.mark-read"
        static let conversationKey = "conversation"
        static let kindKey = "kind"
        static let accountKey = "account"
        /// The object the Push Service adds next to `aps`.
        static let pushRoutingKey = "waddle"
    }

    override init() {
        super.init()
        center.delegate = self
        // Foreground: the app must be running with a live session to send;
        // a background wake could be suspended with the reply still queued.
        let reply = UNTextInputNotificationAction(
            identifier: Identifier.reply,
            title: "Reply",
            options: [.foreground],
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
            await updateAuthorizationStatus()
        }
    }

    func refreshAuthorizationStatus() {
        Task { await updateAuthorizationStatus() }
    }

    private func updateAuthorizationStatus() async {
        let settings = await center.notificationSettings()
        var isGranted = settings.authorizationStatus == .authorized
            || settings.authorizationStatus == .provisional
        #if os(iOS)
        isGranted = isGranted || settings.authorizationStatus == .ephemeral
        #endif
        onAuthorizationResult?(isGranted)
    }

    /// Posts `alert` for `account` unless the user is already looking at it.
    func post(_ alert: IncomingAlert, for account: BareJID, isVisible: Bool) {
        guard !isVisible else { return }
        let content = UNMutableNotificationContent()
        if alert.conversation.isRoom {
            content.title = "#\(alert.conversationTitle)"
            content.subtitle = alert.senderName
        } else {
            content.title = alert.senderName
        }
        content.body = showsPreviews ? alert.body : Self.hiddenPreview(mentionsMe: alert.mentionsMe)
        content.sound = .default
        content.threadIdentifier = alert.conversation.description
        content.categoryIdentifier = Identifier.category
        content.userInfo = [
            Identifier.conversationKey: alert.conversation.jid.description,
            Identifier.kindKey: alert.conversation.isRoom ? "room" : "direct",
            Identifier.accountKey: account.description,
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

    private static func hiddenPreview(mentionsMe: Bool) -> String {
        mentionsMe ? "Mentioned you" : "New message"
    }

    nonisolated private static func account(from userInfo: [AnyHashable: Any]) -> BareJID? {
        (userInfo[Identifier.accountKey] as? String).flatMap { BareJID(parsing: $0) }
    }

    /// The account and conversation a tapped notification leads to: the
    /// keys a local alert carries, else the routing object of an APNs push.
    nonisolated private static func target(from userInfo: [AnyHashable: Any]) -> (BareJID, ConversationID)? {
        if let account = account(from: userInfo), let conversation = conversation(from: userInfo) {
            return (account, conversation)
        }
        guard let route = pushRoute(from: userInfo),
              let account = PushRegistrationStore.account(forNode: route.node)
        else { return nil }
        return (account, route.conversation)
    }

    nonisolated private static func pushRoute(from userInfo: [AnyHashable: Any]) -> PushRoute? {
        guard let object = userInfo[Identifier.pushRoutingKey] as? [String: Any] else { return nil }
        return PushRoute(
            version: object["v"] as? Int,
            node: object["node"] as? String,
            conversation: object["conversation"] as? String,
            notificationClass: object["class"] as? String
        )
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
        if notification.request.trigger is UNPushNotificationTrigger {
            let account = Self.target(from: notification.request.content.userInfo)?.0
            let hasLiveSession = await MainActor.run {
                self.isAppActive && account.map { self.hasLiveSession?($0) == true } == true
            }
            // The matching live XMPP session posts a richer local alert for
            // messages it receives. When that session is absent, keep the
            // APNs banner visible instead of suppressing the only alert.
            return hasLiveSession ? [] : [.banner, .sound, .list]
        }
        return [.banner, .sound, .list]
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        // Parse before hopping: the userInfo dictionary is not Sendable.
        let userInfo = response.notification.request.content.userInfo
        guard let target = Self.target(from: userInfo) else { return }
        let (account, conversation) = target
        let action = response.actionIdentifier
        let text = (response as? UNTextInputNotificationResponse)?.userText
        await MainActor.run {
            switch action {
            case Identifier.reply:
                guard let text, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
                Task { await self.onReply?(account, conversation, text) }
            case Identifier.markRead:
                Task { await self.onMarkRead?(account, conversation) }
                self.clear(conversation)
            default:
                self.onOpenConversation?(account, conversation)
            }
        }
    }
}
