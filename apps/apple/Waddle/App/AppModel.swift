import Foundation
import SwiftUI
import UserNotifications
import os

private let logger = Logger(subsystem: "social.waddle.ios", category: "AppModel")

@MainActor
final class DebugLog: ObservableObject {
    static let shared = DebugLog()
    @Published var lines: [String] = []

    func log(_ message: String) {
        let timestamp = DateFormatter.localizedString(from: Date(), dateStyle: .none, timeStyle: .medium)
        let line = "[\(timestamp)] \(message)"
        lines.append(line)
        if lines.count > 200 { lines.removeFirst(lines.count - 200) }
        logger.info("\(line)")
    }
}

private func dlog(_ msg: String) {
    Task { @MainActor in DebugLog.shared.log(msg) }
}

private enum ChatSendError: LocalizedError {
    case noSession
    case noRoom

    var errorDescription: String? {
        switch self {
        case .noSession:
            return "Sign in again to reconnect live chat."
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
    /// Display name fallback used before XEP-0503 topology has loaded.
    @Published var spaceName: String?
    @Published var spaces: [SpaceSummary] = []
    @Published var selectedSpaceID: String?
    @Published var channels: [ChannelSummary] = []
    @Published var selectedChannelID: String?
    @Published var members: [MemberSummary] = []
    @Published var deviceAuth: DeviceStartResponse?
    @Published var errorMessage = ""
    @Published var isLoadingProviders = false
    @Published var isLoadingStructure = false
    @Published var isCreatingSpace = false

    /// XEP-0084 avatar cache keyed by bare JID (lowercase). A present-but-
    /// empty `Data` value means "we asked and they have no avatar" — that
    /// sentinel lets us avoid hammering the server with repeat requests.
    @Published var avatarDataByJID: [String: Data] = [:]
    /// Senders whose avatar fetch is currently in flight, to deduplicate
    /// simultaneous row requests.
    private var inFlightAvatarFetches: Set<String> = []

    let chatStore: ChatSurfaceStore

    private var serverURL: URL
    private var client: WaddleAPIClient
    private var devicePollTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private var uploadServiceJID: String?
    private var rustClient: RustXmppClient?
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
        chatStore.setSendHandler { [weak self] text, room, replyTo, threadRootID in
            guard let self else { return }
            try await self.sendMessage(text, room: room, replyTo: replyTo, threadRootID: threadRootID)
        }
        chatStore.setRoomHistoryLoadHandler { [weak self] room, before in
            guard let self else {
                return ChatRoomHistoryPage(messages: [], hasMoreOlderMessages: false)
            }
            return await self.loadRoomHistory(for: room, before: before)
        }
        updateChatSurfaceState()
        Task { await bootstrap() }
    }

    var selectedSpace: SpaceSummary? {
        if let selectedSpaceID,
           let space = spaces.first(where: { $0.id == selectedSpaceID }) {
            return space
        }
        if let selectedChannel,
           let spaceID = selectedChannel.spaceID,
           let space = spaces.first(where: { $0.id == spaceID }) {
            return space
        }
        if let first = spaces.first {
            return first
        }
        guard let spaceName else { return nil }
        return SpaceSummary(
            id: "",
            name: spaceName,
            description: nil
        )
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

    func reloadRooms() async {
        await loadRooms()
    }

    func selectChannel(_ channelID: String?) async {
        dlog(" selectChannel: \(channelID ?? "nil")")
        selectedChannelID = channelID
        selectedSpaceID = channels.first(where: { $0.id == channelID })?.spaceID ?? selectedSpaceID
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
            dlog(" joined, loading history for roomJID=\(self.currentRoomJID ?? "nil")")
            await chatStore.refreshSelectedRoomHistory()
            syncChatMessages()
            dlog(" history done: store=\(self.chatStore.messages.count) cached=\(self.messagesByRoomJID[self.currentRoomJID ?? ""]?.count ?? 0)")
            updateChatSurfaceState()
        } catch {
            dlog(" selectChannel error: \(error)")
            errorMessage = error.localizedDescription
            chatStore.setBannerState(.error(message: error.localizedDescription))
            chatStore.failRoomHistoryLoad(error.localizedDescription)
            updateChatSurfaceState()
        }
    }

    func reloadSelectedSpaceStructure() async {
        await loadRooms()
    }

    func createSpace(name: String, description: String?) async {
        guard let session else { return }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            errorMessage = "Space name is required."
            return
        }

        isCreatingSpace = true
        defer { isCreatingSpace = false }

        do {
            try await client.createSpace(
                sessionID: session.sessionID,
                name: trimmedName,
                description: description
            )
            await loadRooms()
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

        guard let rustClient, let session else { return }
        let archive = await rustClient.fetchDmHistory(peerJID: bareJID, max: 50)
        let messages = archive.messages.compactMap { archiveMsg -> ChatTimelineMessage? in
            let event = archiveMsg.message
            let text = (event.body ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty || !event.sharedFiles.isEmpty else { return nil }
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
                isRetracted: false,
                sharedFiles: event.sharedFiles.isEmpty ? nil : event.sharedFiles
            )
        }
        dmMessagesByPeer[bareJID] = messages.sorted { $0.sentAt < $1.sentAt }
        if chatStore.activeDmPeerJID == bareJID {
            chatStore.dmMessages = dmMessagesByPeer[bareJID] ?? []
        }
    }

    func sendDm(body: String, sharedFiles: [WaddleSharedFile] = [], peerJID: String? = nil) async {
        guard let targetPeerJID = peerJID ?? chatStore.activeDmPeerJID,
              let rustClient,
              let session else {
            return
        }

        let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
        let wireBody = !text.isEmpty ? text : (sharedFiles.first?.url ?? "")
        guard !wireBody.isEmpty || !sharedFiles.isEmpty else { return }

        let optimistic = ChatTimelineMessage(
            id: UUID().uuidString,
            roomID: targetPeerJID,
            senderID: barePeerJID(session.jid),
            senderDisplayName: session.username,
            body: wireBody,
            sentAt: Date(),
            editedAt: nil,
            deliveryState: .sending,
            isOutgoing: true,
            isAction: false,
            senderInitials: initials(from: session.username),
            reactions: nil,
            isRetracted: false,
            sharedFiles: sharedFiles.isEmpty ? nil : sharedFiles.map(timelineSharedFile(from:))
        )
        var messages = dmMessagesByPeer[targetPeerJID] ?? []
        messages.append(optimistic)
        dmMessagesByPeer[targetPeerJID] = messages
        if chatStore.activeDmPeerJID == targetPeerJID {
            chatStore.dmMessages = messages
        }

        let options = sharedFiles.isEmpty
            ? nil
            : WaddleSendOptions(
                stanzaId: nil,
                reply: nil,
                fallback: nil,
                thread: nil,
                sharedFiles: sharedFiles
            )
        await rustClient.sendDirectMessage(peerJID: targetPeerJID, body: wireBody, options: options)
        updateDmConversation(
            peerJID: targetPeerJID,
            body: dmConversationPreview(body: text, sharedFiles: sharedFiles.map(timelineSharedFile(from:))),
            date: Date()
        )
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
        guard !text.isEmpty || !event.sharedFiles.isEmpty else { return }

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
            msgs.removeAll { $0.isOutgoing && $0.deliveryState == .sending && $0.body == message.body }
            msgs.append(message)
            dmMessagesByPeer[peerJID] = msgs
        } else {
            var msgs = dmMessagesByPeer[peerJID] ?? []
            msgs.append(message)
            dmMessagesByPeer[peerJID] = msgs
        }

        ensureDmConversation(peerJID: peerJID, peerUsername: peerUsername)
        updateDmConversation(
            peerJID: peerJID,
            body: dmConversationPreview(body: text, sharedFiles: event.sharedFiles),
            date: event.timestamp ?? Date()
        )

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
        guard let roomJID = currentRoomJID, let rustClient else { return }
        await rustClient.sendForumTopic(roomJID: roomJID, body: body, title: title)
    }

