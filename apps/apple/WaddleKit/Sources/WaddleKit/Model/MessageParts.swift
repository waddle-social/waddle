import Foundation

/// RFC 6121 message `type`.
public enum MessageType: Hashable, Sendable {
    case chat
    case groupchat
    case normal
    case headline
    case error

    /// Unknown values fall back to `normal`, as RFC 6121 §5.2.2 requires.
    public init(wire: String) {
        switch wire {
        case "chat": self = .chat
        case "groupchat": self = .groupchat
        case "headline": self = .headline
        case "error": self = .error
        default: self = .normal
        }
    }
}

/// A XEP-0359 `<stanza-id/>` and the entity that assigned it.
public struct StanzaID: Hashable, Sendable {
    public let id: String
    public let by: BareJID

    public init(id: String, by: BareJID) {
        self.id = id
        self.by = by
    }
}

/// Every wire identity a message carries. Actions and dedupe pick from this
/// according to the XEP that governs them; see `TimelineItem`.
public struct MessageIdentity: Hashable, Sendable {
    /// The stanza `@id`.
    public var messageID: String?
    /// XEP-0359 `<origin-id/>`, author-assigned.
    public var originID: String?
    /// The primary XEP-0359 stanza id the core selected.
    public var stanzaID: StanzaID?
    /// Every XEP-0359 `<stanza-id/>`, document order.
    public var stanzaIDs: [StanzaID]

    public init(messageID: String? = nil, originID: String? = nil, stanzaID: StanzaID? = nil, stanzaIDs: [StanzaID] = []) {
        self.messageID = messageID
        self.originID = originID
        self.stanzaID = stanzaID
        self.stanzaIDs = stanzaIDs
    }

    /// Dedupe key: stanza id, else origin id, else message id.
    public var primary: String? {
        stanzaID?.id ?? originID ?? messageID
    }

    /// All ids this message answers to.
    public var all: Set<String> {
        var ids = Set(stanzaIDs.map(\.id))
        if let id = stanzaID?.id { ids.insert(id) }
        if let originID { ids.insert(originID) }
        if let messageID { ids.insert(messageID) }
        return ids
    }

    /// XEP-0359 ids only, which are unique by construction (unlike `@id`).
    public var uniqueWireIDs: Set<String> {
        var ids = Set<String>()
        if let id = stanzaID?.id { ids.insert(id) }
        if let originID { ids.insert(originID) }
        return ids
    }

    /// The stanza id assigned by `authority`. Scans the full list: the first
    /// element may be sender-injected, trust comes from `by`.
    public func stanzaID(assignedBy authority: BareJID) -> String? {
        if let match = stanzaIDs.first(where: { $0.by == authority }) {
            return match.id
        }
        if let stanzaID, stanzaID.by == authority {
            return stanzaID.id
        }
        return nil
    }
}

/// XEP-0085 chat state.
public enum ChatState: Hashable, Sendable {
    case active
    case composing
    case paused
    case inactive
    case gone
}

/// XEP-0394 markup span. Offsets count Unicode scalars over the wire body.
public struct MarkupSpan: Hashable, Sendable, Codable {
    public enum Kind: Hashable, Sendable, Codable {
        case bold
        case italic
        case strikethrough
        case code
        case codeBlock
        case blockquote
        case link(URL)
    }

    public let kind: Kind
    public let start: Int
    public let end: Int

    public init(kind: Kind, start: Int, end: Int) {
        self.kind = kind
        self.start = start
        self.end = end
    }
}

/// XEP-0372 reference. Offsets count Unicode scalars over the wire body.
public struct Reference: Hashable, Sendable, Codable {
    public enum Kind: Hashable, Sendable, Codable {
        case mention
        case data
        case other(String)
    }

    public let kind: Kind
    public let uri: String
    public let begin: Int
    public let end: Int

    public init(kind: Kind, uri: String, begin: Int, end: Int) {
        self.kind = kind
        self.uri = uri
        self.begin = begin
        self.end = end
    }

    /// The mentioned JID for an `xmpp:` mention URI.
    public var mentionedJID: JID? {
        guard kind == .mention, uri.hasPrefix("xmpp:") else { return nil }
        let raw = uri.dropFirst("xmpp:".count).split(separator: "?", maxSplits: 1).first.map(String.init) ?? ""
        return JID(parsing: raw.removingPercentEncoding ?? raw)
    }
}

