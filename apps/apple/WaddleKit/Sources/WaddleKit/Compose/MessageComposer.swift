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
    public var mentions: [MentionDraft]
    public var reply: ReplyContext?
    public var thread: String?
    public var attachments: [SharedFile]

    public init(
        text: String,
        mentions: [MentionDraft] = [],
        reply: ReplyContext? = nil,
        thread: String? = nil,
        attachments: [SharedFile] = []
    ) {
        self.text = text
        self.mentions = mentions
        self.reply = reply
        self.thread = thread
        self.attachments = attachments
    }
}

/// Turns a draft into the wire body and typed send options. Pure.
public enum MessageComposer {
    public static func compose(_ draft: Draft) -> (body: String, options: OutboundOptions)? {
        let (trimmed, leading) = trim(draft.text)
        let mentions = draft.mentions.compactMap { shift($0, by: -leading, within: trimmed.unicodeScalars.count) }
        let markdown = ComposerMarkdown.compose(trimmed, mentions: mentions)
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
            thread: draft.thread,
            markupSpans: markdown.spans.map { MarkupSpan(kind: $0.kind, start: $0.start + offset, end: $0.end + offset) },
            references: markdown.mentions.map {
                Reference(kind: .mention, uri: $0.target.uri, begin: $0.range.lowerBound + offset, end: $0.range.upperBound + offset)
            },
            sharedFiles: draft.attachments,
            requestDisplayedMarker: true
        )
        return (body, options)
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

    private static func shift(_ mention: MentionDraft, by delta: Int, within length: Int) -> MentionDraft? {
        let lower = max(0, mention.range.lowerBound + delta)
        let upper = min(length, mention.range.upperBound + delta)
        guard lower < upper else { return nil }
        var shifted = mention
        shifted.range = lower..<upper
        return shifted
    }
}
