import Foundation

enum ChatDeliveryState: Hashable {
    case pending
    case sending
    case sent
    case delivered
    case read
    case failed(reason: String? = nil)

    var label: String {
        switch self {
        case .pending:
            return "Pending"
        case .sending:
            return "Sending"
        case .sent:
            return "Sent"
        case .delivered:
            return "Delivered"
        case .read:
            return "Read"
        case .failed(let reason):
            return reason?.isEmpty == false ? reason! : "Failed"
        }
    }
}

enum ChatPresenceState: Hashable {
    case available
    case away
    case dnd
    case offline
    case unknown(String)

    var label: String {
        switch self {
        case .available:
            return "Available"
        case .away:
            return "Away"
        case .dnd:
            return "Do not disturb"
        case .offline:
            return "Offline"
        case .unknown(let value):
            return value
        }
    }
}

struct ChatRoomSelection: Identifiable, Hashable {
    let id: String
    var title: String
    var subtitle: String?
    var unreadCount: Int
    var isMuted: Bool
    var lastActivityAt: Date?
}

struct ChatRoomMember: Identifiable, Hashable {
    let id: String
    var displayName: String
    var presence: ChatPresenceState
    var isSelf: Bool
    var role: String?
    var affiliation: String?
    var avatarInitials: String?
}

struct ChatTimelineMessage: Identifiable, Hashable {
    let id: String
    var roomID: String
    var senderID: String
    var senderDisplayName: String
    var body: String
    var sentAt: Date
    var editedAt: Date?
    var deliveryState: ChatDeliveryState
    var isOutgoing: Bool
    var isAction: Bool
    var senderInitials: String?
    var reactions: [String: [String]]?
    var isRetracted: Bool
    var replyToID: String?
    var replyToSenderName: String?
    var replyToBody: String?
    var markupSpans: [XMPPMarkupSpan]?
}

struct ChatRoomHistoryState: Hashable {
    var roomID: String?
    var isLoadingInitialHistory: Bool
    var isLoadingOlderMessages: Bool
    var hasMoreOlderMessages: Bool
    var oldestLoadedAt: Date?
    var newestLoadedAt: Date?
    var loadedMessageCount: Int
    var errorMessage: String?

    init(
        roomID: String? = nil,
        isLoadingInitialHistory: Bool = false,
        isLoadingOlderMessages: Bool = false,
        hasMoreOlderMessages: Bool = false,
        oldestLoadedAt: Date? = nil,
        newestLoadedAt: Date? = nil,
        loadedMessageCount: Int = 0,
        errorMessage: String? = nil
    ) {
        self.roomID = roomID
        self.isLoadingInitialHistory = isLoadingInitialHistory
        self.isLoadingOlderMessages = isLoadingOlderMessages
        self.hasMoreOlderMessages = hasMoreOlderMessages
        self.oldestLoadedAt = oldestLoadedAt
        self.newestLoadedAt = newestLoadedAt
        self.loadedMessageCount = loadedMessageCount
        self.errorMessage = errorMessage
    }

    var canLoadOlderMessages: Bool {
        roomID != nil && hasMoreOlderMessages && !isLoadingInitialHistory && !isLoadingOlderMessages
    }

    mutating func reset(for roomID: String?) {
        self.roomID = roomID
        isLoadingInitialHistory = false
        isLoadingOlderMessages = false
        hasMoreOlderMessages = false
        oldestLoadedAt = nil
        newestLoadedAt = nil
        loadedMessageCount = 0
        errorMessage = nil
    }

    mutating func beginInitialLoad() {
        isLoadingInitialHistory = true
        isLoadingOlderMessages = false
        errorMessage = nil
    }

    mutating func finishInitialLoad(
        loadedMessageCount: Int,
        oldestLoadedAt: Date?,
        newestLoadedAt: Date?,
        hasMoreOlderMessages: Bool
    ) {
        isLoadingInitialHistory = false
        isLoadingOlderMessages = false
        self.loadedMessageCount = loadedMessageCount
        self.oldestLoadedAt = oldestLoadedAt
        self.newestLoadedAt = newestLoadedAt
        self.hasMoreOlderMessages = hasMoreOlderMessages
        errorMessage = nil
    }

    mutating func beginOlderLoad() {
        isLoadingOlderMessages = true
        errorMessage = nil
    }

