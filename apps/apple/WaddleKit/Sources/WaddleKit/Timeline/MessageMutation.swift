import Foundation

/// A stanza that changes an existing row instead of inserting one.
/// Extraction precedence follows destructiveness: moderation and
/// retraction are terminal, a correction replaces content, reactions and
/// safety scores only annotate.
public enum MessageMutation: Hashable, Sendable {
    /// XEP-0444: `emojis` is the sender's complete current set and replaces
    /// their previous one (empty clears).
    case reaction(targetID: String, from: JID, senderKey: String, isMine: Bool, emojis: [String])
    /// XEP-0308: only the original author may correct. The correction
    /// replaces the text and the payloads tied to its offsets.
    case correction(targetID: String, from: JID, content: CorrectedContent)
    /// XEP-0424: only the original author may retract.
    case retraction(targetID: String, from: JID)
    /// XEP-0425: only the room itself may moderate.
    case moderation(targetID: String, from: JID, moderatedBy: String?, reason: String?)
    /// XEP-0422 `urn:waddle:safety-scores:1`: only the room itself may set
    /// scores. They replace any earlier scores; nil clears them.
    case safetyScores(targetID: String, from: JID, scores: SafetyScores?)

    public var targetID: String {
        switch self {
        case let .reaction(targetID, _, _, _, _),
             let .correction(targetID, _, _),
             let .retraction(targetID, _),
             let .moderation(targetID, _, _, _),
             let .safetyScores(targetID, _, _):
            return targetID
        }
    }

    public var from: JID {
        switch self {
        case let .reaction(_, from, _, _, _),
             let .correction(_, from, _),
             let .retraction(_, from),
             let .moderation(_, from, _, _),
             let .safetyScores(_, from, _):
            return from
        }
    }

    /// Corrections and retractions may only touch the sender's own rows.
    var isSenderScoped: Bool {
        switch self {
        case .correction, .retraction: return true
        case .reaction, .moderation, .safetyScores: return false
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
        // Scores are a room annotation. In 1:1 no sender is defined as
        // trusted to set them, so a peer's claim is ignored outright.
        if let fastening = message.safetyScores, isGroupchat {
            return .safetyScores(targetID: fastening.targetID, from: from, scores: fastening.scores)
        }
        if let retractsID = message.retractsID {
            return .retraction(targetID: retractsID, from: from)
        }
        if let replacesID = message.replacesID, let body = message.body {
            return .correction(targetID: replacesID, from: from, content: CorrectedContent(wireBody: body, message: message))
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

/// What a XEP-0308 correction replaces. Markup and references carry
/// offsets over the correction's own wire body, so they are replaced
/// together with the text; keeping the original's would style the wrong
/// characters. Attachments are kept unless the correction carries its own,
/// because clients that re-send only the text are common.
public struct CorrectedContent: Hashable, Sendable {
    /// Display body: the reply fallback already removed.
    public let body: String
    public let markupSpans: [MarkupSpan]
    public let references: [Reference]
    /// The correction's reply fallback range, which the offsets above
    /// include.
    public let replyFallback: Range<Int>?
    public let sharedFiles: [SharedFile]

    public init(body: String, markupSpans: [MarkupSpan], references: [Reference], replyFallback: Range<Int>?, sharedFiles: [SharedFile]) {
        self.body = body
        self.markupSpans = markupSpans
        self.references = references
        self.replyFallback = replyFallback
        self.sharedFiles = sharedFiles
    }

    /// From a received correction stanza.
    init(wireBody: String, message: WireMessage) {
        self.init(
            body: ReplyFallback.strip(wireBody, range: message.reply?.fallback),
            markupSpans: message.markupSpans,
            references: message.references,
            replyFallback: message.reply?.fallback,
            sharedFiles: message.sharedFiles
        )
    }

    /// From a correction this client sends.
    init(body wireBody: String, options: OutboundOptions) {
        self.init(
            body: ReplyFallback.strip(wireBody, range: options.reply?.fallback),
            markupSpans: options.markupSpans,
            references: options.references,
            replyFallback: options.reply?.fallback,
            sharedFiles: options.sharedFiles
        )
    }
}
