import Foundation
import WaddleKit

/// The single VoiceOver label of a message row: author, time, content,
/// then reactions and replies.
enum MessageAccessibilityText {
    static func label(for item: TimelineItem, authorName: String, replyCount: Int, time: String) -> String {
        var parts = [authorName, time]
        parts.append(content(of: item))
        if item.isEdited, item.tombstone == nil {
            parts.append("Edited")
        }
        let reactionCount = item.reactions.reduce(0) { $0 + $1.count }
        if reactionCount > 0 {
            parts.append(reactionCount == 1 ? "1 reaction" : "\(reactionCount) reactions")
        }
        if replyCount > 0 {
            parts.append(replyCount == 1 ? "1 reply" : "\(replyCount) replies")
        }
        if item.safetyScores?.notableSignalRows.contains(where: { $0.category == .isQuestion }) == true {
            parts.append("Question signal")
        }
        switch item.safetyScores?.severity {
        case .alert: parts.append("Content alert")
        case .notice: parts.append("Content notice")
        case nil: break
        }
        return parts.filter { !$0.isEmpty }.joined(separator: ", ")
    }

    static func content(of item: TimelineItem) -> String {
        switch item.tombstone {
        case .retracted:
            return "This message was deleted"
        case let .moderated(_, reason):
            if let reason, !reason.isEmpty {
                return "Removed by a moderator: \(reason)"
            }
            return "Removed by a moderator"
        case nil:
            break
        }
        var parts: [String] = []
        if let imageURL = MessageContent.inlineImageURL(of: item) {
            parts.append(MessageContent.inlineImageNoun(imageURL))
        } else if let body = MessageContent.visibleBody(of: item) {
            let author = MessageAuthor.resolve(item, occupant: nil).name
            parts.append(MeAction.presentation(ofBody: body, actor: author) ?? body)
        }
        for file in item.message.sharedFiles {
            let kind = MessageAttachmentKind(file)
            let label = "\(kind.noun), \(file.displayName)"
            parts.append(file.encrypted == nil ? label : "\(label), encrypted")
        }
        return parts.joined(separator: ", ")
    }

    /// "👍 3, reacted by bob, carol".
    static func label(for reaction: ReactionGroup) -> String {
        let names = reaction.reactors.joined(separator: ", ")
        return names.isEmpty ? "\(reaction.emoji) \(reaction.count)" : "\(reaction.emoji) \(reaction.count), reacted by \(names)"
    }
}
