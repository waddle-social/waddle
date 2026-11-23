import Foundation
import Observation
import WaddleKit

/// App-wide state: which server, whether we are signed in, and the live
/// session once we are.
@MainActor
@Observable
final class AppState {
    static let shared = AppState()
    static let isHostingTests = ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil

    enum PushRegistrationStatus: Equatable {
        case waiting
        case registering
        case registered
        case failed(String)

        var title: String {
            switch self {
            case .waiting: return "Waiting for device token"
            case .registering: return "Registering device"
            case .registered: return "Device registered"
            case .failed: return "Needs attention"
            }
        }
    }

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
    var pushRegistrationStatus: PushRegistrationStatus = .waiting
    private(set) var notificationPermissionGranted: Bool?
    private(set) var session: ActiveSession?
    @ObservationIgnored var finishPendingPushRegistration: (@MainActor (SessionCoordinator) async -> Void)?

    @ObservationIgnored var restartPushRegistration: (@MainActor () -> Void)?
    @ObservationIgnored var setVoIPPushEnabled: (@MainActor (Bool) -> Void)?

    let notifications = NotificationController()
    @ObservationIgnored private let userInput = UserInputMonitor()
    @ObservationIgnored private lazy var ringer = CallRinger(notifications: notifications)
    #if os(iOS)
    @ObservationIgnored private lazy var systemCalls = makeSystemCalls()
    @ObservationIgnored private var pendingCallPushes: [String: CallPush] = [:]
    @ObservationIgnored private var pushDeadlines: [String: Task<Void, Never>] = [:]
    @ObservationIgnored private var finishedCallPushes: [CallPush] = []
    @ObservationIgnored private var pendingCallDeclines: [String: CallPush] = [:]
    #endif
    let preferences = Preferences()

    @ObservationIgnored private var pollTask: Task<Void, Never>?
    @ObservationIgnored private var isBootstrapping = false
    @ObservationIgnored private var hasBootstrapped = false
    @ObservationIgnored private var restoreTask: Task<ActiveSession?, Never>?
    @ObservationIgnored private var sessionGeneration = 0
    @ObservationIgnored private var pendingNotificationActions: [(owner: PushRegistrationStore.Owner, action: NotificationAction)] = []
    @ObservationIgnored private var client: AuthClient

    init() {
        let server = ServerSettings.current
        self.server = server
        self.client = AuthClient(baseURL: server)
        userInput.onInput = { [weak self] instant in
            self?.session?.coordinator.noteActivity(at: instant)
        }
        userInput.onRefresh = { [weak self] in
            guard let coordinator = self?.session?.coordinator else { return }
            Task { await coordinator.refreshOwnPresence() }
        }
        userInput.start()
        notifications.onAuthorizationResult = { [weak self] isGranted in
            self?.notificationPermissionGranted = isGranted
        }
        notifications.hasLiveSession = { [weak self] account in
            guard let coordinator = self?.session?.coordinator else { return false }
            return coordinator.account.jid == account && coordinator.connection == .online
        }
        notifications.hasLivePushSession = { [weak self] owner in
            guard let self, let coordinator = self.session?.coordinator else { return false }
            return owner == PushRegistrationStore.Owner(server: self.server, account: coordinator.account.jid)
                && coordinator.connection == .online
        }
        notifications.onPushAction = { [weak self] owner, action in
            await self?.perform(action, for: owner.account, on: owner.server)
        }
        notifications.onOpenConversation = { [weak self] account, conversation in
            Task { await self?.perform(.open(conversation), for: account) }
        }
        notifications.onReply = { [weak self] account, conversation, text in
            await self?.perform(.reply(conversation, text), for: account)
        }
        notifications.onMarkRead = { [weak self] account, conversation in
            await self?.perform(.markRead(conversation), for: account)
        }
        notifications.onAnswerCall = { [weak self] account, sid in
            await self?.perform(.answerCall(sid: sid), for: account)
        }
        notifications.onDeclineCall = { [weak self] account, sid in
            await self?.perform(.declineCall(sid: sid), for: account)
        }
    }

