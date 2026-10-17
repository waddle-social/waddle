import Foundation

/// What a XEP-0201 thread root shows about its replies: how many there
/// are, who wrote them and the newest one. Built on the client from the
/// replies that are loaded; nothing here travels on the wire.
public struct ThreadSummary: Hashable, Sendable {
    /// Faces shown before the overflow count.
    public static let maxFaces = 3
    /// Longest snippet kept; the row ellipsises long before this.
    static let maxSnippetLength = 200

    /// A reply author as the root shows them.
    public struct Participant: Hashable, Sendable {
        public let name: String
        /// The author's real bare JID, when known.
        public let jid: BareJID?

        public init(name: String, jid: BareJID?) {
            self.name = name
            self.jid = jid
        }

        /// XEP-0392 input: the bare JID when known, else the name.
        public var colorKey: String { jid?.description ?? name }
    }

    /// One plain-text line describing a reply.
    public enum Snippet: Hashable, Sendable {
        /// Body text with whitespace collapsed to single spaces.
        case text(String)
        /// A XEP-0245 `/me` line, "* name action"; it already names the author.
        case action(String)
        /// Only an image: a shared image file or an inline image URL body.
        case photo
        /// Only a shared file that is not an image.
        case attachment
        /// XEP-0424: retracted by its author.
        case deleted
        /// XEP-0425: removed by a moderator.
        case removed
    }

    /// The newest loaded reply.
    public struct LatestReply: Hashable, Sendable {
        public let authorName: String
        public let authorJID: BareJID?
        public let snippet: Snippet
        public let sentAt: Date

        public init(authorName: String, authorJID: BareJID?, snippet: Snippet, sentAt: Date) {
            self.authorName = authorName
            self.authorJID = authorJID
            self.snippet = snippet
            self.sentAt = sentAt
        }
    }

    public let replyCount: Int
    /// Nil when no reply is loaded although the count is known.
    public let latestReply: LatestReply?
    /// Up to `maxFaces` distinct reply authors, newest reply first.
    public let faces: [Participant]
    /// Distinct reply authors beyond `faces`.
    public let otherParticipantCount: Int

    /// `replies` are the loaded replies, oldest first. `replyCount` is the
    /// known total; it never reads lower than the loaded replies.
    public init(replyCount: Int, replies: [TimelineItem]) {
        self.replyCount = max(replyCount, replies.count)
        latestReply = replies.last.map { reply in
            LatestReply(
                authorName: reply.authorName,
                authorJID: reply.authorJID,
                snippet: Self.snippet(of: reply),
                sentAt: reply.sentAt
            )
        }
        var seen = Set<String>()
        var participants: [Participant] = []
        for reply in replies.reversed() {
            let participant = Participant(name: reply.authorName, jid: reply.authorJID)
            let key = participant.jid.map { "jid:\($0.description)" } ?? "name:\(participant.name)"
            guard !participant.name.isEmpty || participant.jid != nil, seen.insert(key).inserted else { continue }
            participants.append(participant)
        }
        faces = Array(participants.prefix(Self.maxFaces))
        otherParticipantCount = participants.count - faces.count
    }

    /// The summary of loaded `replies`, or nil when there are none.
    public static func of(replies: [TimelineItem]) -> ThreadSummary? {
        replies.isEmpty ? nil : ThreadSummary(replyCount: replies.count, replies: replies)
    }

    /// One plain line for `reply`. XEP-0394 markup travels outside the
    /// body, so the body is already plain text.
    public static func snippet(of reply: TimelineItem) -> Snippet {
        switch reply.tombstone {
        case .retracted: return .deleted
        case .moderated: return .removed
        case nil: break
        }
        if let action = MeAction.presentation(ofBody: reply.body, actor: reply.authorName) {
            return .action(collapsed(action))
        }
        let files = reply.message.sharedFiles
        let trimmed = reply.body.trimmingCharacters(in: .whitespacesAndNewlines)
        // Attachment-only sends carry the first file URL as their body.
        let bodyOnlyRepeatsFile = files.contains { $0.url.absoluteString == trimmed }
        if !trimmed.isEmpty, !bodyOnlyRepeatsFile {
            if files.isEmpty, InlineImageBody.url(in: trimmed) != nil {
                return .photo
            }
            return .text(collapsed(trimmed))
        }
        if let file = files.first {
            return file.isImage ? .photo : .attachment
        }
        return .text("")
    }

    /// Runs of whitespace, newlines included, become one space.
    static func collapsed(_ text: String) -> String {
        let line = text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        guard line.count > maxSnippetLength else { return line }
        return String(line.prefix(maxSnippetLength))
    }
}
