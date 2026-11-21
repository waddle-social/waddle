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
    case call(CallEvent)
    /// XEP-0198 acknowledged the outbound stanza with this client id.
    case deliveryAcked(stanzaID: String)
    /// Transport failure, which cannot override an acknowledgement.
    case deliveryFailed(stanzaID: String)
    /// Explicit error, subject to outbound id and address validation.
    case messageRejected(stanzaID: String, from: JID, to: JID?)
    /// XEP-0084 metadata notification from a peer's PEP service; `id` is
    /// nil when the avatar was disabled.
    case avatarChanged(jid: BareJID, id: String?)
    /// Another device of this account picked a status or reset to
    /// automatic (ADR-010). The server does not send a device its own pick.
    case presenceModeChanged(PresenceMode)
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
    /// `idleSince` is when input stopped on this device (XEP-0319).
    func sendPresence(_ availability: Availability, status: String?, idleSince: Date?) async
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
    /// One room thread's replies: the room archive filtered by the Waddle
    /// MAM thread field, newest page first when `cursor` is nil.
    func fetchThreadHistory(in room: BareJID, threadID: String, before cursor: String?, max: Int) async throws -> ArchivePage
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
    /// Fetches the room settings offered by the server.
    func fetchRoomSettings(of room: BareJID) async throws -> RoomSettings
    /// Updates only the fields included in the patch.
    func updateRoomSettings(_ patch: RoomSettingsPatch, of room: BareJID) async throws
    /// Destroys the room for all occupants.
    func destroyRoom(_ room: BareJID, reason: String?) async throws
    /// Renames a group DM; nil clears its name.
    func renameGroupDM(_ room: BareJID, name: String?) async throws
    /// Sends an invitation; a later refusal arrives as a message error.
    func inviteToGroupDM(_ room: BareJID, invitee: BareJID, history: GroupDMHistoryAccess) async throws
    /// Leaves the group DM and removes our membership.
    func leaveGroupDM(_ room: BareJID) async throws
    func listMembers(of room: BareJID) async throws -> [RoomMember]
    /// The bots the room declares; the affiliation lists never name them.
    func listRoomBots(in room: BareJID) async throws -> [RoomBot]
    func setAffiliation(_ affiliation: RoomAffiliation, of user: BareJID, in room: BareJID, reason: String?) async throws
    func kick(nick: String, from room: BareJID, reason: String?) async throws
    func searchUsers(_ query: String) async throws -> [UserSearchResult]
    /// Creates a channel room and returns its JID.
    func createRoom(localpart: String, name: String, summary: String?, nick: String) async throws -> BareJID
    func createGroupDM(name: String, members: [BareJID]) async throws -> BareJID
    /// Stores both settings of a conversation; they travel together.
    func setNotifyMode(_ mode: NotifyMode, showsMessagesInPush: Bool, for conversation: ConversationID) async throws
    func fetchNotifySettings() async throws -> NotifySettings
}

/// The signed-in user's published profile and other users' avatars.
public protocol ProfilePort: AnyObject, Sendable {
    /// `knownID` is the id of the avatar already held, if any.
    func fetchAvatar(of jid: BareJID, knownID: String?) async -> AvatarFetch
    /// Returns the XEP-0084 item id the avatar was published under.
    func publishAvatar(_ image: AvatarImage) async throws -> String
    func removeAvatar() async throws
    func fetchMood(of jid: BareJID) async throws -> UserMood?
    func publishMood(_ mood: UserMood) async throws
    func retractMood() async throws
    /// The account's stored presence mode; automatic when none is stored.
    func fetchPresenceMode() async throws -> PresenceMode
    /// Stores the mode for every device of the account to take up.
    func publishPresenceMode(_ mode: PresenceMode) async throws
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

/// The community feed (XEP-0472) and stories (XEP-0501) of the account's
/// own server.
public protocol CommunityPort: AnyObject, Sendable {
    /// The newest posts, in the server's order.
    func fetchFeed(max: Int?) async throws -> [FeedPost]
    /// Returns the post as published.
    func publishFeedPost(_ draft: FeedPostDraft) async throws -> FeedPost
    /// Expired stories included.
    func fetchStories(max: Int?) async throws -> [Story]
    /// Returns the story as published.
    func publishStory(_ draft: StoryDraft) async throws -> Story
    /// The account's private "seen" state: story id to when it was seen.
    func fetchStoryReads() async throws -> [String: Date]
    /// Replaces the whole "seen" state.
    func publishStoryReads(_ reads: [String: Date]) async throws
    func fetchStoryReactions(of storyID: String) async throws -> StoryReactions
    /// Makes `emojis` the account's reactions to the story; empty removes them.
    func setStoryReactions(_ emojis: [String], on storyID: String) async throws
}

/// The community calendar of the account's own server: xCal events on
/// `community.<domain>`, each member's replies beside them.
public protocol EventsPort: AnyObject, Sendable {
    /// Every event, each with every member's reply.
    func fetchEvents() async throws -> [CommunityEvent]
    /// Creates the event, or replaces it when the draft names one. Returns
    /// it as stored.
    func publishEvent(_ draft: EventDraft) async throws -> CommunityEvent
    /// Removes the event, every date of a series.
    func cancelEvent(_ eventID: String) async throws
    /// Cancels the one date of a series that `recurrenceStart` names, and
    /// returns the series.
    func cancelOccurrence(_ recurrenceStart: CalendarMoment, of eventID: String) async throws -> CommunityEvent
    /// Sets the account's reply to the event, every date of a series.
    func reply(_ reply: EventReply, toEvent uid: String) async throws
    /// Whether the account may create, edit and cancel events. False when
    /// that cannot be found out.
    func isCommunityOwner() async -> Bool
    /// The dates of `event` that may fall in `range`, worked out by the
    /// shared core so every client names the same ones. Empty if they
    /// cannot be worked out.
    @MainActor func occurrences(of event: CommunityEvent, in range: Range<Date>) -> [EventOccurrence]
}

/// XEP-0449 sticker packs: the account's own, and one of anyone's by
/// where it is.
public protocol StickerPort: AnyObject, Sendable {
    /// Every pack the account has published; none is an empty list.
    func fetchStickerPacks() async throws -> [StickerPack]
    /// The pack at `location`, or nil when it is not there.
    func fetchStickerPack(at location: StickerPackLocation) async throws -> StickerPack?
    /// Publishes a pack and answers with the id its content gives it.
    func publishStickerPack(_ draft: StickerPackDraft) async throws -> String
    /// Publishes the pack at `location` again on the account's own
    /// packs, as it is, and answers with its id there. A pack its author
    /// restricted is refused.
    func copyStickerPack(from location: StickerPackLocation) async throws -> String
    /// Removing a pack that is already gone succeeds.
    func retractStickerPack(_ packID: String) async throws
}

/// The full surface the session coordinator drives.
public typealias XmppPort = ConnectionPort & MessagingPort & ArchivePort & ReadStatePort & DirectoryPort & ProfilePort & UploadPort & PushPort & CommandPort & CallPort & CommunityPort & EventsPort & StickerPort
