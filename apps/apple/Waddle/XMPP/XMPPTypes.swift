import Foundation

struct XMPPJID: Sendable, Equatable {
    let localpart: String?
    let domain: String
    let resource: String?

    init?(string: String) {
        guard let parts = parseJid(input: string) else {
            return nil
        }
        localpart = parts.localpart
        domain = parts.domain
        resource = parts.resource
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
            ?? URL(string: "wss://\(domain)/ws")!

        let suffix = session.sessionID.prefix(8)
        resource = "waddle-\(suffix.isEmpty ? "client" : suffix)"
    }
}

struct XMPPDiscoveredSpace: Sendable, Equatable, Identifiable {
    let id: String
    let serviceJID: String
    let name: String
    let description: String?
}

struct XMPPDiscoveredChannel: Sendable, Equatable, Identifiable {
    let id: String
    let roomJID: String
    let name: String
    let description: String?
    let channelType: String
    let position: Int
    let spaceID: String
}

struct XMPPDiscoveredTopology: Sendable, Equatable {
    let spaces: [XMPPDiscoveredSpace]
    let channels: [XMPPDiscoveredChannel]
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

enum XMPPMucAffiliation: Sendable, Equatable, Hashable {
    case owner
    case admin
    case member
    case outcast
    case none
}

enum XMPPMucRole: Sendable, Equatable, Hashable {
    case moderator
    case participant
    case visitor
    case none
}

struct XMPPPresenceEvent: Sendable, Equatable {
    let from: String?
    let to: String?
    let type: String?
    let status: String?
    let show: String?
    let hats: [XMPPPresenceHat]
    let mucAffiliation: XMPPMucAffiliation?
    let mucRole: XMPPMucRole?
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

struct XMPPDeliveryAttemptRef: Sendable, Equatable, Hashable {
    let id: String
    let connectionGeneration: UInt64
}

struct XMPPDeliverySignal: Sendable, Equatable {
    let attempt: XMPPDeliveryAttemptRef
    let stanzaID: String
}

enum XMPPSessionReadyKind: Sendable, Equatable {
    case fresh
    case resumed
}

enum XMPPSessionBootstrapPlan: Sendable, Equatable {
    case establishFreshSession
    case preserveResumedSession
}

extension XMPPSessionReadyKind {
    var bootstrapPlan: XMPPSessionBootstrapPlan {
        switch self {
        case .fresh:
            return .establishFreshSession
        case .resumed:
            return .preserveResumedSession
        }
    }
}

enum XMPPSaslCondition: Sendable, Equatable {
    case aborted
    case accountDisabled
    case credentialsExpired
    case encryptionRequired
    case incorrectEncoding
    case invalidAuthzid
    case invalidMechanism
    case malformedRequest
    case mechanismTooWeak
    case notAuthorized
    case temporaryAuthFailure
    case unknown
}

enum XMPPSaslRetryDisposition: Sendable, Equatable {
    case retry
    case stopCredential
    case stopConfiguration
    case stopAborted
    case stopUnknown
}

struct StoppedXMPPAuthentication: Sendable, Equatable {
    let loginGeneration: UInt64
    let condition: XMPPSaslCondition
    let disposition: XMPPSaslRetryDisposition
}

struct XMPPConnectionAdmission: Sendable, Equatable {
    private(set) var generation: UInt64 = 0
    private(set) var isOpen = false

    @discardableResult
    mutating func open() -> UInt64 {
        generation &+= 1
        isOpen = true
        return generation
    }

    mutating func close() {
        generation &+= 1
        isOpen = false
    }

