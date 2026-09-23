import Foundation
@testable import WaddleKit

/// Records every port call and answers with scripted values.
@MainActor
final class FakePort: XmppPort {
    nonisolated let events: AsyncStream<XmppEvent>
    private let continuation: AsyncStream<XmppEvent>.Continuation

    var sent: [OutboundMessage] = []
    var sendOutcome: (OutboundMessage) -> SendOutcome = { .sent(stanzaID: $0.clientID) }
    var reactions: [(target: String, emojis: [String])] = []
    var corrections: [(target: String, body: String, options: OutboundOptions)] = []
    var retractions: [String] = []
    var displayed: [(id: String, conversation: ConversationID)] = []
    var publishedCursors: [DisplayedCursor] = []
    var chatStates: [(ChatState, ConversationID)] = []
    var joined: [BareJID] = []
    var inboxReads: [BareJID] = []
    var topology: Topology = .empty
    var inbox: [InboxEntry] = []
    var cursors: [DisplayedCursor] = []
    var historyPages: [ArchivePage] = []
    var historyRequests: [(ConversationID, String?)] = []
    var failingHistoryRequests = 0
    var connectCount = 0
    var probeConnectionResult = true
    var probeConnectionCount = 0

    init() {
        var captured: AsyncStream<XmppEvent>.Continuation!
        events = AsyncStream { captured = $0 }
        continuation = captured
    }

    func emit(_ event: XmppEvent) {
        continuation.yield(event)
    }

    var connectDelay: UInt64 = 0
    var disconnectCount = 0

    func connect() async {
        connectCount += 1
        if connectDelay > 0 {
            try? await Task.sleep(nanoseconds: connectDelay)
        }
    }

    func disconnect() async {
        disconnectCount += 1
        emit(.disconnected)
    }
    func probeConnection() async -> Bool {
        probeConnectionCount += 1
        return probeConnectionResult
    }
    func sendPresence(_ availability: Availability, status: String?) async {}

    func send(_ message: OutboundMessage) async -> SendOutcome {
        sent.append(message)
        return sendOutcome(message)
    }

    func sendCorrection(of targetID: String, body: String, in conversation: ConversationID, options: OutboundOptions) async -> SendOutcome {
        corrections.append((targetID, body, options))
        return .sent(stanzaID: "correction")
    }

    func sendReaction(to targetID: String, emojis: [String], in conversation: ConversationID) async -> Bool {
        reactions.append((targetID, emojis))
        return true
    }

    func sendRetraction(of targetID: String, in conversation: ConversationID) async -> Bool {
        retractions.append(targetID)
        return true
    }

    func sendModeration(of targetID: String, in room: BareJID, reason: String?) async -> Bool { true }

    func sendChatState(_ state: ChatState, in conversation: ConversationID) async -> Bool {
        chatStates.append((state, conversation))
        return true
    }

    func setPinned(_ pinned: Bool, targetID: String, in conversation: ConversationID) async -> Bool { true }
    func fetchPins(in room: BareJID) async throws -> [PinEntry] { [] }

    /// While set, history fetches suspend until `releaseHistory()`.
    var holdsHistory = false
    private(set) var heldHistory: [CheckedContinuation<Void, Never>] = []

    func releaseHistory() {
        let held = heldHistory
        heldHistory = []
        held.forEach { $0.resume() }
    }

    func fetchHistory(of conversation: ConversationID, before cursor: String?, max: Int) async throws -> ArchivePage {
        historyRequests.append((conversation, cursor))
        if holdsHistory {
            await withCheckedContinuation { heldHistory.append($0) }
        }
        if failingHistoryRequests > 0 {
            failingHistoryRequests -= 1
            throw PortError.failed
        }
        guard !historyPages.isEmpty else { return ArchivePage(messages: [], first: nil, isComplete: true) }
        return historyPages.removeFirst()
    }

    func searchHistory(of conversation: ConversationID, query: String, max: Int) async throws -> ArchivePage {
        ArchivePage(messages: [], first: nil, isComplete: true)
    }

    func sendDisplayed(stanzaID: String, in conversation: ConversationID) async -> Bool {
        displayed.append((stanzaID, conversation))
        return true
    }

    func publishDisplayedCursor(_ cursor: DisplayedCursor) async -> Bool {
        publishedCursors.append(cursor)
        return true
    }

    func supportsDisplayedCursorPublish() async -> Bool { true }
    func fetchDisplayedCursors() async throws -> [DisplayedCursor] { cursors }
    func subscribeDisplayedCursors() async -> Bool { true }
    func fetchInbox() async throws -> [InboxEntry] { inbox }

    func markInboxRead(partner: BareJID, threadID: String?) async throws {
        inboxReads.append(partner)
    }

    func discoverTopology() async -> Result<Topology, PortError> { .success(topology) }

    func joinRoom(_ room: BareJID, nick: String) async {
        joined.append(room)
    }

    func leaveRoom(_ room: BareJID, nick: String) async {}
    func listMembers(of room: BareJID) async throws -> [RoomMember] { [] }
    func setAffiliation(_ affiliation: RoomAffiliation, of user: BareJID, in room: BareJID, reason: String?) async throws {}
    func kick(nick: String, from room: BareJID, reason: String?) async throws {}
    func searchUsers(_ query: String) async throws -> [UserSearchResult] { [] }

    func createRoom(localpart: String, name: String, summary: String?, nick: String) async throws -> BareJID {
        BareJID(localpart: localpart, domain: "muc.waddle.test")!
    }

    func createGroupDM(name: String, members: [BareJID]) async throws -> BareJID {
        BareJID(localpart: "group", domain: "muc.waddle.test")!
    }

    func setNotifyMode(_ mode: NotifyMode, for conversation: ConversationID) async throws {}
    func fetchNotifyModes() async throws -> [ConversationID: NotifyMode] { [:] }
    func fetchAvatar(of jid: BareJID) async -> AvatarImage? { nil }
    func publishAvatar(_ image: AvatarImage) async throws {}
    func removeAvatar() async throws {}
    func fetchMood(of jid: BareJID) async throws -> UserMood? { nil }
    func publishMood(_ mood: UserMood) async throws {}
    func retractMood() async throws {}
    func requestUploadSlot(filename: String, size: Int, mediaType: String) async -> UploadSlot? { nil }
    func registerPush(deviceToken: String, environment: PushEnvironment, appID: String) async -> PushRegistration? { nil }
    func disablePush(_ registration: PushRegistration) async -> Bool { true }
}
