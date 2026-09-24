import Foundation

/// Why a port request failed. Diagnostics stay on the event stream's
/// `error` channel; callers only need to decide what to show.
public enum PortError: Error, Equatable, Sendable {
    case notConnected
    case timeout
    /// The server answered with an error (forbidden, not-allowed, …).
    case rejected
    /// The request itself was malformed (bad JID, empty value).
    case invalidRequest
    case failed
}

/// Outcome of a message send.
public enum SendOutcome: Equatable, Sendable {
    /// Written to the stream under this client stanza id.
    case sent(stanzaID: String)
    /// No live session; the caller may queue and retry.
    case notConnected
    /// The transport dropped it; the caller may queue and retry.
    case transportError
    /// Permanent: invalid recipient, invalid options or a stanza error.
    case rejected
}

/// Everything the XMPP core reports.
public enum XmppEvent: Sendable {
    /// The session is bound and ready.
    case connected
    /// The stream closed.
    case disconnected
    case message(WireMessage)
    case presence(WirePresence)
    /// XEP-0198 acknowledged the outbound stanza with this client id.
    case deliveryAcked(stanzaID: String)
    /// Transport failure, which cannot override an acknowledgement.
    case deliveryFailed(stanzaID: String)
    /// Explicit error, subject to outbound id and address validation.
    case messageRejected(stanzaID: String, from: JID, to: JID?)
    /// Live `urn:waddle:inbox:0` unread push.
    case inboxPush(InboxEntry)
    /// SASL failure: the presented credential is dead.
    case authenticationFailed
}

/// Stream lifecycle and presence.
public protocol ConnectionPort: AnyObject, Sendable {
    /// Every event the core emits, in order. Single consumer.
    var events: AsyncStream<XmppEvent> { get }
    func connect() async
    func disconnect() async
    /// Confirms that an online XMPP stream still answers requests.
    func probeConnection() async -> Bool
    func sendPresence(_ availability: Availability, status: String?) async
}

/// Conversation messaging verbs.
public protocol MessagingPort: AnyObject, Sendable {
    func send(_ message: OutboundMessage) async -> SendOutcome
    func sendCorrection(of targetID: String, body: String, in conversation: ConversationID, options: OutboundOptions) async -> SendOutcome
    func sendReaction(to targetID: String, emojis: [String], in conversation: ConversationID) async -> Bool
    func sendRetraction(of targetID: String, in conversation: ConversationID) async -> Bool
    func sendModeration(of targetID: String, in room: BareJID, reason: String?) async -> Bool
    func sendChatState(_ state: ChatState, in conversation: ConversationID) async -> Bool
    func setPinned(_ pinned: Bool, targetID: String, in conversation: ConversationID) async -> Bool
    func fetchPins(in room: BareJID) async throws -> [PinEntry]
}

/// History (XEP-0313) and search. Both throw when the query failed, so a
/// failure is never mistaken for an empty archive.
public protocol ArchivePort: AnyObject, Sendable {
    func fetchHistory(of conversation: ConversationID, before cursor: String?, max: Int) async throws -> ArchivePage
    func searchHistory(of conversation: ConversationID, query: String, max: Int) async throws -> ArchivePage
}

/// Read state: XEP-0333 markers, XEP-0490 sync, XEP-0430 inbox.
public protocol ReadStatePort: AnyObject, Sendable {
    func sendDisplayed(stanzaID: String, in conversation: ConversationID) async -> Bool
    func publishDisplayedCursor(_ cursor: DisplayedCursor) async -> Bool
    /// XEP-0490 §3: the account's PEP service supports publish-options.
    func supportsDisplayedCursorPublish() async -> Bool
    func fetchDisplayedCursors() async throws -> [DisplayedCursor]
    func subscribeDisplayedCursors() async -> Bool
    func fetchInbox() async throws -> [InboxEntry]
    func markInboxRead(partner: BareJID, threadID: String?) async throws
}

/// Spaces, rooms, members and users.
public protocol DirectoryPort: AnyObject, Sendable {
    func discoverTopology() async -> Result<Topology, PortError>
    func joinRoom(_ room: BareJID, nick: String) async
    func leaveRoom(_ room: BareJID, nick: String) async
    func listMembers(of room: BareJID) async throws -> [RoomMember]
    func setAffiliation(_ affiliation: RoomAffiliation, of user: BareJID, in room: BareJID, reason: String?) async throws
    func kick(nick: String, from room: BareJID, reason: String?) async throws
    func searchUsers(_ query: String) async throws -> [UserSearchResult]
    /// Creates a channel room and returns its JID.
    func createRoom(localpart: String, name: String, summary: String?, nick: String) async throws -> BareJID
    func createGroupDM(name: String, members: [BareJID]) async throws -> BareJID
    func setNotifyMode(_ mode: NotifyMode, for conversation: ConversationID) async throws
    func fetchNotifyModes() async throws -> [ConversationID: NotifyMode]
}

/// The signed-in user's published profile and other users' avatars.
public protocol ProfilePort: AnyObject, Sendable {
    func fetchAvatar(of jid: BareJID) async -> AvatarImage?
    func publishAvatar(_ image: AvatarImage) async throws
    func removeAvatar() async throws
    func fetchMood(of jid: BareJID) async throws -> UserMood?
    func publishMood(_ mood: UserMood) async throws
    func retractMood() async throws
}

/// XEP-0363 upload.
public protocol UploadPort: AnyObject, Sendable {
    func requestUploadSlot(filename: String, size: Int, mediaType: String) async -> UploadSlot?
}

/// XEP-0357 push registration.
public protocol PushPort: AnyObject, Sendable {
    /// Registers an APNs token with `push.<domain>` and enables XEP-0357 on
    /// the account. Returns the registration to keep for disabling.
    func registerPush(deviceToken: String, environment: PushEnvironment, appID: String) async -> PushRegistration?
    func enablePush(_ registration: PushRegistration) async -> Bool
    func disablePush(_ registration: PushRegistration) async -> Bool
}

public extension PushPort {
    func enablePush(_ registration: PushRegistration) async -> Bool { false }
}

/// XEP-0050 ad-hoc commands of the server's extension service. In a
/// room, `room` travels as the `waddle#room_jid` submit field.
public protocol CommandPort: AnyObject, Sendable {
    func discoverExtensionCommands() async throws -> [ExtensionCommand]
    /// XEP-0050 §2.4: starts `command` with `execute`.
    func invokeExtensionCommand(_ command: ExtensionCommand, room: BareJID?) async throws -> ExtensionCommandResult
    /// XEP-0050 §3: advances the session with `action`.
    func submitExtensionCommandForm(
        _ command: ExtensionCommand,
        sessionID: String?,
        values: [ExtensionFormValue],
        action: ExtensionCommandAction,
        room: BareJID?
    ) async throws -> ExtensionCommandResult
}

/// The full surface the session coordinator drives.
public typealias XmppPort = ConnectionPort & MessagingPort & ArchivePort & ReadStatePort & DirectoryPort & ProfilePort & UploadPort & PushPort & CommandPort
