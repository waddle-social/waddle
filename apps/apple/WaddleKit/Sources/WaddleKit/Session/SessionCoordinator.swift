import Foundation
import Observation

/// A message the platform layer should surface as a notification.
public struct IncomingAlert: Hashable, Sendable {
    public let conversation: ConversationID
    public let conversationTitle: String
    public let senderName: String
    public let body: String
    /// The row id, for grouping and mark-read actions.
    public let messageID: String
    public let mentionsMe: Bool
}

/// Owns one signed-in session: drives the `XmppPort`, routes its events
/// into the stores, and exposes the conversation actions the UI calls.
@MainActor
@Observable
public final class SessionCoordinator {
    public let account: AccountIdentity
    public internal(set) var status: SessionStatus = .init()
    public var connection: ConnectionStatus { status.connection }

    public let timelines: TimelineStore
    public let directory: DirectoryStore
    public let presence: PresenceStore
    public let typing: TypingStore
    public let unread: UnreadStore
    public let deliveries: DeliveryStore
    public let pins: PinStore
    public let avatars: AvatarStore
    public let history: HistoryStore
    @ObservationIgnored let inbox: InboxStore
    @ObservationIgnored let readCursors: ReadCursorStore
    /// XEP-0050 extension commands, discovered once the session is ready.
    public internal(set) var extensionCommands: [ExtensionCommand] = []

    /// Called for each message that should notify. The platform layer
    /// applies app-state policy (foreground, focus) on top.
    @ObservationIgnored public var onAlert: (@MainActor (IncomingAlert) -> Void)?
    /// Called when the server rejects the credential.
    @ObservationIgnored public var onAuthenticationFailed: (@MainActor () -> Void)?
    /// Whether XEP-0333 displayed markers are sent (read receipts).
    @ObservationIgnored public var sendsReadReceipts = true

    @ObservationIgnored let port: any XmppPort
    @ObservationIgnored private let reconnectPolicy: ReconnectPolicy
    @ObservationIgnored private var eventTask: Task<Void, Never>?
    @ObservationIgnored private var reconnectTask: Task<Void, Never>?
    @ObservationIgnored var foregroundProbeTask: Task<Void, Never>?
    @ObservationIgnored private var typingSweepTask: Task<Void, Never>?
    @ObservationIgnored private(set) var readyTask: Task<Void, Never>?
    @ObservationIgnored private var reconnectAttempt = 0
    @ObservationIgnored private(set) var isStopped = false
    @ObservationIgnored var outboundQueue: [OutboundMessage] = []
    @ObservationIgnored var failedOutbound: [String: OutboundMessage] = [:]
    @ObservationIgnored var sentOutbound: [String: OutboundMessage] = [:]
    @ObservationIgnored var sentOrder: [String] = []
    /// Keep confirmed sends for this session: SM acknowledgement does not
    /// impose a deadline on a later recipient rejection.
    @ObservationIgnored var recentlyAcknowledgedOutbound: [String: OutboundMessage] = [:]
    /// IDs explicitly retried while a previous send continuation may still
    /// be suspended. Its eventual result must not settle the retry.
    @ObservationIgnored var retryingOutboundIDs: Set<String> = []
    /// A bounce can arrive after a generic failure was retried but before
    /// that retry begins; retire the still-live stream before sending it.
    @ObservationIgnored var resetBeforeRetryIDs: Set<String> = []
    @ObservationIgnored let outboxStore: any OutboxStore
    /// The saved outbox was loaded this start; only then is it saved.
    @ObservationIgnored var isOutboxLoaded = false
    @ObservationIgnored var savedOutbox: [PersistedOutbound] = []
    /// True once the ready pipeline rejoined rooms; sends drain only then.
    @ObservationIgnored var isSendReady = false
    @ObservationIgnored var isFlushing = false
    @ObservationIgnored var sentChatStates: [ConversationID: ChatState] = [:]
    @ObservationIgnored var typingPauseTasks: [ConversationID: Task<Void, Never>] = [:]
    @ObservationIgnored var pendingDisplayed: Set<ConversationID> = []
    @ObservationIgnored var visibleConversation: ConversationID?
    @ObservationIgnored var isAppActive = true
    @ObservationIgnored var mdsPublishSupported: Bool?
    /// Rooms opened or created this session; rejoined after reconnects.
    @ObservationIgnored var onDemandRooms: Set<BareJID> = []