    mutating func finishOlderLoad(
        loadedMessageCount: Int,
        oldestLoadedAt: Date?,
        newestLoadedAt: Date?,
        hasMoreOlderMessages: Bool
    ) {
        isLoadingInitialHistory = false
        isLoadingOlderMessages = false
        self.loadedMessageCount = loadedMessageCount
        self.oldestLoadedAt = oldestLoadedAt
        self.newestLoadedAt = newestLoadedAt
        self.hasMoreOlderMessages = hasMoreOlderMessages
        errorMessage = nil
    }

    mutating func fail(_ message: String) {
        isLoadingInitialHistory = false
        isLoadingOlderMessages = false
        errorMessage = message
    }
}

struct ChatRoomHistoryPage {
    var messages: [ChatTimelineMessage]
    var hasMoreOlderMessages: Bool

    init(messages: [ChatTimelineMessage], hasMoreOlderMessages: Bool) {
        self.messages = messages
        self.hasMoreOlderMessages = hasMoreOlderMessages
    }

    var oldestLoadedAt: Date? {
        messages.first?.sentAt
    }

    var newestLoadedAt: Date? {
        messages.last?.sentAt
    }
}

extension ChatTimelineMessage {
    var styledBody: AttributedString {
        guard let spans = markupSpans, !spans.isEmpty, !body.isEmpty else {
            return AttributedString(body)
        }

        var attributed = AttributedString(body)
        let utf8View = body.utf8

        for span in spans {
            guard span.start >= 0, span.end <= utf8View.count, span.start < span.end else { continue }
            guard let startIndex = body.utf8.index(body.startIndex, offsetBy: span.start, limitedBy: body.endIndex),
                  let endIndex = body.utf8.index(body.startIndex, offsetBy: span.end, limitedBy: body.endIndex) else {
                continue
            }
            let startAttr = AttributedString.Index(startIndex, within: attributed)
            let endAttr = AttributedString.Index(endIndex, within: attributed)
            guard let startAttr, let endAttr, startAttr < endAttr else { continue }

            switch span.type {
            case .bold:
                attributed[startAttr..<endAttr].inlinePresentationIntent = .stronglyEmphasized
            case .italic:
                attributed[startAttr..<endAttr].inlinePresentationIntent = .emphasized
            case .strikethrough:
                attributed[startAttr..<endAttr].strikethroughStyle = .single
            case .code:
                attributed[startAttr..<endAttr].inlinePresentationIntent = .code
                attributed[startAttr..<endAttr].backgroundColor = .secondary.opacity(0.12)
            case .codeBlock:
                attributed[startAttr..<endAttr].inlinePresentationIntent = .code
                attributed[startAttr..<endAttr].backgroundColor = .secondary.opacity(0.12)
            case .blockquote:
                attributed[startAttr..<endAttr].inlinePresentationIntent = .emphasized
            case .link:
                if let uri = span.uri, let url = URL(string: uri) {
                    attributed[startAttr..<endAttr].link = url
                }
            }
        }
        return attributed
    }

    var timelineSortDate: Date {
        editedAt ?? sentAt
    }

    func formsCompactCluster(with previous: ChatTimelineMessage?) -> Bool {
        guard let previous else { return false }
        guard senderID == previous.senderID,
              roomID == previous.roomID,
              isOutgoing == previous.isOutgoing,
              !isAction,
              !previous.isAction,
              Calendar.current.isDate(sentAt, inSameDayAs: previous.sentAt) else {
            return false
        }

        return sentAt.timeIntervalSince(previous.sentAt) < 5 * 60
    }

    func startsTimelineDay(after previous: ChatTimelineMessage?) -> Bool {
        guard let previous else { return true }
        return !Calendar.current.isDate(sentAt, inSameDayAs: previous.sentAt)
    }