/// XEP-0448 encryption envelope of a shared file.
public struct EncryptedFileSource: Hashable, Sendable, Codable {
    public let cipher: FileCipher
    public let keyBase64: String
    public let ivBase64: String
    /// Digests of the ciphertext (XEP-0448 `<encrypted>` hashes).
    public let digests: FileDigests
    public let sources: [URL]

    public init(cipher: FileCipher, keyBase64: String, ivBase64: String, digests: FileDigests, sources: [URL]) {
        self.cipher = cipher
        self.keyBase64 = keyBase64
        self.ivBase64 = ivBase64
        self.digests = digests
        self.sources = sources
    }
}

/// XEP-0446/0447 file metadata.
public struct SharedFile: Hashable, Sendable, Codable {
    public enum Disposition: Hashable, Sendable, Codable {
        case inline
        case attachment
    }

    public let url: URL
    public let name: String?
    public let mediaType: String?
    public let size: Int?
    public let width: Int?
    public let height: Int?
    public let description: String?
    public let disposition: Disposition
    /// Digests of the plaintext file (XEP-0446 `<file>` hashes).
    public let digests: FileDigests
    public let encrypted: EncryptedFileSource?

    public init(
        url: URL,
        name: String? = nil,
        mediaType: String? = nil,
        size: Int? = nil,
        width: Int? = nil,
        height: Int? = nil,
        description: String? = nil,
        disposition: Disposition = .attachment,
        digests: FileDigests = FileDigests(),
        encrypted: EncryptedFileSource? = nil
    ) {
        self.url = url
        self.name = name
        self.mediaType = mediaType
        self.size = size
        self.width = width
        self.height = height
        self.description = description
        self.disposition = disposition
        self.digests = digests
        self.encrypted = encrypted
    }

    public var isImage: Bool { mediaType?.hasPrefix("image/") == true }
    public var isVideo: Bool { mediaType?.hasPrefix("video/") == true }
    public var isAudio: Bool { mediaType?.hasPrefix("audio/") == true }

    public var displayName: String {
        name ?? url.lastPathComponent
    }
}

/// Server-attached `urn:waddle:link-preview:0` card.
public struct LinkPreview: Hashable, Sendable {
    public let url: URL
    public let title: String?
    public let summary: String?
    public let imageURL: URL?
    public let imageAlt: String?

    public init(url: URL, title: String?, summary: String?, imageURL: URL?, imageAlt: String?) {
        self.url = url
        self.title = title
        self.summary = summary
        self.imageURL = imageURL
        self.imageAlt = imageAlt
    }
}

/// `urn:waddle:pin:0` pin preview.
public struct PinPreview: Hashable, Sendable {
    public let author: JID?
    public let authorNick: String?
    public let text: String
    public let messageTimestamp: Date?

    public init(author: JID?, authorNick: String?, text: String, messageTimestamp: Date?) {
        self.author = author
        self.authorNick = authorNick
        self.text = text
        self.messageTimestamp = messageTimestamp
    }
}

/// A live `urn:waddle:pin:0` pin or unpin broadcast.
public struct PinEvent: Hashable, Sendable {
    public enum Action: Hashable, Sendable {
        case pinned
        case unpinned
    }

    public let action: Action
    public let targetStanzaID: String
    public let by: JID?
    public let preview: PinPreview?

    public init(action: Action, targetStanzaID: String, by: JID?, preview: PinPreview?) {
        self.action = action
        self.targetStanzaID = targetStanzaID
        self.by = by
        self.preview = preview
    }
}

/// A pinned message from a `fetch_room_pins` snapshot.
public struct PinEntry: Hashable, Sendable {
    public let targetStanzaID: String
    public let pinner: JID?
    public let pinnedAt: Date?
    public let preview: PinPreview

    public init(targetStanzaID: String, pinner: JID?, pinnedAt: Date?, preview: PinPreview) {
        self.targetStanzaID = targetStanzaID
        self.pinner = pinner
        self.pinnedAt = pinnedAt
        self.preview = preview
    }
}

/// XEP-0490 displayed cursor for one conversation.
public struct DisplayedCursor: Hashable, Sendable {
    public let conversation: BareJID
    public let stanzaID: String
    public let stanzaIDBy: BareJID

    public init(conversation: BareJID, stanzaID: String, stanzaIDBy: BareJID) {
        self.conversation = conversation
        self.stanzaID = stanzaID
        self.stanzaIDBy = stanzaIDBy
    }
}

/// XEP-0201 forum post shape carried by Waddle forum channels.
public enum ForumPostKind: Hashable, Sendable {
    case topic
    case reply
}
