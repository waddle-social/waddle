import Foundation
import WaddleKit
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

/// Owns token delivery for the lifetime of the process, including scene-less launches.
@MainActor
final class AppDelegate: NSObject {
    weak var appState: AppState?
    private lazy var alertRegistrar = PushTokenRegistrar(kind: .alert) { [weak self] status in
        self?.appState?.pushRegistrationStatus = status
    }
    #if os(iOS)
    private let voipRegistrar = PushTokenRegistrar(kind: .voip)
    private var voipRegistry: VoIPPushRegistry?
    #elseif os(macOS)
    private var isWaitingForCallTermination = false
    #endif

    func attach(_ appState: AppState) {
        self.appState = appState
        if !appState.preferences.receivesPush {
            alertRegistrar.setEnabled(false)
            #if os(iOS)
            voipRegistrar.setEnabled(false)
            #endif
        }
        alertRegistrar.attach(appState)
        #if os(iOS)
        voipRegistrar.attach(appState)
        #endif
        appState.preferences.onReceivesPushChanged = { [weak self] in self?.applyPushPreference() }
        appState.finishPendingPushRegistration = { [weak self] coordinator in
            guard let self else { return }
            await self.alertRegistrar.finishPendingRegistration(for: coordinator)
            #if os(iOS)
            await self.voipRegistrar.finishPendingRegistration(for: coordinator)
            #endif
        }
        // With push off the registrars only retire what is still queued.
        appState.restartPushRegistration = { [weak self, weak appState] in
            guard let self, let appState else { return }
            if appState.preferences.receivesPush {
                Self.registerForRemoteNotifications()
            }
            self.alertRegistrar.startPendingRegistrationIfPossible()
            #if os(iOS)
            if appState.preferences.receivesPush, let token = self.voipRegistry?.token {
                self.voipRegistrar.didRegister(deviceToken: token)
            }
            self.voipRegistrar.startPendingRegistrationIfPossible()
            #endif
        }
        #if os(iOS)
        appState.setVoIPPushEnabled = { [weak self, weak appState] enabled in
            guard let self, let appState else { return }
            let enabled = enabled && appState.preferences.receivesPush
            self.voipRegistrar.setEnabled(enabled)
            self.voipRegistry?.setEnabled(enabled)
        }
        #endif
    }

    /// Follows the "Push notifications" setting. Off retires this
    /// install's registrations and stops asking Apple for tokens; on
    /// registers again.
    private func applyPushPreference() {
        guard let appState else { return }
        if appState.preferences.receivesPush {
            alertRegistrar.setEnabled(true)
            #if os(iOS)
            appState.setVoIPPushEnabled?(appState.phase != .signedOut)
            #endif
            appState.restartPushRegistration?()
        } else {
            alertRegistrar.turnOff()
            Self.unregisterForRemoteNotifications()
            #if os(iOS)
            voipRegistrar.turnOff()
            voipRegistry?.setEnabled(false)
            #endif
            appState.pushRegistrationStatus = .waiting
        }
    }

    private func launched() {
        guard !AppState.isHostingTests else { return }
        attach(AppState.shared)
        #if os(iOS)
        if voipRegistry == nil {
            voipRegistry = VoIPPushRegistry(
                isEnabled: AppState.shared.preferences.receivesPush,
                onToken: { [weak self] token in self?.voipRegistrar.didRegister(deviceToken: token) },
                onInvalidated: { [weak self] in self?.voipRegistrar.invalidate() }
            )
        }
        #endif
    }

    /// Asks APNs for a token. Called once a session is online.
    static func registerForRemoteNotifications() {
        #if os(iOS)
        UIApplication.shared.registerForRemoteNotifications()
        #elseif os(macOS)
        NSApplication.shared.registerForRemoteNotifications()
        #endif
    }

    /// Stops delivery to this install even if a retirement is still queued.
    private static func unregisterForRemoteNotifications() {
        #if os(iOS)
        UIApplication.shared.unregisterForRemoteNotifications()
        #elseif os(macOS)
        NSApplication.shared.unregisterForRemoteNotifications()
        #endif
    }
}

/// The push registration each account made from this install, so sign-out
/// disables exactly the account's own device row. A registration is only
/// forgotten once the push service confirmed the disable; one left behind
/// (offline sign-out, expired session) is retried the next time that
/// account signs out.
enum PushRegistrationStore {
    struct Owner: Hashable, Sendable {
        let server: URL
        let account: BareJID

        fileprivate func key(for kind: PushTokenKind) -> String {
            "\(server.absoluteString)|\(account)" + (kind == .voip ? "|voip" : "")
        }
    }

    private struct Context: Codable, Equatable {
        let token: String
        let environment: String
        let appID: String

        init(token: String, environment: PushEnvironment, appID: String) {
            self.token = token
            switch environment {
            case .production: self.environment = "production"
            case .sandbox: self.environment = "sandbox"
            }
            self.appID = appID
        }
    }

    private static let registrationsKey = "waddle.apple.push-registrations"
    private static let contextsKey = "waddle.apple.push-registration-contexts"
    private static let legacyTokensKey = "waddle.apple.push-tokens"
    private static let retirementsKey = "waddle.apple.push-retirements"

    static func registration(for owner: Owner, kind: PushTokenKind = .alert) -> PushRegistration? {
        guard let data = registrations[owner.key(for: kind)] else { return nil }
        return try? JSONDecoder().decode(PushRegistration.self, from: data)
    }

    static func contextMatches(token: String, environment: PushEnvironment, appID: String, for owner: Owner, kind: PushTokenKind = .alert) -> Bool {
        guard let data = contexts[owner.key(for: kind)],
              let context = try? JSONDecoder().decode(Context.self, from: data)
        else { return false }
        return context == Context(token: token, environment: environment, appID: appID)
    }

