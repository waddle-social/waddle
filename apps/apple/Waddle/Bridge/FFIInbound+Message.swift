import Foundation
import WaddleKit

/// Fields `WaddleMessage` and `WaddleArchivedMessage` share, so live
/// stanzas and archive rows go through one conversion.
protocol FFIMessageFields {
    var id: String? { get }
    var from: String? { get }
    var to: String? { get }
    var body: String? { get }
    var subject: String? { get }
    var messageType: String { get }
    var timestamp: String? { get }
    var stanzaId: String? { get }
    var stanzaIdBy: String? { get }
    var stanzaIds: [WaddleStanzaId] { get }
    var originId: String? { get }
    var replacesId: String? { get }
    var retractsId: String? { get }
    var isRetracted: Bool { get }
    var moderationTargetId: String? { get }
    var moderatedBy: String? { get }
    var moderationReason: String? { get }
    var reactionTargetId: String? { get }
    var reactionEmojis: [String] { get }
    var thread: String? { get }
    var parentThreadId: String? { get }
    var replyToId: String? { get }
    var replyToSender: String? { get }
    var replyFallbackStart: UInt32? { get }
    var replyFallbackEnd: UInt32? { get }
    var markupSpans: [WaddleMarkupSpan] { get }
    var broadcastMention: String? { get }
    var references: [WaddleReference] { get }
    var forumPostKind: WaddleForumPostKind? { get }
    var forumTitle: String? { get }
    var isSticker: Bool { get }
    var sharedFiles: [WaddleSharedFile] { get }
    var linkPreviews: [WaddleLinkPreview] { get }
}

extension WaddleMessage: FFIMessageFields {}
extension WaddleArchivedMessage: FFIMessageFields {}

extension FFIInbound {
    /// A live stanza, with the live-only payloads (chat state, markers,
    /// pin broadcasts, XEP-0490 sibling cursors).
    static func wireMessage(_ message: WaddleMessage) -> WireMessage? {
        guard var wire = wireMessage(fields: message, source: .live) else { return nil }
        wire.chatState = message.chatState.map(chatState)
        wire.displayedMarkerRequested = message.displayedMarkerRequested
        wire.displayedMarkerID = message.displayedMarkerId
        wire.pinEvent = message.pinEvent.map(pinEvent)
        wire.displayedCursors = message.mdsDisplayed.map { $0.compactMap(displayedCursor) }
        return wire
    }

    /// A XEP-0313 row. Rows carrying call signalling are dropped: calls
    /// are not part of the timeline surface.
    static func wireMessage(_ archived: WaddleArchivedMessage) -> WireMessage? {
        guard archived.callEvent == nil else { return nil }
        return wireMessage(fields: archived, source: .archive(mamID: archived.mamId))
    }

    static func archivePage(_ page: WaddleMamPage) -> ArchivePage {
        ArchivePage(
            messages: page.messages.compactMap { wireMessage($0) },
            first: page.firstId,
            isComplete: page.isComplete
        )
    }

    /// Nil when a sender is present but unparseable: an absent `from`
    /// means the account's own server (RFC 6120 §8.1.2.1), so it must not
    /// stand in for one we could not read.
    static func wireMessage(fields: some FFIMessageFields, source: WireMessage.Source) -> WireMessage? {
        let from = jid(fields.from)
        guard fields.from == nil || from != nil else { return nil }
        return WireMessage(
            source: source,
            type: MessageType(wire: fields.messageType),
            from: from,
            to: jid(fields.to),
            identity: identity(fields),
            timestamp: date(fields.timestamp),
            body: fields.body,
            subject: fields.subject,
            replacesID: fields.replacesId,
            retractsID: fields.retractsId,
            isRetracted: fields.isRetracted,
            moderation: moderation(fields),
            reaction: reaction(fields),
            thread: fields.thread,
            parentThread: fields.parentThreadId,
            reply: replyTarget(fields),
            markupSpans: fields.markupSpans.compactMap(markupSpan),
            references: fields.references.map(reference),
            broadcastMention: fields.broadcastMention,
            forumPostKind: fields.forumPostKind.map(forumPostKind),
            forumTitle: fields.forumTitle,
            isSticker: fields.isSticker,
            sharedFiles: fields.sharedFiles.compactMap(sharedFile),
            linkPreviews: fields.linkPreviews.compactMap(linkPreview)
        )
    }

    /// XEP-0359 ids. `by` is kept unverified; WaddleKit checks it against
    /// the expected authority before trusting an id.
    static func identity(_ fields: some FFIMessageFields) -> MessageIdentity {
        MessageIdentity(
            messageID: fields.id,
            originID: fields.originId,
            stanzaID: stanzaID(id: fields.stanzaId, by: fields.stanzaIdBy),
            stanzaIDs: fields.stanzaIds.compactMap { stanzaID(id: $0.id, by: $0.by) }
        )
    }

    static func stanzaID(id: String?, by: String?) -> StanzaID? {
        guard let id, let authority = jid(by)?.bare else { return nil }
        return StanzaID(id: id, by: authority)
    }

    static func moderation(_ fields: some FFIMessageFields) -> WireMessage.Moderation? {
        fields.moderationTargetId.map {
            WireMessage.Moderation(targetID: $0, moderatedBy: fields.moderatedBy, reason: fields.moderationReason)
        }
    }

    /// XEP-0444: an empty emoji set is a clear, so only the target decides.
    static func reaction(_ fields: some FFIMessageFields) -> WireMessage.Reaction? {
        fields.reactionTargetId.map { WireMessage.Reaction(targetID: $0, emojis: fields.reactionEmojis) }
    }

    static func replyTarget(_ fields: some FFIMessageFields) -> WireMessage.ReplyTarget? {
        fields.replyToId.map {
            WireMessage.ReplyTarget(
                id: $0,
                author: jid(fields.replyToSender),
                fallback: range(start: fields.replyFallbackStart, end: fields.replyFallbackEnd)
            )
        }
    }
}