    /// A notification action. One that arrives before the session exists
    /// (the app was launched by the action) waits for it instead of being
    /// dropped. Actions only ever run as the account they were posted for.
    enum NotificationAction: Sendable {
        case open(ConversationID)
        case reply(ConversationID, String)
        case markRead(ConversationID)
        case answerCall(sid: String)
        case declineCall(sid: String)
    }

    func perform(_ action: NotificationAction, for account: BareJID, on expectedServer: URL? = nil) async {
        guard expectedServer == nil || expectedServer == server else { return }
        guard let active = session else {
            pendingNotificationActions.append((PushRegistrationStore.Owner(server: server, account: account), action))
            return
        }
        let coordinator = active.coordinator
        guard coordinator.account.jid == account else { return }
        switch action {
        case let .open(conversation):
            active.navigation.open(conversation)
        case let .reply(conversation, text):
            await coordinator.send(Draft(text: text), in: conversation)
        case let .markRead(conversation):
            await coordinator.markDisplayed(conversation)
            if expectedServer == server, session === active { notifications.clear(conversation) }
        case let .answerCall(sid):
            // Only the call that rang: by now another may hold the slot.
            _ = await coordinator.calls.answer(sid: sid)
        case let .declineCall(sid):
            // Decline is for a call still ringing, never one since answered.
            guard case .incoming = coordinator.calls.state else { return }
            await coordinator.calls.end(sid: sid)
        }
    }

    /// Keeps the ringer, and on an iPhone or iPad the system's call screen,
    /// in step with `active`'s call slot for as long as it is the session;
    /// whoever ends the session stops both.
    private func followCalls(of active: ActiveSession) {
        let coordinator = active.coordinator
        let (state, isDoNotDisturb, isMediaConnected, isMuted, _) = withObservationTracking {
            (
                coordinator.calls.state, coordinator.status.availability == .doNotDisturb,
                active.callMedia.phase == .connected, active.callMedia.isMicrophoneMutedByChoice,
                coordinator.directory.channels
            )
        } onChange: { [weak self, weak active] in
            Task { @MainActor in
                guard let self, let active else { return }
                self.followCalls(of: active)
            }
        }
        guard session === active else { return }
        #if os(iOS)
        // A proposal and its end can both arrive before this observer runs.
        // A finished invitation must not keep the pre-session screen ringing,
        // nor an answer given before the session waiting on it: the stream
        // has said the call is over, unlike a push deadline passing.
        for (sid, push) in pendingCallPushes where coordinator.calls.isFinished(sid: sid) {
            expireCallPush(push, cancellingAnswer: true)
        }
        systemCalls.update(state: state, isMediaConnected: isMediaConnected, isMuted: isMuted, isDoNotDisturb: isDoNotDisturb)
        systemCalls.refreshName()
        for sid in Array(pendingCallPushes.keys) where state.isLive && state.sid == sid {
            pushDeadlines.removeValue(forKey: sid)?.cancel()
            pendingCallPushes[sid] = nil
        }
        #else
        _ = (isMediaConnected, isMuted)
        #endif
        ringer.show(CallAlert(state: state, isDoNotDisturb: isDoNotDisturb).map { alert in
            var callerName = ""
            if case .ring(let ring) = alert { callerName = coordinator.directory.title(for: .direct(ring.from)) }
            return .init(alert: alert, account: coordinator.account.jid, callerName: callerName)
        })
    }

    /// Registration follows the connection even on a launch without a scene.
    private func followConnection(of active: ActiveSession) {
        let isOnline = withObservationTracking {
            active.coordinator.connection == .online
        } onChange: { [weak self, weak active] in
            Task { @MainActor in
                guard let self, let active else { return }
                self.followConnection(of: active)
            }
        }
        guard session === active else { return }
        // Several buffered connection events can arrive before this task
        // rearms. Process a final online state even if the offline state
        // between two online states was not observed.
        if isOnline {
            restartPushRegistration?()
        }
    }