    static func save(_ registration: PushRegistration, token: String, environment: PushEnvironment, appID: String, for owner: Owner, kind: PushTokenKind = .alert) {
        guard let encoded = try? JSONEncoder().encode(registration) else { return }
        var all = registrations
        all[owner.key(for: kind)] = encoded
        UserDefaults.standard.set(all, forKey: registrationsKey)
        var allContexts = contexts
        allContexts[owner.key(for: kind)] = try? JSONEncoder().encode(Context(token: token, environment: environment, appID: appID))
        UserDefaults.standard.set(allContexts, forKey: contextsKey)
    }

    /// The account whose registration owns a push node, for routing a
    /// tapped APNs push on an install with several accounts.
    static func account(forNode node: String) -> Owner? {
        var owners = Set<Owner>()
        for (key, data) in registrations {
            guard let registration = try? JSONDecoder().decode(PushRegistration.self, from: data),
                  registration.node == node
            else { continue }
            guard let owner = owner(forKey: key) else { continue }
            owners.insert(owner)
        }
        // An ambiguous node must never route a push as an arbitrary account.
        return owners.count == 1 ? owners.first : nil
    }

    /// A token invalidation applies to every account that used this topic.
    static func retireAll(kind: PushTokenKind) {
        for (key, data) in registrations {
            guard let owner = owner(forKey: key), owner.key(for: kind) == key,
                  let registration = try? JSONDecoder().decode(PushRegistration.self, from: data)
            else { continue }
            enqueueRetirement(registration, for: owner, kind: kind)
            forget(owner, kind: kind)
        }
    }

    private static func owner(forKey key: String) -> Owner? {
        let ownerKey = key.hasSuffix("|voip") ? String(key.dropLast("|voip".count)) : key
        guard let separator = ownerKey.firstIndex(of: "|"),
              let server = URL(string: String(ownerKey[..<separator])),
              let account = BareJID(parsing: String(ownerKey[ownerKey.index(after: separator)...]))
        else { return nil }
        return Owner(server: server, account: account)
    }

    static func forget(_ owner: Owner, kind: PushTokenKind = .alert) {
        var all = registrations
        all[owner.key(for: kind)] = nil
        UserDefaults.standard.set(all, forKey: registrationsKey)
        var allContexts = contexts
        allContexts[owner.key(for: kind)] = nil
        UserDefaults.standard.set(allContexts, forKey: contextsKey)
        var legacyTokens = UserDefaults.standard.dictionary(forKey: legacyTokensKey) as? [String: String] ?? [:]
        legacyTokens[owner.key(for: kind)] = nil
        UserDefaults.standard.set(legacyTokens, forKey: legacyTokensKey)
    }

    static func pendingRetirements(for owner: Owner, kind: PushTokenKind = .alert) -> [PushRegistration] {
        guard let data = retirements[owner.key(for: kind)] else { return [] }
        return (try? JSONDecoder().decode([PushRegistration].self, from: data)) ?? []
    }

    static func enqueueRetirement(_ registration: PushRegistration, for owner: Owner, kind: PushTokenKind = .alert) {
        var items = pendingRetirements(for: owner, kind: kind)
        guard !items.contains(registration) else { return }
        items.append(registration)
        var all = retirements
        all[owner.key(for: kind)] = try? JSONEncoder().encode(items)
        UserDefaults.standard.set(all, forKey: retirementsKey)
    }

    static func removePendingRetirement(_ registration: PushRegistration, for owner: Owner, kind: PushTokenKind = .alert) {
        var items = pendingRetirements(for: owner, kind: kind)
        items.removeAll { $0 == registration }
        var all = retirements
        if items.isEmpty {
            all[owner.key(for: kind)] = nil
        } else {
            all[owner.key(for: kind)] = try? JSONEncoder().encode(items)
        }
        UserDefaults.standard.set(all, forKey: retirementsKey)
    }

    private static var registrations: [String: Data] {
        UserDefaults.standard.dictionary(forKey: registrationsKey) as? [String: Data] ?? [:]
    }

    private static var contexts: [String: Data] {
        UserDefaults.standard.dictionary(forKey: contextsKey) as? [String: Data] ?? [:]
    }

    private static var retirements: [String: Data] {
        UserDefaults.standard.dictionary(forKey: retirementsKey) as? [String: Data] ?? [:]
    }
}

#if os(iOS)
extension AppDelegate: UIApplicationDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        launched()
        return true
    }

    func applicationWillTerminate(_: UIApplication) {
        var finished = false
        let shouldWait = appState?.session?.coordinator.beginCallTermination { finished = true } ?? false
        guard shouldWait else { return }

        // UIKit needs this callback to return. If it arrived on the main
        // dispatch queue, the nested run loop can't service that queue.
        // In that case the timer ends the wait without running the hang-up.
        while !finished {
            RunLoop.main.run(mode: .default, before: Date(timeIntervalSinceNow: 0.01))
        }
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        alertRegistrar.didRegister(deviceToken: deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        appState?.pushRegistrationStatus = .failed("Could not register for push notifications: \(error.localizedDescription)")
    }
}
#elseif os(macOS)
extension AppDelegate: NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        launched()
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if isWaitingForCallTermination { return .terminateLater }
        let shouldWait = appState?.session?.coordinator.beginCallTermination {
            sender.reply(toApplicationShouldTerminate: true)
        } ?? false
        guard shouldWait else { return .terminateNow }
        isWaitingForCallTermination = true
        return .terminateLater
    }

    func application(_ application: NSApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        alertRegistrar.didRegister(deviceToken: deviceToken)
    }

    func application(_ application: NSApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        appState?.pushRegistrationStatus = .failed("Could not register for push notifications: \(error.localizedDescription)")
    }
}
#endif
