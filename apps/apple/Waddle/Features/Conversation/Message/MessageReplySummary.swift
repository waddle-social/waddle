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

/// Copy for the attached reply card under a thread root.
enum MessageThreadCardText {
    static func count(_ count: Int) -> String {
        count == 1 ? "1 reply" : "\(count) replies"
    }

    /// "2 hr. ago", relative to now.
    static func time(_ date: Date) -> String {
        date.formatted(.relative(presentation: .named, unitsStyle: .abbreviated))
    }

    static func author(_ latest: ThreadSummary.LatestReply) -> String {
        latest.authorName.isEmpty ? "Unknown" : latest.authorName
    }

    static func snippet(_ snippet: ThreadSummary.Snippet) -> String {
        switch snippet {
        case let .text(text): return text
        case let .action(line): return line
        case .photo: return "Photo"
        case .attachment: return "Attachment"
        case .deleted: return "Message deleted"
        case .removed: return "Removed by a moderator"
        }
    }

    /// Whether the card prefixes the snippet with its author; a `/me`
    /// line already names them.
    static func showsAuthor(_ latest: ThreadSummary.LatestReply) -> Bool {
        if case .action = latest.snippet { return false }
        return true
    }

    /// "3 replies, latest from guzzle: Love this, 2 hr. ago".
    static func summary(_ thread: ThreadSummary) -> String {
        var text = count(thread.replyCount)
        if let latest = thread.latestReply {
            text += ", latest from \(author(latest)): \(snippet(latest.snippet)), \(time(latest.sentAt))"
        }
        return text
    }

    /// The card's VoiceOver label.
    static func accessibilityLabel(_ thread: ThreadSummary) -> String {
        "\(summary(thread)). Open thread"
    }
}
