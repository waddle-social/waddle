import Foundation
import WaddleKit

/// Words the Feed and stories screens share.
enum CommunityCopy {
    static func name(of author: BareJID?) -> String {
        author.map { $0.localpart ?? $0.domain } ?? "Someone"
    }

    /// What a post the server wrote for a member is about.
    static func label(for source: FeedSource?) -> String? {
        switch source {
        case nil: nil
        case .mood: "Mood"
        case .activity: "Activity"
        case .tune: "Listening"
        case .avatar: "New photo"
        case .profile: "Profile update"
        case .eventReply: "Event reply"
        case .other: "Update"
        }
    }

    /// "now", "5m", "3h", "2d": how long ago, counted from `now`.
    static func age(of date: Date?, at now: Date) -> String? {
        guard let date else { return nil }
        let seconds = max(0, now.timeIntervalSince(date))
        switch seconds {
        case ..<60: return "now"
        case ..<3600: return "\(Int(seconds / 60))m"
        case ..<86400: return "\(Int(seconds / 3600))h"
        default: return "\(Int(seconds / 86400))d"
        }
    }

    /// "Mood · 5m": the line under a post's author.
    static func byline(for post: FeedPost, at now: Date) -> String {
        [label(for: post.source), age(of: post.publishedAt, at: now)]
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    static func storyLabel(_ story: Story, isRead: Bool, at now: Date) -> String {
        let seen = isRead ? "seen" : "new"
        let age = age(of: story.postedAt, at: now).map { ", \($0) ago" } ?? ""
        return "Story from \(name(of: story.author)), \(seen)\(age)"
    }

    static func reactionLabel(_ reaction: StoryReactions.Count, isMine: Bool) -> String {
        let people = reaction.count == 1 ? "1 person" : "\(reaction.count) people"
        return isMine ? "\(reaction.emoji), \(people), including you" : "\(reaction.emoji), \(people)"
    }
}