    func admits(generation candidate: UInt64) -> Bool {
        isOpen && generation == candidate
    }
}

extension XMPPSaslCondition {
    var retryDisposition: XMPPSaslRetryDisposition {
        switch self {
        case .temporaryAuthFailure:
            return .retry
        case .notAuthorized, .accountDisabled, .credentialsExpired, .invalidAuthzid:
            return .stopCredential
        case .invalidMechanism, .mechanismTooWeak, .encryptionRequired,
             .incorrectEncoding, .malformedRequest:
            return .stopConfiguration
        case .aborted:
            return .stopAborted
        case .unknown:
            return .stopUnknown
        }
    }
}

func updatedStoppedXMPPAuthentication(
    _ current: StoppedXMPPAuthentication?,
    loginGeneration: UInt64,
    condition: XMPPSaslCondition
) -> StoppedXMPPAuthentication? {
    let disposition = condition.retryDisposition
    guard disposition != .retry else { return current }
    guard current?.loginGeneration != loginGeneration else { return current }
    return StoppedXMPPAuthentication(
        loginGeneration: loginGeneration,
        condition: condition,
        disposition: disposition
    )
}

func xmppReconnectAllowed(
    admission: XMPPConnectionAdmission,
    stopped: StoppedXMPPAuthentication?
) -> Bool {
    admission.isOpen && stopped?.loginGeneration != admission.generation
}

enum XMPPEvent: Sendable, Equatable {
    case streamFeatures(XMPPStreamFeatures)
    case authenticated
    case authenticationFailed(XMPPSaslCondition)
    case resourceBound(jid: String)
    case sessionReady(kind: XMPPSessionReadyKind, attempt: XMPPDeliveryAttemptRef)
    case message(XMPPMessageEvent)
    case presence(XMPPPresenceEvent)
    case messageDeliveryAcked(XMPPDeliverySignal)
    case messageDeliveryFailed(XMPPDeliverySignal)
    case streamError(name: String, text: String?)
    case error(String)
    case disconnected
    /// XEP-0353 JMI or XEP-0166 Jingle session control event surfaced
    /// by the Rust client. Drives the ringing UI and the in-call HUD.
    case call(XMPPCallEvent)
}

// MARK: - A/V call event (XEP-0353 JMI + XEP-0166 Jingle)

/// Media kinds offered or accepted on a call.
struct XMPPCallMedia: Sendable, Equatable {
    let audio: Bool
    let video: Bool
}

/// LiveKit join credentials supplied by the server on
/// `session-initiate` / `session-accept` via the
/// `urn:waddle:transports:livekit:0` transport.
struct XMPPLiveKitJoin: Sendable, Equatable {
    let url: String
    let room: String
    let identity: String
    let token: String
}

/// XEP-0166 §7.4 session-terminate condition. Mirrors the 17
/// variants of `xmpp_parsers::jingle::Reason` (carried through the
/// FFI as a typed enum, never a raw string).
enum XMPPJingleReason: Sendable, Equatable {
    case alternativeSession
    case busy
    case cancel
    case connectivityError
    case decline
    case expired
    case failedApplication
    case failedTransport
    case generalError
    case gone
    case incompatibleParameters
    case mediaError
    case securityError
    case success
    case timeout
    case unsupportedApplications
    case unsupportedTransports
}

/// Inbound A/V call event variants. Mirrors `WaddleCallEventKind`
/// from the Rust FFI 1:1 so the AppModel can pattern-match without
/// importing UniFFI types into its own surface.
enum XMPPCallEventKind: Sendable, Equatable {
    case propose(media: XMPPCallMedia)
    case ringing
    case proceed
    case reject(reason: XMPPJingleReason?, tieBreak: Bool)
    case retract(reason: XMPPJingleReason?, tieBreak: Bool)
    case finish(reason: XMPPJingleReason?, migratedTo: String?)
    case sessionInitiate(join: XMPPLiveKitJoin, media: XMPPCallMedia)
    case sessionAccept(join: XMPPLiveKitJoin, media: XMPPCallMedia)
    case sessionTerminate(reason: XMPPJingleReason?)
}

struct XMPPCallEvent: Sendable, Equatable {
    /// Sender JID stamped by the server. A *full* JID for
    /// propose / session-initiate (XEP-0353 §0.6) so responses can
    /// be addressed back to the originating resource.
    let from: String
    /// Stanza recipient stamped by the parser when available. This
    /// lets multi-resource clients distinguish self-originated
    /// carbons without guessing from the sender alone.
    let to: String?
    /// Jingle session id correlating every stanza for this call.
    let sid: String
    let kind: XMPPCallEventKind
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
    let callEvent: XMPPCallEvent?
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
    XMPPJID(string: fullJID)?.bare ?? fullJID
}

func jidDomain(_ jid: String) -> String {
    XMPPJID(string: jid)?.domain ?? jid
}

func parseManagedRoomBareJID(_ roomJID: String) -> String? {
    XMPPJID(string: roomJID)?.localpart
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
