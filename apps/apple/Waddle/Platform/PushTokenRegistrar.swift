import Foundation
import WaddleKit

/// The APNs topic of a registration; alert registrations keep their keys.
enum PushTokenKind: CaseIterable, Sendable {
    case alert
    case voip
}

/// Where a registrar finds the signed-in session to register with.
@MainActor
protocol PushSessionHost: AnyObject {
    var server: URL { get }
    /// The live session's coordinator, if someone is signed in.
    var pushCoordinator: SessionCoordinator? { get }
}

extension AppState: PushSessionHost {
    var pushCoordinator: SessionCoordinator? { session?.coordinator }
}

/// Registers one token topic, fencing superseded attempts before retirement.
@MainActor
final class PushTokenRegistrar {
    private enum RegistrationAttempt: Equatable {
        case registered
        case failed
        case superseded
    }

    private let kind: PushTokenKind
    private let status: @MainActor (AppState.PushRegistrationStatus) -> Void
    private weak var host: (any PushSessionHost)?

    init(kind: PushTokenKind, status: @escaping @MainActor (AppState.PushRegistrationStatus) -> Void = { _ in }) {
        self.kind = kind
        self.status = status
    }
    private var isEnabled = true
    private var pendingDeviceToken: Data?
    private var registrationTask: Task<Void, Never>?
    private var registrationCoordinator: SessionCoordinator?
    private var registrationGeneration = 0
    private var isFinishingRegistration = false
    private var registrationRetryRequested = false
    private weak var registrationFencedCoordinator: SessionCoordinator?
    private var retirementTask: Task<Void, Never>?

    func attach(_ host: any PushSessionHost) {
        self.host = host
        startPendingRegistrationIfPossible()
    }

    func setEnabled(_ enabled: Bool) {
        isEnabled = enabled
        if enabled {
            startPendingRegistrationIfPossible()
        } else {
            pendingDeviceToken = nil
        }
    }

    /// Turns push off for this install: every account's registration of
    /// this topic is retired, and none is made until push is enabled
    /// again. A retirement the server could not confirm stays queued.
    func turnOff() {
        setEnabled(false)
        invalidate()
    }

    func didRegister(deviceToken: Data) {
        guard isEnabled else { return }
        if registrationTask != nil { registrationRetryRequested = true }
        pendingDeviceToken = deviceToken
        startPendingRegistrationIfPossible()
    }