    func merged(with other: ChatTimelineMessage) -> ChatTimelineMessage {
        let isRetracted = isRetracted || other.isRetracted
        let mergedBody: String
        if isRetracted {
            mergedBody = ""
        } else if other.body.isEmpty {
            mergedBody = body
        } else {
            mergedBody = other.body
        }

        return ChatTimelineMessage(
            id: id,
            roomID: other.roomID.isEmpty ? roomID : other.roomID,
            senderID: other.senderID.isEmpty ? senderID : other.senderID,
            senderDisplayName: other.senderDisplayName.isEmpty ? senderDisplayName : other.senderDisplayName,
            body: mergedBody,
            sentAt: min(sentAt, other.sentAt),
            editedAt: maxDate(editedAt, other.editedAt),
            deliveryState: mergedDeliveryState(with: other.deliveryState),
            isOutgoing: isOutgoing || other.isOutgoing,
            isAction: isAction || other.isAction,
            senderInitials: other.senderInitials ?? senderInitials,
            reactions: mergedReactions(existing: reactions, incoming: other.reactions),
            isRetracted: isRetracted,
            replyToID: other.replyToID ?? replyToID,
            replyToSenderName: other.replyToSenderName ?? replyToSenderName,
            replyToBody: other.replyToBody ?? replyToBody,
            markupSpans: other.markupSpans ?? markupSpans
        )
    }

    private func maxDate(_ lhs: Date?, _ rhs: Date?) -> Date? {
        switch (lhs, rhs) {
        case let (lhs?, rhs?):
            return max(lhs, rhs)
        case let (lhs?, nil):
            return lhs
        case let (nil, rhs?):
            return rhs
        case (nil, nil):
            return nil
        }
    }

    private func mergedDeliveryState(with other: ChatDeliveryState) -> ChatDeliveryState {
        if deliveryState == other {
            return deliveryState
        }

        switch (deliveryState, other) {
        case (.failed, let next):
            return next
        case (let current, .failed):
            return current
        default:
            return deliveryPriority(other) >= deliveryPriority(deliveryState) ? other : deliveryState
        }
    }

    private func deliveryPriority(_ state: ChatDeliveryState) -> Int {
        switch state {
        case .pending:
            return 0
        case .sending:
            return 1
        case .sent:
            return 2
        case .delivered:
            return 3
        case .read:
            return 4
        case .failed:
            return -1
        }
    }

    private func mergedReactions(
        existing: [String: [String]]?,
        incoming: [String: [String]]?
    ) -> [String: [String]]? {
        guard existing != nil || incoming != nil else {
            return nil
        }

        var merged = existing ?? [:]
        for (emoji, senders) in incoming ?? [:] {
            var combined = merged[emoji] ?? []
            for sender in senders where !combined.contains(sender) {
                combined.append(sender)
            }
            merged[emoji] = combined
        }

        return merged.isEmpty ? nil : merged
    }
}

extension Array where Element == ChatTimelineMessage {
    func mergedTimelineMessages(_ incoming: [ChatTimelineMessage]) -> [ChatTimelineMessage] {
        var mergedByID: [String: ChatTimelineMessage] = [:]

        for message in self {
            mergedByID[message.id] = message
        }

        for message in incoming {
            if let existing = mergedByID[message.id] {
                mergedByID[message.id] = existing.merged(with: message)
            } else {
                mergedByID[message.id] = message
            }
        }

        return mergedByID.values.sorted {
            if $0.sentAt == $1.sentAt {
                return $0.id < $1.id
            }
            return $0.sentAt < $1.sentAt
        }
    }

    func appendingTimelineMessages(_ incoming: [ChatTimelineMessage]) -> [ChatTimelineMessage] {
        mergedTimelineMessages(incoming)
    }

    static func mergedArchiveAndLive(archive: [ChatTimelineMessage], live: [ChatTimelineMessage]) -> [ChatTimelineMessage] {
        archive.mergedTimelineMessages(live)
    }
}

enum ChatConnectionBannerState: Hashable {
    case hidden
    case connecting(message: String = "Connecting…")
    case connected(message: String = "Connected")
    case reconnecting(message: String = "Reconnecting…")
    case disconnected(message: String)
    case error(message: String)

    var isVisible: Bool {
        if case .hidden = self {
            return false
        }
        return true
    }

    var message: String {
        switch self {
        case .hidden:
            return ""
        case .connecting(let message),
             .connected(let message),
             .reconnecting(let message),
             .disconnected(let message),
             .error(let message):
            return message
        }
    }

    var symbolName: String {
        switch self {
        case .hidden, .connected:
            return "checkmark.circle.fill"
        case .connecting, .reconnecting:
            return "arrow.triangle.2.circlepath"
        case .disconnected:
            return "wifi.exclamationmark"
        case .error:
            return "exclamationmark.triangle.fill"
        }
    }
}
