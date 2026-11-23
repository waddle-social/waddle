import Foundation
import Observation

/// A message the platform layer should surface as a notification.
public struct IncomingAlert: Hashable, Sendable {
    public let conversation: ConversationID
    public let conversationTitle: String
    public let senderName: String
    public let body: String
    /// The timeline row id.
    public let messageID: String
    /// The notification request's identifier: the id the server's push for
    /// this message collapses on, so the two replace each other.
    public let notificationID: String
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

    public let calls: CallStore
    public let timelines: TimelineStore
    public let directory: DirectoryStore
    public let presence: PresenceStore
    /// The bots each room declares; they hold no affiliation and are in a
    /// room only for one send, so presence and member lists never name them.
    public let roomBots: RoomBotStore
    public let typing: TypingStore
    public let unread: UnreadStore
    public let deliveries: DeliveryStore
    public let pins: PinStore
    public let avatars: AvatarStore
    /// The account's contact list; nil until it has been loaded once, so
    /// "not known yet" is never shown as "no contacts".
    public internal(set) var contacts: [Contact]?
    /// Where the last lookup of the contact list stands.
    public internal(set) var contactsLoad = ContactsLoad.idle
    /// The account owns this community, so it may open the admin console.
    /// False until the server has said so.
    public internal(set) var isCommunityOwner = false
    public let history: HistoryStore
    @ObservationIgnored let inbox: InboxStore
    @ObservationIgnored let readCursors: ReadCursorStore
    /// Thread replies fetched for a thread screen, kept out of the
    /// conversation timeline so they never shift its paging or read cursor.
    public let threadHistory: ThreadHistoryStore
    /// The Activity overview: unread rooms with their unread messages and
    /// threads.
    public let unreadOverview: UnreadOverviewStore
    /// The community feed and stories.
    public let community: CommunityStore
    /// The account's sticker packs.
    public let stickers: StickerStore
    /// The community calendar.
    public let events: EventsStore
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
    @ObservationIgnored private let handlesCalls: Bool
    @ObservationIgnored private let callLifecycleOutbox: CallLifecycleOutbox?
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
    /// Server inbox reads made offline, replayed after the next hydrate.
    @ObservationIgnored var pendingInboxReads: [InboxReadKey: PendingInboxRead] = [:]
    @ObservationIgnored var inboxHydrateTask: Task<Void, Never>?
    @ObservationIgnored var inboxHydrateRetryDelays: [TimeInterval] = [2, 8]
    /// A directory lookup that could not read the account's bookmarks is
    /// tried again after these delays, then after the last one until it
    /// reads them (`scheduleDirectoryRetry`).
    @ObservationIgnored var directoryRetryTask: Task<Void, Never>?
    @ObservationIgnored var directoryRetryDelays: [TimeInterval] = [2, 8, 30, 60]
    /// Rows whose last `<mark-read/>` the server refused, with the newest
    /// stanza id that read covered. The row is not read again automatically
    /// while the server still reports that id; a newer message retries.
    @ObservationIgnored var refusedInboxReads: [InboxReadKey: PendingInboxRead] = [:]
    /// XEP-0055 results from `@` mention searches, per room. Users found
    /// there stay addressable by username in that room's sends.
    @ObservationIgnored var mentionSearches = MentionDirectorySearch()
    @ObservationIgnored var visibleConversation: ConversationID?
    /// The conversation `open` was last called for. Screens open and close
    /// a conversation more than once while it stays the one being read:
    /// the split view builds its screen twice, and on a phone a thread or
    /// its details cover it and uncover it again.
    @ObservationIgnored var lastOpenedConversation: ConversationID?
    /// Where the last opened conversation puts its "New messages" divider:
    /// the first row that was unread when it was opened. Kept until another
    /// conversation is opened, as the web does.
    var unreadAnchors: [ConversationID: String] = [:]
    /// The room thread on screen, if any.
    @ObservationIgnored var visibleThread: ThreadKey?
    @ObservationIgnored var isAppActive = true
    @ObservationIgnored var mdsPublishSupported: Bool?
    /// Rooms opened or created this session; rejoined after reconnects.
    @ObservationIgnored var onDemandRooms: Set<BareJID> = []
    /// Rooms deleted this session, here or on another device. None is
    /// joined (a join would make it again) until a lookup that began after
    /// the deletion finds it made again. The app closes them.
    public internal(set) var destroyedRooms: Set<BareJID> = []
    /// For each of `destroyedRooms`, how many directory lookups had begun
    /// when it was deleted.
    @ObservationIgnored var lookupsBeforeDestruction: [BareJID: Int] = [:]
    /// Directory lookups begun this session.
    @ObservationIgnored var lookupsBegun = 0
    /// Rooms whose delete got no clear answer (timed out, cut off). They
    /// may be gone, so they are not joined again this session.
    @ObservationIgnored var unconfirmedDeletions: Set<BareJID> = []
    /// Rooms a space listed when it was deleted here. The server deletes
    /// some and keeps others without saying which, so none is joined
    /// until a discovery lists it again.
    @ObservationIgnored var roomsOfDeletedSpaces: Set<BareJID> = []
    /// Rooms being deleted or left; not joined until the request settles.
    @ObservationIgnored var departingRooms: Set<BareJID> = []
    /// Rooms with a management action in flight; one at a time per room.
    @ObservationIgnored var roomsBeingManaged: Set<BareJID> = []
    /// Local changes replayed over topology requests that began before
    /// them; emptied when no request is in flight.
    @ObservationIgnored var directoryMutations: [DirectoryMutation] = []
    @ObservationIgnored var directoryRefreshesInFlight = 0
    /// The last join sent to each room that has one on its way.
    @ObservationIgnored var joinTails: [BareJID: Task<Void, Never>] = [:]
    /// The last notification-settings write per conversation, so they run in order.
    @ObservationIgnored var notifyWriteTails: [ConversationID: Task<Void, Error>] = [:]
    /// Fences room actions across session teardown, including suspended port calls.
    @ObservationIgnored var roomManagementGeneration = 0

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

