import Foundation

/// A message stanza parsed once at the FFI boundary. Live stanzas and MAM
/// archive results share this shape; `source` records which one it was.
public struct WireMessage: Hashable, Sendable {
    public enum Source: Hashable, Sendable {
        case live
        /// A XEP-0313 result, with the archive's result id.
        case archive(mamID: String)
    }

    public struct Moderation: Hashable, Sendable {
        public let targetID: String
        public let moderatedBy: String?
        public let reason: String?

        public init(targetID: String, moderatedBy: String?, reason: String?) {
            self.targetID = targetID
            self.moderatedBy = moderatedBy
            self.reason = reason
        }
    }

    public struct Reaction: Hashable, Sendable {
        public let targetID: String
        /// The sender's complete current set (XEP-0444 replace semantics).
        public let emojis: [String]

        public init(targetID: String, emojis: [String]) {
            self.targetID = targetID
            self.emojis = emojis
        }
    }

    public struct ReplyTarget: Hashable, Sendable {
        public let id: String
        public let author: JID?
        /// XEP-0428 fallback range over the body, in Unicode scalars.
        public let fallback: Range<Int>?

        public init(id: String, author: JID?, fallback: Range<Int>?) {
            self.id = id
            self.author = author
            self.fallback = fallback
        }
    }

    public var source: Source
    public var type: MessageType
    public var from: JID?
    public var to: JID?
    public var identity: MessageIdentity
    public var timestamp: Date?
    public var body: String?
    public var subject: String?
    public var replacesID: String?
    public var retractsID: String?
    /// The archive returned this message as a XEP-0424 tombstone.
    public var isRetracted: Bool
    public var moderation: Moderation?
    public var reaction: Reaction?
    /// XEP-0422 `urn:waddle:safety-scores:1` fastening, sender unverified.
    public var safetyScores: SafetyScoresFastening?
    public var chatState: ChatState?
    public var displayedMarkerRequested: Bool
    public var displayedMarkerID: String?
    public var thread: String?
    public var parentThread: String?
    public var reply: ReplyTarget?
    public var markupSpans: [MarkupSpan]
    public var references: [Reference]
    public var broadcastMention: String?
    public var forumPostKind: ForumPostKind?
    public var forumTitle: String?
    public var isSticker: Bool
    public var sharedFiles: [SharedFile]
    public var linkPreviews: [LinkPreview]
    public var pinEvent: PinEvent?
    /// XEP-0490 cursors from a sibling device's PEP notification.
    public var displayedCursors: [DisplayedCursor]?

    public init(
        source: Source = .live,
        type: MessageType,
        from: JID?,
        to: JID?,
        identity: MessageIdentity,
        timestamp: Date? = nil,
        body: String? = nil,
        subject: String? = nil,
        replacesID: String? = nil,
        retractsID: String? = nil,
        isRetracted: Bool = false,
        moderation: Moderation? = nil,
        reaction: Reaction? = nil,
        safetyScores: SafetyScoresFastening? = nil,
        chatState: ChatState? = nil,
        displayedMarkerRequested: Bool = false,
        displayedMarkerID: String? = nil,
        thread: String? = nil,
        parentThread: String? = nil,
        reply: ReplyTarget? = nil,
        markupSpans: [MarkupSpan] = [],
        references: [Reference] = [],
        broadcastMention: String? = nil,
        forumPostKind: ForumPostKind? = nil,
        forumTitle: String? = nil,
        isSticker: Bool = false,
        sharedFiles: [SharedFile] = [],
        linkPreviews: [LinkPreview] = [],
        pinEvent: PinEvent? = nil,
        displayedCursors: [DisplayedCursor]? = nil
    ) {
        self.source = source
        self.type = type
        self.from = from
        self.to = to
        self.identity = identity
        self.timestamp = timestamp
        self.body = body
        self.subject = subject
        self.replacesID = replacesID
        self.retractsID = retractsID
        self.isRetracted = isRetracted
        self.moderation = moderation
        self.reaction = reaction
        self.safetyScores = safetyScores
        self.chatState = chatState
        self.displayedMarkerRequested = displayedMarkerRequested
        self.displayedMarkerID = displayedMarkerID
        self.thread = thread
        self.parentThread = parentThread
        self.reply = reply
        self.markupSpans = markupSpans
        self.references = references
        self.broadcastMention = broadcastMention
        self.forumPostKind = forumPostKind
        self.forumTitle = forumTitle
        self.isSticker = isSticker
        self.sharedFiles = sharedFiles
        self.linkPreviews = linkPreviews
        self.pinEvent = pinEvent
        self.displayedCursors = displayedCursors
    }

    public var isGroupchat: Bool { type == .groupchat }

    public var isLive: Bool { source == .live }

    /// True when the message only mutates another row (reaction,
    /// correction, retraction, moderation, safety scores). Such stanzas
    /// never notify, never bump unread and never reorder recency.
    public var isMutation: Bool {
        moderation != nil || retractsID != nil || replacesID != nil || reaction != nil || safetyScores != nil
    }

    /// True when the message is a thread reply that renders only inside its
    /// thread, not in the main feed.
    public var isThreadReply: Bool {
        guard let thread else { return false }
        return !identity.all.contains(thread)
    }

    /// The mentioned JIDs from XEP-0372 references.
    public var mentionedJIDs: [JID] {
        references.compactMap(\.mentionedJID)
    }
}