    @ObservationIgnored private let connectBudget: TimeInterval
    @ObservationIgnored private var connectWatchdog: Task<Void, Never>?
    /// One connect at a time: the FFI keeps a single stream handle and a
    /// second attempt would overwrite, and orphan, the first one's stream.
    @ObservationIgnored private var isConnectInFlight = false
    /// A transport is being retired; the next attempt must wait for its
    /// terminal `.disconnected` event.
    @ObservationIgnored var isConnectResetting = false
    /// A retry was due while the FFI connect call was still running.
    @ObservationIgnored private var retryWhenConnectSettles = false
    @ObservationIgnored private var attempt = 0
    @ObservationIgnored var connectionEpoch = 0

    /// `connectBudget`: how long an attempt may take to reach the ready
    /// state before it counts as failed (the core reports connect failures
    /// only as diagnostics, never as a disconnect). `outboxStore`: where
    /// unsent messages are kept across launches.
    public init(
        account: AccountIdentity,
        port: any XmppPort,
        reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
        connectBudget: TimeInterval = 15,
        outboxStore: any OutboxStore = InMemoryOutboxStore(),
        timelineCapacity: Int = 500
    ) {
        self.account = account
        self.port = port
        self.outboxStore = outboxStore
        self.reconnectPolicy = reconnectPolicy
        self.connectBudget = connectBudget
        let directory = DirectoryStore()
        self.directory = directory
        self.timelines = TimelineStore(maxItemsPerConversation: timelineCapacity)
        self.presence = PresenceStore(isRoom: { directory.isRoom($0) })
        self.typing = TypingStore()
        self.unread = UnreadStore()
        self.deliveries = DeliveryStore()
        self.pins = PinStore()
        self.avatars = AvatarStore()
        self.history = HistoryStore()
        self.inbox = InboxStore()
        self.readCursors = ReadCursorStore()
        timelines.account = account
        timelines.onArchiveTrimmed = { [weak self] conversation, cursor in
            self?.archiveTrimmed(conversation, cursor: cursor)
        }
    }

    // MARK: - Lifecycle

    /// Restores the saved outbox, starts consuming events and connects.
    public func start() {
        isStopped = false
        connectionEpoch += 1
        isConnectResetting = false
        retryWhenConnectSettles = false
        restoreOutboxIfNeeded()
        guard eventTask == nil else { return }
        let events = port.events
        eventTask = Task { [weak self] in
            for await event in events {
                guard let self else { return }
                self.handle(event)
            }
        }
        connectNow()
    }

    /// Disconnects and clears all session state. The saved outbox is kept
    /// for the next `start()`; `signOut()` also deletes it.
    public func stop() async {
        isStopped = true
        reconnectTask?.cancel()
        reconnectTask = nil
        foregroundProbeTask?.cancel()
        foregroundProbeTask = nil
        connectionEpoch += 1
        isConnectResetting = false
        retryWhenConnectSettles = false
        typingSweepTask?.cancel()
        typingSweepTask = nil
        readyTask?.cancel()
        readyTask = nil
        connectWatchdog?.cancel()
        connectWatchdog = nil
        await port.disconnect()
        eventTask?.cancel()
        eventTask = nil
        status.connection = .signedOut
        clearStores()
    }

    /// Recovers the session on foreground, including queued sends on a
    /// transport that still appears online after iOS suspended the app.
    public func resume() {
        restoreOutboxIfNeeded()
        switch status.connection {
        case .offline, .connecting:
            if case .connecting = status.connection { return }
            reconnectTask?.cancel()
            reconnectTask = nil
            connectNow()
        case .online:
            guard !isConnectResetting else { return }
            guard foregroundProbeTask == nil else { return }
            let epoch = connectionEpoch
            foregroundProbeTask = Task { [weak self] in
                guard let self else { return }
                let isHealthy = await self.port.probeConnection()
                guard !Task.isCancelled, epoch == self.connectionEpoch else { return }
                self.foregroundProbeTask = nil
                guard !self.isStopped, self.status.connection == .online else { return }
                guard isHealthy else {
                    await self.resetStaleConnection()
                    return
                }
                if self.isSendReady, !self.outboundQueue.isEmpty {
                    await self.flushOutboundQueue()
                }
            }
        case .signedOut, .authenticationFailed:
            return
        }
    }

