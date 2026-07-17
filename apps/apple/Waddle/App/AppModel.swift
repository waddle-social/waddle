import Foundation
import SwiftUI
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

func dlog(_ msg: String) {
    Task { @MainActor in DebugLog.shared.log(msg) }
}

enum ChatSendError: LocalizedError {
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
    var inFlightAvatarFetches: Set<String> = []

    let chatStore: ChatSurfaceStore

    var serverURL: URL
    var client: WaddleAPIClient
    var devicePollTask: Task<Void, Never>?
    var reconnectTask: Task<Void, Never>?
    var uploadServiceJID: String?
    let xmppLifecycle = XMPPClientLifecycleController<RustXmppClient>()
    var isStructureLoadRunning = false
    var structureLoadRerunRequested = false
    var structureLoadWaiters: [CheckedContinuation<Void, Never>] = []
    var structureLoadGeneration = 0
    var structureLoadSurfaceError: (title: String, message: String)?
    var messagesByRoomJID: [String: [ChatTimelineMessage]] = [:]
    var presenceByRoomJID: [String: [String: ChatPresenceState]] = [:]
    var hatsByRoomJID: [String: [String: [XMPPPresenceHat]]] = [:]
    var joinedRoomJIDs: Set<String> = []
    var roomJoinContinuations: [String: CheckedContinuation<Void, Error>] = [:]
    var roomJoinTimeoutTasks: [String: Task<Void, Never>] = [:]
    var roomHistoryBeforeCursorByRoomJID: [String: String] = [:]
    let roomHistoryPageSize = 50

    var rustClient: RustXmppClient? {
        xmppLifecycle.currentClient
    }

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

    @Published var selectedForumThreadID: String?

    var dmMessagesByPeer: [String: [ChatTimelineMessage]] = [:]
    var dmPresence: [String: ChatPresenceState] = [:]

    @Published var pushNotificationsEnabled = false
    @Published var currentMood: XMPPUserMood?
    @Published var currentActivity: XMPPUserActivity?
    @Published var currentTune: XMPPUserTune?
    @Published var inboxEntries: [XMPPInboxEntry] = []

    @Published var isCreatingChannel = false
    @Published var isUploadingFile = false
    let maxUploadFileBytes = 10 * 1024 * 1024

    var currentRoomJID: String? {
        guard let selectedChannelID else {
            return nil
        }
        return roomJID(for: selectedChannelID)
    }

    var pendingEchoBodies: Set<String> = []

    var toastDismissTask: Task<Void, Never>?

    var typingTimers: [String: Task<Void, Never>] = [:]

    var composingTimer: Task<Void, Never>?
    var lastSentChatState: String?

    func roomJID(for channelID: String) -> String? {
        let jid = channels.first(where: { $0.id == channelID })?.roomJid
        return jid.flatMap { $0.isEmpty ? nil : $0 }
    }

    func syncChatRooms() {
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

    func syncChatMessages() {
        let key = currentRoomJID ?? ""
        let msgs = messagesByRoomJID[key] ?? []
        dlog(" syncChatMessages: key=\(key) count=\(msgs.count)")
        chatStore.replaceMessages(msgs)
    }

    func syncChatMembers() {
        chatStore.replaceMembers(chatMembers)
    }

    func updateChatSurfaceState() {
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

        if let structureLoadSurfaceError, channels.isEmpty {
            chatStore.setSurfaceState(.error(
                title: structureLoadSurfaceError.title,
                message: structureLoadSurfaceError.message
            ))
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

    func initials(from value: String) -> String {
        let parts = value.split(separator: " ").prefix(2)
        let letters = parts.compactMap { $0.first }.map(String.init)
        return letters.isEmpty ? "?" : letters.joined().uppercased()
    }

}
