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
        if let imageURL = MessageContent.inlineImageURL(of: parent) {
            return MessageContent.inlineImageNoun(imageURL)
        }
        if let body = MessageContent.visibleBody(of: parent) {
            let shown = MeAction.presentation(ofBody: body, actor: MessageAuthor.resolve(parent, occupant: nil).name) ?? body
            let line = shown.split(whereSeparator: \.isNewline).first.map(String.init) ?? shown
            return line.trimmingCharacters(in: .whitespaces)
        }
        if let file = parent.message.sharedFiles.first {
            return MessageAttachmentKind(file).noun
        }
        return "Message"
    }
}