    #if os(iOS)
    /// The system may act before there is a signed-in session.
    private func makeSystemCalls() -> SystemCallCenter {
        let center = SystemCallCenter()
        center.handlers = .init(
            answer: { [weak self] sid in
                guard let active = self?.session, active.coordinator.connection == .online else { return false }
                return await active.coordinator.calls.answer(sid: sid)
            },
            end: { [weak self] sid in await self?.session?.coordinator.calls.end(sid: sid) },
            finish: { [weak self] sid, decline in self?.finishSystemCall(sid: sid, decline: decline) },
            expirePush: { [weak self] sid in
                guard let self, let push = self.pendingCallPushes[sid] else { return }
                self.expireCallPush(push)
            },
            setMuted: { [weak self] sid, isMuted in
                guard let active = self?.session, active.coordinator.calls.state.sid == sid else { return }
                await active.callMedia.setMicrophone(!isMuted)
            },
            isReady: { [weak self] sid in
                guard let active = self?.session else { return false }
                return active.coordinator.connection == .online && active.coordinator.calls.state.sid == sid
                    && active.coordinator.calls.state.isLive
            },
            state: { [weak self] in self?.session?.coordinator.calls.state ?? .idle },
            isFinished: { [weak self] sid in
                self?.session?.coordinator.calls.isFinished(sid: sid) == true
                    || self?.finishedCallPushes.contains(where: { $0.sid == sid }) == true
            },
            name: { [weak self] conversation in
                self?.session?.coordinator.directory.title(for: conversation)
                    ?? DirectoryStore().title(for: conversation)
            }
        )
        return center
    }

    /// Reports before any network work, including when no scene was created.
    func receiveCallPush(_ push: CallPush?, callerName: String, completion: @escaping @MainActor () -> Void) {
        let verdict = systemCalls.receive(push, callerName: callerName, completion: completion)
        guard verdict == .awaiting, let push else { return }
        pendingCallPushes[push.sid] = push
        // Start the clock before restoration: authentication can take longer
        // than the invitation has left to ring.
        pushDeadlines[push.sid] = Task { [weak self] in
            do {
                while !push.isExpired(at: .now) {
                    try await Task.sleep(for: .seconds(min(60, max(0, push.expires.timeIntervalSinceNow))))
                }
            } catch { return }
            guard let self, self.pendingCallPushes[push.sid] == push else { return }
            self.expireCallPush(push)
        }
        let generation = sessionGeneration
        Task { [weak self] in
            guard let self else { return }
            var backoff = 0.5
            while self.pendingCallPushes[push.sid] == push {
                guard self.sessionGeneration == generation else {
                    self.expireCallPush(push, cancellingAnswer: true)
                    return
                }
                guard !Task.isCancelled, !push.isExpired(at: .now) else {
                    self.expireCallPush(push)
                    return
                }
                let active = await self.ensureSession()
                guard self.sessionGeneration == generation else {
                    self.expireCallPush(push, cancellingAnswer: true)
                    return
                }
                guard !push.isExpired(at: .now) else {
                    self.expireCallPush(push)
                    return
                }
                guard self.pendingCallPushes[push.sid] == push else { return }
                guard let active, self.session === active else {
                    // Offline restoration and a keychain locked before first
                    // unlock are temporary; neither means the call is over.
                    if case .missing = CredentialStore.readSessionID(for: self.server) {
                        self.expireCallPush(push)
                        return
                    }
                    do {
                        try await Task.sleep(for: .seconds(min(backoff, max(0, push.expires.timeIntervalSinceNow))))
                    } catch {
                        self.expireCallPush(push)
                        return
                    }
                    backoff = min(backoff * 2, 2)
                    continue
                }
                guard self.owns(push, in: active) else {
                    self.rememberFinished(push)
                    self.expireCallPush(push, cancellingAnswer: true)
                    return
                }
                self.transferFinishedCallPushes(to: active)
                guard !active.coordinator.calls.isFinished(sid: push.sid) else {
                    self.expireCallPush(push)
                    return
                }
                if active.coordinator.calls.state.isLive && active.coordinator.calls.state.sid != push.sid {
                    // Leave the later stream proposal to the reducer's busy
                    // handling, which sends the caller its normal rejection.
                    self.expireCallPush(push)
                    return
                }
                // CallKit waits for the stream proposal to fill the slot;
                // push metadata never establishes protocol state. A session
                // kept through suspension can still say online on a dead
                // socket, and no scene comes to probe it on a push wake.
                active.coordinator.resume()
                return
            }
        }
    }

    private func owns(_ push: CallPush, in active: ActiveSession) -> Bool {
        PushRegistrationStore.account(forNode: push.node)
            == PushRegistrationStore.Owner(server: server, account: active.coordinator.account.jid)
    }

