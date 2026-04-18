import Foundation
import SwiftUI

private enum ChatSendError: LocalizedError {
    case noSession
    case noWaddle
    case noRoom

    var errorDescription: String? {
        switch self {
        case .noSession:
            return "Sign in again to reconnect live chat."
        case .noWaddle:
            return "Choose a waddle before sending a message."
        case .noRoom:
            return "Choose a channel before sending a message."
        }
    }
}

private struct TimelineEventDescriptor {
    let event: XMPPMessageEvent
    let fallbackID: String?
}

private struct TimelineCorrectionUpdate {
    let targetID: String
    let body: String
    let timestamp: Date?
}

private struct TimelineReactionUpdate {
    let targetID: String
    let senderName: String
    let emojis: [String]
}

@MainActor
final class AppModel: ObservableObject {
    @Published var serverURLText: String
    @Published var providers: [AuthProvider] = []
    @Published var session: WaddleSession?
    @Published var publicWaddles: [WaddleSummary] = []
    @Published var selectedWaddleID: String?
    @Published var channels: [ChannelSummary] = []
    @Published var selectedChannelID: String?
    @Published var members: [MemberSummary] = []
    @Published var searchQuery = ""
    @Published var joinedWaddleIDs: Set<String> = []
    @Published var deviceAuth: DeviceStartResponse?
    @Published var errorMessage = ""
    @Published var isLoadingProviders = false
    @Published var isLoadingWaddles = false
    @Published var isLoadingStructure = false
    @Published var isCreatingWaddle = false

    let chatStore: ChatSurfaceStore

    private var serverURL: URL
    private var client: WaddleAPIClient
    private var publicCatalogWaddles: [WaddleSummary] = []
    private var accessibleWaddles: [WaddleSummary] = []
    private var devicePollTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private var searchTask: Task<Void, Never>?
    private var xmppService: XMPPService?
    private var xmppEventsTask: Task<Void, Never>?
    private var messagesByRoomJID: [String: [ChatTimelineMessage]] = [:]
    private var presenceByRoomJID: [String: [String: ChatPresenceState]] = [:]
    private var joinedRoomJIDs: Set<String> = []
    private var roomJoinContinuations: [String: CheckedContinuation<Void, Error>] = [:]
    private var roomJoinTimeoutTasks: [String: Task<Void, Never>] = [:]
    private var roomHistoryBeforeCursorByRoomJID: [String: String] = [:]
    private let roomHistoryPageSize = 50

    init() {
        let persistedServerURL = AppConfig.persistedServerURL
        serverURL = persistedServerURL
        serverURLText = persistedServerURL.absoluteString
        client = WaddleAPIClient(serverURL: persistedServerURL)
        chatStore = ChatSurfaceStore()
        chatStore.setSendHandler { [weak self] text, room, replyTo in
            guard let self else { return }
            try await self.sendMessage(text, room: room, replyTo: replyTo)
        }
        chatStore.setRoomHistoryLoadHandler { [weak self] room, before in
            guard let self else {
                return ChatRoomHistoryPage(messages: [], hasMoreOlderMessages: false)
            }
            return try await self.loadRoomHistory(for: room, before: before)
        }
        updateChatSurfaceState()
        Task { await bootstrap() }
    }

    var selectedWaddle: WaddleSummary? {
        guard let selectedWaddleID else { return nil }
        return publicWaddles.first(where: { $0.id == selectedWaddleID })
    }

    var selectedChannel: ChannelSummary? {
        guard let selectedChannelID else { return nil }
        return channels.first(where: { $0.id == selectedChannelID })
    }

    var chatMembers: [ChatRoomMember] {
        let presence = presenceByRoomJID[currentRoomJID ?? ""] ?? [:]
        return members.map { member in
            ChatRoomMember(
                id: member.userID,
                displayName: member.username,
                presence: presence[member.username] ?? .offline,
                isSelf: member.userID == session?.userID,
                role: member.role,
                affiliation: nil,
                avatarInitials: initials(from: member.username)
            )
        }
    }

    func applyServerURL() async {
        guard let next = AppConfig.normalizedServerURL(from: serverURLText) else {
            errorMessage = "Enter a valid server URL."
            return
        }

        if next == serverURL {
            return
        }

        serverURL = next
        serverURLText = next.absoluteString
        client = WaddleAPIClient(serverURL: next)
        AppConfig.saveServerURL(next)
        await clearSessionState()
        await bootstrap()
    }

