import Foundation

/// The message a reply quotes.
public struct ReplyContext: Hashable, Sendable {
    /// `TimelineItem.actionTargetID` of the parent.
    public let targetID: String
    /// `TimelineItem.replyAuthor` of the parent.
    public let author: JID
    /// The parent's displayed body, quoted in the XEP-0428 fallback.
    public let parentBody: String
    public let parentAuthorName: String

    public init(targetID: String, author: JID, parentBody: String, parentAuthorName: String) {
        self.targetID = targetID
        self.author = author
        self.parentBody = parentBody
        self.parentAuthorName = parentAuthorName
    }

    /// The reply context for `item`, or nil when the row has no id other
    /// clients can target.
    public init?(replyingTo item: TimelineItem) {
        guard let targetID = item.actionTargetID, let author = item.replyAuthor else { return nil }
        self.init(targetID: targetID, author: author, parentBody: item.body, parentAuthorName: item.authorName)
    }
}

/// What the user composed.
public struct Draft: Hashable, Sendable {
    public var text: String
    public var references: DraftReferences
    public var reply: ReplyContext?
    public var thread: String?
    public var attachments: [SharedFile]

    public init(
        text: String,
        references: DraftReferences = .init(),
        reply: ReplyContext? = nil,
        thread: String? = nil,
        attachments: [SharedFile] = []
    ) {
        self.text = text
        self.references = references
        self.reply = reply
        self.thread = thread
        self.attachments = attachments
    }
}

/// Turns a draft into the wire body and typed send options. Pure.
public enum MessageComposer {
    public static func compose(_ draft: Draft) -> (body: String, options: OutboundOptions)? {
        let (trimmed, leading) = trim(draft.text)
        let length = trimmed.unicodeScalars.count
        let references = draft.references.shifted(by: -leading, within: length)
        let markdown = ComposerMarkdown.compose(trimmed, references: references)
        var body = markdown.body
        // An attachment-only send carries the first file URL as its body
        // so clients without XEP-0447 still get a link.
        if body.isEmpty {
            guard let first = draft.attachments.first else { return nil }
            body = first.url.absoluteString
        }

        var fallback: Range<Int>?
        if let reply = draft.reply, let quote = ReplyFallback.quote(parentBody: reply.parentBody) {
            body = quote.prefix + body
            fallback = quote.range
        }
        let offset = fallback?.upperBound ?? 0
        let options = OutboundOptions(
            reply: draft.reply.map { OutboundOptions.Reply(targetID: $0.targetID, author: $0.author, fallback: fallback) },
            thread: draft.thread.map { ThreadTarget(id: $0) },
            markupSpans: markdown.spans.map { MarkupSpan(kind: $0.kind, start: $0.start + offset, end: $0.end + offset) },
            references: markdown.references.wireReferences(offset: offset),
            sharedFiles: draft.attachments,
            requestDisplayedMarker: true
        )
        return (body, options)
    }

    /// A sticker message: the description as its text, taken as it is
    /// (no markdown), and the picture as its one file. Nil for a sticker
    /// without a picture to point at.
    public static func compose(
        _ sticker: Sticker,
        from pack: StickerPack,
        reply: ReplyContext? = nil,
        thread: String? = nil
    ) -> (body: String, options: OutboundOptions)? {
        guard let file = sticker.file else { return nil }
        let quote = reply.flatMap { ReplyFallback.quote(parentBody: $0.parentBody) }
        let options = OutboundOptions(
            reply: reply.map { OutboundOptions.Reply(targetID: $0.targetID, author: $0.author, fallback: quote?.range) },
            thread: thread.map { ThreadTarget(id: $0) },
            sharedFiles: [file],
            requestDisplayedMarker: true,
            sticker: StickerReference(packID: pack.id)
        )
        return ((quote?.prefix ?? "") + sticker.text, options)
    }

    /// Trims surrounding whitespace; returns the trimmed text and how many
    /// leading scalars were dropped.
    static func trim(_ text: String) -> (String, Int) {
        let scalars = Array(text.unicodeScalars)
        guard let first = scalars.firstIndex(where: { !CharacterSet.whitespacesAndNewlines.contains($0) }) else {
            return ("", 0)
        }
        let last = scalars.lastIndex(where: { !CharacterSet.whitespacesAndNewlines.contains($0) })!
        var view = String.UnicodeScalarView()
        view.append(contentsOf: scalars[first...last])
        return (String(view), first)
    }
}
