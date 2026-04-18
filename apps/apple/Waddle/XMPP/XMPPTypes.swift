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
    let markupSpans: [XMPPMarkupSpan]
}

struct XMPPPresenceEvent: Sendable, Equatable {
    let from: String?
    let to: String?
    let type: String?
    let status: String?
    let show: String?
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

struct XMPPDiscoveredWaddle: Sendable, Equatable, Identifiable {
    let id: String
    let name: String
    let isPublic: Bool
}

struct XMPPDiscoveredChannel: Sendable, Equatable, Identifiable {
    let id: String
    let name: String
    let channelType: String
    let position: Int
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
