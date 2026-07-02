import Foundation
import SwiftUI
import UserNotifications

// MARK: - Push Notifications

extension AppModel {
    func requestPushNotificationPermission() {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .badge, .sound]) { [weak self] granted, _ in
            Task { @MainActor in
                self?.pushNotificationsEnabled = granted
                if granted {
#if os(iOS)
                    UIApplication.shared.registerForRemoteNotifications()
#elseif os(macOS)
                    NSApplication.shared.registerForRemoteNotifications()
#endif
                }
            }
        }
    }

    func registerPushToken(_ tokenData: Data) async {
        let token = tokenData.map { String(format: "%02x", $0) }.joined()
        guard let rustClient, let session else { return }
        let pushServiceJID = "push.\(jidDomain(session.jid))"
        let node = "waddle-apple-\(session.userID)"
        await rustClient.enablePushNotifications(pushServiceJID: pushServiceJID, node: node, token: token)
        pushNotificationsEnabled = true
    }
}
