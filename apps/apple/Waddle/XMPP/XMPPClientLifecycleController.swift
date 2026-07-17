import Foundation

struct XMPPLoginSessionIdentity: Sendable, Equatable, Hashable {
    let value: String
}

struct XMPPAuthenticationDecision: Sendable, Equatable {
    let disposition: XMPPSaslRetryDisposition
    let wasAlreadyStopped: Bool
}

@MainActor
protocol XMPPManagedClient: AnyObject {
    func disconnect() async
}

extension RustXmppClient: XMPPManagedClient {}

/// Single ownership authority for the AppModel's current native client and
/// consumer task.
///
/// The controller invalidates publication synchronously, then disconnects the
/// native client to wake any pending pull, and finally joins the canceled
/// consumer before a replacement may publish.
@MainActor
final class XMPPClientLifecycleController<Client: XMPPManagedClient> {
    private struct ActiveConnection {
        let client: Client
        let loginGeneration: UInt64
        let sessionIdentity: XMPPLoginSessionIdentity
        var consumerTask: Task<Void, Never>?
    }

    private var activeConnection: ActiveConnection?
    private var teardownTask: Task<Void, Never>?
    private(set) var admission = XMPPConnectionAdmission()
    private(set) var sessionIdentity: XMPPLoginSessionIdentity?
    private(set) var stoppedAuthentication: StoppedXMPPAuthentication?

    var currentClient: Client? {
        activeConnection?.client
    }

    var loginGeneration: UInt64 {
        admission.generation
    }

    var reconnectAllowed: Bool {
        sessionIdentity != nil
            && xmppReconnectAllowed(
                admission: admission,
                stopped: stoppedAuthentication
            )
    }

    func openLogin(sessionIdentity: XMPPLoginSessionIdentity) {
        admission.open()
        self.sessionIdentity = sessionIdentity
        stoppedAuthentication = nil
    }

    func closeLogin() async {
        // This state change deliberately precedes the first suspension.
        admission.close()
        sessionIdentity = nil
        stoppedAuthentication = nil
        await detachCurrentConnection()
    }

    func beginReplacement(
        sessionIdentity expectedSession: XMPPLoginSessionIdentity
    ) async -> UInt64? {
        let generation = admission.generation
        guard admitsLogin(
            generation: generation,
            sessionIdentity: expectedSession
        ) else {
            return nil
        }

        await detachCurrentConnection()

        guard admitsLogin(
            generation: generation,
            sessionIdentity: expectedSession
        ) else {
            return nil
        }
        return generation
    }

    func install(
        client: Client,
        loginGeneration: UInt64,
        sessionIdentity expectedSession: XMPPLoginSessionIdentity
    ) -> Bool {
        guard
            teardownTask == nil,
            activeConnection == nil,
            admitsLogin(
                generation: loginGeneration,
                sessionIdentity: expectedSession
            )
        else {
            return false
        }

        activeConnection = ActiveConnection(
            client: client,
            loginGeneration: loginGeneration,
            sessionIdentity: expectedSession,
            consumerTask: nil
        )
        return true
    }

    func startConsumer(
        for client: Client,
        loginGeneration: UInt64,
        sessionIdentity expectedSession: XMPPLoginSessionIdentity,
        operation: @escaping @MainActor () async -> Void
    ) -> Bool {
        guard admits(
            client: client,
            loginGeneration: loginGeneration,
            sessionIdentity: expectedSession
        ) else {
            return false
        }
        let task = Task { @MainActor in
            await operation()
        }
        guard var connection = activeConnection else {
            task.cancel()
            return false
        }
        connection.consumerTask = task
        activeConnection = connection
        return true
    }

    func remove(
        client: Client,
        loginGeneration: UInt64,
        sessionIdentity expectedSession: XMPPLoginSessionIdentity
    ) async {
        guard admits(
            client: client,
            loginGeneration: loginGeneration,
            sessionIdentity: expectedSession
        ) else {
            return
        }
        await detachCurrentConnection()
    }

    func admits(
        client: Client,
        loginGeneration: UInt64,
        sessionIdentity expectedSession: XMPPLoginSessionIdentity
    ) -> Bool {
        guard
            admitsLogin(
                generation: loginGeneration,
                sessionIdentity: expectedSession
            ),
            let activeConnection
        else {
            return false
        }
        return activeConnection.client === client
            && activeConnection.loginGeneration == loginGeneration
            && activeConnection.sessionIdentity == expectedSession
    }

    func recordAuthenticationFailure(
        _ condition: XMPPSaslCondition
    ) -> XMPPAuthenticationDecision {
        let wasAlreadyStopped = !reconnectAllowed
        stoppedAuthentication = updatedStoppedXMPPAuthentication(
            stoppedAuthentication,
            loginGeneration: admission.generation,
            condition: condition
        )
        return XMPPAuthenticationDecision(
            disposition: condition.retryDisposition,
            wasAlreadyStopped: wasAlreadyStopped
        )
    }

    func performFreshBootstrap(
        client: Client,
        loginGeneration: UInt64,
        sessionIdentity expectedSession: XMPPLoginSessionIdentity,
        sendPresence: @MainActor () async -> Void,
        admitRoomLoad: @MainActor () -> Void
    ) async {
        guard
            !Task.isCancelled,
            admits(
                client: client,
                loginGeneration: loginGeneration,
                sessionIdentity: expectedSession
            )
        else {
            return
        }

        await sendPresence()

        guard
            !Task.isCancelled,
            admits(
                client: client,
                loginGeneration: loginGeneration,
                sessionIdentity: expectedSession
            )
        else {
            return
        }
        admitRoomLoad()
    }

    func admitsLogin(
        generation: UInt64,
        sessionIdentity expectedSession: XMPPLoginSessionIdentity
    ) -> Bool {
        admission.admits(generation: generation)
            && sessionIdentity == expectedSession
    }

    private func detachCurrentConnection() async {
        if let teardownTask {
            await teardownTask.value
            self.teardownTask = nil
            return
        }
        guard let connection = activeConnection else {
            return
        }

        // Remove correctness authority before cancel/disconnect can suspend.
        activeConnection = nil
        connection.consumerTask?.cancel()

        let task = Task { @MainActor in
            await connection.client.disconnect()
            if let consumerTask = connection.consumerTask {
                await consumerTask.value
            }
        }
        teardownTask = task
        await task.value
        teardownTask = nil
    }
}