    /// Retires a transport whose health or send result is uncertain. The
    /// reconnect loop starts only after the port emits its terminal event.
    func resetStaleConnection() async {
        guard !isStopped, status.connection == .online, !isConnectResetting else { return }
        isConnectResetting = true
        foregroundProbeTask?.cancel()
        foregroundProbeTask = nil
        isSendReady = false
        readyTask?.cancel()
        readyTask = nil
        await port.disconnect()
    }

    private func connectNow() {
        guard !isStopped, status.connection != .online else { return }
        guard !isConnectResetting else {
            retryWhenConnectSettles = true
            return
        }
        guard !isConnectInFlight else {
            retryWhenConnectSettles = true
            return
        }
        status.connection = .connecting
        attempt += 1
        let current = attempt
        let port = self.port
        isConnectInFlight = true
        Task { [weak self] in
            await port.connect()
            // A connect that completes after sign-out (or after the
            // coordinator is gone) must not leave a live stream behind.
            guard let self, !self.isStopped else {
                self?.isConnectInFlight = false
                await port.disconnect()
                return
            }
            self.connectSettled()
        }
        connectWatchdog?.cancel()
        connectWatchdog = Task { [weak self, connectBudget] in
            try? await Task.sleep(nanoseconds: UInt64(connectBudget * 1_000_000_000))
            guard !Task.isCancelled else { return }
            await self?.connectAttemptTimedOut(current)
        }
    }

    /// The FFI connect call returned. Retire its driver first if a retry
    /// became due while transport setup was still running.
    private func connectSettled() {
        isConnectInFlight = false
        guard retryWhenConnectSettles else { return }
        retryWhenConnectSettles = false
        guard !isStopped else { return }
        guard status.connection != .online else {
            isConnectResetting = false
            return
        }
        // `port.connect()` can return after opening the transport but before
        // XMPP binding finishes. If a retry became due while it was pending,
        // close this attempt and wait for its terminal event before opening
        // another driver.
        isConnectResetting = true
        Task { [weak self] in await self?.port.disconnect() }
    }

    private func connectAttemptTimedOut(_ timedOut: Int) async {
        guard timedOut == attempt, status.connection == .connecting, !isStopped else { return }
        guard !isConnectInFlight else {
            // The FFI has not returned a handle yet, so a disconnect cannot
            // stop this attempt. Wait for it to settle, then close it before
            // allowing the reconnect loop to start another driver.
            isConnectResetting = true
            retryWhenConnectSettles = true
            status.connection = .offline(retryAt: nil)
            return
        }
        isConnectResetting = true
        await port.disconnect()
    }