    func startPendingRegistrationIfPossible() {
        guard isEnabled else {
            retirePendingRegistrationsIfPossible()
            return
        }
        guard registrationTask == nil, retirementTask == nil,
              !isFinishingRegistration,
              let deviceToken = pendingDeviceToken,
              let host,
              let coordinator = host.pushCoordinator,
              coordinator.connection == .online
        else { return }
        guard registrationFencedCoordinator !== coordinator else { return }
        registrationFencedCoordinator = nil
        guard let bundleID = Bundle.main.bundleIdentifier else { return }
        let appID = bundleID + (kind == .voip ? ".voip" : "")
        status(.registering)
        let generation = registrationGeneration
        let environment = Self.apnsEnvironment
        registrationCoordinator = coordinator
        registrationTask = Task { [weak self, weak host] in
            guard let self else { return }
            guard let host else {
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
                host: host,
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

    /// While push is off no registration runs, so retirements left over
    /// from an offline moment are sent here once the account is online.
    private func retirePendingRegistrationsIfPossible() {
        guard retirementTask == nil, registrationTask == nil, !isFinishingRegistration,
              let host,
              let coordinator = host.pushCoordinator,
              coordinator.connection == .online
        else { return }
        let owner = PushRegistrationStore.Owner(server: host.server, account: coordinator.account.jid)
        let kind = self.kind
        let pending = PushRegistrationStore.pendingRetirements(for: owner, kind: kind)
        guard !pending.isEmpty else { return }
        retirementTask = Task { [weak self] in
            for registration in pending {
                if await coordinator.disablePush(registration) {
                    PushRegistrationStore.removePendingRetirement(registration, for: owner, kind: kind)
                }
            }
            guard let self else { return }
            self.retirementTask = nil
            // Push may have been turned on again meanwhile.
            if self.isEnabled { self.startPendingRegistrationIfPossible() }
        }
    }

    private func register(
        deviceToken: Data,
        environment: PushEnvironment,
        appID: String,
        host: any PushSessionHost,
        coordinator: SessionCoordinator,
        generation: Int
    ) async -> RegistrationAttempt {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        let owner = PushRegistrationStore.Owner(server: host.server, account: coordinator.account.jid)

        guard isCurrent(generation, deviceToken: deviceToken, host: host, coordinator: coordinator) else { return .superseded }
        var couldNotRetirePreviousRegistration = false
        for retirement in PushRegistrationStore.pendingRetirements(for: owner, kind: kind) {
            if await coordinator.disablePush(retirement) {
                PushRegistrationStore.removePendingRetirement(retirement, for: owner, kind: kind)
            } else {
                couldNotRetirePreviousRegistration = true
            }
        }

        if let registration = PushRegistrationStore.registration(for: owner, kind: kind),
           PushRegistrationStore.contextMatches(token: token, environment: environment, appID: appID, for: owner, kind: kind) {
            guard await coordinator.enablePush(registration) else {
                if isCurrent(generation, deviceToken: deviceToken, host: host, coordinator: coordinator) {
                    status(.failed("Push re-registration with the XMPP server failed. It will retry after reconnecting."))
                }
                return isCurrent(generation, deviceToken: deviceToken, host: host, coordinator: coordinator) ? .failed : .superseded
            }
            if isCurrent(generation, deviceToken: deviceToken, host: host, coordinator: coordinator) {
                pendingDeviceToken = nil
                if couldNotRetirePreviousRegistration {
                    status(.failed("A previous push registration could not be retired. It will retry after reconnecting."))
                    return .failed
                }
                status(.registered)
                return .registered
            }
            return .superseded
        }

        guard !couldNotRetirePreviousRegistration else {
            status(.failed("A previous push registration could not be retired. It will retry after reconnecting."))
            return .failed
        }

        // A stored registration from an earlier environment or bundle topic
        // must be retired before replacing it; otherwise it can remain an
        // active server-side device after the local cache is overwritten.
        if let registration = PushRegistrationStore.registration(for: owner, kind: kind) {
            PushRegistrationStore.enqueueRetirement(registration, for: owner, kind: kind)
            guard await coordinator.disablePush(registration) else {
                if isCurrent(generation, deviceToken: deviceToken, host: host, coordinator: coordinator) {
                    status(.failed("The previous push registration could not be retired. It will retry after reconnecting."))
                }
                return isCurrent(generation, deviceToken: deviceToken, host: host, coordinator: coordinator) ? .failed : .superseded
            }
            PushRegistrationStore.removePendingRetirement(registration, for: owner, kind: kind)
            PushRegistrationStore.forget(owner, kind: kind)
        }

        guard isCurrent(generation, deviceToken: deviceToken, host: host, coordinator: coordinator) else { return .superseded }
        guard let registration = await coordinator.registerPush(deviceToken: token, environment: environment, appID: appID) else {
            if isCurrent(generation, deviceToken: deviceToken, host: host, coordinator: coordinator) {
                status(.failed("Push registration with the XMPP server failed. It will retry after reconnecting."))
            }
            return isCurrent(generation, deviceToken: deviceToken, host: host, coordinator: coordinator) ? .failed : .superseded
        }
        guard isCurrent(generation, deviceToken: deviceToken, host: host, coordinator: coordinator) else {
            PushRegistrationStore.enqueueRetirement(registration, for: owner, kind: kind)
            if await coordinator.disablePush(registration) {
                PushRegistrationStore.removePendingRetirement(registration, for: owner, kind: kind)
                return .superseded
            }
            if self.host === host {
                status(.failed("A new push registration needs cleanup. It will retry after reconnecting."))
            }
            return .failed
        }
        PushRegistrationStore.save(registration, token: token, environment: environment, appID: appID, for: owner, kind: kind)
        pendingDeviceToken = nil
        status(.registered)
        return .registered
    }

    private func isCurrent(
        _ generation: Int,
        deviceToken: Data,
        host: any PushSessionHost,
        coordinator: SessionCoordinator
    ) -> Bool {
        isEnabled && registrationGeneration == generation
            && pendingDeviceToken == deviceToken
            && self.host === host
            && host.pushCoordinator === coordinator
            && coordinator.connection == .online
    }

    private static var apnsEnvironment: PushEnvironment {
        #if DEBUG
        .sandbox
        #else
        .production
        #endif
    }

    func finishPendingRegistration(for coordinator: SessionCoordinator) async {
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

    /// Stops using an invalidated token before a later callback can replace it.
    func invalidate() {
        pendingDeviceToken = nil
        let coordinator = host?.pushCoordinator
        let owner = host.flatMap { state in
            coordinator.map { PushRegistrationStore.Owner(server: state.server, account: $0.account.jid) }
        }
        PushRegistrationStore.retireAll(kind: kind)
        isFinishingRegistration = true
        registrationRetryRequested = false
        registrationGeneration += 1
        let generation = registrationGeneration
        let task = registrationTask
        task?.cancel()

        Task { [weak self] in
            // The superseded attempt rolls back a row created before its
            // cancellation. New token callbacks wait behind that cleanup.
            await task?.value
            guard let self, self.registrationGeneration == generation else { return }
            self.registrationTask = nil
            self.registrationCoordinator = nil
            if let coordinator, let owner {
                for registration in PushRegistrationStore.pendingRetirements(for: owner, kind: self.kind) {
                    if await coordinator.disablePush(registration) {
                        PushRegistrationStore.removePendingRetirement(registration, for: owner, kind: self.kind)
                    }
                }
            }
            guard self.registrationGeneration == generation else { return }
            self.isFinishingRegistration = false
            // Invalidation never removes a fence installed by session teardown.
            self.startPendingRegistrationIfPossible()
        }
    }
}
