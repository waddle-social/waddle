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

    fileprivate func didRegister(deviceToken: Data) {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { await register(token: token) }
    }

    private func register(token: String) async {
        guard let appState,
              let coordinator = appState.session?.coordinator,
              let appID = Bundle.main.bundleIdentifier
        else { return }
        let owner = PushRegistrationStore.Owner(server: appState.server, account: coordinator.account.jid)
        // register-device creates a device row per call; only register a
        // token this account has not already registered from this install.
        if PushRegistrationStore.token(for: owner) == token, PushRegistrationStore.registration(for: owner) != nil {
            return
        }
        #if DEBUG
        let environment = PushEnvironment.sandbox
        #else
        let environment = PushEnvironment.production
        #endif
        guard let registration = await coordinator.registerPush(deviceToken: token, environment: environment, appID: appID) else {
            return
        }
        PushRegistrationStore.save(registration, token: token, for: owner)
    }

    /// Asks APNs for a token. Called once a session is online.
    static func registerForRemoteNotifications() {
        #if os(iOS)
        UIApplication.shared.registerForRemoteNotifications()
        #elseif os(macOS)
        NSApplication.shared.registerForRemoteNotifications()
        #endif
    }
}

/// The push registration each account made from this install, so sign-out
/// disables exactly the account's own device row. A registration is only
/// forgotten once the push service confirmed the disable; one left behind
/// (offline sign-out, expired session) is retried the next time that
/// account signs out.
enum PushRegistrationStore {
    struct Owner: Hashable {
        let server: URL
        let account: BareJID

        fileprivate var key: String { "\(server.absoluteString)|\(account)" }
    }

    private static let registrationsKey = "waddle.apple.push-registrations"
    private static let tokensKey = "waddle.apple.push-tokens"

    static func registration(for owner: Owner) -> PushRegistration? {
        guard let data = registrations[owner.key] else { return nil }
        return try? JSONDecoder().decode(PushRegistration.self, from: data)
    }

    static func token(for owner: Owner) -> String? {
        tokens[owner.key]
    }

    static func save(_ registration: PushRegistration, token: String, for owner: Owner) {
        guard let encoded = try? JSONEncoder().encode(registration) else { return }
        var all = registrations
        all[owner.key] = encoded
        UserDefaults.standard.set(all, forKey: registrationsKey)
        var allTokens = tokens
        allTokens[owner.key] = token
        UserDefaults.standard.set(allTokens, forKey: tokensKey)
    }

    static func forget(_ owner: Owner) {
        var all = registrations
        all[owner.key] = nil
        UserDefaults.standard.set(all, forKey: registrationsKey)
        var allTokens = tokens
        allTokens[owner.key] = nil
        UserDefaults.standard.set(allTokens, forKey: tokensKey)
    }

    private static var registrations: [String: Data] {
        UserDefaults.standard.dictionary(forKey: registrationsKey) as? [String: Data] ?? [:]
    }

    private static var tokens: [String: String] {
        UserDefaults.standard.dictionary(forKey: tokensKey) as? [String: String] ?? [:]
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
