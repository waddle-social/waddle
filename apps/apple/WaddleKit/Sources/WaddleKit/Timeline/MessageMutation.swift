import Foundation

/// A stanza that changes an existing row instead of inserting one.
/// Extraction precedence follows destructiveness: moderation and
/// retraction are terminal, a correction replaces content, a reaction only
/// annotates.
public enum MessageMutation: Hashable, Sendable {
    /// XEP-0444: `emojis` is the sender's complete current set and replaces
    /// their previous one (empty clears).
    case reaction(targetID: String, from: JID, senderKey: String, isMine: Bool, emojis: [String])
    /// XEP-0308: only the original author may correct.
    case correction(targetID: String, from: JID, body: String)
    /// XEP-0424: only the original author may retract.
    case retraction(targetID: String, from: JID)
    /// XEP-0425: only the room itself may moderate.
    case moderation(targetID: String, from: JID, moderatedBy: String?, reason: String?)

    public var targetID: String {
        switch self {
        case let .reaction(targetID, _, _, _, _),
             let .correction(targetID, _, _),
             let .retraction(targetID, _),
             let .moderation(targetID, _, _, _):
            return targetID
        }
    }

    public var from: JID {
        switch self {
        case let .reaction(_, from, _, _, _),
             let .correction(_, from, _),
             let .retraction(_, from),
             let .moderation(_, from, _, _):
            return from
        }
    }

    /// Corrections and retractions may only touch the sender's own rows.
    var isSenderScoped: Bool {
        switch self {
        case .correction, .retraction: return true
        case .reaction, .moderation: return false
        }
    }

    /// Extracts the mutation a stanza carries, if any.
    public static func of(_ message: WireMessage, isMine: Bool) -> MessageMutation? {
        guard let from = message.from else { return nil }
        let isGroupchat = message.isGroupchat
        // XEP-0425 is a MUC feature. A 1:1 peer claiming moderation is
        // ignored outright.
        if let moderation = message.moderation, isGroupchat {
            return .moderation(
                targetID: moderation.targetID,
                from: from,
                moderatedBy: moderation.moderatedBy,
                reason: moderation.reason
            )
        }
        if let retractsID = message.retractsID {
            return .retraction(targetID: retractsID, from: from)
        }
        if let replacesID = message.replacesID, let body = message.body {
            // A correction of a reply re-sends the fallback quote; strip it
            // like the insert path or the quote renders twice.
            return .correction(
                targetID: replacesID,
                from: from,
                body: ReplyFallback.strip(body, range: message.reply?.fallback)
            )
        }
        if let reaction = message.reaction {
            return .reaction(
                targetID: reaction.targetID,
                from: from,
                senderKey: isGroupchat ? from.description : from.bare.description,
                isMine: isMine,
                emojis: reaction.emojis
            )
        }
        return nil
    }
}

/// XEP-0424/0308 author check: in a room the full occupant JID must match
/// (another occupant may not rewrite history); in 1:1 any resource of the
/// same account may.
func isSameAuthor(_ mutationFrom: JID, _ originalFrom: JID?, isGroupchat: Bool) -> Bool {
    guard let originalFrom else { return false }
    if isGroupchat {
        return mutationFrom == originalFrom
    }
    return mutationFrom.bare == originalFrom.bare
}
