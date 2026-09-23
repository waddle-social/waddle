import Foundation
import WaddleKit

/// The one-line quote above a XEP-0461 reply.
enum MessageReplySummary {
    static func author(parent: TimelineItem?, target: WireMessage.ReplyTarget, isRoom: Bool) -> String {
        if let parent, !parent.authorName.isEmpty {
            return parent.authorName
        }
        guard let author = target.author else { return "Unknown" }
        if isRoom, let nick = author.resource {
            return nick
        }
        return author.bare.localpart ?? author.bare.domain
    }

    static func snippet(parent: TimelineItem?) -> String {
        guard let parent else { return "Original message not loaded" }
        if parent.tombstone != nil {
            return "Deleted message"
        }
        if let body = MessageContent.visibleBody(of: parent) {
            let line = body.split(whereSeparator: \.isNewline).first.map(String.init) ?? body
            return line.trimmingCharacters(in: .whitespaces)
        }
        if let file = parent.message.sharedFiles.first {
            return MessageAttachmentKind(file).noun
        }
        return "Message"
    }
}
