import Foundation

/// A change to the account's XEP-0191 block list.
public enum BlockListChange: Sendable {
    case blocked([JID])
    case unblocked([JID])
    case unblockedAll
}

/// The XEP-0201 thread a message goes to: `<thread parent='…'>id</thread>`.
public struct ThreadTarget: Hashable, Sendable {
    public let id: String
    /// The thread `id` hangs off, for a sub-thread.
    public var parent: String?

    public init(id: String, parent: String? = nil) {
        self.id = id
        self.parent = parent
    }
}

/// Structured options a send carries beside its body.
public struct OutboundOptions: Hashable, Sendable, Codable {
    public var reply: Reply?
    public var thread: ThreadTarget? {
        get { threadID.map { ThreadTarget(id: $0, parent: parentThreadID) } }
        set {
            threadID = newValue?.id
            parentThreadID = newValue?.parent
        }
    }
    /// Kept flat, as the outbox has always saved the thread id.
    private var threadID: String?
    private var parentThreadID: String?
    public var markupSpans: [MarkupSpan]
    public var references: [Reference]
    public var sharedFiles: [SharedFile]
    public var requestDisplayedMarker: Bool
    /// XEP-0085 §5.1: carry `<active/>` on the content message. Stamped by
    /// the coordinator from the chat-state preference at send time.
    public var chatStateActive: Bool
    /// Room sends: resolve typed `@nick` tokens to mention references.
    public var mentionDirectory: MentionDirectory?
    /// XEP-0449: the message is a sticker, whose picture is the one file
    /// in `sharedFiles`.
    public var sticker: StickerReference?
    public var linkPreviewToken: LinkPreviewToken? { linkPreview?.token }
    /// Omitted from CodingKeys. This authorization expires and is valid for
    /// the live send only, so a restored outbox must never replay it.
    var linkPreview: LinkPreviewPayload? = nil

    /// XEP-0461 reply with its XEP-0428 fallback range.
    public struct Reply: Hashable, Sendable, Codable {
        public let targetID: String
        public let author: JID
        public let fallback: Range<Int>?

        public init(targetID: String, author: JID, fallback: Range<Int>?) {
            self.targetID = targetID
            self.author = author
            self.fallback = fallback
        }
    }

    private enum CodingKeys: String, CodingKey {
        case reply
        case threadID = "thread"
        case parentThreadID = "parentThread"
        case markupSpans, references, sharedFiles, requestDisplayedMarker, chatStateActive, mentionDirectory, sticker
    }

    public init(
        reply: Reply? = nil,
        thread: ThreadTarget? = nil,
        markupSpans: [MarkupSpan] = [],
        references: [Reference] = [],
        sharedFiles: [SharedFile] = [],
        requestDisplayedMarker: Bool = false,
        chatStateActive: Bool = false,
        mentionDirectory: MentionDirectory? = nil,
        sticker: StickerReference? = nil
    ) {
        self.reply = reply
        threadID = thread?.id
        parentThreadID = thread?.parent
        self.markupSpans = markupSpans
        self.references = references
        self.sharedFiles = sharedFiles
        self.requestDisplayedMarker = requestDisplayedMarker
        self.chatStateActive = chatStateActive
        self.mentionDirectory = mentionDirectory
        self.sticker = sticker
    }
}

/// A message about to be sent. `clientID` is stamped as both the stanza
/// `@id` and the XEP-0359 `<origin-id/>`.
public struct OutboundMessage: Hashable, Sendable, Codable {
    public let clientID: String
    public let conversation: ConversationID
    public let body: String
    public let options: OutboundOptions

    public init(clientID: String, conversation: ConversationID, body: String, options: OutboundOptions) {
        self.clientID = clientID
        self.conversation = conversation
        self.body = body
        self.options = options
    }
}

/// XEP-0084 avatar bytes.
public struct AvatarImage: Hashable, Sendable {
    public let data: Data
    public let mediaType: String
    public let width: Int
    public let height: Int

    public init(data: Data, mediaType: String, width: Int, height: Int) {
        self.data = data
        self.mediaType = mediaType
        self.width = width
        self.height = height
    }
}

/// Outcome of a XEP-0084 avatar lookup (vCard-temp PHOTO as fallback).
public enum AvatarFetch: Hashable, Sendable {
    /// The peer advertises `id`, and these are its bytes.
    case published(id: String, image: AvatarImage)
    /// The advertised id is the known one passed in; no data was
    /// transferred (XEP-0084 §4.2).
    case unchanged
    /// The peer has no avatar.
    case absent
    /// The lookup failed; what is held stays.
    case failed
}

/// One `<header/>` of a XEP-0363 slot's `<put/>`.
public struct UploadHeader: Hashable, Sendable {
    public let name: String
    public let value: String

    public init(name: String, value: String) {
        self.name = name
        self.value = value
    }
}

/// XEP-0363 upload slot. `headers` keeps every `<header/>` in wire order:
/// a name may repeat and the relative order of its values must survive.
public struct UploadSlot: Hashable, Sendable {
    public let putURL: URL
    public let getURL: URL
    public let headers: [UploadHeader]

    public init(putURL: URL, getURL: URL, headers: [UploadHeader]) {
        self.putURL = putURL
        self.getURL = getURL
        self.headers = headers
    }
}

public enum PushEnvironment: Hashable, Sendable {
    case production
    case sandbox
}

/// What `push.<domain>` assigned to this device.
public struct PushRegistration: Hashable, Sendable, Codable {
    public let serviceJID: String
    public let node: String
    public let deviceID: String

    public init(serviceJID: String, node: String, deviceID: String) {
        self.serviceJID = serviceJID
        self.node = node
        self.deviceID = deviceID
    }
}