    func sendForumReply(body: String, threadID: String) async {
        guard let roomJID = currentRoomJID, let rustClient else { return }
        await rustClient.sendForumReply(roomJID: roomJID, body: body, threadID: threadID)
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
        guard let rustClient else { return }
        inboxEntries = await rustClient.fetchInbox()
    }

    func setMood(_ mood: String, text: String? = nil) async {
        guard let rustClient else { return }
        await rustClient.publishMood(mood, text: text)
        currentMood = XMPPUserMood(mood: mood, text: text)
    }

    func clearMood() async {
        guard let rustClient else { return }
        await rustClient.clearMood()
        currentMood = nil
    }

    func setActivity(_ activity: String, text: String? = nil) async {
        guard let rustClient else { return }
        await rustClient.publishActivity(activity, text: text)
        currentActivity = XMPPUserActivity(activity: activity, text: text)
    }

    func setTune(artist: String?, title: String?, source: String? = nil, uri: String? = nil) async {
        guard let rustClient else { return }
        await rustClient.publishTune(artist: artist, title: title, source: source, uri: uri)
        currentTune = XMPPUserTune(artist: artist, title: title, source: source, length: nil, uri: uri)
    }

    func clearTune() async {
        guard let rustClient else { return }
        await rustClient.clearTune()
        currentTune = nil
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

    /// Kick off a XEP-0084 avatar fetch for the given sender. Idempotent: if
    /// the JID is already cached, currently being fetched, or resolves to the
    /// local session, the call is a no-op. A missing/empty avatar is stored
    /// as `Data()` so we don't re-request on every scroll.
    func requestAvatarIfNeeded(forSenderID senderID: String) {
        guard !senderID.isEmpty, let session, let rustClient else { return }
        let key = avatarJID(forSenderID: senderID, session: session).lowercased()
        guard !key.isEmpty else { return }
        if avatarDataByJID[key] != nil { return }
        if inFlightAvatarFetches.contains(key) { return }
        inFlightAvatarFetches.insert(key)
        Task { [weak self] in
            let avatar = await rustClient.requestAvatar(jid: key)
            let avatarData = await Self.avatarData(from: avatar)
            guard let self else { return }
            await MainActor.run {
                self.inFlightAvatarFetches.remove(key)
                if let avatarData {
                    // Empty Data is a sentinel for users without a published
                    // avatar. Failed URL fetches are not cached so they can
                    // recover on a later request.
                    self.avatarDataByJID[key] = avatarData
                }
            }
        }
    }

    private nonisolated static func avatarData(from avatar: WaddleAvatar?) async -> Data? {
        guard let avatar else { return Data() }
        if !avatar.data.isEmpty { return avatar.data }
        guard
            let value = avatar.url,
            let url = URL(string: value),
            url.scheme == "https"
        else {
            return Data()
        }
        do {
            let (data, response) = try await URLSession.shared.data(from: url)
            guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
                return nil
            }
            if let contentType = http.value(forHTTPHeaderField: "Content-Type")?.lowercased(),
               !contentType.hasPrefix("image/") {
                return nil
            }
            return data
        } catch {
            return nil
        }
    }

