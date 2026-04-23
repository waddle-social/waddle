import Foundation

struct XMPPJID: Sendable, Equatable {
    let localpart: String?
    let domain: String
    let resource: String?

    init?(string: String) {
        let trimmed = string.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return nil
        }

        let resourceParts = trimmed.split(separator: "/", maxSplits: 1, omittingEmptySubsequences: false)
        let bare = String(resourceParts[0])
        resource = resourceParts.count > 1 ? String(resourceParts[1]) : nil

        let jidParts = bare.split(separator: "@", maxSplits: 1, omittingEmptySubsequences: false)
        if jidParts.count == 2 {
            localpart = String(jidParts[0])
            domain = String(jidParts[1])
        } else {
            localpart = nil
            domain = bare
        }
    }

    var bare: String {
        if let localpart {
            return "\(localpart)@\(domain)"
        }
        return domain
    }
}

struct XMPPCredentials: Sendable, Equatable {
    let jid: String
    let bareJID: String
    let domain: String
    let username: String
    let bearerToken: String
    let websocketURL: URL
    let resource: String

    init(session: WaddleSession) {
        jid = session.jid
        username = session.username
        bearerToken = session.sessionID

        let parsedJID = XMPPJID(string: session.jid)
        bareJID = parsedJID?.bare ?? session.jid
        domain = parsedJID?.domain ?? AppConfig.defaultServerURL.host ?? "xmpp.waddle.social"
        websocketURL = URL(string: session.xmppWebsocketURL)
            ?? URL(string: "wss://\(domain)/xmpp-websocket")!

        let suffix = session.sessionID.prefix(8)
        resource = "waddle-\(suffix.isEmpty ? "client" : suffix)"
    }
}

enum XMPPConnectionState: Equatable {
    case disconnected
    case connecting
    case negotiating
    case authenticating
    case binding
    case ready
    case disconnecting
    case failed(String)
}

struct XMPPStreamFeatures: Sendable, Equatable {
    var mechanisms: Set<String> = []
    var supportsBind = false
    var supportsSession = false
    var supportsSM3 = false

    var supportsOAUTHBearer: Bool {
        mechanisms.contains("OAUTHBEARER")
    }
}

struct XMPPMarkupSpan: Sendable, Equatable, Hashable {
    enum SpanType: String, Sendable, Equatable, Hashable {
        case bold = "b"
        case italic = "i"
        case strikethrough = "s"
        case code = "code"
        case codeBlock = "code-block"
        case blockquote = "blockquote"
        case link = "link"
    }

    let type: SpanType
    let start: Int
    let end: Int
    let uri: String?
}

struct XMPPSharedFile: Sendable, Equatable, Hashable, Identifiable {
    var id: String { url }
    let url: String
    let name: String?
    let mediaType: String?
    let size: Int?
    let width: Int?
    let height: Int?
    let disposition: String
    let encryptedSource: XMPPEncryptedSource?

    var isInlineImage: Bool {
        disposition == "inline" && (mediaType?.hasPrefix("image/") == true)
    }

    var isInlineVideo: Bool {
        disposition == "inline" && (mediaType?.hasPrefix("video/") == true)
    }

    var isInlineAudio: Bool {
        disposition == "inline" && (mediaType?.hasPrefix("audio/") == true)
    }

    var isInlinePdf: Bool {
        disposition == "inline" && mediaType == "application/pdf"
    }

    var isEncrypted: Bool {
        encryptedSource != nil
    }
}

struct XMPPMessageEvent: Sendable, Equatable {
    let from: String?
    let to: String?
    let type: String?
    let id: String?
    let stanzaID: String?
    let body: String?
    let subject: String?
    let thread: String?
    let timestamp: Date?
    let replacesID: String?
    let retractsID: String?
    let reactionTargetID: String?
    let reactionEmojis: [String]
    let replyToID: String?
    let replyToSender: String?
    /// XEP-0428 fallback range (char offsets, end exclusive) identifying the
    /// quoted-reply prefix inside `body` that supporting clients should strip.
    let replyFallbackRange: Range<Int>?
    let markupSpans: [XMPPMarkupSpan]
    let chatState: String?
    let displayedMarkerID: String?
    let sharedFiles: [XMPPSharedFile]
    let broadcastMention: String?
    let mentionURIs: [String]
    let forumPostKind: String?
    let forumTitle: String?
    let threadID: String?
    let parentThreadID: String?
    let isSticker: Bool
}

struct XMPPPresenceHat: Sendable, Equatable, Hashable {
    let uri: String
    let title: String
}

struct XMPPPresenceEvent: Sendable, Equatable {
    let from: String?
    let to: String?
    let type: String?
    let status: String?
    let show: String?
    let hats: [XMPPPresenceHat]
}

// MARK: - XEP-0430 Inbox

