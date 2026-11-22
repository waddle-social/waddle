import Foundation

/// A button on a message row's hover toolbar.
public enum MessageToolbarAction: Hashable, Sendable {
    case react(String)
    case addReaction
    case reply
    /// "Open thread" when the thread (inside a thread, the reply's
    /// sub-thread) has messages, else "Reply in thread".
    case thread(opens: Bool)
    case pin(isPinned: Bool)
    case edit
    case delete
}

/// What a message row's hover toolbar offers, in web's order (quick
/// reactions, add reaction, reply, thread, pin, then edit and delete for
/// one's own message), under the row context menu's conditions.
public enum MessageToolbar {
    /// `inThread` is the thread the row is shown in, nil for the main
    /// feed; `replyCount` counts the thread the thread button opens.
    public static func actions(
        for item: TimelineItem,
        inThread: String?,
        replyCount: Int,
        isPinned: Bool,
        quickReactions: [String]
    ) -> [MessageToolbarAction] {
        guard item.tombstone == nil else { return [] }
        let canTarget = item.actionTargetID != nil
        var actions: [MessageToolbarAction] = []
        if canTarget {
            actions += quickReactions.map(MessageToolbarAction.react)
            actions += [.addReaction, .reply]
        }
        if item.threadTarget(inside: inThread) != nil {
            actions.append(.thread(opens: replyCount > 0))
        }
        if item.conversation.isRoom, canTarget {
            actions.append(.pin(isPinned: isPinned))
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
}