    /// Resolve the bare JID we should query for an avatar from a timeline
    /// message's `senderID`. MUC occupant ids arrive as bare nicknames; turn
    /// those into `nick@domain` using the session's domain. DM/1:1 senders
    /// already arrive as bare JIDs (`localpart@domain`).
    private func avatarJID(forSenderID senderID: String, session: WaddleSession) -> String {
        if senderID.contains("@") {
            return senderID
        }
        let domain = jidDomain(session.jid)
        return "\(senderID)@\(domain)"
    }

    /// Raw avatar image data for a given message `senderID`, or nil when the
    /// fetch hasn't completed or the user has no avatar. Intended for use by
    /// SwiftUI row renderers alongside an initials fallback.
    func avatarData(forSenderID senderID: String) -> Data? {
        guard !senderID.isEmpty, let session else { return nil }
        let key = avatarJID(forSenderID: senderID, session: session).lowercased()
        guard let data = avatarDataByJID[key], !data.isEmpty else { return nil }
        return data
    }

    func registerPushToken(_ tokenData: Data) async {
        let token = tokenData.map { String(format: "%02x", $0) }.joined()
        guard let rustClient, let session else { return }
        let pushServiceJID = "push.\(jidDomain(session.jid))"
        let node = "waddle-apple-\(session.userID)"
        await rustClient.enablePushNotifications(pushServiceJID: pushServiceJID, node: node, token: token)
        pushNotificationsEnabled = true
    }

