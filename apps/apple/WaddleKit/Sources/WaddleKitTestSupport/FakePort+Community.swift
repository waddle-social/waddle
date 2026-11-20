import Foundation
import WaddleKit

/// The scripted community feed and stories of a `FakePort`.
@MainActor
final class FakeCommunity {
    var feed: Result<[FeedPost], PortError> = .success([])
    var stories: Result<[Story], PortError> = .success([])
    var reads: Result<[String: Date], PortError> = .success([:])
    var reactions: [String: Result<StoryReactions, PortError>] = [:]
    var postResult: Result<Void, PortError> = .success(())
    var storyResult: Result<Void, PortError> = .success(())
    var readsPublishResult: Result<Void, PortError> = .success(())
    var reactionResult: Result<Void, PortError> = .success(())

    var feedRequests = 0
    var storyRequests = 0
    var readsRequests = 0
    var reactionRequests: [String] = []
    var posts: [FeedPostDraft] = []
    var postedStories: [StoryDraft] = []
    var publishedReads: [[String: Date]] = []
    var reactionWrites: [(story: String, emojis: [String])] = []

    /// Verbs that wait for `release()` before answering.
    var held: Set<String> = []
    private var waiting: [CheckedContinuation<Void, Never>] = []

    func hold(_ verb: String) async {
        if held.contains(verb) {
            await withCheckedContinuation { waiting.append($0) }
        }
    }

    func release() {
        let waiting = waiting
        self.waiting = []
        waiting.forEach { $0.resume() }
    }
}

extension FakePort {
    func fetchFeed(max: Int?) async throws -> [FeedPost] {
        community.feedRequests += 1
        await community.hold("fetchFeed")
        return try community.feed.get()
    }

    func publishFeedPost(_ draft: FeedPostDraft) async throws -> FeedPost {
        community.posts.append(draft)
        await community.hold("publishFeedPost")
        try community.postResult.get()
        return FeedPost(id: "post-\(community.posts.count)", body: draft.body, author: me.jid, publishedAt: date(500))
    }

    func fetchStories(max: Int?) async throws -> [Story] {
        community.storyRequests += 1
        return try community.stories.get()
    }

    func publishStory(_ draft: StoryDraft) async throws -> Story {
        community.postedStories.append(draft)
        try community.storyResult.get()
        return Story(
            id: "story-\(community.postedStories.count)",
            body: draft.body,
            media: draft.media,
            mediaType: draft.mediaType,
            author: me.jid,
            postedAt: date(500),
            expiresAt: date(500 + 86400)
        )
    }

    func fetchStoryReads() async throws -> [String: Date] {
        community.readsRequests += 1
        return try community.reads.get()
    }

    func publishStoryReads(_ reads: [String: Date]) async throws {
        community.publishedReads.append(reads)
        try community.readsPublishResult.get()
    }

    func fetchStoryReactions(of storyID: String) async throws -> StoryReactions {
        community.reactionRequests.append(storyID)
        return try (community.reactions[storyID] ?? .success(.none)).get()
    }

    func setStoryReactions(_ emojis: [String], on storyID: String) async throws {
        community.reactionWrites.append((storyID, emojis))
        await community.hold("setStoryReactions")
        try community.reactionResult.get()
    }
}
