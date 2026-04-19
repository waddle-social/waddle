import Foundation
import SwiftUI
import UserNotifications
import os

private let logger = Logger(subsystem: "social.waddle.ios", category: "AppModel")

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
    private var uploadServiceJID: String?
    private var xmppService: XMPPService?
    private var xmppEventsTask: Task<Void, Never>?
    private var messagesByRoomJID: [String: [ChatTimelineMessage]] = [:]
    private var presenceByRoomJID: [String: [String: ChatPresenceState]] = [:]
    private var hatsByRoomJID: [String: [String: [XMPPPresenceHat]]] = [:]
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
        logger.info("[WADDLE] selectChannel: \(channelID ?? "nil")")
        selectedChannelID = channelID
        selectedForumThreadID = nil
        syncChatRooms()
        syncChatMembers()
        syncChatMessages()
        updateChatSurfaceState()

        guard channelID != nil else {
            return
        }

        do {
            try await joinSelectedChannel()
            logger.info("[WADDLE] joined, loading history for roomJID=\(self.currentRoomJID ?? "nil")")
            await chatStore.refreshSelectedRoomHistory()
            syncChatMessages()
            logger.info("[WADDLE] history done: store=\(self.chatStore.messages.count) cached=\(self.messagesByRoomJID[self.currentRoomJID ?? ""]?.count ?? 0)")
            updateChatSurfaceState()
        } catch {
            logger.info("[WADDLE] selectChannel error: \(error)")
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

    @Published var selectedForumThreadID: String?

    private var dmMessagesByPeer: [String: [ChatTimelineMessage]] = [:]
    private var dmPresence: [String: ChatPresenceState] = [:]

    func openDm(peerJID: String, peerUsername: String) async {
        let bareJID = barePeerJID(peerJID)
        ensureDmConversation(peerJID: bareJID, peerUsername: peerUsername)
        chatStore.activeDmPeerJID = bareJID
        markDmRead(peerJID: bareJID)
        chatStore.dmMessages = dmMessagesByPeer[bareJID] ?? []

        guard let xmppService, let session else { return }
        do {
            let archive = try await xmppService.fetchDmHistory(peerJID: bareJID, max: 50)
            let messages = archive.messages.compactMap { archiveMsg -> ChatTimelineMessage? in
                let event = archiveMsg.message
                let text = (event.body ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                guard !text.isEmpty else { return nil }
                let senderBare = barePeerJID(event.from ?? "")
                let senderName = XMPPJID(string: event.from ?? "")?.localpart ?? senderBare
                let isOutgoing = senderBare == barePeerJID(session.jid)
                let messageID = event.id ?? event.stanzaID ?? archiveMsg.mamID ?? UUID().uuidString
                return ChatTimelineMessage(
                    id: messageID,
                    roomID: bareJID,
                    senderID: senderBare,
                    senderDisplayName: isOutgoing ? session.username : peerUsername,
                    body: text,
                    sentAt: archiveMsg.delayedDeliveryTimestamp ?? event.timestamp ?? Date(),
                    editedAt: nil,
                    deliveryState: .delivered,
                    isOutgoing: isOutgoing,
                    isAction: false,
                    senderInitials: initials(from: isOutgoing ? session.username : peerUsername),
                    reactions: nil,
                    isRetracted: false
                )
            }
            dmMessagesByPeer[bareJID] = messages.sorted { $0.sentAt < $1.sentAt }
            if chatStore.activeDmPeerJID == bareJID {
                chatStore.dmMessages = dmMessagesByPeer[bareJID] ?? []
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func sendDm(body: String) async {
        guard let peerJID = chatStore.activeDmPeerJID, let xmppService, let session else { return }
        let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }

        let optimistic = ChatTimelineMessage(
            id: UUID().uuidString,
            roomID: peerJID,
            senderID: barePeerJID(session.jid),
            senderDisplayName: session.username,
            body: text,
            sentAt: Date(),
            editedAt: nil,
            deliveryState: .sending,
            isOutgoing: true,
            isAction: false,
            senderInitials: initials(from: session.username),
            reactions: nil,
            isRetracted: false
        )
        var messages = dmMessagesByPeer[peerJID] ?? []
        messages.append(optimistic)
        dmMessagesByPeer[peerJID] = messages
        chatStore.dmMessages = messages

        do {
            try await xmppService.sendDirectMessage(peerJID: peerJID, body: text)
            updateDmConversation(peerJID: peerJID, body: text, date: Date())
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func closeDm() {
        chatStore.activeDmPeerJID = nil
        chatStore.dmMessages = []
        chatStore.dmComposerText = ""
    }

    private func handleIncomingDm(_ event: XMPPMessageEvent) {
        guard let session else { return }
        guard event.type == "chat" || event.type == "normal" || event.type == nil else { return }
        let text = (event.body ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }

        let fromBare = barePeerJID(event.from ?? "")
        let toBare = barePeerJID(event.to ?? "")
        let selfBare = barePeerJID(session.jid)
        let isSelf = fromBare == selfBare
        let peerJID = isSelf ? toBare : fromBare
        let peerUsername = XMPPJID(string: isSelf ? (event.to ?? "") : (event.from ?? ""))?.localpart ?? peerJID

        if peerJID.contains("@muc.") { return }

        let messageID = event.id ?? event.stanzaID ?? UUID().uuidString
        let message = ChatTimelineMessage(
            id: messageID,
            roomID: peerJID,
            senderID: fromBare,
            senderDisplayName: isSelf ? session.username : peerUsername,
            body: text,
            sentAt: event.timestamp ?? Date(),
            editedAt: nil,
            deliveryState: .delivered,
            isOutgoing: isSelf,
            isAction: false,
            senderInitials: initials(from: isSelf ? session.username : peerUsername),
            reactions: nil,
            isRetracted: false,
            markupSpans: event.markupSpans.isEmpty ? nil : event.markupSpans,
            sharedFiles: event.sharedFiles.isEmpty ? nil : event.sharedFiles
        )

        if isSelf {
            var msgs = dmMessagesByPeer[peerJID] ?? []
            msgs.removeAll { $0.isOutgoing && $0.deliveryState == .sending && $0.body == text }
            msgs.append(message)
            dmMessagesByPeer[peerJID] = msgs
        } else {
            var msgs = dmMessagesByPeer[peerJID] ?? []
            msgs.append(message)
            dmMessagesByPeer[peerJID] = msgs
        }

        ensureDmConversation(peerJID: peerJID, peerUsername: peerUsername)
        updateDmConversation(peerJID: peerJID, body: text, date: event.timestamp ?? Date())

        if !isSelf, chatStore.activeDmPeerJID != peerJID {
            incrementDmUnread(peerJID: peerJID)
        }

        if chatStore.activeDmPeerJID == peerJID {
            chatStore.dmMessages = dmMessagesByPeer[peerJID] ?? []
        }
    }

    private func ensureDmConversation(peerJID: String, peerUsername: String) {
        if !chatStore.dmConversations.contains(where: { $0.peerJID == peerJID }) {
            chatStore.dmConversations.append(DmConversation(
                id: peerJID,
                peerJID: peerJID,
                peerUsername: peerUsername,
                unreadCount: 0,
                presenceShow: dmPresence[peerJID] ?? .offline
            ))
        }
    }

    private func updateDmConversation(peerJID: String, body: String, date: Date) {
        if let idx = chatStore.dmConversations.firstIndex(where: { $0.peerJID == peerJID }) {
            chatStore.dmConversations[idx].lastMessageBody = body
            chatStore.dmConversations[idx].lastMessageAt = date
        }
    }

    private func incrementDmUnread(peerJID: String) {
        if let idx = chatStore.dmConversations.firstIndex(where: { $0.peerJID == peerJID }) {
            chatStore.dmConversations[idx].unreadCount += 1
        }
    }

    private func markDmRead(peerJID: String) {
        if let idx = chatStore.dmConversations.firstIndex(where: { $0.peerJID == peerJID }) {
            chatStore.dmConversations[idx].unreadCount = 0
        }
    }

    func sendForumTopic(title: String, body: String) async {
        guard let roomJID = currentRoomJID, let xmppService else { return }
        do {
            try await xmppService.sendForumTopic(roomJID: roomJID, body: body, title: title)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func sendForumReply(body: String, threadID: String) async {
        guard let roomJID = currentRoomJID, let xmppService else { return }
        do {
            try await xmppService.sendForumReply(roomJID: roomJID, body: body, threadID: threadID)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    var forumTopics: [ChatTimelineMessage] {
        chatStore.messages.filter { $0.isForumTopic }
    }

    func threadReplies(for threadID: String) -> [ChatTimelineMessage] {
        chatStore.messages.filter { $0.threadID == threadID && $0.isForumReply }
    }

    @Published var pushNotificationsEnabled = false
    @Published var currentMood: XMPPUserMood?
    @Published var currentActivity: XMPPUserActivity?
    @Published var currentTune: XMPPUserTune?
    @Published var inboxEntries: [XMPPInboxEntry] = []

    func fetchInbox() async {
        guard let xmppService else { return }
        do {
            inboxEntries = try await xmppService.fetchInbox()
        } catch {
            logger.info("[WADDLE] inbox fetch error: \(error.localizedDescription)")
        }
    }

    func setMood(_ mood: String, text: String? = nil) async {
        guard let xmppService else { return }
        do {
            try await xmppService.publishMood(mood, text: text)
            currentMood = XMPPUserMood(mood: mood, text: text)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func clearMood() async {
        guard let xmppService else { return }
        do {
            try await xmppService.clearMood()
            currentMood = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func setActivity(_ activity: String, text: String? = nil) async {
        guard let xmppService else { return }
        do {
            try await xmppService.publishActivity(activity, text: text)
            currentActivity = XMPPUserActivity(activity: activity, text: text)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func setTune(artist: String?, title: String?, source: String? = nil, uri: String? = nil) async {
        guard let xmppService else { return }
        do {
            try await xmppService.publishTune(artist: artist, title: title, source: source, uri: uri)
            currentTune = XMPPUserTune(artist: artist, title: title, source: source, length: nil, uri: uri)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func clearTune() async {
        guard let xmppService else { return }
        do {
            try await xmppService.clearTune()
            currentTune = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func requestPushNotificationPermission() {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .badge, .sound]) { [weak self] granted, _ in
            Task { @MainActor in
                self?.pushNotificationsEnabled = granted
                if granted {
#if os(iOS)
                    UIApplication.shared.registerForRemoteNotifications()
#elseif os(macOS)
                    NSApplication.shared.registerForRemoteNotifications()
#endif
                }
            }
        }
    }

    func registerPushToken(_ tokenData: Data) async {
        let token = tokenData.map { String(format: "%02x", $0) }.joined()
        guard let xmppService, let session else { return }
        let pushServiceJID = "push.\(jidDomain(session.jid))"
        let node = "waddle-apple-\(session.userID)"
        do {
            try await xmppService.enablePushNotifications(
                pushServiceJID: pushServiceJID,
                node: node,
                token: token
            )
            pushNotificationsEnabled = true
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func updateWaddle(name: String, description: String?) async {
        guard let session, let waddleID = selectedWaddleID else { return }
        do {
            try await client.updateWaddle(sessionID: session.sessionID, waddleID: waddleID, name: name, description: description)
            await refreshPublicWaddles()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func deleteWaddle() async {
        guard let session, let waddleID = selectedWaddleID else { return }
        do {
            try await client.deleteWaddle(sessionID: session.sessionID, waddleID: waddleID)
            selectedWaddleID = nil
            channels = []
            selectedChannelID = nil
            members = []
            await refreshPublicWaddles()
            updateChatSurfaceState()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func updateChannel(channelID: String, name: String, description: String?, position: Int) async {
        guard let session, let waddleID = selectedWaddleID else { return }
        do {
            try await client.updateChannel(sessionID: session.sessionID, waddleID: waddleID, channelID: channelID, name: name, description: description, position: position)
            await reloadSelectedWaddleStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func addMember(userID: String, role: String = "member") async {
        guard let session, let waddleID = selectedWaddleID else { return }
        do {
            try await client.addMember(sessionID: session.sessionID, waddleID: waddleID, userID: userID, role: role)
            await reloadSelectedWaddleStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func removeMember(userID: String) async {
        guard let session, let waddleID = selectedWaddleID else { return }
        do {
            try await client.removeMember(sessionID: session.sessionID, waddleID: waddleID, userID: userID)
            await reloadSelectedWaddleStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func changeMemberRole(userID: String, role: String) async {
        guard let session, let waddleID = selectedWaddleID else { return }
        do {
            try await client.updateMemberRole(sessionID: session.sessionID, waddleID: waddleID, userID: userID, role: role)
            await reloadSelectedWaddleStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    @Published var isCreatingChannel = false
    @Published var isUploadingFile = false

    func uploadAndSendFile(data: Data, fileName: String, mediaType: String) async {
        guard let roomJID = currentRoomJID, let xmppService else {
            errorMessage = "Select a channel before uploading."
            return
        }

        isUploadingFile = true
        defer { isUploadingFile = false }

        do {
            if uploadServiceJID == nil {
                uploadServiceJID = try await xmppService.discoverUploadService()
            }
            guard let serviceJID = uploadServiceJID else {
                errorMessage = "File upload is not available on this server."
                return
            }

            let slot = try await xmppService.requestUploadSlot(
                serviceJID: serviceJID,
                filename: fileName,
                size: data.count,
                contentType: mediaType
            )

            var request = URLRequest(url: URL(string: slot.putURL)!)
            request.httpMethod = "PUT"
            request.setValue(mediaType, forHTTPHeaderField: "Content-Type")
            for (name, value) in slot.putHeaders {
                request.setValue(value, forHTTPHeaderField: name)
            }

            let (_, response) = try await URLSession.shared.upload(for: request, from: data)
            guard let httpResponse = response as? HTTPURLResponse,
                  (200..<300).contains(httpResponse.statusCode) else {
                errorMessage = "File upload failed."
                return
            }

            try await xmppService.sendGroupchatFileMessage(
                roomJID: roomJID,
                fileURL: slot.getURL,
                fileName: fileName,
                mediaType: mediaType,
                size: data.count
            )
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func retractMessage(_ message: ChatTimelineMessage) async {
        guard let roomJID = currentRoomJID, let xmppService else { return }
        do {
            try await xmppService.retractMessage(roomJID: roomJID, messageID: message.id)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func createChannel(name: String, description: String?, channelType: String) async {
        guard let selectedWaddleID, let xmppService else { return }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            errorMessage = "Channel name is required."
            return
        }

        isCreatingChannel = true
        defer { isCreatingChannel = false }

        do {
            let position = channels.count
            let result = try await xmppService.createChannel(
                waddleID: selectedWaddleID,
                name: trimmedName,
                description: description,
                channelType: channelType,
                position: position
            )
            await loadStructure(for: selectedWaddleID)
            if let channelID = result.channelID {
                await selectChannel(channelID)
            }
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
            logger.info("[WADDLE] streamFeatures received")
            updateConnectionBanner(for: .negotiating)
        case .authenticated:
            logger.info("[WADDLE] authenticated")
            updateConnectionBanner(for: .authenticating)
        case .resourceBound(let jid):
            logger.info("[WADDLE] resourceBound: \(jid)")
            updateConnectionBanner(for: .binding)
        case .sessionReady:
            logger.info("[WADDLE] sessionReady")
            reconnectTask?.cancel()
            reconnectTask = nil
            updateConnectionBanner(for: .ready)
            do {
                try await xmppService?.sendPresence()
                logger.info("[WADDLE] presence sent")
            } catch {
                logger.info("[WADDLE] presence error: \(error)")
                errorMessage = error.localizedDescription
            }
            await refreshAccessibleWaddles()
            logger.info("[WADDLE] accessible waddles: \(self.accessibleWaddles.count), public: \(self.publicWaddles.count)")
            if let selectedWaddleID {
                logger.info("[WADDLE] loading structure for \(selectedWaddleID)")
                await loadStructure(for: selectedWaddleID)
            } else if let nextWaddleID = publicWaddles.first?.id {
                logger.info("[WADDLE] selecting first waddle \(nextWaddleID)")
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
            logger.info("[WADDLE] authenticationFailed: \(message)")
            errorMessage = message
            chatStore.setBannerState(.error(message: message))
            updateChatSurfaceState()
        case .streamError(let name, let text):
            let message = text ?? name
            logger.info("[WADDLE] streamError: \(name) \(text ?? "")")
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
        logger.info("[WADDLE] loadStructure for \(waddleID)")
        guard let session else { logger.info("[WADDLE] loadStructure: no session"); return }
        guard let xmppService else {
            logger.info("[WADDLE] loadStructure: no xmppService")
            updateChatSurfaceState()
            return
        }

        isLoadingStructure = true
        updateChatSurfaceState()

        do {
            async let discoveredChannels = xmppService.discoverChannels(waddleID: waddleID)
            async let loadedMembers = client.listMembers(sessionID: session.sessionID, waddleID: waddleID)

            let (xmppChannels, loadedMembersValue) = try await (discoveredChannels, loadedMembers)
            logger.info("[WADDLE] discovered \(xmppChannels.count) channels, \(loadedMembersValue.count) members")
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

    private var pendingEchoBodies: Set<String> = []

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

        let optimisticID = UUID().uuidString
        let optimistic = ChatTimelineMessage(
            id: optimisticID,
            roomID: roomJID,
            senderID: session.username.lowercased(),
            senderDisplayName: session.username,
            body: text,
            sentAt: Date(),
            editedAt: nil,
            deliveryState: .sending,
            isOutgoing: true,
            isAction: false,
            senderInitials: initials(from: session.username),
            reactions: nil,
            isRetracted: false,
            replyToID: replyTo?.id,
            replyToSenderName: replyTo?.senderDisplayName,
            replyToBody: replyTo?.body
        )

        let messages = (messagesByRoomJID[roomJID] ?? []).appendingTimelineMessages([optimistic])
        messagesByRoomJID[roomJID] = messages
        pendingEchoBodies.insert(text)

        if roomJID == currentRoomJID {
            syncChatMessages()
        }

        let (plainText, markupSpans) = XMPPXML.parseMarkdownToMarkupSpans(text)

        if let replyTo {
            try await xmppService.sendGroupchatReplyMessage(
                roomJID: roomJID,
                body: plainText,
                replyToID: replyTo.id,
                replyToSender: replyTo.senderID,
                replyToBody: replyTo.body
            )
        } else if !markupSpans.isEmpty {
            try await xmppService.sendGroupchatMessageWithMarkup(
                roomJID: roomJID,
                body: plainText,
                spans: markupSpans
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
        logger.info("[WADDLE] loadRoomHistory: roomJID=\(roomJID) archive=\(archivePage.messages.count) delta=\(deltaMessages.count) merged=\(mergedMessages.count)")

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

        if event.type == "chat" {
            handleIncomingDm(event)
            return
        }

        let roomJID = barePeerJID(event.from ?? event.to ?? "")
        guard parseManagedRoomBareJID(roomJID) != nil else {
            return
        }

        let senderNick = XMPPJID(string: event.from ?? "")?.resource ?? ""

        if let chatState = event.chatState, senderNick != session.username, roomJID == currentRoomJID {
            handleChatState(chatState, from: senderNick)
            if event.body == nil, event.subject == nil, event.replacesID == nil,
               event.retractsID == nil, event.reactionTargetID == nil, event.displayedMarkerID == nil {
                return
            }
        }

        if event.displayedMarkerID != nil {
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

        var existing = messagesByRoomJID[roomJID] ?? []

        if senderNick == session.username {
            for delta in deltaMessages {
                if pendingEchoBodies.contains(delta.body) {
                    pendingEchoBodies.remove(delta.body)
                    existing.removeAll { $0.isOutgoing && $0.deliveryState == .sending && $0.body == delta.body }
                }
            }
        }

        let messages = existing.appendingTimelineMessages(deltaMessages)
        messagesByRoomJID[roomJID] = messages

        if senderNick != session.username, roomJID == currentRoomJID {
            removeTypingUser(senderNick)
        }

        syncChatRooms()
        if roomJID == currentRoomJID {
            syncChatMessages()
            updateChatSurfaceState()
        }

        if roomJID == currentRoomJID, !deltaMessages.isEmpty,
           let lastMessage = deltaMessages.last, !lastMessage.isOutgoing {
            sendDisplayedMarkerForCurrentRoom(messageID: lastMessage.id)
        }

        for message in deltaMessages where !message.isOutgoing {
            if message.broadcastMention != nil {
                let incomingRoomJID = roomJID
                let channelName = channels.first(where: { self.roomJID(for: $0.id) == incomingRoomJID })?.name
                showNotificationToast(sender: message.senderDisplayName, body: message.body, channelName: channelName)
            }
        }
    }

    private var toastDismissTask: Task<Void, Never>?

    private func showNotificationToast(sender: String, body: String, channelName: String?) {
        let toast = ChatNotificationToast(
            senderName: sender,
            body: String(body.prefix(100)),
            channelName: channelName
        )
        chatStore.notificationToast = toast
        toastDismissTask?.cancel()
        toastDismissTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            guard !Task.isCancelled else { return }
            if self?.chatStore.notificationToast?.id == toast.id {
                self?.chatStore.notificationToast = nil
            }
        }
    }

    private var typingTimers: [String: Task<Void, Never>] = [:]

    private func handleChatState(_ state: String, from nick: String) {
        if state == "composing" {
            addTypingUser(nick)
        } else {
            removeTypingUser(nick)
        }
    }

    private func addTypingUser(_ nick: String) {
        var users = chatStore.typingUsers
        if !users.contains(nick) {
            users.append(nick)
            chatStore.typingUsers = users
        }
        typingTimers[nick]?.cancel()
        typingTimers[nick] = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            self?.removeTypingUser(nick)
        }
    }

    private func removeTypingUser(_ nick: String) {
        typingTimers[nick]?.cancel()
        typingTimers.removeValue(forKey: nick)
        var users = chatStore.typingUsers
        users.removeAll { $0 == nick }
        chatStore.typingUsers = users
    }

    private func sendDisplayedMarkerForCurrentRoom(messageID: String) {
        guard let roomJID = currentRoomJID, let xmppService else { return }
        Task {
            try? await xmppService.sendDisplayedMarker(roomJID: roomJID, messageID: messageID)
        }
    }

    private var composingTimer: Task<Void, Never>?
    private var lastSentChatState: String?

    func notifyComposing() {
        guard let roomJID = currentRoomJID, let xmppService else { return }
        if lastSentChatState != "composing" {
            lastSentChatState = "composing"
            Task { try? await xmppService.sendChatState(roomJID: roomJID, state: "composing") }
        }
        composingTimer?.cancel()
        composingTimer = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            guard let self, self.lastSentChatState == "composing" else { return }
            self.lastSentChatState = "paused"
            if let roomJID = self.currentRoomJID, let xmppService = self.xmppService {
                try? await xmppService.sendChatState(roomJID: roomJID, state: "paused")
            }
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
            markupSpans: event.markupSpans.isEmpty ? nil : event.markupSpans,
            sharedFiles: event.sharedFiles.isEmpty ? nil : event.sharedFiles,
            broadcastMention: event.broadcastMention,
            hatTitles: hatsByRoomJID[roomJID]?[senderName]?.map(\.title),
            mentionURIs: event.mentionURIs.isEmpty ? nil : event.mentionURIs,
            forumPostKind: event.forumPostKind,
            forumTitle: event.forumTitle,
            threadID: event.threadID,
            parentThreadID: event.parentThreadID,
            isSticker: event.isSticker ? true : nil
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
        let bareFrom = barePeerJID(from)

        if parseManagedRoomBareJID(bareFrom) == nil {
            let presence = presenceState(from: event)
            dmPresence[bareFrom] = presence
            if let idx = chatStore.dmConversations.firstIndex(where: { $0.peerJID == bareFrom }) {
                chatStore.dmConversations[idx].presenceShow = presence
            }
            return
        }

        let roomJID = bareFrom

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

        if !event.hats.isEmpty {
            var roomHats = hatsByRoomJID[roomJID] ?? [:]
            roomHats[nick] = event.hats
            hatsByRoomJID[roomJID] = roomHats
        }

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
        let key = currentRoomJID ?? ""
        let msgs = messagesByRoomJID[key] ?? []
        logger.info("[WADDLE] syncChatMessages: key=\(key) count=\(msgs.count)")
        chatStore.replaceMessages(msgs)
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