    func updateSpace(name: String, description: String?) async {
        guard let session else { return }
        do {
            try await client.updateSpace(sessionID: session.sessionID, name: name, description: description)
            spaceName = name
            if let selectedSpaceID,
               let index = spaces.firstIndex(where: { $0.id == selectedSpaceID }) {
                spaces[index] = SpaceSummary(id: selectedSpaceID, name: name, description: description)
            } else if spaces.count == 1 {
                spaces[0] = SpaceSummary(id: spaces[0].id, name: name, description: description)
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func deleteSpace() async {
        guard let session else { return }
        do {
            try await client.deleteSpace(sessionID: session.sessionID)
            spaceName = nil
            spaces = []
            selectedSpaceID = nil
            channels = []
            selectedChannelID = nil
            members = []
            updateChatSurfaceState()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func updateChannel(channelID: String, name: String, description: String?, position: Int) async {
        guard let session else { return }
        let apiChannelID = channels.first(where: { $0.id == channelID })?.apiID ?? channelID
        do {
            try await client.updateChannel(sessionID: session.sessionID, channelID: apiChannelID, name: name, description: description, position: position)
            await reloadSelectedSpaceStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func addMember(userID: String, role: String = "member") async {
        guard let session else { return }
        do {
            try await client.addMember(sessionID: session.sessionID, userID: userID, role: role)
            await reloadSelectedSpaceStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func removeMember(userID: String) async {
        guard let session else { return }
        do {
            try await client.removeMember(sessionID: session.sessionID, userID: userID)
            await reloadSelectedSpaceStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func changeMemberRole(userID: String, role: String) async {
        guard let session else { return }
        do {
            try await client.updateMemberRole(sessionID: session.sessionID, userID: userID, role: role)
            await reloadSelectedSpaceStructure()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    @Published var isCreatingChannel = false
    @Published var isUploadingFile = false
    private let maxUploadFileBytes = 10 * 1024 * 1024

    func uploadAndSendFile(
        data: Data,
        fileName: String,
        mediaType: String,
        replyTo: ChatTimelineMessage? = nil,
        threadRootID: String? = nil
    ) async {
        guard currentRoomJID != nil else {
            errorMessage = "Select a channel before uploading."
            return
        }

        guard let sharedFile = await uploadSharedFile(data: data, fileName: fileName, mediaType: mediaType) else {
            return
        }

        do {
            try await sendMessage(
                "",
                room: chatStore.selectedRoom,
                replyTo: replyTo,
                threadRootID: threadRootID,
                sharedFiles: [sharedFile]
            )
            if replyTo != nil {
                chatStore.setReplyingTo(nil)
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func uploadAndSendDmFile(data: Data, fileName: String, mediaType: String, peerJID: String) async {
        guard let sharedFile = await uploadSharedFile(data: data, fileName: fileName, mediaType: mediaType) else {
            return
        }
        await sendDm(body: "", sharedFiles: [sharedFile], peerJID: peerJID)
    }

    func retractMessage(_ message: ChatTimelineMessage) async {
        guard let roomJID = currentRoomJID, let rustClient else { return }
        await rustClient.retractMessage(roomJID: roomJID, messageID: message.id)
    }

    func createChannel(name: String, description: String?, channelType: String) async {
        guard let rustClient else { return }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            errorMessage = "Channel name is required."
            return
        }

        isCreatingChannel = true
        defer { isCreatingChannel = false }

        let position = channels.count
        let result = await rustClient.createChannel(
            name: trimmedName,
            description: description,
            channelType: channelType,
            position: position
        )
        await loadRooms()
        if let channelID = result?.channelID {
            await selectChannel(channelID)
        }
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
                    errorMessage = ""
                    switch result {
                    case .pending:
                        break
                    case .complete(let complete):
                        try await finalizeSignedInState(sessionID: complete.sessionID)
                        return
                    }
                } catch {
                    if isTransientDeviceAuthPollError(error) {
                        errorMessage = "Connection interrupted. Retrying sign-in…"
                        try? await Task.sleep(nanoseconds: UInt64(flow.interval) * 1_000_000_000)
                        continue
                    }
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
        if let raw = flow.verificationURIComplete,
           let url = normalizedVerificationURL(from: raw) {
            return url
        }

        if let raw = flow.verificationURI,
           let base = normalizedVerificationURL(from: raw),
           var components = URLComponents(url: base, resolvingAgainstBaseURL: false) {
            components.queryItems = [URLQueryItem(name: "code", value: flow.userCode)]
            if let url = components.url {
                return url
            }
        }

        var components = URLComponents(url: serverURL, resolvingAgainstBaseURL: false)
        components?.path = "/api/auth/device/verify"
        components?.queryItems = [URLQueryItem(name: "code", value: flow.userCode)]
        return components?.url
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

    private func isTransientDeviceAuthPollError(_ error: Error) -> Bool {
        let nsError = error as NSError
        guard nsError.domain == NSURLErrorDomain else {
            return false
        }
        let code = URLError.Code(rawValue: nsError.code)

        switch code {
        case .networkConnectionLost, .timedOut:
            return true
        default:
            return false
        }
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
    }

    private func connectXMPP(using session: WaddleSession) async {
        reconnectTask?.cancel()
        reconnectTask = nil
        xmppEventsTask?.cancel()
        failPendingRoomJoins(with: XMPPServiceError.disconnected)
        joinedRoomJIDs.removeAll()
        presenceByRoomJID.removeAll()
        if let rustClient {
            await rustClient.disconnect()
        }

        let rustConfig = WaddleConfig(
            serverUrl: session.xmppWebsocketURL,
            jid: session.jid,
            accessToken: session.sessionID,
            resource: session.xmppCredentials.resource
        )
        rustClient = RustXmppClient(config: rustConfig)

        updateConnectionBanner(for: .connecting)

        xmppEventsTask = Task { [weak self] in
            guard let self, let client = self.rustClient else { return }
            for await event in client.events {
                await self.handleXMPPEvent(event)
            }
        }

        await rustClient!.connect()
    }

    private func handleXMPPEvent(_ event: XMPPEvent) async {
        switch event {
        case .streamFeatures:
            dlog(" streamFeatures received")
            updateConnectionBanner(for: .negotiating)
        case .authenticated:
            dlog(" authenticated")
            updateConnectionBanner(for: .authenticating)
        case .resourceBound(let jid):
            dlog(" resourceBound: \(jid)")
            updateConnectionBanner(for: .binding)
        case .sessionReady:
            dlog(" sessionReady")
            reconnectTask?.cancel()
            reconnectTask = nil
            updateConnectionBanner(for: .ready)
            await rustClient?.sendPresence()
            dlog(" presence sent")
            // Fire room loading in a separate Task so this event-loop iteration
            // completes and can process incoming events (e.g. the MUC self-presence that
            // resolves the join) while the load is in flight. Without this, the
            // event loop deadlocks: joinSelectedChannel waits for a self-presence event that
            // can never be delivered because the event loop is blocked on joinSelectedChannel.
            Task { @MainActor [weak self] in
                guard let self else { return }
                dlog(" loading rooms")
                await self.loadRooms()
            }
        case .message(let message):
            handleIncomingMessage(message)
        case .presence(let presence):
            handleIncomingPresence(presence)
        case .messageDeliveryAcked(let stanzaID):
            dlog(" messageDeliveryAcked: \(stanzaID)")
        case .messageDeliveryFailed(let stanzaID):
            dlog(" messageDeliveryFailed: \(stanzaID)")
        case .authenticationFailed(let detail):
            let message = detail ?? "The server rejected the XMPP bearer token."
            dlog(" authenticationFailed: \(message)")
            errorMessage = message
            chatStore.setBannerState(.error(message: message))
            updateChatSurfaceState()
        case .streamError(let name, let text):
            let message = text ?? name
            dlog(" streamError: \(name) \(text ?? "")")
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

    /// Loads the XEP-0503 spaces topology and member list.
    private func loadRooms() async {
        guard let session, let rustClient else {
            updateChatSurfaceState()
            return
        }

        isLoadingStructure = true
        updateChatSurfaceState()

        do {
            async let xmppTopology = rustClient.discoverTopology()
            async let loadedMembers = client.listMembers(sessionID: session.sessionID)

            let (topology, loadedMembersValue) = try await (xmppTopology, loadedMembers)
            dlog(" loadRooms: \(topology.spaces.count) spaces, \(topology.channels.count) rooms, \(loadedMembersValue.count) members")

            spaces = topology.spaces.map { space in
                SpaceSummary(
                    id: space.id,
                    name: space.name,
                    description: space.description
                )
            }

            channels = topology.channels
                .map { channel in
                    return ChannelSummary(
                        id: channel.id,
                        apiID: parseManagedRoomBareJID(channel.roomJID),
                        roomJid: channel.roomJID,
                        name: channel.name,
                        description: channel.description,
                        channelType: channel.channelType,
                        position: channel.position,
                        spaceID: channel.spaceID
                    )
                }
                .sorted {
                    ($0.position ?? 0, $0.name.lowercased()) < ($1.position ?? 0, $1.name.lowercased())
                }
            members = loadedMembersValue.sorted { $0.username.lowercased() < $1.username.lowercased() }

            if let selectedChannelID,
               let selectedChannel = channels.first(where: { $0.id == selectedChannelID }) {
                self.selectedChannelID = selectedChannelID
                selectedSpaceID = selectedChannel.spaceID
            } else {
                self.selectedChannelID = channels.first?.id
                selectedSpaceID = channels.first?.spaceID ?? spaces.first?.id
            }
            spaceName = selectedSpace?.name ?? serverURL.host ?? "Waddle"

            syncChatRooms()
            syncChatMembers()
            syncChatMessages()
            updateChatSurfaceState()

            if let channelID = self.selectedChannelID {
                await selectChannel(channelID)
            }
        } catch {
            errorMessage = error.localizedDescription
            chatStore.setSurfaceState(.error(title: "Unable to load channels", message: error.localizedDescription))
        }

        isLoadingStructure = false
        updateChatSurfaceState()
    }

    private func joinSelectedChannel() async throws {
        guard let session,
              let selectedChannelID,
              let rustClient else {
            throw ChatSendError.noRoom
        }

        guard let roomJID = channels.first(where: { $0.id == selectedChannelID })?.roomJid,
              !roomJID.isEmpty else {
            throw ChatSendError.noRoom
        }

        dlog("joinSelectedChannel: roomJID=\(roomJID) nick=\(session.username) alreadyJoined=\(joinedRoomJIDs.contains(roomJID))")
        if joinedRoomJIDs.contains(roomJID) {
            if roomJID == currentRoomJID {
                let roomTitle = chatStore.selectedRoom?.title ?? selectedChannel?.name ?? "chat"
                chatStore.setBannerState(.connected(message: "Connected to #\(roomTitle)"))
            }
            return
        }

        updateConnectionBanner(for: .ready)
        await rustClient.joinRoom(roomJID, nick: session.username)
        try await waitForRoomJoin(roomJID: roomJID, nick: session.username)

        if roomJID == currentRoomJID {
            let roomTitle = chatStore.selectedRoom?.title ?? selectedChannel?.name ?? "chat"
            chatStore.setBannerState(.connected(message: "Connected to #\(roomTitle)"))
        }
    }

    private var pendingEchoBodies: Set<String> = []

    private func sendMessage(
        _ text: String,
        room: ChatRoomSelection?,
        replyTo: ChatTimelineMessage? = nil,
        threadRootID: String? = nil,
        sharedFiles: [WaddleSharedFile] = []
    ) async throws {
        guard let session else {
            throw ChatSendError.noSession
        }

        let channelID = room?.id ?? selectedChannelID
        guard let channelID else {
            throw ChatSendError.noRoom
        }

        guard let rustClient else {
            throw ChatSendError.noSession
        }

        guard let roomJID = channels.first(where: { $0.id == channelID })?.roomJid, !roomJID.isEmpty else {
            throw ChatSendError.noRoom
        }
        let trimmedText = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let bodyForWire = !trimmedText.isEmpty ? trimmedText : (sharedFiles.first?.url ?? "")
        guard !bodyForWire.isEmpty || !sharedFiles.isEmpty else {
            return
        }

        // Build the wire body. For replies we prepend a XEP-0428 fallback
        // quote (so non-supporting clients see the context) and compute the
        // Unicode-scalar range that supporting clients will strip.
        let (wireBody, fallbackRange) = composeWireBody(userText: bodyForWire, replyTo: replyTo)

        let optimisticID = UUID().uuidString
        let optimistic = ChatTimelineMessage(
            id: optimisticID,
            roomID: roomJID,
            senderID: session.username.lowercased(),
            senderDisplayName: session.username,
            body: wireBody,
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
            replyToBody: replyTo?.displayBody,
            replyFallbackRange: fallbackRange,
            sharedFiles: sharedFiles.isEmpty ? nil : sharedFiles.map(timelineSharedFile(from:)),
            threadID: threadRootID
        )

        let messages = (messagesByRoomJID[roomJID] ?? []).appendingTimelineMessages([optimistic])
        messagesByRoomJID[roomJID] = messages
        pendingEchoBodies.insert(wireBody)

        if roomJID == currentRoomJID {
            syncChatMessages()
        }

        let (_, markupSpans) = parseMarkdownToMarkupSpans(trimmedText)

        // Compose structured send-options once so reply, thread, and fallback
        // metadata all travel together down the FFI in a single typed payload.
        let replyTarget: WaddleReplyTarget? = replyTo.map { target in
            // For MUC the reply `to` must be the occupant full JID
            // (room@muc.domain/nick) per XEP-0461; senderDisplayName is the
            // nick that will render as the resource.
            WaddleReplyTarget(
                authorJid: "\(roomJID)/\(target.senderDisplayName)",
                messageId: target.id
            )
        }
        let fallbackOpt: WaddleFallbackRange? = fallbackRange.map { range in
            WaddleFallbackRange(
                start: UInt32(range.lowerBound),
                end: UInt32(range.upperBound)
            )
        }
        let threadTarget: WaddleThreadTarget? = threadRootID.map { rootID in
            WaddleThreadTarget(id: rootID, parent: nil)
        }

        let hasOptions = replyTarget != nil || fallbackOpt != nil || threadTarget != nil || !sharedFiles.isEmpty
        let options: WaddleSendOptions? = hasOptions
            ? WaddleSendOptions(
                stanzaId: nil,
                reply: replyTarget,
                fallback: fallbackOpt,
                thread: threadTarget,
                sharedFiles: sharedFiles
            )
            : nil

        if options == nil, !markupSpans.isEmpty {
            await rustClient.sendGroupchatMessageWithMarkup(
                roomJID: roomJID,
                body: wireBody,
                spans: markupSpans
            )
        } else {
            await rustClient.sendGroupchatMessage(
                roomJID: roomJID,
                body: wireBody,
                options: options
            )
        }
    }

    /// Compose the outbound body for a send. For replies this prepends a
    /// XEP-0428 fallback quote (so non-supporting clients still see what is
    /// being quoted) and returns the Unicode-scalar range covering that
    /// prefix. Supporting clients use the range to hide the quote and render
    /// the structured reply-to indicator instead.
    private func composeWireBody(
        userText: String,
        replyTo: ChatTimelineMessage?
    ) -> (body: String, fallbackRange: Range<Int>?) {
        guard let replyTo else {
            return (userText, nil)
        }
        // Truncate quoted lines to keep fallback quotes readable when the
        // original body is long; supporting clients hide it anyway.
        let maxQuoteChars = 240
        let sourceBody = replyTo.displayBody
        let quoteBody: String = {
            if sourceBody.unicodeScalars.count <= maxQuoteChars {
                return sourceBody
            }
            let scalars = sourceBody.unicodeScalars
            let cutoff = scalars.index(scalars.startIndex, offsetBy: maxQuoteChars)
            return String(scalars[..<cutoff]) + "…"
        }()

        var quote = ""
        quote += "> "
        quote += replyTo.senderDisplayName
        quote += " wrote:\n"
        for line in quoteBody.split(separator: "\n", omittingEmptySubsequences: false) {
            quote += "> "
            quote += String(line)
            quote += "\n"
        }
        quote += "\n"

        let body = quote + userText
        let fallbackEnd = quote.unicodeScalars.count
        return (body, 0..<fallbackEnd)
    }

    private func loadRoomHistory(for room: ChatRoomSelection, before: Date?) async -> ChatRoomHistoryPage {
        dlog(" loadRoomHistory called for room.id=\(room.id) room.title=\(room.title) before=\(String(describing: before))")
        guard let session,
              let rustClient,
              let roomJID = roomJID(for: room.id) else {
            dlog(" loadRoomHistory: guard failed — session=\(self.session != nil) rustClient=\(self.rustClient != nil) roomJID=\(self.roomJID(for: room.id) ?? "nil")")
            return ChatRoomHistoryPage(messages: [], hasMoreOlderMessages: false)
        }

        dlog(" loadRoomHistory: roomJID=\(roomJID)")
        let requestBefore = before == nil ? "" : roomHistoryBeforeCursorByRoomJID[roomJID]
        if before != nil, requestBefore == nil {
            dlog(" loadRoomHistory: no cursor for older load, returning empty")
            return ChatRoomHistoryPage(messages: [], hasMoreOlderMessages: false)
        }

        let archivePage = await rustClient.fetchRoomHistory(
            roomJID: roomJID,
            max: UInt32(roomHistoryPageSize),
            before: requestBefore
        )

        let deltaMessages = timelineMessages(
            from: archivePage.messages.map { TimelineEventDescriptor(event: $0.message, fallbackID: $0.mamID ?? $0.stanzaID) },
            roomJID: roomJID,
            session: session
        )

        var mergedMessages = (messagesByRoomJID[roomJID] ?? []).appendingTimelineMessages(deltaMessages)

        // Back-fill reply previews for messages whose parent wasn't loaded yet
        for i in mergedMessages.indices {
            if mergedMessages[i].replyToID != nil && mergedMessages[i].replyToBody == nil {
                if let parent = mergedMessages.first(where: { $0.id == mergedMessages[i].replyToID }) {
                    mergedMessages[i].replyToSenderName = parent.senderDisplayName
                    mergedMessages[i].replyToBody = String(parent.body.prefix(100))
                }
            }
        }

        messagesByRoomJID[roomJID] = mergedMessages
        if roomJID == currentRoomJID {
            syncChatMessages()
        }
        dlog(" loadRoomHistory: roomJID=\(roomJID) archive=\(archivePage.messages.count) delta=\(deltaMessages.count) merged=\(mergedMessages.count)")

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
        guard let roomJID = currentRoomJID, let rustClient else { return }
        Task {
            await rustClient.sendDisplayedMarker(roomJID: roomJID, messageID: messageID)
        }
    }

    private var composingTimer: Task<Void, Never>?
    private var lastSentChatState: String?

    func notifyComposing() {
        guard let roomJID = currentRoomJID, let rustClient else { return }
        if lastSentChatState != "composing" {
            lastSentChatState = "composing"
            Task { await rustClient.sendChatState(roomJID: roomJID, state: "composing") }
        }
        composingTimer?.cancel()
        composingTimer = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            guard let self, self.lastSentChatState == "composing" else { return }
            self.lastSentChatState = "paused"
            if let roomJID = self.currentRoomJID, let rustClient = self.rustClient {
                await rustClient.sendChatState(roomJID: roomJID, state: "paused")
            }
        }
    }

    private func roomJID(for channelID: String) -> String? {
        let jid = channels.first(where: { $0.id == channelID })?.roomJid
        return jid.flatMap { $0.isEmpty ? nil : $0 }
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
        guard !text.isEmpty || !event.sharedFiles.isEmpty else {
            return nil
        }

        let senderName = XMPPJID(string: event.from ?? "")?.resource ?? "Unknown"
        let messageID = event.id ?? event.stanzaID ?? fallbackID ?? UUID().uuidString

        var replyToSenderName: String?
        var replyToBody: String?
        if let replyToID = event.replyToID {
            if let parentMessage = messagesByRoomJID[roomJID]?.first(where: { $0.id == replyToID }) {
                replyToSenderName = parentMessage.senderDisplayName
                // Preview should reflect what the user saw on their screen,
                // not the wire body (which may include a XEP-0428 fallback
                // quote).
                replyToBody = parentMessage.displayBody
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
            replyFallbackRange: event.replyFallbackRange,
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

    private func uploadSharedFile(data: Data, fileName: String, mediaType: String) async -> WaddleSharedFile? {
        guard data.count <= maxUploadFileBytes else {
            let sizeMb = Double(data.count) / 1024.0 / 1024.0
            errorMessage = "File too large (\(String(format: "%.1f", sizeMb)) MB). Maximum upload size is 10 MB."
            return nil
        }

        guard let rustClient else {
            errorMessage = ChatSendError.noSession.errorDescription ?? "Sign in again to reconnect live chat."
            return nil
        }

        isUploadingFile = true
        defer { isUploadingFile = false }

        if uploadServiceJID == nil {
            uploadServiceJID = await rustClient.discoverUploadService()
        }
        guard let serviceJID = uploadServiceJID else {
            errorMessage = "File upload is not available on this server."
            return nil
        }

        guard let slot = await rustClient.requestUploadSlot(
            serviceJID: serviceJID,
            filename: fileName,
            size: data.count,
            contentType: mediaType
        ) else {
            errorMessage = "Failed to request an upload slot."
            return nil
        }

        guard let putURL = URL(string: slot.putURL) else {
            errorMessage = "Upload slot returned an invalid URL."
            return nil
        }

        do {
            var request = URLRequest(url: putURL)
            request.httpMethod = "PUT"
            request.setValue(mediaType, forHTTPHeaderField: "Content-Type")
            for (name, value) in slot.putHeaders {
                request.setValue(value, forHTTPHeaderField: name)
            }

            let (_, response) = try await URLSession.shared.upload(for: request, from: data)
            guard let httpResponse = response as? HTTPURLResponse,
                  (200..<300).contains(httpResponse.statusCode) else {
                errorMessage = "File upload failed."
                return nil
            }
        } catch {
            errorMessage = error.localizedDescription
            return nil
        }

        return WaddleSharedFile(
            url: slot.getURL,
            name: fileName,
            mediaType: mediaType,
            size: UInt64(data.count),
            width: nil,
            height: nil,
            disposition: sharedFileDisposition(for: mediaType),
            encrypted: nil
        )
    }

    private func sharedFileDisposition(for mediaType: String) -> String {
        if mediaType.hasPrefix("image/")
            || mediaType.hasPrefix("video/")
            || mediaType.hasPrefix("audio/")
            || mediaType == "application/pdf" {
            return "inline"
        }
        return "attachment"
    }

    private func timelineSharedFile(from file: WaddleSharedFile) -> XMPPSharedFile {
        XMPPSharedFile(
            url: file.url,
            name: file.name,
            mediaType: file.mediaType,
            size: file.size.flatMap(Int.init),
            width: file.width.flatMap(Int.init),
            height: file.height.flatMap(Int.init),
            disposition: file.disposition,
            encryptedSource: nil
        )
    }

    private func dmConversationPreview(body: String, sharedFiles: [XMPPSharedFile]) -> String {
        let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty, !sharedFiles.contains(where: { $0.url == trimmed }) {
            return trimmed
        }
        if let first = sharedFiles.first {
            return first.name ?? "Sent an attachment"
        }
        return trimmed
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

        let eventHats = mergedPresenceHats(from: event)
        if !eventHats.isEmpty {
            var roomHats = hatsByRoomJID[roomJID] ?? [:]
            roomHats[nick] = eventHats
            hatsByRoomJID[roomJID] = roomHats
            refreshHatTitles(in: roomJID, for: nick)
        }

        dlog("presence: room=\(roomJID) nick=\(nick) type=\(event.type ?? "nil") sessionUser=\(session?.username ?? "nil") match=\(session?.username == nick)")
        if session?.username == nick {
            let joinKey = roomJoinKey(roomJID: roomJID, nick: nick)
            dlog("presence: self-presence! joinKey=\(joinKey) pendingKeys=\(Array(roomJoinContinuations.keys))")
            if event.type == "unavailable" {
                joinedRoomJIDs.remove(roomJID)
                failPendingRoomJoin(key: joinKey, error: XMPPServiceError.disconnected)
            } else {
                joinedRoomJIDs.insert(roomJID)
                let wasPending = finishPendingRoomJoin(key: joinKey)
                if roomJID == currentRoomJID {
                    let roomTitle = chatStore.selectedRoom?.title ?? selectedChannel?.name ?? "chat"
                    chatStore.setBannerState(.connected(message: "Connected to #\(roomTitle)"))
                    if !wasPending {
                        // Self-presence arrived after the join timeout fired; load history now.
                        Task {
                            await chatStore.refreshSelectedRoomHistory()
                            syncChatMessages()
                            updateChatSurfaceState()
                        }
                    }
                }
            }
        }

        if roomJID == currentRoomJID {
            syncChatMembers()
            syncChatMessages()
        }
    }

    private func mergedPresenceHats(from event: XMPPPresenceEvent) -> [XMPPPresenceHat] {
        var hats: [XMPPPresenceHat] = []
        if event.mucAffiliation == .owner {
            hats.append(XMPPPresenceHat(uri: "urn:xmpp:hats:owner", title: "Owner"))
        } else if event.mucAffiliation == .admin {
            hats.append(XMPPPresenceHat(uri: "urn:xmpp:hats:admin", title: "Admin"))
        }
        if event.mucRole == .moderator {
            hats.append(XMPPPresenceHat(uri: "urn:xmpp:hats:moderator", title: "Moderator"))
        }
        for hat in event.hats where !hats.contains(where: { $0.uri == hat.uri }) {
            hats.append(hat)
        }
        return hats
    }

    private func refreshHatTitles(in roomJID: String, for nick: String) {
        guard let titles = hatsByRoomJID[roomJID]?[nick]?.map(\.title) else { return }
        guard var messages = messagesByRoomJID[roomJID] else { return }
        var changed = false
        for index in messages.indices where messages[index].senderDisplayName == nick {
            messages[index].hatTitles = titles
            changed = true
        }
        if changed {
            messagesByRoomJID[roomJID] = messages
        }
    }

    private func syncChatRooms() {
        let rooms = channels.map { channel in
            let jid = channel.roomJid.isEmpty ? nil : channel.roomJid
            let lastMessage = jid.flatMap { messagesByRoomJID[$0]?.last }
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
        dlog(" syncChatMessages: key=\(key) count=\(msgs.count)")
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

        guard !channels.isEmpty || isLoadingStructure else {
            chatStore.setSurfaceState(.empty(title: "Connecting to space", message: "Loading channels…"))
            return
        }

        guard rustClient != nil else {
            chatStore.setSurfaceState(.empty(title: "Live chat unavailable", message: "Reconnect to start the XMPP session."))
            return
        }

        if let connectionState = rustClient?.connectionState {
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
            chatStore.setSurfaceState(.empty(title: "No channels yet", message: "Channels will appear here once the server space has rooms."))
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
        if let rustClient {
            await rustClient.disconnect()
        }
        rustClient = nil

        session = nil
        spaceName = nil
        spaces = []
        selectedSpaceID = nil
        channels = []
        selectedChannelID = nil
        members = []
        joinedRoomJIDs.removeAll()
        messagesByRoomJID.removeAll()
        presenceByRoomJID.removeAll()
        roomHistoryBeforeCursorByRoomJID.removeAll()
        chatStore.clearComposer()
        chatStore.replaceRooms([])
        chatStore.replaceMembers([])
        chatStore.replaceMessages([])
        chatStore.setBannerState(.hidden)
        chatStore.setSurfaceState(.empty(title: "Sign in", message: "Sign in to load the server space and connect to live rooms."))
    }

    func handleAppBecameActive() {
        guard let session else { return }
        guard let rustClient else {
            Task { await connectXMPP(using: session) }
            return
        }
        switch rustClient.connectionState {
        case .ready:
            break
        case .connecting, .negotiating, .authenticating, .binding:
            break
        case .disconnected, .failed, .disconnecting:
            Task { await connectXMPP(using: session) }
        }
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
                do {
                    try await Task.sleep(nanoseconds: 30_000_000_000)
                } catch {
                    // Task was cancelled (join completed or connection dropped); do not time out.
                    return
                }
                await self?.handleRoomJoinTimeout(key: key, roomJID: roomJID)
            }
        }
    }

    @discardableResult
    private func finishPendingRoomJoin(key: String) -> Bool {
        roomJoinTimeoutTasks.removeValue(forKey: key)?.cancel()
        guard let continuation = roomJoinContinuations.removeValue(forKey: key) else {
            return false
        }
        continuation.resume()
        return true
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
