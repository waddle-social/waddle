import Foundation
import WaddleKit
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

/// Receives the APNs device token and registers it with `push.<domain>`
/// (XEP-0357 + the Waddle register-device XEP-0050 command).
@MainActor
final class AppDelegate: NSObject {
    weak var appState: AppState?

    private static let registrationKey = "waddle.apple.push-registration"
    private static let tokenKey = "waddle.apple.push-token"

    fileprivate func didRegister(deviceToken: Data) {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { await register(token: token) }
    }

    private func register(token: String) async {
        // register-device creates a device row per call; only register a
        // token the service has not seen from this install.
        if Self.storedRegistration != nil, UserDefaults.standard.string(forKey: Self.tokenKey) == token {
            return
        }
        guard let coordinator = appState?.session?.coordinator,
              let appID = Bundle.main.bundleIdentifier
        else { return }
        #if DEBUG
        let environment = PushEnvironment.sandbox
        #else
        let environment = PushEnvironment.production
        #endif
        guard let registration = await coordinator.registerPush(deviceToken: token, environment: environment, appID: appID) else {
            return
        }
        if let encoded = try? JSONEncoder().encode(registration) {
            UserDefaults.standard.set(encoded, forKey: Self.registrationKey)
            UserDefaults.standard.set(token, forKey: Self.tokenKey)
        }
    }

    /// Asks APNs for a token. Called once a session is online.
    static func registerForRemoteNotifications() {
        #if os(iOS)
        UIApplication.shared.registerForRemoteNotifications()
        #elseif os(macOS)
        NSApplication.shared.registerForRemoteNotifications()
        #endif
    }

    /// The registration from the last successful `registerPush`, for
    /// disabling on sign-out.
    static var storedRegistration: PushRegistration? {
        guard let data = UserDefaults.standard.data(forKey: registrationKey) else { return nil }
        return try? JSONDecoder().decode(PushRegistration.self, from: data)
    }

    static func forgetRegistration() {
        UserDefaults.standard.removeObject(forKey: registrationKey)
        UserDefaults.standard.removeObject(forKey: tokenKey)
    }
}

#if os(iOS)
extension AppDelegate: UIApplicationDelegate {
    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        didRegister(deviceToken: deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {}
}
#elseif os(macOS)
extension AppDelegate: NSApplicationDelegate {
    func application(_ application: NSApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        didRegister(deviceToken: deviceToken)
    }

    func application(_ application: NSApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {}
}
#endif
