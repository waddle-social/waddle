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
    @ObservationIgnored private var typingSweepTask: Task<Void, Never>?
    @ObservationIgnored private var readyTask: Task<Void, Never>?
    @ObservationIgnored private var reconnectAttempt = 0
    @ObservationIgnored private var isStopped = false
    @ObservationIgnored var outboundQueue: [OutboundMessage] = []
    @ObservationIgnored var failedOutbound: [String: OutboundMessage] = [:]
    @ObservationIgnored var sentOutbound: [String: OutboundMessage] = [:]
    @ObservationIgnored var sentOrder: [String] = []
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
    @ObservationIgnored private var attempt = 0

    /// `connectBudget`: how long an attempt may take to reach the ready
    /// state before it counts as failed (the core reports connect failures
    /// only as diagnostics, never as a disconnect).
    public init(
        account: AccountIdentity,
        port: any XmppPort,
        reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
        connectBudget: TimeInterval = 15
    ) {
        self.account = account
        self.port = port
        self.reconnectPolicy = reconnectPolicy
        self.connectBudget = connectBudget
        let directory = DirectoryStore()
        self.directory = directory
        self.timelines = TimelineStore()
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
    }

    // MARK: - Lifecycle

    /// Starts consuming events and connects.
    public func start() {
        isStopped = false
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

    /// Disconnects for good (sign-out). Clears all session state.
    public func stop() async {
        isStopped = true
        reconnectTask?.cancel()
        reconnectTask = nil
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

    /// Reconnects immediately if offline (app foregrounded, network back).
    public func resume() {
        switch status.connection {
        case .offline, .connecting:
            if case .connecting = status.connection { return }
            reconnectTask?.cancel()
            reconnectTask = nil
            connectNow()
        case .online, .signedOut, .authenticationFailed:
            return
        }
    }

    private func connectNow() {
        guard !isStopped else { return }
        status.connection = .connecting
        attempt += 1
        let current = attempt
        let port = self.port
        Task { await port.connect() }
        connectWatchdog?.cancel()
        connectWatchdog = Task { [weak self, connectBudget] in
            try? await Task.sleep(nanoseconds: UInt64(connectBudget * 1_000_000_000))
            guard !Task.isCancelled else { return }
            await self?.connectAttemptTimedOut(current)
        }
    }

    private func connectAttemptTimedOut(_ timedOut: Int) async {
        guard timedOut == attempt, status.connection == .connecting, !isStopped else { return }
        await port.disconnect()
        guard timedOut == attempt, status.connection == .connecting else { return }
        scheduleReconnect()
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
        isSendReady = false
        sentChatStates.removeAll()
        typingPauseTasks.values.forEach { $0.cancel() }
        typingPauseTasks.removeAll()
        pendingDisplayed.removeAll()
        onDemandRooms.removeAll()
    }

    // MARK: - Events

    func handle(_ event: XmppEvent) {
        switch event {
        case .connected:
            connectWatchdog?.cancel()
            connectWatchdog = nil
            status.connection = .online
            // Runs beside the event loop: the pipeline awaits server
            // round-trips whose answers arrive as events.
            readyTask?.cancel()
            readyTask = Task { [weak self] in await self?.sessionReady() }
        case .disconnected:
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
        case let .deliveryFailed(stanzaID):
            sentMessageFailed(stanzaID, bounced: false)
        case let .inboxPush(entry):
            applyInbox(entry)
        case .authenticationFailed:
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
        if !Task.isCancelled {
            reconnectAttempt = 0
        }
    }

    // MARK: - Message routing

    func route(_ message: WireMessage) {
        if let cursors = message.displayedCursors {
            // A sibling device's XEP-0490 notification is read-state
            // metadata only, and only trusted from our own account.
            guard message.from?.bare == account.jid || message.from == nil else { return }
            cursors.forEach(applyDisplayedCursor)
            return
        }
        // Error bounces are not conversation content; a bounce of one of
        // our sends marks it failed.
        if message.type == .error {
            if let id = message.identity.messageID, deliveries.state(of: id) != nil {
                sentMessageFailed(id, bounced: true)
            }
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