    private func scheduleReconnect() {
        guard !isStopped, reconnectTask == nil else { return }
        let delay = reconnectPolicy.delay(forAttempt: reconnectAttempt, unit: Double.random(in: 0..<1))
        reconnectAttempt += 1
        let retryAt = Date().addingTimeInterval(delay)
        status.connection = .offline(retryAt: retryAt)
        reconnectTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            guard !Task.isCancelled, let self else { return }
            self.reconnectTask = nil
            self.connectNow()
        }
    }

    private func clearStores() {
        timelines.clear()
        directory.clear()
        presence.clear()
        typing.clear()
        unread.clearAll()
        deliveries.clear()
        pins.clear()
        avatars.clear()
        history.clear()
        inbox.clear()
        readCursors.clear()
        outboundQueue.removeAll()
        failedOutbound.removeAll()
        sentOutbound.removeAll()
        sentOrder.removeAll()
        recentlyAcknowledgedOutbound.removeAll()
        retryingOutboundIDs.removeAll()
        resetBeforeRetryIDs.removeAll()
        isOutboxLoaded = false
        savedOutbox.removeAll()
        isSendReady = false
        sentChatStates.removeAll()
        typingPauseTasks.values.forEach { $0.cancel() }
        typingPauseTasks.removeAll()
        pendingDisplayed.removeAll()
        onDemandRooms.removeAll()
        extensionCommands.removeAll()
    }

    // MARK: - Events

    func handle(_ event: XmppEvent) {
        switch event {
        case .connected:
            // Signed out: a late connect is torn down by its own task.
            guard !isStopped else { return }
            guard !isConnectResetting else { return }
            connectionEpoch += 1
            foregroundProbeTask?.cancel()
            foregroundProbeTask = nil
            retryWhenConnectSettles = false
            connectWatchdog?.cancel()
            connectWatchdog = nil
            // A slow attempt that succeeds after the watchdog gave up wins;
            // the scheduled retry would only reconnect a live stream.
            reconnectTask?.cancel()
            reconnectTask = nil
            status.connection = .online
            // Runs beside the event loop: the pipeline awaits server
            // round-trips whose answers arrive as events.
            readyTask?.cancel()
            readyTask = Task { [weak self] in await self?.sessionReady() }
        case .disconnected:
            // Every disconnect creates a fresh FFI driver; its old
            // XEP-0198 state is not resumed, so replay unconfirmed sends.
            requeueUnconfirmedSendsForFreshStream()
            isConnectResetting = false
            retryWhenConnectSettles = false
            connectionEpoch += 1
            foregroundProbeTask?.cancel()
            foregroundProbeTask = nil
            readyTask?.cancel()
            readyTask = nil
            isSendReady = false
            // Messages may have been missed while offline: loaded pages are
            // no longer known to be current.
            history.markAllStale()
            presence.clear()
            mdsPublishSupported = nil
            if !isStopped, status.connection != .authenticationFailed {
                scheduleReconnect()
            }
        case let .message(message):
            route(message)
        case let .presence(wirePresence):
            presence.apply(wirePresence)
        case let .deliveryAcked(stanzaID):
            deliveries.acknowledged(stanzaID)
            // A late ack can arrive after a reset has put this message back
            // in the queue. The server already has it, so don't send it again.
            if deliveries.state(of: stanzaID) == .acknowledged {
                sentMessageAcknowledged(stanzaID)
            }
            persistOutbox()
        case let .deliveryFailed(stanzaID):
            sentMessageFailed(stanzaID, bounced: false)
        case let .messageRejected(stanzaID, from, to):
            messageRejected(stanzaID, from: from, to: to)
        case let .inboxPush(entry):
            applyInbox(entry)
        case .authenticationFailed:
            connectionEpoch += 1
            foregroundProbeTask?.cancel()
            foregroundProbeTask = nil
            isStopped = true
            reconnectTask?.cancel()
            reconnectTask = nil
            status.connection = .authenticationFailed
            onAuthenticationFailed?()
        }
    }

    /// The ready pipeline: presence, directory, joins, read state, queue.
    private func sessionReady() async {
        await port.sendPresence(status.availability, status: status.statusText)
        guard !Task.isCancelled else { return }
        await refreshDirectory()
        guard !Task.isCancelled else { return }
        await hydrateInbox()
        guard !Task.isCancelled else { return }
        await bootstrapDisplayedCursors()
        guard !Task.isCancelled else { return }
        await loadNotifyModes()
        guard !Task.isCancelled else { return }
        isSendReady = true
        await flushOutboundQueue()
        guard !Task.isCancelled else { return }
        await drainPendingDisplayed()
        await reloadActiveConversation()
        // Only a session that got all the way through resets the backoff,
        // so a stream that drops right after binding keeps backing off.
        guard !Task.isCancelled else { return }
        reconnectAttempt = 0
        // Last: discovery is several round-trips and nothing above needs it.
        await refreshExtensionCommands()
    }

    // MARK: - Message routing

    func route(_ message: WireMessage) {
        // Errors only change send state through the typed rejection event,
        // where the outbound id and addresses are checked together.
        guard message.type != .error else { return }
        if let cursors = message.displayedCursors {
            // A sibling device's XEP-0490 notification is read-state
            // metadata only, and only trusted from our own account.
            guard message.from?.bare == account.jid || message.from == nil else { return }
            cursors.forEach(applyDisplayedCursor)
            return
        }
        guard
              let route = account.route(from: message.from, to: message.to, isGroupchat: message.isGroupchat)
        else { return }
        // MUC private messages (type chat from room/nick) are not 1:1
        // conversations with the room; they are unsupported for now.
        if route.conversation.kind == .direct, directory.isRoom(route.conversation.jid) {
            return
        }
        if let pin = message.pinEvent {
            if route.conversation.isRoom {
                pins.apply(pin, in: route.conversation.jid)
            }
            return
        }
        trackChatState(message, route: route)
        let result = timelines.ingest(message, route: route)
        if route.isMine {
            // Our own copy back from the server confirms an unacked send,
            // and delivers one the core had reported failed and replayed.
            confirmOwnCopy(message)
            persistOutbox()
        }
        guard case let .inserted(item) = result else { return }
        if !route.conversation.isRoom, message.isLive {
            directory.touchDirect(route.conversation.jid, at: item.sentAt, preview: preview(of: item))
        }
        recordActivity(item, message: message, route: route)
    }

    private func trackChatState(_ message: WireMessage, route: MessageRoute) {
        guard !route.isMine, let from = message.from else { return }
        let name = route.conversation.isRoom ? (from.resource ?? "") : (from.bare.localpart ?? from.bare.domain)
        guard !name.isEmpty else { return }
        if let state = message.chatState {
            typing.apply(state, from: name, in: route.conversation)
            armTypingSweep()
        }
        if message.body != nil {
            typing.messageArrived(from: name, in: route.conversation)
        }
    }

    private func armTypingSweep() {
        guard typingSweepTask == nil else { return }
        typingSweepTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                guard let self else { return }
                if !self.typing.sweep() {
                    self.typingSweepTask = nil
                    return
                }
            }
        }
    }

    /// Unread and alerts for a genuinely new, live, feed-visible row.
    private func recordActivity(_ item: TimelineItem, message: WireMessage, route: MessageRoute) {
        guard message.isLive, !route.isMine, item.isFeedVisible else { return }
        let conversation = route.conversation
        var ids = item.identity.all
        ids.insert(item.id)
        let mentionsMe = mentions(message, in: conversation)
        if !inbox.wasAccounted(conversation.jid, ids: ids) {
            unread.liveMessage(in: conversation, isMine: false, mentionsMe: mentionsMe)
        }
        if conversation == unread.activeConversation {
            Task { await self.markDisplayedIfVisible(conversation) }
        }
        let mode = directory.notifyMode(for: conversation)
        let shouldAlert: Bool
        switch mode {
        case .always: shouldAlert = true
        case .onMention: shouldAlert = mentionsMe
        case .never: shouldAlert = false
        }
        guard shouldAlert else { return }
        onAlert?(IncomingAlert(
            conversation: conversation,
            conversationTitle: directory.title(for: conversation),
            senderName: item.authorName,
            body: preview(of: item) ?? "",
            messageID: item.id,
            mentionsMe: mentionsMe
        ))
    }

    /// XEP-0372 mention of the account (bare JID, or our occupant JID in
    /// the room), or a room-wide broadcast mention.
    func mentions(_ message: WireMessage, in conversation: ConversationID) -> Bool {
        if message.broadcastMention != nil { return true }
        for jid in message.mentionedJIDs {
            if jid.bare == account.jid { return true }
            if conversation.isRoom, jid.bare == conversation.jid, jid.resource == account.nick { return true }
        }
        return false
    }

    func preview(of item: TimelineItem) -> String? {
        if item.tombstone != nil { return nil }
        if let action = MeAction.presentation(ofBody: item.body, actor: item.authorName) { return action }
        let text = item.body.trimmingCharacters(in: .whitespacesAndNewlines)
        if !text.isEmpty, !item.message.sharedFiles.contains(where: { $0.url.absoluteString == text }) {
            return text
        }
        if let file = item.message.sharedFiles.first {
            if file.isImage { return "📷 Photo" }
            if file.isVideo { return "🎬 Video" }
            return "📎 \(file.displayName)"
        }
        return text.isEmpty ? nil : text
    }

    func applyInbox(_ entry: InboxEntry) {
        guard let applied = inbox.apply(entry), applied.threadID == nil else { return }
        let conversation: ConversationID = applied.kind == .room ? .room(applied.partner) : .direct(applied.partner)
        unread.set(applied.unread, for: conversation)
        if applied.kind == .direct, let date = applied.lastUpdatedDate {
            directory.touchDirect(applied.partner, at: date, preview: applied.preview)
        }
    }
}

/// Observable session-wide status the UI binds to.
public struct SessionStatus: Equatable, Sendable {
    public var connection: ConnectionStatus = .signedOut
    public var availability: Availability = .available
    public var statusText: String?
    public var mood: UserMood?

    public init() {}
}