    /// The clock presence reads; tests set it.
    @ObservationIgnored var now: () -> Date = Date.init
    @ObservationIgnored var presenceModeSync = PresenceModeSync()
    @ObservationIgnored var idleClock = IdleClock()
    /// What the current stream was last told; nil on a fresh stream.
    @ObservationIgnored var sentPresence: SentPresence?
    @ObservationIgnored var profileEdits = ProfileEdits()

    /// `connectBudget`: how long an attempt may take to reach the ready
    /// state before it counts as failed (the core reports connect failures
    /// only as diagnostics, never as a disconnect). `outboxStore`: where
    /// unsent messages are kept across launches. `handlesCalls` is only for a
    /// host that shows call UI: an invisible invitation must not ring back.
    public init(
        account: AccountIdentity,
        port: any XmppPort,
        reconnectPolicy: ReconnectPolicy = ReconnectPolicy(),
        connectBudget: TimeInterval = 15,
        outboxStore: any OutboxStore = InMemoryOutboxStore(),
        timelineCapacity: Int = 500,
        handlesCalls: Bool = false,
        callMedia: (any CallMediaController)? = nil
    ) {
        self.account = account
        self.port = port
        self.handlesCalls = handlesCalls
        let callLifecycleOutbox = handlesCalls ? CallLifecycleOutbox(port: port) : nil
        self.callLifecycleOutbox = callLifecycleOutbox
        self.calls = CallStore(
            port: port, ownJID: { account.fullJID }, ownBareJID: account.jid, media: callMedia,
            report: { [weak callLifecycleOutbox] in callLifecycleOutbox?.submit($0) })
        self.outboxStore = outboxStore
        self.reconnectPolicy = reconnectPolicy
        self.connectBudget = connectBudget
        let directory = DirectoryStore()
        self.directory = directory
        self.timelines = TimelineStore(maxItemsPerConversation: timelineCapacity)
        self.presence = PresenceStore(isRoom: { directory.isRoom($0) })
        self.roomBots = RoomBotStore()
        self.typing = TypingStore()
        self.unread = UnreadStore()
        self.deliveries = DeliveryStore()
        self.pins = PinStore()
        self.avatars = AvatarStore()
        self.history = HistoryStore()
        self.inbox = InboxStore()
        self.readCursors = ReadCursorStore()
        self.threadHistory = ThreadHistoryStore()
        self.unreadOverview = UnreadOverviewStore()
        self.community = CommunityStore()
        self.stickers = StickerStore()
        self.events = EventsStore(account: account.jid)
        avatars.fetch = { [port] jid, knownID in
            await port.fetchAvatar(of: jid, knownID: knownID)
        }
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
        roomManagementGeneration += 1
        callLifecycleOutbox?.stop()
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
        resetPresenceMode()
        contacts = nil
        contactsLoad = .idle
        isCommunityOwner = false
        await stopCalls()
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
        callLifecycleOutbox?.setOnline(false)
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
        calls.groupPresence.clear()
        roomBots.clear()
        typing.clear()
        unread.clearAll()
        unread.silence([])
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
        pendingInboxReads.removeAll()
        refusedInboxReads.removeAll()
        cancelInboxHydrate()
        cancelDirectoryRetry()
        lastOpenedConversation = nil
        unreadAnchors.removeAll()
        threadHistory.clear()
        unreadOverview.reset()
        community.reset()
        stickers.reset()
        events.reset()
        onDemandRooms.removeAll()
        destroyedRooms.removeAll()
        lookupsBeforeDestruction.removeAll()
        departingRooms.removeAll()
        roomsBeingManaged.removeAll()
        unconfirmedDeletions.removeAll()
        roomsOfDeletedSpaces.removeAll()
        joinTails.removeAll()
        directoryMutations.removeAll()
        extensionCommands.removeAll()
        mentionSearches.clear()
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
            cancelInboxHydrate()
            cancelDirectoryRetry()
            retryWhenConnectSettles = false
            connectWatchdog?.cancel()
            connectWatchdog = nil
            // A slow attempt that succeeds after the watchdog gave up wins;
            // the scheduled retry would only reconnect a live stream.
            reconnectTask?.cancel()
            reconnectTask = nil
            status.connection = .online
            callLifecycleOutbox?.setOnline(true)
            // Avatars may have changed while we were away; rows on screen
            // ask again.
            avatars.markAllStale()
            // Runs beside the event loop: the pipeline awaits server
            // round-trips whose answers arrive as events.
            readyTask?.cancel()
            readyTask = Task { [weak self] in await self?.sessionReady() }
        case .disconnected:
            callLifecycleOutbox?.setOnline(false)
            // Like Android, the call slot survives stream loss and reconnects;
            // call sends can fail while offline, without new recovery semantics.
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
            cancelInboxHydrate()
            cancelDirectoryRetry()
            isSendReady = false
            // Messages may have been missed while offline: loaded pages are
            // no longer known to be current.
            history.markAllStale()
            sentPresence = nil
            presence.clear()
            calls.groupPresence.clear()
            mdsPublishSupported = nil
            if !isStopped, status.connection != .authenticationFailed {
                scheduleReconnect()
            }
        case let .call(event):
            _ = receiveCall(event)
        case let .message(message):
            route(message)
        case let .presence(wirePresence):
            let update = presence.apply(wirePresence)
            if case let .joined(room) = update, directory.hasLeft(room) {
                // A join sent before the room was found left on another
                // device, and answered only now.
                Task { await self.departIfLeft(room) }
            }
            if case let .destroyed(room) = update { forgetDestroyedRoom(room) }
            if handlesCalls, !isStopped {
                switch update {
                case .left(let room), .destroyed(let room), .joinFailed(let room, _):
                    // Out of the room, so out of earshot of its call.
                    calls.groupPresence.clearRoom(room)
                case .contact, .occupant, .joined, .ignored:
                    calls.handleGroupPresence(wirePresence)
                }
            }
            refreshRoomBotsIfUndeclared(by: wirePresence)
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
        case let .avatarChanged(jid, id):
            avatars.avatarChanged(jid, id: id)
        case let .presenceModeChanged(mode):
            presenceModePicked(elsewhere: mode)
        case let .inboxPush(entry):
            applyInbox(entry)
        case .authenticationFailed:
            callLifecycleOutbox?.stop()
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

    /// Stream arrivals follow the host's call-handling policy.
    func receiveCall(_ event: CallEvent) -> Bool {
        guard handlesCalls, !isStopped else { return false }
        calls.handle(event)
        return calls.state.isLive && calls.state.sid == event.sid
    }

    /// The ready pipeline: presence, directory, joins, read state, queue.
    private func sessionReady() async {
        await refreshOwnPresence()
        guard !Task.isCancelled else { return }
        // Start loading the roster in parallel with topology discovery, room
        // joins, and inbox hydration so the People list is ready immediately.
        async let contactsBootstrap: Void = refreshContacts()
        await syncPresenceModeOnConnect()
        guard !Task.isCancelled else {
            _ = await contactsBootstrap
            return
        }
        await refreshDirectory()
        guard !Task.isCancelled else {
            _ = await contactsBootstrap
            return
        }
        async let notifyBootstrap: Void = loadNotifyModes()
        if await !hydrateInbox() {
            scheduleInboxHydrate()
        }
        guard !Task.isCancelled else {
            _ = await notifyBootstrap
            _ = await contactsBootstrap
            return
        }
        await bootstrapDisplayedCursors()
        await notifyBootstrap
        guard !Task.isCancelled else {
            _ = await contactsBootstrap
            return
        }
        isSendReady = true
        await flushOutboundQueue()
        guard !Task.isCancelled else {
            _ = await contactsBootstrap
            return
        }
        await drainPendingDisplayed()
        await reloadActiveConversation()
        await contactsBootstrap
        guard !Task.isCancelled else { return }
        // Only a session that got all the way through resets the backoff,
        // so a stream that drops right after binding keeps backing off.
        guard !Task.isCancelled else { return }
        reconnectAttempt = 0
        await refreshCommunityOwnership()
        guard !Task.isCancelled else { return }
        // Last: discovery is several round-trips and nothing above needs it.
        await refreshExtensionCommands()
    }

    // MARK: - Message routing

    func route(_ message: WireMessage) {
        // Errors only change send state through the typed rejection event,
        // where the outbound id and addresses are checked together.
        guard message.type != .error else { return }
        // PEP notifications (avatar, mood…) are headlines without a body;
        // their typed payloads arrive as their own events.
        if message.type == .headline, message.body == nil, message.displayedCursors == nil, message.pinEvent == nil {
            return
        }
        var message = message
        if let cursors = message.displayedCursors {
            // A sibling device's XEP-0490 notification is read-state
            // metadata only, and only trusted from our own account.
            guard message.from?.bare == account.jid || message.from == nil else { return }
            cursors.forEach(applyDisplayedCursor)
            return
        }
        guard let route = account.route(message) else { return }
        if let pin = message.pinEvent {
            if route.conversation.isRoom {
                pins.apply(pin, in: route.conversation.jid)
            }
            return
        }
        trackChatState(message, route: route)
        // An undelayed live message is being spoken now by the present
        // occupant: pin the row to them, so a later holder of the nick never
        // takes it over. That is the real JID the room disclosed for them,
        // else us when the nick is the one our self-presence carries. Our
        // configured nick alone proves nothing: the room may have given us
        // another (status 210) and someone else this one. A delayed message
        // (XEP-0203: room history, replay) may predate a handover, so it
        // keeps only what the room vouched for.
        if route.conversation.isRoom, message.isLive, message.timestamp == nil,
           message.authorRealJID == nil, let nick = message.from?.resource {
            let room = route.conversation.jid
            message.authorRealJID = presence.occupant(named: nick, in: room)?.realJID
                ?? (presence.selfNicks[room] == nick ? account.jid : nil)
        }
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
        // A push is named after the id the room's archive gave the message,
        // or the recipient's own archive for a 1:1 message (never the
        // server's domain, which `directArchiveAuthorities` also accepts).
        let archive = conversation.isRoom ? conversation.jid : account.jid
        onAlert?(IncomingAlert(
            conversation: conversation,
            conversationTitle: directory.title(for: conversation),
            senderName: item.authorName,
            body: preview(of: item) ?? "",
            messageID: item.id,
            notificationID: item.identity.stanzaID(assignedBy: archive) ?? "\(conversation)#\(item.id)",
            mentionsMe: mentionsMe
        ))
    }

    /// Whether `item` mentions the account: by address, by our nickname in
    /// the room, or as a room-wide broadcast. Our own messages never do.
    public func mentionsMe(_ item: TimelineItem) -> Bool {
        !item.isMine && mentions(item.message, in: item.conversation)
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
        guard let applied = inbox.apply(entry) else { return }
        // An answer from before the room was deleted counts nothing for it.
        if applied.kind == .room, destroyedRooms.contains(applied.partner) { return }
        if let threadID = applied.threadID {
            // Only room threads have a badge; the server also keeps thread
            // rows for direct conversations (call threads), which must not
            // overwrite the conversation's own count.
            guard applied.kind == .room else { return }
            let thread = ThreadKey(room: applied.partner, threadID: threadID)
            unread.setThread(applied.unread, for: thread)
            if applied.unread > 0, thread == unread.activeThread,
               !wasRefused(InboxReadKey(partner: thread.room, threadID: thread.threadID), newest: applied.lastStanzaID) {
                Task { await self.markThreadReadIfVisible(thread) }
            }
            return
        }
        let conversation: ConversationID = applied.kind == .room ? .room(applied.partner) : .direct(applied.partner)
        unread.set(applied.unread, for: conversation)
        if applied.kind == .direct, let date = applied.lastUpdatedDate {
            directory.touchDirect(applied.partner, at: date, preview: applied.preview)
        }
        // The server counted something (a thread reply, a reaction) while
        // the conversation is on screen: read it so the row does not stay
        // unread on the server.
        if applied.unread > 0, conversation == unread.activeConversation,
           !wasRefused(InboxReadKey(partner: conversation.jid, threadID: nil), newest: applied.lastStanzaID) {
            Task { await self.markDisplayedIfVisible(conversation) }
        }
    }
}

/// Observable session-wide status the UI binds to.
public struct SessionStatus: Equatable, Sendable {
    public var connection: ConnectionStatus = .signedOut
    /// How the account's presence is decided; the same on every device.
    public var presenceMode: PresenceMode = .automatic
    /// What this device shows: the picked status, or what its idle time
    /// says while the mode is automatic.
    public var availability: Availability = .available
    public var statusText: String?
    public var mood: UserMood?
    public var activity: UserActivity?
    public var tune: UserTune?
    /// The account's own details, once loaded.
    public var details: ProfileDetails?

    public init() {}
}
