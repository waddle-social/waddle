import Foundation

#if os(iOS)
import UIKit

final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        NotificationCenter.default.post(
            name: .waddleDidRegisterForRemoteNotifications,
            object: deviceToken
        )
    }
}
#elseif os(macOS)
import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    func application(
        _ application: NSApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        NotificationCenter.default.post(
            name: .waddleDidRegisterForRemoteNotifications,
            object: deviceToken
        )
    }
}
#endif

extension Notification.Name {
    static let waddleDidRegisterForRemoteNotifications =
        Notification.Name("waddle.didRegisterForRemoteNotifications")
}
