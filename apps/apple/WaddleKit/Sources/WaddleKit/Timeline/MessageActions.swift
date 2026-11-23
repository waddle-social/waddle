import Foundation

/// Something one can do to a message from its row: the hover toolbar, the
/// row's menu and VoiceOver all offer these, by these names.
public enum MessageAction: Hashable, Sendable {
    case react(String)
    case addReaction
    /// A XEP-0461 reply, quoting the message.
    case reply
    /// Opens the thread the message starts (inside a thread, the reply's
    /// sub-thread), which has no messages yet.
    case startThread
    /// Opens that thread, which has messages.
    case openThread
    case pin
    case unpin
    /// Makes the message's words selectable (a Mac shows it).
    case selectText
    case edit
    case delete

    public var title: String {
        switch self {
        case let .react(emoji): "React with \(emoji)"
        case .addReaction: "Add reaction"
        case .reply: "Reply"
        case .startThread: "Reply in thread"
        case .openThread: "Open thread"
        case .pin: "Pin to room"
        case .unpin: "Unpin from room"
        case .selectText: "Select text"
        case .edit: "Edit"
        case .delete: "Delete"
        }
    }

    public var accessibilityLabel: String {
        if case let .react(emoji) = self { return "React to message with \(emoji)" }
        return title
    }

    /// Edit and delete: only on one's own message.
    public var isOwn: Bool { self == .edit || self == .delete }
}

/// Whether the account may pin in a conversation. Only in a room it is in:
/// owners and admins always may, anyone else where the room lets anyone
/// pin, which its disco#info says. A 1:1 conversation has no pins.
public enum PinAccess: Hashable, Sendable {
    case allowed
    case unknown
    case denied
}

/// What a row's actions depend on besides the message.
public struct MessageActionContext: Hashable, Sendable {
    /// The thread the row is shown in; nil in the main feed.
    public var thread: String?
    /// Messages in the thread the thread action opens.
    public var replyCount: Int
    public var isPinned: Bool
    public var pinning: PinAccess

    public init(thread: String?, replyCount: Int, isPinned: Bool, pinning: PinAccess) {
        self.thread = thread
        self.replyCount = replyCount
        self.isPinned = isPinned
        self.pinning = pinning
    }
}

public enum MessageActions {
    /// Web's quick reactions, on the toolbar and the menu.
    public static let quickReactions = ["👍", "❤️", "😂", "🎉", "👀"]

    /// Every action the row offers, in web's toolbar order: quick
    /// reactions, add reaction, reply, the thread, pin, select text, then
    /// edit and delete for one's own message. A deleted message has none.
    public static func available(for item: TimelineItem, in context: MessageActionContext) -> [MessageAction] {
        guard item.tombstone == nil else { return [] }
        let canTarget = item.actionTargetID != nil
        var actions: [MessageAction] = []
        if canTarget {
            actions += quickReactions.map(MessageAction.react)
            actions += [.addReaction, .reply]
        }
        if item.threadTarget(inside: context.thread) != nil {
            actions.append(context.replyCount > 0 ? .openThread : .startThread)
        }
        if item.conversation.isRoom, canTarget, context.pinning != .denied {
            actions.append(context.isPinned ? .unpin : .pin)
        }
        if hasWords(item) {
            actions.append(.selectText)
        }
        if item.isMine {
            if item.correctionTargetID != nil, !item.isLocalEcho, !item.message.isSticker {
                actions.append(.edit)
            }
            if item.retractionTargetID != nil {
                actions.append(.delete)
            }
        }
        return actions
    }

    /// The hover toolbar's share: none while the message is being edited
    /// (as on web), no select text, pin only where surely allowed, and
    /// quick reactions dropped from the end until `maxButtons` fit.
    public static func toolbar(
        for item: TimelineItem,
        in context: MessageActionContext,
        isBeingEdited: Bool,
        maxButtons: Int
    ) -> [MessageAction] {
        guard !isBeingEdited else { return [] }
        var actions = available(for: item, in: context).filter { action in
            switch action {
            case .selectText: false
            case .pin, .unpin: context.pinning == .allowed
            default: true
            }
        }
        while actions.count > maxButtons, let last = actions.lastIndex(where: { if case .react = $0 { true } else { false } }) {
            actions.remove(at: last)
        }
        return actions
    }

    /// Text or a `/me` line to select, not only a picture or a file.
    private static func hasWords(_ item: TimelineItem) -> Bool {
        switch ThreadSummary.snippet(of: item) {
        case let .text(text), let .action(text): !text.isEmpty
        default: false
        }
    }
}