    private func rememberFinished(_ push: CallPush) {
        if !finishedCallPushes.contains(where: { $0.sid == push.sid && $0.node == push.node }) {
            finishedCallPushes.append(push)
            if finishedCallPushes.count > 32 { finishedCallPushes.removeFirst() }
        }
        if let active = session, owns(push, in: active) { active.coordinator.calls.markFinished(sid: push.sid) }
    }

    private func transferFinishedCallPushes(to active: ActiveSession) {
        for push in finishedCallPushes where owns(push, in: active) {
            active.coordinator.calls.markFinished(sid: push.sid)
        }
        for (sid, push) in pendingCallDeclines where owns(push, in: active) {
            _ = active.coordinator.calls.declinePushedCall(sid: sid)
            pendingCallDeclines[sid] = nil
        }
    }

    private func expireCallPush(_ push: CallPush, cancellingAnswer: Bool = false) {
        // Once adopted, the slot owns the call's unanswered timer.
        guard pendingCallPushes[push.sid] == push else { return }
        if let active = session, active.coordinator.calls.state.isLive,
           active.coordinator.calls.state.sid == push.sid, owns(push, in: active) {
            pendingCallPushes[push.sid] = nil
            pushDeadlines.removeValue(forKey: push.sid)?.cancel()
            return
        }
        // The push deadline ends this wake attempt and its system screen,
        // not the stream proposal's independent ring window. Unlike the
        // plan's blanket "past deadline is over", do not suppress a later
        // proposal when restoration never succeeded (or the slot was busy).
        // A pending system answer instead has its own CallKit deadline.
        let isAnswerPending = systemCalls.pushExpired(sid: push.sid, cancellingAnswer: cancellingAnswer)
        // Keep the owner until an in-flight answer settles: the person can
        // still decline before a session exists, even after push expiry.
        if !isAnswerPending { pendingCallPushes[push.sid] = nil }
        pushDeadlines.removeValue(forKey: push.sid)?.cancel()
    }

    private func finishSystemCall(sid: String, decline: Bool) {
        if let push = pendingCallPushes.removeValue(forKey: sid) {
            rememberFinished(push)
            if decline {
                if let active = session {
                    if owns(push, in: active) { _ = active.coordinator.calls.declinePushedCall(sid: sid) }
                } else {
                    pendingCallDeclines[sid] = push
                }
            }
            pushDeadlines.removeValue(forKey: sid)?.cancel()
        } else {
            session?.coordinator.calls.markFinished(sid: sid)
        }
    }
    #endif

    // MARK: - Lifecycle

    /// Restores a stored session or shows sign-in.
    func bootstrap() async {
        // A push may already have restored the session. The first scene
        // still loads providers; recreated windows share that work.
        guard !Self.isHostingTests, !hasBootstrapped, !isBootstrapping else { return }
        isBootstrapping = true
        defer { isBootstrapping = false }
        DecryptedFileStore.purge()
        await loadProviders()
        _ = await ensureSession()
        // A keychain that cannot be read yet (locked, or briefly not
        // available) is neither a session nor a sign-out. Ask again until it
        // answers; a cancelled wait leaves the next scene to start over.
        var delay = 1.0
        while session == nil, phase == .launching, CredentialStore.readSessionID(for: server) == .unavailable {
            do { try await Task.sleep(for: .seconds(delay)) } catch { return }
            delay = min(delay * 2, 5)
            _ = await ensureSession()
        }
        hasBootstrapped = true
    }

