import Foundation
import SwiftUI

typealias ChatMessageSendHandler = @Sendable (
    _ text: String,
    _ room: ChatRoomSelection?,
    _ replyTo: ChatTimelineMessage?,
    _ threadRootID: String?
) async throws -> Void
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
    @Published var mentionQuery: String?
    @Published var dmConversations: [DmConversation] = []
    @Published var activeDmPeerJID: String?
    @Published var dmMessages: [ChatTimelineMessage] = []
    @Published var dmComposerText: String = ""

    /// Per-room watermark used to render the "New" unread divider. Messages
    /// strictly newer than the watermark (and not outgoing) appear below the
    /// divider. The watermark is seeded on a room's first load and advances
    /// whenever the user switches away from that room, so messages received
    /// while the room was open count as "new" until the user leaves.
    @Published var unreadWatermarkByRoomID: [String: Date] = [:]

    /// Navigation stack of thread-root message ids. Empty when the panel is
    /// closed. The last element is the thread currently rendered in the panel;
    /// earlier elements are the ancestor threads the user drilled down from
    /// and can pop back to.
    @Published var activeThreadStack: [String] = []

    /// Convenience accessor for the currently visible thread's root id.
    var activeThreadParentID: String? {
        activeThreadStack.last
    }

    /// Composer text for the thread panel. Kept separate from `composerText`
    /// so draft state persists across opening/closing the panel and does not
    /// leak into the channel composer.
    @Published var threadComposerText: String = ""

    /// True while a thread-panel send is in flight. Used to disable the send
    /// button and show a spinner in the panel composer.
    @Published var isSendingThreadMessage: Bool = false

    /// Message-id lookup rebuilt on every `replaceMessages`. Used by the
    /// thread panel to resolve the root and by the timeline to answer
    /// "how many children does message X have?".
    private(set) var messagesByID: [String: ChatTimelineMessage] = [:]

    /// Maps a thread-root message id to an ascending-by-sent-at list of its
    /// child message ids (XEP-0201 `<thread>` children). Excludes the root
    /// itself. Rebuilt on every `replaceMessages`.
    private(set) var childrenByThreadID: [String: [String]] = [:]

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
        // Advance the outgoing room's watermark to the newest message we
        // showed, so returning visits don't re-mark those as "new".
        if let prev = previousRoomID, prev != id, let last = messages.last {
            unreadWatermarkByRoomID[prev] = last.sentAt
        }
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
        rebuildThreadIndexes()
        if roomHistoryState.roomID == selectedRoomID {
            roomHistoryState.loadedMessageCount = messages.count
            roomHistoryState.oldestLoadedAt = messages.first?.sentAt
            roomHistoryState.newestLoadedAt = messages.last?.sentAt
        }
        // Seed this room's unread watermark on first load so pre-existing
        // history doesn't all light up as unread. Subsequent replaces keep
        // the same watermark — that's how "new since last visit" works.
        if let roomID = selectedRoomID, unreadWatermarkByRoomID[roomID] == nil {
            unreadWatermarkByRoomID[roomID] = messages.last?.sentAt ?? .distantPast
        }
        // Prune any ancestors from the thread-nav stack whose root scrolled
        // out of view. If the entire stack goes stale the panel closes.
        if !activeThreadStack.isEmpty {
            activeThreadStack.removeAll { messagesByID[$0] == nil }
        }
    }

    /// Id of the first message in the current room that is strictly newer
    /// than the watermark and not outgoing. Used by `ChatTimelineView` to
    /// place the "New" divider.
    var firstUnreadMessageID: String? {
        guard let roomID = selectedRoomID,
              let watermark = unreadWatermarkByRoomID[roomID] else { return nil }
        return mainTimelineMessages.first(where: { $0.sentAt > watermark && !$0.isOutgoing })?.id
    }

    /// Messages to show in the main channel timeline.
    ///
    /// Thread replies (messages whose `threadID` points at a *different*
    /// message) belong inside the thread panel — they MUST NOT appear in the
    /// main timeline alongside their siblings. Thread roots (either no
    /// `threadID`, or `threadID == id`) stay in the timeline so the user can
    /// open the thread from the channel view.
    var mainTimelineMessages: [ChatTimelineMessage] {
        messages.filter { message in
            guard let tid = message.threadID, !tid.isEmpty else { return true }
            return tid == message.id
        }
    }

    private func rebuildThreadIndexes() {
        var byID: [String: ChatTimelineMessage] = [:]
        for m in messages {
            byID[m.id] = m
        }
        var childrenByParent: [String: [String]] = [:]
        for m in messages {
            guard let tid = m.threadID, !tid.isEmpty, tid != m.id else { continue }
            childrenByParent[tid, default: []].append(m.id)
        }
        // Sort each child list ascending by sent-at so the panel renders
        // oldest → newest (matching main-timeline order).
        for (tid, ids) in childrenByParent {
            childrenByParent[tid] = ids.sorted { lhs, rhs in
                guard let a = byID[lhs], let b = byID[rhs] else { return false }
                return a.sentAt < b.sentAt
            }
        }
        messagesByID = byID
        childrenByThreadID = childrenByParent
    }

    /// Number of thread children indexed under the given root message id.
    func threadChildCount(forRootID id: String) -> Int {
        childrenByThreadID[id]?.count ?? 0
    }

    func openThreadPanel(forRootID id: String) {
        guard messagesByID[id] != nil else { return }
        activeThreadStack = [id]
        threadComposerText = ""
    }

    /// Push a nested thread onto the navigation stack. Used when the user
    /// taps "View thread" on a reply that itself has children. No-op if the
    /// target is already at the top of the stack.
    func pushThreadPanel(forRootID id: String) {
        guard messagesByID[id] != nil else { return }
        if activeThreadStack.last == id { return }
        activeThreadStack.append(id)
        threadComposerText = ""
    }

    /// Pop one level off the thread-nav stack, returning to the parent
    /// thread. Closes the panel entirely when the last level is popped.
    func popThreadPanel() {
        guard !activeThreadStack.isEmpty else { return }
        activeThreadStack.removeLast()
        threadComposerText = ""
    }

    func closeThreadPanel() {
        activeThreadStack.removeAll()
        threadComposerText = ""
    }

    /// True when the thread panel has at least one ancestor to pop back to.
    var canPopThreadPanel: Bool {
        activeThreadStack.count > 1
    }

    /// Resolves the root message currently backing the thread panel, if any.
    var threadPanelRoot: ChatTimelineMessage? {
        guard let id = activeThreadParentID else { return nil }
        return messagesByID[id]
    }

    /// Ordered (oldest → newest) thread-child messages for the currently open
    /// thread panel. Empty when no panel is open or the root has no replies.
    var threadPanelChildren: [ChatTimelineMessage] {
        guard let id = activeThreadParentID,
              let ids = childrenByThreadID[id] else { return [] }
        return ids.compactMap { messagesByID[$0] }
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
            try await sendHandler(text, selectedRoom, replyTo, nil)
            composerText = ""
            replyingToMessage = nil
        } catch {
            surfaceState = .error(title: "Message not sent", message: error.localizedDescription)
        }
    }

    /// Post the current `threadComposerText` as a XEP-0201 threaded reply to
    /// the message identified by `activeThreadParentID`. No-op when the panel
    /// is closed or the composer is empty. The text clears on success; on
    /// failure the draft is preserved so the user can retry.
    func sendThreadComposerMessage() async {
        let text = threadComposerText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty,
              let sendHandler,
              let rootID = activeThreadParentID else {
            return
        }

        isSendingThreadMessage = true
        defer { isSendingThreadMessage = false }

        do {
            try await sendHandler(text, selectedRoom, nil, rootID)
            threadComposerText = ""
        } catch {
            surfaceState = .error(title: "Reply not sent", message: error.localizedDescription)
        }
    }
}
