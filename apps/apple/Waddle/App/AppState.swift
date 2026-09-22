import Foundation
import Observation
import WaddleKit

/// App-wide state: which server, whether we are signed in, and the live
/// session once we are.
@MainActor
@Observable
final class AppState {
    enum Phase: Equatable {
        case launching
        case signedOut
        /// Waiting for the user to approve the device code in a browser.
        case authorizing(DeviceAuthorization)
        case signedIn
    }

    private(set) var phase: Phase = .launching
    private(set) var server: URL
    private(set) var providers: [AuthProvider] = []
    private(set) var isLoadingProviders = false
    var errorMessage: String?
    private(set) var session: ActiveSession?

    let notifications = NotificationController()
    let preferences = Preferences()

    @ObservationIgnored private var pollTask: Task<Void, Never>?
    @ObservationIgnored private var client: AuthClient

    init() {
        let server = ServerSettings.current
        self.server = server
        self.client = AuthClient(baseURL: server)
        notifications.onOpenConversation = { [weak self] conversation in
            self?.session?.navigation.open(conversation)
        }
        notifications.onReply = { [weak self] conversation, text in
            guard let session = self?.session?.coordinator else { return }
            await session.send(Draft(text: text), in: conversation)
        }
        notifications.onMarkRead = { [weak self] conversation in
            await self?.session?.coordinator.markDisplayed(conversation)
        }
    }

    // MARK: - Lifecycle

    /// Restores a stored session or shows sign-in.
    func bootstrap() async {
        await loadProviders()
        guard let stored = CredentialStore.sessionID(for: server) else {
            phase = .signedOut
            return
        }
        do {
            if let restored = try await client.session(stored) {
                start(restored)
            } else {
                CredentialStore.remove(for: server)
                phase = .signedOut
            }
        } catch {
            // Offline at launch: stay signed out rather than guessing; the
            // user can retry, and the credential is kept.
            errorMessage = error.localizedDescription
            phase = .signedOut
        }
    }

    func sceneActivityChanged(isActive: Bool) {
        notifications.isAppActive = isActive
        guard let coordinator = session?.coordinator else { return }
        if isActive {
            coordinator.resume()
        }
        Task { await coordinator.setAppActive(isActive) }
    }

    // MARK: - Server

    func changeServer(to input: String) async {
        guard let next = ServerSettings.normalized(from: input) else {
            errorMessage = AuthClientError.invalidServerURL.localizedDescription
            return
        }
        guard next != server else { return }
        await endSession()
        server = next
        client = AuthClient(baseURL: next)
        ServerSettings.save(next)
        phase = .signedOut
        await loadProviders()
    }

    private func loadProviders() async {
        isLoadingProviders = true
        defer { isLoadingProviders = false }
        do {
            providers = try await client.providers()
            errorMessage = nil
        } catch {
            providers = []
            errorMessage = error.localizedDescription
        }
    }

    func retryProviders() async {
        await loadProviders()
    }

    // MARK: - Sign in

    /// Starts RFC 8628 device authorization and returns the page to open.
    func beginSignIn(with provider: AuthProvider) async -> URL? {
        cancelSignIn()
        errorMessage = nil
        do {
            let authorization = try await client.startDeviceAuthorization(provider: provider)
            phase = .authorizing(authorization)
            poll(authorization)
            return client.verificationURL(for: authorization)
        } catch {
            errorMessage = error.localizedDescription
            return nil
        }
    }

    func verificationURL() -> URL? {
        guard case let .authorizing(authorization) = phase else { return nil }
        return client.verificationURL(for: authorization)
    }

    func cancelSignIn() {
        pollTask?.cancel()
        pollTask = nil
        if case .authorizing = phase {
            phase = .signedOut
        }
    }

    private func poll(_ authorization: DeviceAuthorization) {
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(authorization.interval) * 1_000_000_000)
                guard !Task.isCancelled, let self else { return }
                do {
                    guard case let .complete(sessionID) = try await self.client.poll(authorization) else { continue }
                    guard let loaded = try await self.client.session(sessionID) else {
                        self.errorMessage = "The server did not return a session. Try again."
                        self.phase = .signedOut
                        return
                    }
                    CredentialStore.save(loaded.sessionID, for: self.server)
                    self.start(loaded)
                    return
                } catch let error as URLError where error.code == .networkConnectionLost || error.code == .timedOut {
                    continue
                } catch {
                    self.errorMessage = error.localizedDescription
                    self.phase = .signedOut
                    return
                }
            }
        }
    }

    // MARK: - Session

    private func start(_ auth: AuthSession) {
        guard let active = ActiveSession(auth: auth, server: server, preferences: preferences) else {
            errorMessage = "The server returned an invalid account address."
            phase = .signedOut
            return
        }
        active.coordinator.onAlert = { [weak self] alert in
            guard let self else { return }
            self.notifications.post(alert, isVisible: self.isOnScreen(alert.conversation))
        }
        active.coordinator.onAuthenticationFailed = { [weak self] in
            Task { await self?.expireSession() }
        }
        session = active
        phase = .signedIn
        active.coordinator.start()
        notifications.requestAuthorizationIfNeeded()
    }

    private func isOnScreen(_ conversation: ConversationID) -> Bool {
        notifications.isAppActive && session?.coordinator.unread.activeConversation == conversation
    }

    func signOut() async {
        let sessionID = session?.auth.sessionID
        if let coordinator = session?.coordinator, let registration = AppDelegate.storedRegistration {
            _ = await coordinator.disablePush(registration)
            AppDelegate.forgetRegistration()
        }
        await endSession()
        if let sessionID {
            try? await client.logout(sessionID)
        }
        CredentialStore.remove(for: server)
        phase = .signedOut
    }

    /// The server rejected the credential: forget it and ask again.
    private func expireSession() async {
        await endSession()
        CredentialStore.remove(for: server)
        errorMessage = "Your session expired. Sign in again."
        phase = .signedOut
    }

    private func endSession() async {
        cancelSignIn()
        guard let active = session else { return }
        session = nil
        await active.coordinator.stop()
        notifications.clearAll()
    }
}

/// A signed-in session: the account, its coordinator and navigation.
@MainActor
@Observable
final class ActiveSession {
    let auth: AuthSession
    let coordinator: SessionCoordinator
    let navigation = NavigationModel()

    init?(auth: AuthSession, server: URL, preferences: Preferences) {
        guard let jid = BareJID(parsing: auth.jid) ?? JID(parsing: auth.jid)?.bare else { return nil }
        let account = AccountIdentity(jid: jid, nick: auth.username)
        let config = WaddleConfig(
            serverUrl: auth.xmppWebsocketURL,
            jid: jid.description,
            accessToken: auth.sessionID,
            resource: ServerSettings.resource
        )
        self.auth = auth
        self.coordinator = SessionCoordinator(account: account, port: FFIXmppPort(config: config))
        coordinator.sendsReadReceipts = preferences.sendsReadReceipts
    }
}