struct XMPPInboxEntry: Sendable, Equatable {
    let jid: String
    let unreadCount: Int
    let lastMessageBody: String?
    let timestamp: Date?
}

// MARK: - XEP-0107/0108/0118 PEP User Status

struct XMPPUserMood: Sendable, Equatable {
    let mood: String
    let text: String?
}

struct XMPPUserActivity: Sendable, Equatable {
    let activity: String
    let text: String?
}

struct XMPPUserTune: Sendable, Equatable {
    let artist: String?
    let title: String?
    let source: String?
    let length: Int?
    let uri: String?
}

// MARK: - XEP-0448 Encrypted File Sharing

struct XMPPEncryptedSource: Sendable, Equatable, Hashable {
    let url: String
    let keyBase64: String
    let ivBase64: String
    let cipher: String
}

enum XMPPEvent: Sendable, Equatable {
    case streamFeatures(XMPPStreamFeatures)
    case authenticated
    case authenticationFailed(String?)
    case resourceBound(jid: String)
    case sessionReady
    case message(XMPPMessageEvent)
    case presence(XMPPPresenceEvent)
    case streamError(name: String, text: String?)
    case error(String)
    case disconnected
}

struct XMPPDiscoItem: Sendable, Equatable {
    let jid: String?
    let name: String?
    let node: String?
}

struct XMPPRSMPageInfo: Sendable, Equatable {
    let first: String?
    let last: String?
    let count: Int?
    let index: Int?
    let isComplete: Bool
}

struct XMPPArchiveMessage: Sendable, Equatable {
    let mamID: String?
    let queryID: String?
    let stanzaID: String?
    let delayedDeliveryTimestamp: Date?
    let message: XMPPMessageEvent
}

struct XMPPArchivePage: Sendable, Equatable {
    let messages: [XMPPArchiveMessage]
    let pageInfo: XMPPRSMPageInfo
}

extension WaddleSession {
    var xmppCredentials: XMPPCredentials {
        XMPPCredentials(session: self)
    }
}

func barePeerJID(_ fullJID: String) -> String {
    fullJID.split(separator: "/", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? fullJID
}

func jidDomain(_ jid: String) -> String {
    let bare = barePeerJID(jid)
    return bare.split(separator: "@", maxSplits: 1, omittingEmptySubsequences: false).last.map(String.init) ?? bare
}

func roomBareJID(accountJID: String, waddleID: String, channelID: String) -> String {
    "\(waddleID)_\(channelID)@muc.\(jidDomain(accountJID))"
}

func parseManagedRoomBareJID(_ roomJID: String) -> (waddleID: String, channelID: String)? {
    let node = barePeerJID(roomJID).split(separator: "@", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? ""
    guard let separator = node.firstIndex(of: "_") else {
        return nil
    }

    let waddleID = String(node[..<separator])
    let channelID = String(node[node.index(after: separator)...])
    guard !waddleID.isEmpty, !channelID.isEmpty else {
        return nil
    }

    return (waddleID, channelID)
}

// MARK: - Channel creation result

struct CreateChannelResult: Sendable {
    let channelID: String?
    let channelJID: String?
}

// MARK: - XMPP session error (used in AppModel pending-join bookkeeping)

enum XMPPServiceError: LocalizedError {
    case notReady
    case disconnected
    case timeout(String)
    case iqError(String)

    var errorDescription: String? {
        switch self {
        case .notReady: return "XMPP session is not ready yet."
        case .disconnected: return "The XMPP session disconnected before the request completed."
        case .timeout(let message): return message
        case .iqError(let message): return message
        }
    }
}

// MARK: - Markup parsing (moved from XMPPXML)

func parseMarkdownToMarkupSpans(_ text: String) -> (plainText: String, spans: [XMPPMarkupSpan]) {
    var spans: [XMPPMarkupSpan] = []
    var result = ""
    var i = text.startIndex

    let patterns: [(marker: String, type: XMPPMarkupSpan.SpanType)] = [
        ("```", .codeBlock),
        ("`", .code),
        ("*", .bold),
        ("_", .italic),
        ("~", .strikethrough),
    ]

    while i < text.endIndex {
        var matched = false
        for (marker, spanType) in patterns {
            guard text[i...].hasPrefix(marker) else { continue }
            let afterOpen = text.index(i, offsetBy: marker.count)
            guard afterOpen < text.endIndex else { continue }
            guard let closeRange = text[afterOpen...].range(of: marker) else { continue }
            let content = String(text[afterOpen..<closeRange.lowerBound])
            guard !content.isEmpty else { continue }
            let start = result.utf8.count
            result += content
            let end = result.utf8.count
            spans.append(XMPPMarkupSpan(type: spanType, start: start, end: end, uri: nil))
            i = text.index(closeRange.lowerBound, offsetBy: marker.count)
            matched = true
            break
        }
        if !matched {
            result.append(text[i])
            i = text.index(after: i)
        }
    }
    return (result, spans)
}