    /// Shares restoration between the UI and a push wake, without waiting
    /// for the sign-in provider list. An offline launch can try again.
    func ensureSession() async -> ActiveSession? {
        guard !Self.isHostingTests else { return nil }
        if let session { return session }
        guard phase == .launching || phase == .signedOut else { return nil }
        if let restoreTask { return await restoreTask.value }
        let generation = sessionGeneration
        let server = server
        let client = client
        let task = Task { [weak self] in
            guard let self else { return nil as ActiveSession? }
            guard !Task.isCancelled, self.sessionGeneration == generation, self.server == server else { return self.session }
            let stored: String
            switch CredentialStore.readSessionID(for: server) {
            case .stored(let value):
                stored = value
            case .missing:
                self.phase = .signedOut
                return self.session
            case .unavailable:
                return self.session
            }
            do {
                let restored = try await client.session(stored)
                guard !Task.isCancelled, self.sessionGeneration == generation, self.server == server,
                      CredentialStore.readSessionID(for: server) == .stored(stored), self.session == nil
                else { return self.session }
                if let restored {
                    self.start(restored)
                } else {
                    CredentialStore.remove(for: server)
                    self.phase = .signedOut
                }
            } catch {
                guard !Task.isCancelled, self.sessionGeneration == generation, self.server == server,
                      self.session == nil else { return self.session }
                // Keep an offline credential so the next push or Retry can restore it.
                self.errorMessage = error.localizedDescription
                if CredentialStore.readSessionID(for: server) != .unavailable { self.phase = .signedOut }
            }
            return self.session
        }
        restoreTask = task
        let active = await task.value
        if sessionGeneration == generation {
            restoreTask = nil
            #if os(iOS)
            // An offline credential must keep receiving pushes so a later
            // wake can retry restoration without opening a scene.
            if active == nil, CredentialStore.readSessionID(for: server) == .missing { setVoIPPushEnabled?(false) }
            #endif
        }
        return active
    }

    private func fenceSessionRestore() {
        sessionGeneration += 1
        restoreTask?.cancel()
        restoreTask = nil
    }

    func sceneActivityChanged(isActive: Bool) {
        notifications.isAppActive = isActive
        if isActive {
            notifications.refreshAuthorizationStatus()
            userInput.appBecameActive()
        }
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
        if case .signedOut = phase {
            _ = await ensureSession()
        }
    }

    // MARK: - Sign in