    func bootstrap() async {
        errorMessage = ""
        await loadProviders()

        guard let storedSessionID = AppConfig.storedSessionID(for: serverURL) else {
            updateChatSurfaceState()
            return
        }

        do {
            guard let loaded = try await client.session(sessionID: storedSessionID), !loaded.isExpired else {
                AppConfig.clearSessionID(for: serverURL)
                updateChatSurfaceState()
                return
            }
            await applyAuthenticatedSession(loaded, persistSession: false)
        } catch {
            errorMessage = error.localizedDescription
            updateChatSurfaceState()
        }
    }

    func loadProviders() async {
        isLoadingProviders = true
        defer { isLoadingProviders = false }

        do {
            providers = try await client.providers()
        } catch {
            providers = []
            errorMessage = error.localizedDescription
        }
    }

    func startDeviceAuthorization(provider: AuthProvider, openURL: OpenURLAction) async {
        errorMessage = ""
        cancelDeviceAuthorization()

        do {
            let flow = try await client.startDeviceAuth(providerID: provider.id)
            deviceAuth = flow
            openVerificationPage(for: flow, openURL: openURL)
            beginPolling(for: flow)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func reopenDeviceVerification(openURL: OpenURLAction) {
        guard let flow = deviceAuth else {
            return
        }
        openVerificationPage(for: flow, openURL: openURL)
    }

    func cancelDeviceAuthorization() {
        devicePollTask?.cancel()
        devicePollTask = nil
        deviceAuth = nil
    }

    func signOut() async {
        let currentSessionID = session?.sessionID
        cancelDeviceAuthorization()
        await clearSessionState()

        if let currentSessionID {
            do {
                try await client.logout(sessionID: currentSessionID)
            } catch {
                errorMessage = error.localizedDescription
            }
        }

        AppConfig.clearSessionID(for: serverURL)
        await loadProviders()
    }

    func refreshPublicWaddles() async {
        guard let session else { return }
        isLoadingWaddles = true
        defer { isLoadingWaddles = false }

        do {
            let previousSelection = selectedWaddleID
            publicCatalogWaddles = try await client.listPublicWaddles(
                sessionID: session.sessionID,
                query: searchQuery
            )
            mergeVisibleWaddles()
            if previousSelection == nil,
               let selectedWaddleID,
               xmppService?.connectionState == .ready {
                await selectWaddle(selectedWaddleID)
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func schedulePublicWaddleSearch() {
        searchTask?.cancel()
        searchTask = Task {
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard !Task.isCancelled else { return }
            await refreshPublicWaddles()
        }
    }

    func selectWaddle(_ waddleID: String?) async {
        guard selectedWaddleID != waddleID || channels.isEmpty else {
            return
        }

        selectedWaddleID = waddleID
        channels = []
        selectedChannelID = nil
        members = []
        syncChatRooms()
        syncChatMembers()
        syncChatMessages()
        updateChatSurfaceState()

        guard let waddleID else {
            return
        }

        await loadStructure(for: waddleID)
    }

    func selectChannel(_ channelID: String?) async {
        selectedChannelID = channelID
        syncChatRooms()
        syncChatMembers()
        syncChatMessages()
        updateChatSurfaceState()

        guard channelID != nil else {
            return
        }

        do {
            try await joinSelectedChannel()
            await chatStore.refreshSelectedRoomHistory()
            updateChatSurfaceState()
        } catch {
            errorMessage = error.localizedDescription
            chatStore.setBannerState(.error(message: error.localizedDescription))
            chatStore.failRoomHistoryLoad(error.localizedDescription)
            updateChatSurfaceState()
        }
    }

    func reloadSelectedWaddleStructure() async {
        guard let selectedWaddleID else { return }
        await loadStructure(for: selectedWaddleID)
    }

    func join(_ waddle: WaddleSummary) async {
        guard let session else { return }

        do {
            try await client.joinWaddle(sessionID: session.sessionID, waddleID: waddle.id)
            joinedWaddleIDs.insert(waddle.id)
            await refreshAccessibleWaddles()
            await selectWaddle(waddle.id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func createWaddle(name: String, description: String?, isPublic: Bool) async {
        guard let session else { return }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            errorMessage = "Waddle name is required."
            return
        }

        isCreatingWaddle = true
        defer { isCreatingWaddle = false }

        do {
            let created = try await client.createWaddle(
                sessionID: session.sessionID,
                name: trimmedName,
                description: description,
                isPublic: isPublic
            )
            joinedWaddleIDs.insert(created.id)
            publicCatalogWaddles.insert(created, at: 0)
            mergeVisibleWaddles()
            await selectWaddle(created.id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func isJoined(_ waddleID: String) -> Bool {
        joinedWaddleIDs.contains(waddleID)
    }

    private var currentRoomJID: String? {
        guard let selectedChannelID else {
            return nil
        }
        return roomJID(for: selectedChannelID)
    }

    private func beginPolling(for flow: DeviceStartResponse) {
        devicePollTask?.cancel()
        devicePollTask = Task {
            while !Task.isCancelled {
                do {
                    let result = try await client.pollDeviceAuth(deviceCode: flow.deviceCode)
                    switch result {
                    case .pending:
                        break
                    case .complete(let complete):
                        try await finalizeSignedInState(sessionID: complete.sessionID)
                        return
                    }
                } catch {
                    errorMessage = error.localizedDescription
                    cancelDeviceAuthorization()
                    return
                }

                try? await Task.sleep(nanoseconds: UInt64(flow.interval) * 1_000_000_000)
            }
        }
    }

    private func openVerificationPage(for flow: DeviceStartResponse, openURL: OpenURLAction) {
        if let url = verificationURL(for: flow) {
            openURL(url)
            return
        }

        errorMessage = "Unable to open verification URL."
    }

    private func verificationURL(for flow: DeviceStartResponse) -> URL? {
        var components = URLComponents(url: serverURL, resolvingAgainstBaseURL: false)
        components?.path = "/api/auth/device/verify"
        components?.queryItems = [URLQueryItem(name: "code", value: flow.userCode)]
        if let url = components?.url {
            return url
        }

        return normalizedVerificationURL(from: flow.verificationURIComplete)
    }

    private func normalizedVerificationURL(from raw: String) -> URL? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if let parsed = URL(string: trimmed),
           !trimmed.contains("%22"),
           !trimmed.contains("%5C%22") {
            return parsed
        }

        let decoded = trimmed.removingPercentEncoding ?? trimmed
        let unescaped = decoded
            .replacingOccurrences(of: "\\\"", with: "")
            .replacingOccurrences(of: "\"", with: "")
        return URL(string: unescaped)
    }

    private func finalizeSignedInState(sessionID: String) async throws {
        guard let loaded = try await client.session(sessionID: sessionID), !loaded.isExpired else {
            throw WaddleAPIError.server(statusCode: 401, message: "Session is not available.")
        }

        await applyAuthenticatedSession(loaded, persistSession: true)
    }

    private func applyAuthenticatedSession(_ loaded: WaddleSession, persistSession: Bool) async {
        if persistSession {
            AppConfig.saveSessionID(loaded.sessionID, for: serverURL)
        }

        session = loaded
        deviceAuth = nil
        errorMessage = ""
        updateChatSurfaceState()

        await connectXMPP(using: loaded)
        await refreshPublicWaddles()
    }

    private func connectXMPP(using session: WaddleSession) async {
        reconnectTask?.cancel()
        reconnectTask = nil
        xmppEventsTask?.cancel()
        failPendingRoomJoins(with: XMPPServiceError.disconnected)
        joinedRoomJIDs.removeAll()
        presenceByRoomJID.removeAll()
        if let xmppService {
            await xmppService.disconnect(emitEvent: false)
        }

        let service = XMPPService()
        xmppService = service
        updateConnectionBanner(for: .connecting)

        xmppEventsTask = Task { [weak self] in
            for await event in service.events {
                await self?.handleXMPPEvent(event)
            }
        }

        do {
            try await service.connect(session: session)
        } catch {
            errorMessage = error.localizedDescription
            chatStore.setBannerState(.error(message: error.localizedDescription))
            updateChatSurfaceState()
        }
    }

    private func handleXMPPEvent(_ event: XMPPEvent) async {
        switch event {
        case .streamFeatures:
            updateConnectionBanner(for: .negotiating)
        case .authenticated:
            updateConnectionBanner(for: .authenticating)
        case .resourceBound:
            updateConnectionBanner(for: .binding)
        case .sessionReady:
            reconnectTask?.cancel()
            reconnectTask = nil
            updateConnectionBanner(for: .ready)
            do {
                try await xmppService?.sendPresence()
            } catch {
                errorMessage = error.localizedDescription
            }
            await refreshAccessibleWaddles()
            if let selectedWaddleID {
                await loadStructure(for: selectedWaddleID)
            } else if let nextWaddleID = publicWaddles.first?.id {
                await selectWaddle(nextWaddleID)
            } else {
                updateChatSurfaceState()
            }
        case .message(let message):
            handleIncomingMessage(message)
        case .presence(let presence):
            handleIncomingPresence(presence)
        case .authenticationFailed(let detail):
            let message = detail ?? "The server rejected the XMPP bearer token."
            errorMessage = message
            chatStore.setBannerState(.error(message: message))
            updateChatSurfaceState()
        case .streamError(let name, let text):
            let message = text ?? name
            errorMessage = message
            chatStore.setBannerState(.error(message: message))
            updateChatSurfaceState()
        case .error(let message):
            errorMessage = message
            chatStore.setBannerState(.error(message: message))
            updateChatSurfaceState()
        case .disconnected:
            joinedRoomJIDs.removeAll()
            presenceByRoomJID.removeAll()
            failPendingRoomJoins(with: XMPPServiceError.disconnected)
            chatStore.setBannerState(.disconnected(message: "Disconnected from live chat."))
            scheduleReconnectIfNeeded()
            updateChatSurfaceState()
        }
    }

    private func refreshAccessibleWaddles() async {
        guard let xmppService else { return }

        do {
            let discovered = try await xmppService.discoverWaddles()
            accessibleWaddles = discovered.map {
                WaddleSummary(
                    id: $0.id,
                    name: $0.name,
                    description: nil,
                    ownerUserID: nil,
                    iconURL: nil,
                    isPublic: $0.isPublic,
                    role: nil,
                    createdAt: nil,
                    updatedAt: nil
                )
            }
            joinedWaddleIDs.formUnion(discovered.map(\.id))
            mergeVisibleWaddles()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func loadStructure(for waddleID: String) async {
        guard let session else { return }
        guard let xmppService else {
            updateChatSurfaceState()
            return
        }

        isLoadingStructure = true
        updateChatSurfaceState()

        do {
            async let discoveredChannels = xmppService.discoverChannels(waddleID: waddleID)
            async let loadedMembers = client.listMembers(sessionID: session.sessionID, waddleID: waddleID)

            let (xmppChannels, loadedMembersValue) = try await (discoveredChannels, loadedMembers)
            guard selectedWaddleID == waddleID else { return }

            channels = xmppChannels
                .map {
                    ChannelSummary(
                        id: $0.id,
                        name: $0.name,
                        description: nil,
                        channelType: $0.channelType,
                        position: $0.position
                    )
                }
                .sorted {
                    ($0.position ?? 0, $0.name.lowercased()) < ($1.position ?? 0, $1.name.lowercased())
                }
            members = loadedMembersValue.sorted { $0.username.lowercased() < $1.username.lowercased() }

            if members.contains(where: { $0.userID == session.userID }) {
                joinedWaddleIDs.insert(waddleID)
            }

            if let selectedChannelID,
               channels.contains(where: { $0.id == selectedChannelID }) {
                self.selectedChannelID = selectedChannelID
            } else {
                self.selectedChannelID = channels.first?.id
            }

            syncChatRooms()
            syncChatMembers()
            syncChatMessages()
            updateChatSurfaceState()

            if let selectedChannelID = self.selectedChannelID {
                await selectChannel(selectedChannelID)
            }
        } catch {
            guard selectedWaddleID == waddleID else { return }
            errorMessage = error.localizedDescription
            chatStore.setSurfaceState(.error(title: "Unable to load channels", message: error.localizedDescription))
        }

        isLoadingStructure = false
        updateChatSurfaceState()
    }

    private func joinSelectedChannel() async throws {
        guard let session,
              let selectedWaddleID,
              let selectedChannelID,
              let xmppService else {
            throw ChatSendError.noRoom
        }

        let roomJID = roomBareJID(accountJID: session.jid, waddleID: selectedWaddleID, channelID: selectedChannelID)
        if joinedRoomJIDs.contains(roomJID) {
            if roomJID == currentRoomJID {
                let roomTitle = chatStore.selectedRoom?.title ?? selectedChannel?.name ?? "chat"
                chatStore.setBannerState(.connected(message: "Connected to #\(roomTitle)"))
            }
            return
        }

        updateConnectionBanner(for: .ready)
        try await xmppService.joinRoom(roomJID, nick: session.username)
        try await waitForRoomJoin(roomJID: roomJID, nick: session.username)

        if roomJID == currentRoomJID {
            let roomTitle = chatStore.selectedRoom?.title ?? selectedChannel?.name ?? "chat"
            chatStore.setBannerState(.connected(message: "Connected to #\(roomTitle)"))
        }
    }

    private func sendMessage(_ text: String, room: ChatRoomSelection?, replyTo: ChatTimelineMessage? = nil) async throws {
        guard let session else {
            throw ChatSendError.noSession
        }
        guard let selectedWaddleID else {
            throw ChatSendError.noWaddle
        }

        let channelID = room?.id ?? selectedChannelID
        guard let channelID else {
            throw ChatSendError.noRoom
        }

        guard let xmppService else {
            throw ChatSendError.noSession
        }

        let roomJID = roomBareJID(accountJID: session.jid, waddleID: selectedWaddleID, channelID: channelID)

        if let replyTo {
            try await xmppService.sendGroupchatReplyMessage(
                roomJID: roomJID,
                body: text,
                replyToID: replyTo.id,
                replyToSender: replyTo.senderID,
                replyToBody: replyTo.body
            )
        } else {
            try await xmppService.sendGroupchatMessage(roomJID: roomJID, body: text)
        }
    }

    private func loadRoomHistory(for room: ChatRoomSelection, before: Date?) async throws -> ChatRoomHistoryPage {
        guard let session,
              let xmppService,
              let roomJID = roomJID(for: room.id) else {
            return ChatRoomHistoryPage(messages: [], hasMoreOlderMessages: false)
        }

        let requestBefore = before == nil ? "" : roomHistoryBeforeCursorByRoomJID[roomJID]
        if before != nil, requestBefore == nil {
            return ChatRoomHistoryPage(messages: [], hasMoreOlderMessages: false)
        }

        let archivePage = try await xmppService.fetchRoomHistory(
            roomJID: roomJID,
            max: roomHistoryPageSize,
            before: requestBefore
        )

        let deltaMessages = timelineMessages(
            from: archivePage.messages.map { TimelineEventDescriptor(event: $0.message, fallbackID: $0.mamID ?? $0.stanzaID) },
            roomJID: roomJID,
            session: session
        )

        let mergedMessages = (messagesByRoomJID[roomJID] ?? []).appendingTimelineMessages(deltaMessages)
        messagesByRoomJID[roomJID] = mergedMessages

        let nextBeforeCursor = archivePage.pageInfo.first ?? archivePage.messages.first?.mamID ?? archivePage.messages.first?.stanzaID
        let hasMoreOlderMessages = !archivePage.pageInfo.isComplete
            && nextBeforeCursor != nil
            && nextBeforeCursor != requestBefore

        if let nextBeforeCursor, hasMoreOlderMessages {
            roomHistoryBeforeCursorByRoomJID[roomJID] = nextBeforeCursor
        } else {
            roomHistoryBeforeCursorByRoomJID.removeValue(forKey: roomJID)
        }

        syncChatRooms()

        return ChatRoomHistoryPage(
            messages: deltaMessages,
            hasMoreOlderMessages: hasMoreOlderMessages
        )
    }

    private func handleIncomingMessage(_ event: XMPPMessageEvent) {
        guard let session else { return }
        let roomJID = barePeerJID(event.from ?? event.to ?? "")
        guard parseManagedRoomBareJID(roomJID) != nil else {
            return
        }

        let deltaMessages = timelineMessages(
            from: [TimelineEventDescriptor(event: event, fallbackID: nil)],
            roomJID: roomJID,
            session: session
        )
        guard !deltaMessages.isEmpty else {
            return
        }

        let messages = (messagesByRoomJID[roomJID] ?? []).appendingTimelineMessages(deltaMessages)
        messagesByRoomJID[roomJID] = messages

        syncChatRooms()
        if roomJID == currentRoomJID {
            syncChatMessages()
            updateChatSurfaceState()
        }
    }

    private func roomJID(for channelID: String) -> String? {
        guard let session, let selectedWaddleID else {
            return nil
        }
        return roomBareJID(accountJID: session.jid, waddleID: selectedWaddleID, channelID: channelID)
    }

    private func timelineMessages(
        from descriptors: [TimelineEventDescriptor],
        roomJID: String,
        session: WaddleSession
    ) -> [ChatTimelineMessage] {
        let existingTimeline = messagesByRoomJID[roomJID] ?? []
        var workingByID = Dictionary(uniqueKeysWithValues: existingTimeline.map { ($0.id, $0) })
        var deltaByID: [String: ChatTimelineMessage] = [:]
        var corrections: [TimelineCorrectionUpdate] = []
        var retractions: [String] = []
        var reactions: [TimelineReactionUpdate] = []

        for descriptor in descriptors {
            let event = descriptor.event
            let senderName = XMPPJID(string: event.from ?? "")?.resource ?? "Unknown"

            if let targetID = event.reactionTargetID, !event.reactionEmojis.isEmpty {
                reactions.append(
                    TimelineReactionUpdate(
                        targetID: targetID,
                        senderName: senderName,
                        emojis: event.reactionEmojis
                    )
                )
                continue
            }

            if let targetID = event.retractsID {
                retractions.append(targetID)
                continue
            }

            if let targetID = event.replacesID {
                corrections.append(
                    TimelineCorrectionUpdate(
                        targetID: targetID,
                        body: (event.body ?? event.subject ?? "").trimmingCharacters(in: .whitespacesAndNewlines),
                        timestamp: event.timestamp
                    )
                )
                continue
            }

            guard let message = timelineMessage(from: event, fallbackID: descriptor.fallbackID, roomJID: roomJID, session: session) else {
                continue
            }

            let merged = workingByID[message.id]?.merged(with: message) ?? message
            workingByID[message.id] = merged
            deltaByID[message.id] = merged
        }

        for correction in corrections {
            guard var target = workingByID[correction.targetID] else {
                continue
            }

            if !correction.body.isEmpty {
                target.body = correction.body
            }
            target.editedAt = latestDate(target.editedAt, correction.timestamp ?? target.sentAt)
            workingByID[target.id] = target
            deltaByID[target.id] = target
        }

        for targetID in retractions {
            guard var target = workingByID[targetID] else {
                continue
            }

            target.body = ""
            target.isRetracted = true
            workingByID[target.id] = target
            deltaByID[target.id] = target
        }

        for reaction in reactions {
            guard var target = workingByID[reaction.targetID] else {
                continue
            }

            var mergedReactions = target.reactions ?? [:]
            for emoji in reaction.emojis {
                var senders = mergedReactions[emoji] ?? []
                if !senders.contains(reaction.senderName) {
                    senders.append(reaction.senderName)
                }
                mergedReactions[emoji] = senders
            }

            target.reactions = mergedReactions.isEmpty ? nil : mergedReactions
            workingByID[target.id] = target
            deltaByID[target.id] = target
        }

        return deltaByID.values.sorted {
            if $0.sentAt == $1.sentAt {
                return $0.id < $1.id
            }
            return $0.sentAt < $1.sentAt
        }
    }

    private func timelineMessage(
        from event: XMPPMessageEvent,
        fallbackID: String?,
        roomJID: String,
        session: WaddleSession
    ) -> ChatTimelineMessage? {
        let text = (event.body ?? event.subject ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else {
            return nil
        }

        let senderName = XMPPJID(string: event.from ?? "")?.resource ?? "Unknown"
        let messageID = event.id ?? event.stanzaID ?? fallbackID ?? UUID().uuidString

        var replyToSenderName: String?
        var replyToBody: String?
        if let replyToID = event.replyToID {
            if let parentMessage = messagesByRoomJID[roomJID]?.first(where: { $0.id == replyToID }) {
                replyToSenderName = parentMessage.senderDisplayName
                replyToBody = parentMessage.body
            } else if let sender = event.replyToSender {
                replyToSenderName = XMPPJID(string: sender)?.resource ?? sender
            }
        }

        return ChatTimelineMessage(
            id: messageID,
            roomID: roomJID,
            senderID: senderName.lowercased(),
            senderDisplayName: senderName,
            body: text,
            sentAt: event.timestamp ?? Date(),
            editedAt: nil,
            deliveryState: .delivered,
            isOutgoing: senderName == session.username,
            isAction: event.type == "subject" || (event.body == nil && event.subject != nil),
            senderInitials: initials(from: senderName),
            reactions: nil,
            isRetracted: false,
            replyToID: event.replyToID,
            replyToSenderName: replyToSenderName,
            replyToBody: replyToBody,
            markupSpans: event.markupSpans.isEmpty ? nil : event.markupSpans
        )
    }

    private func latestDate(_ lhs: Date?, _ rhs: Date?) -> Date? {
        switch (lhs, rhs) {
        case let (lhs?, rhs?):
            return max(lhs, rhs)
        case let (lhs?, nil):
            return lhs
        case let (nil, rhs?):
            return rhs
        case (nil, nil):
            return nil
        }
    }

    private func handleIncomingPresence(_ event: XMPPPresenceEvent) {
        guard let from = event.from else { return }
        let roomJID = barePeerJID(from)
        guard parseManagedRoomBareJID(roomJID) != nil else {
            return
        }

        guard let nick = XMPPJID(string: from)?.resource, !nick.isEmpty else {
            return
        }

        var roomPresence = presenceByRoomJID[roomJID] ?? [:]
        if event.type == "unavailable" {
            roomPresence.removeValue(forKey: nick)
        } else {
            roomPresence[nick] = presenceState(from: event)
        }
        presenceByRoomJID[roomJID] = roomPresence

        if session?.username == nick {
            let joinKey = roomJoinKey(roomJID: roomJID, nick: nick)
            if event.type == "unavailable" {
                joinedRoomJIDs.remove(roomJID)
                failPendingRoomJoin(key: joinKey, error: XMPPServiceError.disconnected)
            } else {
                joinedRoomJIDs.insert(roomJID)
                finishPendingRoomJoin(key: joinKey)
                if roomJID == currentRoomJID {
                    let roomTitle = chatStore.selectedRoom?.title ?? selectedChannel?.name ?? "chat"
                    chatStore.setBannerState(.connected(message: "Connected to #\(roomTitle)"))
                }
            }
        }

        if roomJID == currentRoomJID {
            syncChatMembers()
        }
    }

    private func mergeVisibleWaddles() {
        var byID: [String: WaddleSummary] = [:]
        for waddle in accessibleWaddles {
            byID[waddle.id] = waddle
        }
        for waddle in publicCatalogWaddles {
            byID[waddle.id] = waddle
        }

        publicWaddles = byID.values.sorted { $0.name.lowercased() < $1.name.lowercased() }
        if selectedWaddleID == nil {
            selectedWaddleID = publicWaddles.first?.id
        }
    }

    private func syncChatRooms() {
        let rooms = channels.map { channel in
            let roomJID = session.flatMap {
                roomBareJID(accountJID: $0.jid, waddleID: selectedWaddleID ?? "", channelID: channel.id)
            }
            let lastMessage = roomJID.flatMap { messagesByRoomJID[$0]?.last }
            return ChatRoomSelection(
                id: channel.id,
                title: channel.name,
                subtitle: channel.channelType?.capitalized,
                unreadCount: 0,
                isMuted: false,
                lastActivityAt: lastMessage?.sentAt
            )
        }

        chatStore.replaceRooms(rooms, selectedRoomID: selectedChannelID)
    }

    private func syncChatMessages() {
        chatStore.replaceMessages(messagesByRoomJID[currentRoomJID ?? ""] ?? [])
    }

    private func syncChatMembers() {
        chatStore.replaceMembers(chatMembers)
    }

    private func updateChatSurfaceState() {
        syncChatRooms()
        syncChatMembers()
        syncChatMessages()

        if session == nil {
            chatStore.setSurfaceState(.idle)
            return
        }

        if isLoadingStructure {
            chatStore.setSurfaceState(.loading)
            return
        }

        guard selectedWaddleID != nil else {
            chatStore.setSurfaceState(.empty(title: "Select a waddle", message: "Choose a waddle to browse its live channels."))
            return
        }

        guard xmppService != nil else {
            chatStore.setSurfaceState(.empty(title: "Live chat unavailable", message: "Reconnect to start the XMPP session."))
            return
        }

        if let connectionState = xmppService?.connectionState {
            switch connectionState {
            case .ready:
                break
            case .connecting, .negotiating, .authenticating, .binding, .disconnecting:
                chatStore.setSurfaceState(.loading)
                return
            case .disconnected:
                chatStore.setSurfaceState(.empty(title: "Live chat offline", message: "Reconnect to restore rooms and history."))
                return
            case .failed(let message):
                chatStore.setSurfaceState(.error(title: "Live chat unavailable", message: message))
                return
            }
        }

        guard selectedChannelID != nil else {
            chatStore.setSurfaceState(.empty(title: "No channels yet", message: "Join the waddle or wait for room discovery to finish."))
            return
        }

        chatStore.setSurfaceState(.idle)
    }

    private func updateConnectionBanner(for state: XMPPConnectionState) {
        switch state {
        case .disconnected:
            chatStore.setBannerState(.disconnected(message: "Live chat is offline."))
        case .connecting:
            chatStore.setBannerState(.connecting(message: "Connecting to XMPP…"))
        case .negotiating:
            chatStore.setBannerState(.connecting(message: "Negotiating live session…"))
        case .authenticating:
            chatStore.setBannerState(.connecting(message: "Authenticating live session…"))
        case .binding:
            chatStore.setBannerState(.connecting(message: "Binding live resource…"))
        case .ready:
            chatStore.setBannerState(.connecting(message: "Preparing live chat…"))
        case .disconnecting:
            chatStore.setBannerState(.reconnecting(message: "Disconnecting live chat…"))
        case .failed(let message):
            chatStore.setBannerState(.error(message: message))
        }
    }

    private func presenceState(from event: XMPPPresenceEvent) -> ChatPresenceState {
        if event.type == "unavailable" {
            return .offline
        }

        switch event.show?.lowercased() {
        case "away", "xa":
            return .away
        case "dnd":
            return .dnd
        case nil, "", "chat":
            return .available
        case let value?:
            return .unknown(value)
        }
    }

    private func initials(from value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }

    private func clearSessionState() async {
        reconnectTask?.cancel()
        reconnectTask = nil
        xmppEventsTask?.cancel()
        xmppEventsTask = nil
        failPendingRoomJoins(with: XMPPServiceError.disconnected)
        if let xmppService {
            await xmppService.disconnect(emitEvent: false)
        }
        xmppService = nil

        session = nil
        publicCatalogWaddles = []
        accessibleWaddles = []
        publicWaddles = []
        selectedWaddleID = nil
        channels = []
        selectedChannelID = nil
        members = []
        joinedWaddleIDs.removeAll()
        joinedRoomJIDs.removeAll()
        messagesByRoomJID.removeAll()
        presenceByRoomJID.removeAll()
        roomHistoryBeforeCursorByRoomJID.removeAll()
        chatStore.clearComposer()
        chatStore.replaceRooms([])
        chatStore.replaceMembers([])
        chatStore.replaceMessages([])
        chatStore.setBannerState(.hidden)
        chatStore.setSurfaceState(.empty(title: "Select a waddle", message: "Sign in to browse and join live rooms."))
    }

    private func scheduleReconnectIfNeeded() {
        guard let session, reconnectTask == nil else {
            return
        }

        reconnectTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            guard !Task.isCancelled else { return }
            await self?.connectXMPP(using: session)
        }
    }

    private func roomJoinKey(roomJID: String, nick: String) -> String {
        "\(roomJID)|\(nick.lowercased())"
    }

    private func waitForRoomJoin(roomJID: String, nick: String) async throws {
        if joinedRoomJIDs.contains(roomJID) {
            return
        }

        let key = roomJoinKey(roomJID: roomJID, nick: nick)
        if roomJoinContinuations[key] != nil {
            return
        }

        try await withCheckedThrowingContinuation { continuation in
            roomJoinContinuations[key] = continuation
            roomJoinTimeoutTasks.removeValue(forKey: key)?.cancel()
            roomJoinTimeoutTasks[key] = Task { [weak self] in
                try? await Task.sleep(nanoseconds: 10_000_000_000)
                await self?.handleRoomJoinTimeout(key: key, roomJID: roomJID)
            }
        }
    }

    private func finishPendingRoomJoin(key: String) {
        roomJoinTimeoutTasks.removeValue(forKey: key)?.cancel()
        guard let continuation = roomJoinContinuations.removeValue(forKey: key) else {
            return
        }
        continuation.resume()
    }

    private func failPendingRoomJoin(key: String, error: Error) {
        roomJoinTimeoutTasks.removeValue(forKey: key)?.cancel()
        guard let continuation = roomJoinContinuations.removeValue(forKey: key) else {
            return
        }
        continuation.resume(throwing: error)
    }

    private func failPendingRoomJoins(with error: Error) {
        for task in roomJoinTimeoutTasks.values {
            task.cancel()
        }
        roomJoinTimeoutTasks.removeAll()

        let continuations = roomJoinContinuations.values
        roomJoinContinuations.removeAll()
        for continuation in continuations {
            continuation.resume(throwing: error)
        }
    }

    private func handleRoomJoinTimeout(key: String, roomJID: String) {
        failPendingRoomJoin(
            key: key,
            error: XMPPServiceError.timeout("Timed out joining \(roomJID).")
        )
    }
}
