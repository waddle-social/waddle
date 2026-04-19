import Foundation
import SwiftUI

typealias ChatMessageSendHandler = @Sendable (_ text: String, _ room: ChatRoomSelection?, _ replyTo: ChatTimelineMessage?) async throws -> Void
typealias ChatRoomHistoryLoadHandler = @Sendable (_ room: ChatRoomSelection, _ before: Date?) async throws -> ChatRoomHistoryPage

enum ChatSurfaceState: Equatable {
    case idle
    case loading
    case empty(title: String, message: String)
    case error(title: String, message: String)
}

@MainActor
final class ChatSurfaceStore: ObservableObject {
    @Published var rooms: [ChatRoomSelection]
    @Published var selectedRoomID: ChatRoomSelection.ID?
    @Published var messages: [ChatTimelineMessage]
    @Published var members: [ChatRoomMember]
    @Published var bannerState: ChatConnectionBannerState
    @Published var composerText: String
    @Published var surfaceState: ChatSurfaceState
    @Published var isSendingMessage: Bool
    @Published var roomHistoryState: ChatRoomHistoryState
    @Published var replyingToMessage: ChatTimelineMessage?
    @Published var typingUsers: [String] = []
    @Published var notificationToast: ChatNotificationToast?

    private var sendHandler: ChatMessageSendHandler?
    private var roomHistoryLoadHandler: ChatRoomHistoryLoadHandler?

    init(
        rooms: [ChatRoomSelection] = [],
        selectedRoomID: ChatRoomSelection.ID? = nil,
        messages: [ChatTimelineMessage] = [],
        members: [ChatRoomMember] = [],
        bannerState: ChatConnectionBannerState = .hidden,
        composerText: String = "",
        surfaceState: ChatSurfaceState = .idle,
        isSendingMessage: Bool = false,
        roomHistoryState: ChatRoomHistoryState = .init(),
        sendHandler: ChatMessageSendHandler? = nil
    ) {
        self.rooms = rooms
        self.selectedRoomID = selectedRoomID
        self.messages = messages
        self.members = members
        self.bannerState = bannerState
        self.composerText = composerText
        self.surfaceState = surfaceState
        self.isSendingMessage = isSendingMessage
        self.roomHistoryState = roomHistoryState
        self.sendHandler = sendHandler
    }

    func setSendHandler(_ handler: ChatMessageSendHandler?) {
        sendHandler = handler
    }

    func setRoomHistoryLoadHandler(_ handler: ChatRoomHistoryLoadHandler?) {
        roomHistoryLoadHandler = handler
    }

    var selectedRoom: ChatRoomSelection? {
        rooms.first(where: { $0.id == selectedRoomID })
    }

    func selectRoom(id: ChatRoomSelection.ID?) {
        let previousRoomID = selectedRoomID
        selectedRoomID = id
        if previousRoomID != id {
            roomHistoryState.reset(for: id)
        }
    }

    func replaceRooms(_ rooms: [ChatRoomSelection], selectedRoomID: ChatRoomSelection.ID? = nil) {
        let previousRoomID = self.selectedRoomID
        self.rooms = rooms
        self.selectedRoomID = selectedRoomID ?? self.selectedRoomID ?? rooms.first?.id
        if previousRoomID != self.selectedRoomID {
            roomHistoryState.reset(for: self.selectedRoomID)
        }
    }

    func replaceMessages(_ messages: [ChatTimelineMessage]) {
        self.messages = messages
        if roomHistoryState.roomID == selectedRoomID {
            roomHistoryState.loadedMessageCount = messages.count
            roomHistoryState.oldestLoadedAt = messages.first?.sentAt
            roomHistoryState.newestLoadedAt = messages.last?.sentAt
        }
    }

    func replaceMembers(_ members: [ChatRoomMember]) {
        self.members = members
    }

    func setBannerState(_ state: ChatConnectionBannerState) {
        bannerState = state
    }

    func setSurfaceState(_ state: ChatSurfaceState) {
        surfaceState = state
    }

    func setRoomHistoryState(_ state: ChatRoomHistoryState) {
        roomHistoryState = state
    }

    func resetRoomHistoryState() {
        roomHistoryState.reset(for: selectedRoomID)
    }

    func beginInitialRoomHistoryLoad() {
        roomHistoryState.beginInitialLoad()
    }

    func beginOlderRoomHistoryLoad() {
        roomHistoryState.beginOlderLoad()
    }

    func finishRoomHistoryLoad(
        loadedMessageCount: Int,
        oldestLoadedAt: Date?,
        newestLoadedAt: Date?,
        hasMoreOlderMessages: Bool
    ) {
        roomHistoryState.finishInitialLoad(
            loadedMessageCount: loadedMessageCount,
            oldestLoadedAt: oldestLoadedAt,
            newestLoadedAt: newestLoadedAt,
            hasMoreOlderMessages: hasMoreOlderMessages
        )
    }

    func failRoomHistoryLoad(_ message: String) {
        roomHistoryState.fail(message)
    }

    func refreshSelectedRoomHistory() async {
        guard let room = selectedRoom, let roomHistoryLoadHandler else { return }
        beginInitialRoomHistoryLoad()
        do {
            let page = try await roomHistoryLoadHandler(room, nil)
            applyHistoryPage(page, for: room, isOlderLoad: false)
        } catch {
            failRoomHistoryLoad(error.localizedDescription)
        }
    }

    func loadOlderMessages() async {
        guard let room = selectedRoom,
              let roomHistoryLoadHandler,
              roomHistoryState.canLoadOlderMessages else {
            return
        }

        beginOlderRoomHistoryLoad()
        do {
            let page = try await roomHistoryLoadHandler(room, roomHistoryState.oldestLoadedAt)
            applyHistoryPage(page, for: room, isOlderLoad: true)
        } catch {
            failRoomHistoryLoad(error.localizedDescription)
        }
    }

    private func applyHistoryPage(
        _ page: ChatRoomHistoryPage,
        for room: ChatRoomSelection,
        isOlderLoad: Bool
    ) {
        guard selectedRoomID == room.id, roomHistoryState.roomID == room.id else {
            return
        }

        messages = messages.appendingTimelineMessages(page.messages)

        if isOlderLoad {
            roomHistoryState.finishOlderLoad(
                loadedMessageCount: messages.count,
                oldestLoadedAt: messages.first?.sentAt,
                newestLoadedAt: messages.last?.sentAt,
                hasMoreOlderMessages: page.hasMoreOlderMessages
            )
        } else {
            roomHistoryState.finishInitialLoad(
                loadedMessageCount: messages.count,
                oldestLoadedAt: messages.first?.sentAt,
                newestLoadedAt: messages.last?.sentAt,
                hasMoreOlderMessages: page.hasMoreOlderMessages
            )
        }
    }

    func clearComposer() {
        composerText = ""
    }

    func setReplyingTo(_ message: ChatTimelineMessage?) {
        replyingToMessage = message
    }

    func sendComposerMessage() async {
        let text = composerText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, let sendHandler else {
            return
        }

        let replyTo = replyingToMessage
        isSendingMessage = true
        defer { isSendingMessage = false }

        do {
            try await sendHandler(text, selectedRoom, replyTo)
            composerText = ""
            replyingToMessage = nil
        } catch {
            surfaceState = .error(title: "Message not sent", message: error.localizedDescription)
        }
    }
}
