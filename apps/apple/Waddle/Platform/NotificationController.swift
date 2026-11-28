import Foundation
import UserNotifications
import WaddleKit

/// Local notifications for incoming messages, with inline reply and
/// mark-read actions, grouped per conversation; and for a call that rings
/// while Waddle is not in front, with Accept and Decline.
@MainActor
final class NotificationController: NSObject {
    /// Each callback names the account the notification was posted for.
    var onOpenConversation: ((BareJID, ConversationID) -> Void)?
    var onReply: ((BareJID, ConversationID, String) async -> Void)?
    var onMarkRead: ((BareJID, ConversationID) async -> Void)?
    /// Accept or Decline on a call notification, with the call's session id.
    var onAnswerCall: ((BareJID, String) async -> Void)?
    var onDeclineCall: ((BareJID, String) async -> Void)?
    var showsPreviews = true

    /// Whether the app is frontmost. Set by the scene phase.
    var isAppActive = true
    /// Only a live session for the push's account can replace its alert with
    /// the richer local XMPP notification.
    var hasLiveSession: ((BareJID) -> Bool)?
    var onAuthorizationResult: ((Bool) -> Void)?
    var hasLivePushSession: (@MainActor (PushRegistrationStore.Owner) -> Bool)?
    var onPushAction: (@MainActor (PushRegistrationStore.Owner, AppState.NotificationAction) async -> Void)?

    private let center = UNUserNotificationCenter.current()
    private let messageSounds: MessageSoundPreference
    private var hasRequestedAuthorization = false

    private enum Identifier {
        static let category = "waddle.message"
        static let reply = "waddle.reply"
        static let markRead = "waddle.mark-read"
        static let conversationKey = "conversation"
        static let kindKey = "kind"
        static let accountKey = "account"
        static let callCategory = "waddle.call"
        static let acceptCall = "waddle.call.accept"
        static let declineCall = "waddle.call.decline"
        static let callKey = "call"
        /// The object the Push Service adds next to `aps`.
        static let pushRoutingKey = "waddle"
    }

    init(messageSounds: MessageSoundPreference) {
        self.messageSounds = messageSounds
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
        // Accept brings Waddle forward: the call needs the microphone and
        // somewhere to be seen. Decline is done from where the person is.
        let accept = UNNotificationAction(identifier: Identifier.acceptCall, title: "Accept", options: [.foreground])
        let decline = UNNotificationAction(identifier: Identifier.declineCall, title: "Decline", options: [.destructive])
        center.setNotificationCategories([
            UNNotificationCategory(identifier: Identifier.category, actions: [reply, markRead], intentIdentifiers: [], options: []),
            UNNotificationCategory(identifier: Identifier.callCategory, actions: [accept, decline], intentIdentifiers: [], options: []),
        ])
    }

    /// Posts the ring for a call on a Mac, which plays its own ring. An
    /// iPhone or iPad rings through the system's call screen instead.
    func postIncomingCall(sid: String, callerName: String, isVideo: Bool, for account: BareJID) {
        let content = UNMutableNotificationContent()
        content.title = callerName
        content.body = isVideo ? "Incoming video call" : "Incoming voice call"
        content.categoryIdentifier = Identifier.callCategory
        content.userInfo = [Identifier.accountKey: account.description, Identifier.callKey: sid]
        content.interruptionLevel = .timeSensitive
        center.add(UNNotificationRequest(identifier: Self.callIdentifier(sid), content: content, trigger: nil))
    }

    /// The call was answered, declined or is gone: its ring goes with it.
    func clearIncomingCall(sid: String) {
        center.removePendingNotificationRequests(withIdentifiers: [Self.callIdentifier(sid)])
        center.removeDeliveredNotifications(withIdentifiers: [Self.callIdentifier(sid)])
    }

    private static func callIdentifier(_ sid: String) -> String { "call#\(sid)" }

    /// A tap on the ring itself only brings Waddle forward, where the dock
    /// shows the call.
    private func answerCall(_ action: String, sid: String, for account: BareJID) async {
        switch action {
        case Identifier.acceptCall: await onAnswerCall?(account, sid)
        case Identifier.declineCall: await onDeclineCall?(account, sid)
        default: break
        }
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
        content.sound = messageSounds.isEnabled ? .default : nil
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
        // The same identifier as the push for this message, so whichever
        // arrives second replaces the first instead of adding a banner.
        let request = UNNotificationRequest(identifier: alert.notificationID, content: content, trigger: nil)
        center.add(request)
    }

    /// Removes delivered notifications for a conversation the user read.
    func clear(_ conversation: ConversationID) {
        center.getDeliveredNotifications { notifications in
            let identifiers = notifications
                .filter { Self.threadIdentifiers(of: conversation).contains($0.request.content.threadIdentifier) }
                .map(\.request.identifier)
            UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: identifiers)
        }
    }

    /// The thread identifiers a conversation's notifications carry: a
    /// local alert's, and the bare JID the server puts on a push.
    nonisolated static func threadIdentifiers(of conversation: ConversationID) -> Set<String> {
        [conversation.description, conversation.jid.description]
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
    nonisolated static func target(from userInfo: [AnyHashable: Any]) -> (account: BareJID, conversation: ConversationID, owner: PushRegistrationStore.Owner?)? {
        if let account = account(from: userInfo), let conversation = conversation(from: userInfo) {
            return (account, conversation, nil)
        }
        guard let route = pushRoute(from: userInfo),
              let owner = PushRegistrationStore.account(forNode: route.node)
        else { return nil }
        return (owner.account, route.conversation, owner)
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
            let userInfo = notification.request.content.userInfo
            let target = Self.target(from: userInfo)
            let route = userInfo[Identifier.pushRoutingKey] as? [String: Any]
            let isMissedCall = route?["class"] as? String == "missed_call"
            return await MainActor.run {
                // A matching live session posts a richer local message alert.
                if self.isAppActive, let target {
                    let hasLiveSession = if let owner = target.owner {
                        self.hasLivePushSession?(owner) == true
                    } else {
                        self.hasLiveSession?(target.account) == true
                    }
                    if hasLiveSession { return [] }
                }
                return isMissedCall || self.messageSounds.isEnabled ? [.banner, .sound, .list] : [.banner, .list]
            }
        }
        return [.banner, .sound, .list]
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        // Parse before hopping: the userInfo dictionary is not Sendable.
        let userInfo = response.notification.request.content.userInfo
        if let sid = userInfo[Identifier.callKey] as? String {
            guard let account = Self.account(from: userInfo) else { return }
            // Awaited, so the system keeps the app running until a Decline
            // from the background has been sent.
            await answerCall(response.actionIdentifier, sid: sid, for: account)
            return
        }
        guard let target = Self.target(from: userInfo) else { return }
        let (account, conversation, owner) = target
        let action = response.actionIdentifier
        let text = (response as? UNTextInputNotificationResponse)?.userText
        if let owner {
            let pushAction: AppState.NotificationAction
            switch action {
            case Identifier.reply:
                guard let text, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
                pushAction = .reply(conversation, text)
            case Identifier.markRead: pushAction = .markRead(conversation)
            default: pushAction = .open(conversation)
            }
            await onPushAction?(owner, pushAction)
            return
        }
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
