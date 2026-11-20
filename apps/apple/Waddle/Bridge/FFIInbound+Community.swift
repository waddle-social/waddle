import Foundation
import WaddleKit

/// Community feed and story conversions.
extension FFIInbound {
    static func date(milliseconds: Int64?) -> Date? {
        milliseconds.map { Date(timeIntervalSince1970: TimeInterval($0) / 1000) }
    }

    static func feedPost(_ entry: WaddleFeedEntry) -> FeedPost {
        FeedPost(
            id: entry.id,
            title: entry.title,
            body: entry.body,
            author: bareJID(entry.author),
            publishedAt: date(milliseconds: entry.publishedMs),
            link: webURL(entry.link),
            source: entry.source.map(feedSource)
        )
    }

    static func feedSource(_ source: WaddleFeedSource) -> FeedSource {
        switch source {
        case .mood: .mood
        case .activity: .activity
        case .tune: .tune
        case .avatar: .avatar
        case .profile: .profile
        case .eventReply: .eventReply
        case .other: .other
        }
    }

    /// A story whose media the app would not fetch is dropped.
    static func story(_ story: WaddleStory) -> Story? {
        guard let media = webURL(story.mediaUrl) else { return nil }
        return Story(
            id: story.id,
            body: story.body,
            media: media,
            mediaType: story.mediaType,
            author: bareJID(story.author),
            postedAt: date(milliseconds: story.postedMs),
            expiresAt: date(milliseconds: story.expiresMs)
        )
    }

    static func storyReactions(_ reactions: WaddleStoryReactions) -> StoryReactions {
        StoryReactions(
            counts: reactions.reactions.map { StoryReactions.Count(emoji: $0.emoji, count: Int($0.count)) },
            mine: reactions.mine
        )
    }
}
