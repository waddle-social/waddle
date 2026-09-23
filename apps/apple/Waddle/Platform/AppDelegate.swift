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
    private enum RegistrationAttempt: Equatable {
        case registered
        case failed
        case superseded
    }

    weak var appState: AppState?
    private var pendingDeviceToken: Data?
    private var registrationTask: Task<Void, Never>?
    private var registrationCoordinator: SessionCoordinator?
    private var registrationGeneration = 0
    private var isFinishingRegistration = false
    private var registrationRetryRequested = false
    private weak var registrationFencedCoordinator: SessionCoordinator?

    func attach(_ appState: AppState) {
        self.appState = appState
        appState.finishPendingPushRegistration = { [weak self] coordinator in
            guard let self else { return }
            await self.finishPendingRegistration(for: coordinator)
        }
        startPendingRegistrationIfPossible()
    }

    fileprivate func didRegister(deviceToken: Data) {
        if registrationTask != nil { registrationRetryRequested = true }
        pendingDeviceToken = deviceToken
        startPendingRegistrationIfPossible()
    }

    private func startPendingRegistrationIfPossible() {
        guard registrationTask == nil,
              !isFinishingRegistration,
              let deviceToken = pendingDeviceToken,
              let appState,
              let coordinator = appState.session?.coordinator,
              coordinator.connection == .online
        else { return }
        guard registrationFencedCoordinator !== coordinator else { return }
        registrationFencedCoordinator = nil
        guard let appID = Bundle.main.bundleIdentifier else { return }
        appState.pushRegistrationStatus = .registering
        let generation = registrationGeneration
        let environment = Self.apnsEnvironment
        registrationCoordinator = coordinator
        registrationTask = Task { [weak self, weak appState] in
            guard let self else { return }
            guard let appState else {
                if self.registrationGeneration == generation {
                    self.registrationTask = nil
                    self.registrationCoordinator = nil
                }
                return
            }
            let outcome = await self.register(
                deviceToken: deviceToken,
                environment: environment,
                appID: appID,
                appState: appState,
                coordinator: coordinator,
                generation: generation
            )
            guard self.registrationGeneration == generation else { return }
            let retryRequested = self.registrationRetryRequested
            self.registrationRetryRequested = false
            self.registrationTask = nil
            self.registrationCoordinator = nil
            if outcome == .superseded || retryRequested || self.pendingDeviceToken != deviceToken {
                self.startPendingRegistrationIfPossible()
            }
        }
    }

    private func register(
        deviceToken: Data,
        environment: PushEnvironment,
        appID: String,
        appState: AppState,
        coordinator: SessionCoordinator,
        generation: Int
    ) async -> RegistrationAttempt {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        let owner = PushRegistrationStore.Owner(server: appState.server, account: coordinator.account.jid)

        guard isCurrent(generation, deviceToken: deviceToken, appState: appState, coordinator: coordinator) else { return .superseded }
        var couldNotRetirePreviousRegistration = false
        for retirement in PushRegistrationStore.pendingRetirements(for: owner) {
            if await coordinator.disablePush(retirement) {
                PushRegistrationStore.removePendingRetirement(retirement, for: owner)
            } else {
                couldNotRetirePreviousRegistration = true
            }
        }

        if let registration = PushRegistrationStore.registration(for: owner),
           PushRegistrationStore.contextMatches(token: token, environment: environment, appID: appID, for: owner) {
            guard await coordinator.enablePush(registration) else {
                if isCurrent(generation, deviceToken: deviceToken, appState: appState, coordinator: coordinator) {
                    appState.pushRegistrationStatus = .failed("Push re-registration with the XMPP server failed. It will retry after reconnecting.")
                }
                return isCurrent(generation, deviceToken: deviceToken, appState: appState, coordinator: coordinator) ? .failed : .superseded
            }
            if isCurrent(generation, deviceToken: deviceToken, appState: appState, coordinator: coordinator) {
                pendingDeviceToken = nil
                if couldNotRetirePreviousRegistration {
                    appState.pushRegistrationStatus = .failed("A previous push registration could not be retired. It will retry after reconnecting.")
                    return .failed
                }
                appState.pushRegistrationStatus = .registered
                return .registered
            }
            return .superseded
        }

        guard !couldNotRetirePreviousRegistration else {
            appState.pushRegistrationStatus = .failed("A previous push registration could not be retired. It will retry after reconnecting.")
            return .failed
        }

        // A stored registration from an earlier environment or bundle topic
        // must be retired before replacing it; otherwise it can remain an
        // active server-side device after the local cache is overwritten.
        if let registration = PushRegistrationStore.registration(for: owner) {
            PushRegistrationStore.enqueueRetirement(registration, for: owner)
            guard await coordinator.disablePush(registration) else {
                if isCurrent(generation, deviceToken: deviceToken, appState: appState, coordinator: coordinator) {
                    appState.pushRegistrationStatus = .failed("The previous push registration could not be retired. It will retry after reconnecting.")
                }
                return isCurrent(generation, deviceToken: deviceToken, appState: appState, coordinator: coordinator) ? .failed : .superseded
            }
            PushRegistrationStore.removePendingRetirement(registration, for: owner)
            PushRegistrationStore.forget(owner)
        }

        guard isCurrent(generation, deviceToken: deviceToken, appState: appState, coordinator: coordinator) else { return .superseded }
        guard let registration = await coordinator.registerPush(deviceToken: token, environment: environment, appID: appID) else {
            if isCurrent(generation, deviceToken: deviceToken, appState: appState, coordinator: coordinator) {
                appState.pushRegistrationStatus = .failed("Push registration with the XMPP server failed. It will retry after reconnecting.")
            }
            return isCurrent(generation, deviceToken: deviceToken, appState: appState, coordinator: coordinator) ? .failed : .superseded
        }
        guard isCurrent(generation, deviceToken: deviceToken, appState: appState, coordinator: coordinator) else {
            PushRegistrationStore.enqueueRetirement(registration, for: owner)
            if await coordinator.disablePush(registration) {
                PushRegistrationStore.removePendingRetirement(registration, for: owner)
                return .superseded
            }
            if self.appState === appState {
                appState.pushRegistrationStatus = .failed("A new push registration needs cleanup. It will retry after reconnecting.")
            }
            return .failed
        }
        PushRegistrationStore.save(registration, token: token, environment: environment, appID: appID, for: owner)
        pendingDeviceToken = nil
        appState.pushRegistrationStatus = .registered
        return .registered
    }

    private func isCurrent(
        _ generation: Int,
        deviceToken: Data,
        appState: AppState,
        coordinator: SessionCoordinator
    ) -> Bool {
        registrationGeneration == generation
            && pendingDeviceToken == deviceToken
            && self.appState === appState
            && appState.session?.coordinator === coordinator
            && coordinator.connection == .online
    }

    private static var apnsEnvironment: PushEnvironment {
        #if DEBUG
        .sandbox
        #else
        .production
        #endif
    }

    private var inputCornerRadius: CGFloat {
        #if os(iOS)
        28
        #else
        Theme.Radius.large
        #endif
    }

    private func finishPendingRegistration(for coordinator: SessionCoordinator) async {
        registrationFencedCoordinator = coordinator
        guard registrationCoordinator === coordinator, let registrationTask else { return }
        isFinishingRegistration = true
        registrationRetryRequested = false
        registrationGeneration += 1
        registrationTask.cancel()
        await registrationTask.value
        self.registrationTask = nil
        registrationCoordinator = nil
        isFinishingRegistration = false
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

    static func registration(for owner: Owner) -> PushRegistration? {
        guard let data = registrations[owner.key] else { return nil }
        return try? JSONDecoder().decode(PushRegistration.self, from: data)
    }

    static func contextMatches(token: String, environment: PushEnvironment, appID: String, for owner: Owner) -> Bool {
        guard let data = contexts[owner.key],
              let context = try? JSONDecoder().decode(Context.self, from: data)
        else { return false }
        return context == Context(token: token, environment: environment, appID: appID)
    }

    static func save(_ registration: PushRegistration, token: String, environment: PushEnvironment, appID: String, for owner: Owner) {
        guard let encoded = try? JSONEncoder().encode(registration) else { return }
        var all = registrations
        all[owner.key] = encoded
        UserDefaults.standard.set(all, forKey: registrationsKey)
        var allContexts = contexts
        allContexts[owner.key] = try? JSONEncoder().encode(Context(token: token, environment: environment, appID: appID))
        UserDefaults.standard.set(allContexts, forKey: contextsKey)
    }

    /// The account whose registration owns a push node, for routing a
    /// tapped APNs push on an install with several accounts.
    static func account(forNode node: String) -> BareJID? {
        for (key, data) in registrations {
            guard let registration = try? JSONDecoder().decode(PushRegistration.self, from: data),
                  registration.node == node,
                  let separator = key.lastIndex(of: "|")
            else { continue }
            return BareJID(parsing: String(key[key.index(after: separator)...]))
        }
        return nil
    }

    static func forget(_ owner: Owner) {
        var all = registrations
        all[owner.key] = nil
        UserDefaults.standard.set(all, forKey: registrationsKey)
        var allContexts = contexts
        allContexts[owner.key] = nil
        UserDefaults.standard.set(allContexts, forKey: contextsKey)
        var legacyTokens = UserDefaults.standard.dictionary(forKey: legacyTokensKey) as? [String: String] ?? [:]
        legacyTokens[owner.key] = nil
        UserDefaults.standard.set(legacyTokens, forKey: legacyTokensKey)
    }

    static func pendingRetirements(for owner: Owner) -> [PushRegistration] {
        guard let data = retirements[owner.key] else { return [] }
        return (try? JSONDecoder().decode([PushRegistration].self, from: data)) ?? []
    }

    static func enqueueRetirement(_ registration: PushRegistration, for owner: Owner) {
        var items = pendingRetirements(for: owner)
        guard !items.contains(registration) else { return }
        items.append(registration)
        var all = retirements
        all[owner.key] = try? JSONEncoder().encode(items)
        UserDefaults.standard.set(all, forKey: retirementsKey)
    }

    static func removePendingRetirement(_ registration: PushRegistration, for owner: Owner) {
        var items = pendingRetirements(for: owner)
        items.removeAll { $0 == registration }
        var all = retirements
        if items.isEmpty {
            all[owner.key] = nil
        } else {
            all[owner.key] = try? JSONEncoder().encode(items)
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
    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        didRegister(deviceToken: deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        appState?.pushRegistrationStatus = .failed("Could not register for push notifications: \(error.localizedDescription)")
    }
}
#elseif os(macOS)
extension AppDelegate: NSApplicationDelegate {
    func application(_ application: NSApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        didRegister(deviceToken: deviceToken)
    }

    func application(_ application: NSApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        appState?.pushRegistrationStatus = .failed("Could not register for push notifications: \(error.localizedDescription)")
    }
}
#endif