    /// Starts RFC 8628 device authorization and returns the page to open.
    func beginSignIn(with provider: AuthProvider) async -> URL? {
        fenceSessionRestore()
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
                guard let self, self.isCurrent(authorization) else { return }
                do {
                    let result = try await self.client.poll(authorization)
                    // Cancel, another provider or a server change may have
                    // happened during the request; commit nothing if so.
                    guard self.isCurrent(authorization) else { return }
                    guard case let .complete(sessionID) = result else { continue }
                    let restored = try await self.client.session(sessionID)
                    guard self.isCurrent(authorization) else { return }
                    guard let loaded = restored else {
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
                    guard self.isCurrent(authorization) else { return }
                    self.errorMessage = error.localizedDescription
                    self.phase = .signedOut
                    return
                }
            }
        }
    }

    /// Whether `authorization` is still the sign-in in progress.
    private func isCurrent(_ authorization: DeviceAuthorization) -> Bool {
        !Task.isCancelled && phase == .authorizing(authorization)
    }

    // MARK: - Session

    private func start(_ auth: AuthSession) {
        guard let active = ActiveSession(auth: auth, server: server, preferences: preferences) else {
            errorMessage = "The server returned an invalid account or connection address."
            phase = .signedOut
            return
        }
        let account = active.coordinator.account.jid
        active.coordinator.onAlert = { [weak self] alert in
            guard let self else { return }
            self.notifications.post(alert, for: account, isVisible: self.isOnScreen(alert.conversation))
        }
        active.coordinator.onAuthenticationFailed = { [weak self, weak active] in
            // Only the live session may expire it; a late callback from a
            // session already ended must not sign the next one out.
            guard let self, let active else { return }
            Task { await self.expireSession(active) }
        }
        session = active
        phase = .signedIn
        #if os(iOS)
        transferFinishedCallPushes(to: active)
        setVoIPPushEnabled?(true)
        #endif
        followCalls(of: active)
        followConnection(of: active)
        active.coordinator.start()
        notifications.requestAuthorizationIfNeeded()
        // Actions queued for another account (a failed restore followed by
        // a different sign-in) are dropped, never replayed as this one.
        let pending = pendingNotificationActions.filter { $0.owner == PushRegistrationStore.Owner(server: server, account: account) }
        pendingNotificationActions.removeAll()
        Task {
            for entry in pending {
                await perform(entry.action, for: account, on: entry.owner.server)
            }
        }
    }

    private func isOnScreen(_ conversation: ConversationID) -> Bool {
        notifications.isAppActive && session?.coordinator.unread.activeConversation == conversation
    }

    func signOut() async {
        fenceSessionRestore()
        #if os(iOS)
        setVoIPPushEnabled?(false)
        #endif
        let sessionID = session?.auth.sessionID
        if let coordinator = session?.coordinator {
            await finishPendingPushRegistration?(coordinator)
            let owner = PushRegistrationStore.Owner(server: server, account: coordinator.account.jid)
            for kind in PushTokenKind.allCases {
                for registration in PushRegistrationStore.pendingRetirements(for: owner, kind: kind) {
                    if await coordinator.disablePush(registration) {
                        PushRegistrationStore.removePendingRetirement(registration, for: owner, kind: kind)
                    }
                }
                if let registration = PushRegistrationStore.registration(for: owner, kind: kind),
                   await coordinator.disablePush(registration) {
                    PushRegistrationStore.forget(owner, kind: kind)
                }
            }
        }
        await endSession(signingOut: true)
        if let sessionID {
            try? await client.logout(sessionID)
        }
        CredentialStore.remove(for: server)
        phase = .signedOut
    }

    /// The server rejected the credential: forget it and ask again.
    /// Expires `expired` only if it is still the live session; checked
    /// here, not at the callback, so a session installed in between
    /// survives.
    private func expireSession(_ expired: ActiveSession) async {
        guard session === expired else { return }
        await endSession()
        CredentialStore.remove(for: server)
        errorMessage = "Your session expired. Sign in again."
        phase = .signedOut
    }

    /// Only a sign-out deletes the account's unsent messages; an expired
    /// credential or a server change keeps them for the next sign-in.
    private func endSession(signingOut: Bool = false) async {
        fenceSessionRestore()
        cancelSignIn()
        #if os(iOS)
        setVoIPPushEnabled?(false)
        systemCalls.sessionEnded()
        pendingCallPushes.removeAll()
        for task in pushDeadlines.values { task.cancel() }
        pushDeadlines.removeAll()
        pendingCallDeclines.removeAll()
        #endif
        guard let active = session else { return }
        await finishPendingPushRegistration?(active.coordinator)
        session = nil
        ringer.show(nil)
        #if os(iOS)
        systemCalls.update(state: .idle, isMediaConnected: false, isMuted: false, isDoNotDisturb: false)
        #endif
        pushRegistrationStatus = .waiting
        DecryptedFileStore.purge()
        if signingOut {
            await active.coordinator.signOut()
        } else {
            await active.coordinator.stop()
        }
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
    /// The media of this session's call; the call dock reads and drives it.
    let callMedia: LiveKitCallMediaController
    /// What the person chose on the stage of this session's call.
    let callStage: CallStageModel

    init?(auth: AuthSession, server: URL, preferences: Preferences) {
        guard let jid = BareJID(parsing: auth.jid) ?? JID(parsing: auth.jid)?.bare,
              let webSocket = AuthClient.xmppWebSocketURL(auth.xmppWebsocketURL, server: server)
        else { return nil }
        let account = AccountIdentity(jid: jid, nick: auth.username, resource: ServerSettings.resource)
        let config = WaddleConfig(
            serverUrl: webSocket.absoluteString,
            jid: jid.description,
            accessToken: auth.sessionID,
            resource: ServerSettings.resource,
            // The FFI never hands resume state back, so this app never
            // resumes. Asking the server to park the session would only make
            // it count messages as delivered to a stream nobody resumes and
            // skip the push for them.
            resumable: false
        )
        let outbox: any OutboxStore
        if let file = FileOutboxStore.applicationSupport(for: jid) {
            outbox = file
        } else {
            outbox = InMemoryOutboxStore()
        }
        let callMedia = LiveKitCallMediaController()
        self.auth = auth
        self.callMedia = callMedia
        self.coordinator = SessionCoordinator(
            account: account, port: FFIXmppPort(config: config), outboxStore: outbox,
            handlesCalls: true, callMedia: callMedia
        )
        coordinator.sendsReadReceipts = preferences.sendsReadReceipts
        coordinator.sendsChatStates = preferences.sendsChatStates
        callStage = CallStageModel(calls: coordinator.calls, media: callMedia)
    }
}
