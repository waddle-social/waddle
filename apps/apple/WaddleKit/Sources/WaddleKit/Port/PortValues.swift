import Foundation

/// Structured options a send carries beside its body.
public struct OutboundOptions: Hashable, Sendable {
    public var reply: Reply?
    public var thread: String?
    public var markupSpans: [MarkupSpan]
    public var references: [Reference]
    public var sharedFiles: [SharedFile]
    public var requestDisplayedMarker: Bool

    /// XEP-0461 reply with its XEP-0428 fallback range.
    public struct Reply: Hashable, Sendable {
        public let targetID: String
        public let author: JID
        public let fallback: Range<Int>?

        public init(targetID: String, author: JID, fallback: Range<Int>?) {
            self.targetID = targetID
            self.author = author
            self.fallback = fallback
        }
    }

    public init(
        reply: Reply? = nil,
        thread: String? = nil,
        markupSpans: [MarkupSpan] = [],
        references: [Reference] = [],
        sharedFiles: [SharedFile] = [],
        requestDisplayedMarker: Bool = false
    ) {
        self.reply = reply
        self.thread = thread
        self.markupSpans = markupSpans
        self.references = references
        self.sharedFiles = sharedFiles
        self.requestDisplayedMarker = requestDisplayedMarker
    }
}

/// A message about to be sent. `clientID` is stamped as both the stanza
/// `@id` and the XEP-0359 `<origin-id/>`.
public struct OutboundMessage: Hashable, Sendable {
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

/// XEP-0363 upload slot.
public struct UploadSlot: Hashable, Sendable {
    public let putURL: URL
    public let getURL: URL
    public let headers: [String: String]

    public init(putURL: URL, getURL: URL, headers: [String: String]) {
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
