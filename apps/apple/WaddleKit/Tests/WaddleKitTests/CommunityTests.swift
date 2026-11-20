import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

private let now = date(1000)
private let picture = URL(string: "https://upload.waddle.test/rink.jpg")!

private func post(_ id: String, at seconds: TimeInterval = 0) -> FeedPost {
    FeedPost(id: id, body: "hello", author: bob, publishedAt: date(seconds))
}

private func story(_ id: String, postedAt seconds: TimeInterval = 0, expiresIn lifetime: TimeInterval? = 86400) -> Story {
    Story(
        id: id,
        media: picture,
        mediaType: "image/jpeg",
        author: bob,
        postedAt: date(seconds),
        expiresAt: lifetime.map { date(seconds + $0) }
    )
}

private func reactions(_ counts: [(String, Int)], mine: [String] = []) -> StoryReactions {
    StoryReactions(counts: counts.map { StoryReactions.Count(emoji: $0.0, count: $0.1) }, mine: mine)
}

@MainActor
@Suite("Community feed and stories")
struct CommunityTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        return (coordinator, port)
    }

    // MARK: Loading

    @Test func refreshLoadsTheFeedAndStories() async {
        let (coordinator, port) = online()
        port.community.feed = .success([post("p1", at: 10), post("p2", at: 20)])
        port.community.stories = .success([story("s1")])

        await coordinator.refreshCommunity(at: now)

        #expect(coordinator.community.posts.map(\.id) == ["p2", "p1"])
        #expect(coordinator.community.stories.map(\.id) == ["s1"])
        #expect(coordinator.community.hasLoaded)
        #expect(!coordinator.community.isLoading)
        #expect(!coordinator.community.feedFailed)
        #expect(!coordinator.community.storiesFailed)
    }

    @Test func aFailedFeedKeepsTheStoriesAndThePostsAlreadyShown() async {
        let (coordinator, port) = online()
        port.community.feed = .success([post("p1")])
        await coordinator.refreshCommunity(at: now)

        port.community.feed = .failure(.timeout)
        port.community.stories = .success([story("s1")])
        await coordinator.refreshCommunity(at: now)

        #expect(coordinator.community.feedFailed)
        #expect(!coordinator.community.storiesFailed)
        #expect(coordinator.community.posts.map(\.id) == ["p1"])
        #expect(coordinator.community.stories.map(\.id) == ["s1"])
    }

    @Test func refreshOfflineAsksNothing() async {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)

        await coordinator.refreshCommunity(at: now)

        #expect(port.community.feedRequests == 0)
        #expect(!coordinator.community.hasLoaded)
        #expect(!coordinator.community.isLoading)
    }

    @Test func anAnswerThatArrivesAfterSignOutIsDropped() async {
        let (coordinator, port) = online()
        port.community.feed = .success([post("p1")])
        port.community.held = ["fetchFeed"]

        let refresh = Task { await coordinator.refreshCommunity(at: now) }
        while port.community.feedRequests == 0 { await Task.yield() }
        coordinator.community.reset()
        port.community.release()
        await refresh.value

        #expect(coordinator.community.posts.isEmpty)
        #expect(!coordinator.community.hasLoaded)
    }

    @Test func activeStoriesAreNewestFirstWithoutTheExpired() {
        let store = CommunityStore()
        store.publish(stories: [
            story("old", postedAt: 0),
            story("gone", postedAt: 10, expiresIn: 60),
            story("new", postedAt: 500),
            story("forever", postedAt: 100, expiresIn: nil),
        ], refresh: 0)

        #expect(store.activeStories(at: now).map(\.id) == ["new", "forever", "old"])
    }

    @Test func aStoryIsAVideoByItsTypeOrElseItsFileName() {
        let clip = URL(string: "https://upload.waddle.test/story-1.webm")!
        #expect(Story(id: "a", media: picture, mediaType: "video/mp4").isVideo)
        #expect(!Story(id: "b", media: clip, mediaType: "image/jpeg").isVideo)
        #expect(Story(id: "c", media: clip).isVideo)
        #expect(Story(id: "c", media: clip).isWebM)
        #expect(Story(id: "d", media: picture, mediaType: "video/webm").isWebM)
        #expect(!Story(id: "e", media: picture, mediaType: "image/jpeg").isVideo)
    }

    @Test func theFeedIsNewestFirstWhateverOrderTheServerUses() async {
        let (coordinator, port) = online()
        port.community.feed = .success([post("old", at: 10), post("newest", at: 30), post("newer", at: 20)])

        await coordinator.refreshCommunity(at: now)

        #expect(coordinator.community.posts.map(\.id) == ["newest", "newer", "old"])
    }

    @Test func postsWithoutADateKeepTheServersOrderNewestLast() {
        let store = CommunityStore()
        store.publish(posts: [FeedPost(id: "first", body: "a"), FeedPost(id: "second", body: "b")], refresh: 0)

        #expect(store.posts.map(\.id) == ["second", "first"])
    }

    @Test func aPostMadeDuringARefreshSurvivesItsOlderAnswer() async throws {
        let (coordinator, port) = online()
        port.community.feed = .success([post("p1")])
        port.community.held = ["fetchStories"]
        let refresh = Task { await coordinator.refreshCommunity(at: now) }
        while port.community.storyRequests == 0 { await Task.yield() }

        try await coordinator.postToFeed(#require(FeedPostDraft(body: "hi")))
        try await coordinator.postStory(StoryDraft(body: nil, media: picture, mediaType: "image/jpeg"), at: now)
        port.community.release()
        await refresh.value

        #expect(coordinator.community.posts.map(\.id) == ["post-1", "p1"])
        #expect(coordinator.community.stories.map(\.id) == ["story-1"])
    }

    @Test func aRefreshBegunAfterAPostTrustsTheServersList() async throws {
        let (coordinator, port) = online()
        try await coordinator.postToFeed(#require(FeedPostDraft(body: "hi")))

        port.community.feed = .success([post("p1")])
        await coordinator.refreshCommunity(at: now)

        #expect(coordinator.community.posts.map(\.id) == ["p1"])
    }

    // MARK: Reactions

    @Test func refreshLoadsReactionsOfActiveStoriesOnly() async {
        let (coordinator, port) = online()
        port.community.stories = .success([story("s1"), story("gone", expiresIn: 60)])
        port.community.reactions["s1"] = .success(reactions([("👍", 2)], mine: ["👍"]))

        await coordinator.refreshCommunity(at: now)

        #expect(port.community.reactionRequests == ["s1"])
        #expect(coordinator.community.reactions(of: "s1") == reactions([("👍", 2)], mine: ["👍"]))
        #expect(coordinator.community.reactions(of: "gone") == .none)
    }

    @Test func reactingShowsAtOnceAndSendsAllMyEmoji() async throws {
        let (coordinator, port) = online()
        port.community.stories = .success([story("s1")])
        port.community.reactions["s1"] = .success(reactions([("👍", 2)], mine: ["👍"]))
        await coordinator.refreshCommunity(at: now)
        port.community.held = ["setStoryReactions"]

        let toggle = Task { try await coordinator.toggleStoryReaction("🎉", on: "s1") }
        while port.community.reactionWrites.isEmpty { await Task.yield() }

        #expect(coordinator.community.reactions(of: "s1") == reactions([("👍", 2), ("🎉", 1)], mine: ["👍", "🎉"]))
        port.community.release()
        try await toggle.value
        #expect(port.community.reactionWrites.map(\.emojis) == [["👍", "🎉"]])
    }

    @Test func reactingAgainWithTheSameEmojiTakesItBack() async throws {
        let (coordinator, port) = online()
        port.community.stories = .success([story("s1")])
        port.community.reactions["s1"] = .success(reactions([("👍", 1)], mine: ["👍"]))
        await coordinator.refreshCommunity(at: now)

        try await coordinator.toggleStoryReaction("👍", on: "s1")

        #expect(coordinator.community.reactions(of: "s1") == .none)
        #expect(port.community.reactionWrites.map(\.emojis) == [[]])
    }

    @Test func aRefusedReactionIsUndone() async {
        let (coordinator, port) = online()
        port.community.stories = .success([story("s1")])
        port.community.reactions["s1"] = .success(reactions([("👍", 2)]))
        await coordinator.refreshCommunity(at: now)
        port.community.reactionResult = .failure(.rejected)

        await #expect(throws: PortError.rejected) {
            try await coordinator.toggleStoryReaction("👍", on: "s1")
        }

        #expect(coordinator.community.reactions(of: "s1") == reactions([("👍", 2)]))
    }

    @Test func aRefreshDoesNotUndoAReactionStillBeingSent() async throws {
        let (coordinator, port) = online()
        port.community.stories = .success([story("s1")])
        await coordinator.refreshCommunity(at: now)
        port.community.held = ["setStoryReactions"]
        let toggle = Task { try await coordinator.toggleStoryReaction("🎉", on: "s1") }
        while port.community.reactionWrites.isEmpty { await Task.yield() }

        await coordinator.refreshCommunity(at: now)

        #expect(coordinator.community.reactions(of: "s1") == reactions([("🎉", 1)], mine: ["🎉"]))
        port.community.release()
        try await toggle.value
    }

    @Test func quickTapsAreSentOneAfterAnotherAndEndAsTheLastOne() async throws {
        let (coordinator, port) = online()
        port.community.stories = .success([story("s1")])
        await coordinator.refreshCommunity(at: now)
        port.community.held = ["setStoryReactions"]

        let add = Task { try await coordinator.toggleStoryReaction("🎉", on: "s1") }
        while port.community.reactionWrites.isEmpty { await Task.yield() }
        let remove = Task { try await coordinator.toggleStoryReaction("🎉", on: "s1") }
        for _ in 0..<20 { await Task.yield() }

        // The removal waits for the add, so it cannot be answered first.
        #expect(port.community.reactionWrites.map(\.emojis) == [["🎉"]])
        port.community.held = []
        port.community.release()
        try await add.value
        try await remove.value

        #expect(port.community.reactionWrites.map(\.emojis) == [["🎉"], []])
        #expect(coordinator.community.reactions(of: "s1") == .none)
    }

    @Test func refusedQuickTapsGoBackToWhatTheServerLastAgreedTo() async {
        let (coordinator, port) = online()
        port.community.stories = .success([story("s1")])
        port.community.reactions["s1"] = .success(reactions([("👍", 2)]))
        await coordinator.refreshCommunity(at: now)
        port.community.held = ["setStoryReactions"]
        port.community.reactionResult = .failure(.rejected)

        let first = Task { try await coordinator.toggleStoryReaction("👍", on: "s1") }
        while port.community.reactionWrites.isEmpty { await Task.yield() }
        let second = Task { try await coordinator.toggleStoryReaction("🎉", on: "s1") }
        for _ in 0..<20 { await Task.yield() }
        port.community.held = []
        port.community.release()
        _ = await first.result
        _ = await second.result

        #expect(coordinator.community.reactions(of: "s1") == reactions([("👍", 2)]))
    }

    @Test func aThirteenthReactionIsRefusedWithoutAsking() async {
        let (coordinator, port) = online()
        let twelve = (0..<12).map { "e\($0)" }
        port.community.stories = .success([story("s1")])
        port.community.reactions["s1"] = .success(reactions(twelve.map { ($0, 1) }, mine: twelve))
        await coordinator.refreshCommunity(at: now)

        await #expect(throws: PortError.invalidRequest) {
            try await coordinator.toggleStoryReaction("🎉", on: "s1")
        }
        #expect(port.community.reactionWrites.isEmpty)
    }

    @Test func reactingOfflineFails() async {
        let (coordinator, _) = online()
        coordinator.status.connection = .offline(retryAt: nil)

        await #expect(throws: PortError.notConnected) {
            try await coordinator.toggleStoryReaction("👍", on: "s1")
        }
    }

    // MARK: Read state

    @Test func refreshLoadsWhichStoriesWereSeen() async {
        let (coordinator, port) = online()
        port.community.stories = .success([story("s1"), story("s2")])
        port.community.reads = .success(["s1": date(900)])

        await coordinator.refreshCommunity(at: now)

        #expect(coordinator.community.isRead("s1"))
        #expect(!coordinator.community.isRead("s2"))
    }

    @Test func seeingAStoryPublishesTheWholeSeenState() async {
        let (coordinator, port) = online()
        port.community.reads = .success(["s1": date(900)])
        await coordinator.refreshCommunity(at: now)

        await coordinator.markStoryRead("s2", at: now)

        #expect(coordinator.community.isRead("s2"))
        #expect(port.community.publishedReads == [["s1": date(900), "s2": now]])
    }

    @Test func seeingAStoryAgainPublishesNothing() async {
        let (coordinator, port) = online()
        port.community.reads = .success(["s1": date(900)])
        await coordinator.refreshCommunity(at: now)

        await coordinator.markStoryRead("s1", at: now)

        #expect(port.community.publishedReads.isEmpty)
    }

    @Test func seenStateIsNotPublishedOverOneThatCouldNotBeFetched() async {
        let (coordinator, port) = online()
        port.community.reads = .failure(.timeout)
        await coordinator.refreshCommunity(at: now)

        await coordinator.markStoryRead("s2", at: now)

        #expect(coordinator.community.isRead("s2"))
        #expect(port.community.publishedReads.isEmpty)
    }

    @Test func seenStateFetchedLaterKeepsWhatWasSeenMeanwhile() async {
        let (coordinator, port) = online()
        port.community.reads = .failure(.timeout)
        await coordinator.refreshCommunity(at: now)
        await coordinator.markStoryRead("s2", at: now)

        port.community.reads = .success(["s1": date(900)])
        await coordinator.markStoryRead("s3", at: now)

        #expect(port.community.publishedReads == [["s1": date(900), "s2": now, "s3": now]])
    }

    @Test func oldReadsAreForgottenWhenPublishing() async {
        let (coordinator, port) = online()
        let threeDaysAgo = now.addingTimeInterval(-3 * 86400)
        port.community.reads = .success(["ancient": threeDaysAgo, "s1": date(900)])
        await coordinator.refreshCommunity(at: now)

        await coordinator.markStoryRead("s2", at: now)

        #expect(port.community.publishedReads == [["s1": date(900), "s2": now]])
    }

    @Test func whatAnotherDeviceSawSinceIsKept() async {
        let (coordinator, port) = online()
        port.community.reads = .success(["s1": date(900)])
        await coordinator.refreshCommunity(at: now)

        port.community.reads = .success(["s1": date(900), "s9": date(950)])
        await coordinator.markStoryRead("s2", at: now)

        #expect(port.community.publishedReads == [["s1": date(900), "s2": now, "s9": date(950)]])
        #expect(coordinator.community.isRead("s9"))
    }

    @Test func aStorySeenOfflineIsPublishedOnTheNextRefresh() async {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)
        await coordinator.markStoryRead("s2", at: now)
        #expect(port.community.publishedReads.isEmpty)

        coordinator.status.connection = .online
        await coordinator.refreshCommunity(at: now)

        #expect(port.community.publishedReads == [["s2": now]])
    }

    @Test func refreshPublishesNothingWhenThisDeviceSawNothingNew() async {
        let (coordinator, port) = online()
        let threeDaysAgo = now.addingTimeInterval(-3 * 86400)
        port.community.reads = .success(["ancient": threeDaysAgo, "s1": date(900)])

        await coordinator.refreshCommunity(at: now)

        #expect(port.community.publishedReads.isEmpty)
    }

    @Test func storiesListedTwiceAskForReactionsOnce() async {
        let (coordinator, port) = online()
        port.community.stories = .success([story("s1"), story("s1")])

        await coordinator.refreshCommunity(at: now)

        #expect(port.community.reactionRequests == ["s1"])
    }

    // MARK: Posting

    @Test func aNewPostGoesToTheTopOfTheFeed() async throws {
        let (coordinator, port) = online()
        port.community.feed = .success([post("p1")])
        await coordinator.refreshCommunity(at: now)

        try await coordinator.postToFeed(#require(FeedPostDraft(body: "  good morning  ")))

        #expect(port.community.posts.map(\.body) == ["good morning"])
        #expect(coordinator.community.posts.map(\.id) == ["post-1", "p1"])
    }

    @Test func aRefusedPostLeavesTheFeedAlone() async throws {
        let (coordinator, port) = online()
        port.community.postResult = .failure(.rejected)

        await #expect(throws: PortError.rejected) {
            try await coordinator.postToFeed(#require(FeedPostDraft(body: "hi")))
        }
        #expect(coordinator.community.posts.isEmpty)
    }

    @Test func postingOfflineFailsWithoutAsking() async throws {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)

        await #expect(throws: PortError.notConnected) {
            try await coordinator.postToFeed(#require(FeedPostDraft(body: "hi")))
        }
        #expect(port.community.posts.isEmpty)
    }

    @Test func anEmptyPostIsNotADraft() {
        #expect(FeedPostDraft(body: " \n ") == nil)
    }

    @Test func myNewStoryIsShownAndAlreadySeen() async throws {
        let (coordinator, port) = online()
        await coordinator.refreshCommunity(at: now)

        try await coordinator.postStory(StoryDraft(body: " at the rink ", media: picture, mediaType: "image/jpeg"), at: now)

        #expect(port.community.postedStories.map(\.body) == ["at the rink"])
        #expect(coordinator.community.stories.map(\.id) == ["story-1"])
        #expect(coordinator.community.isRead("story-1"))
        #expect(port.community.publishedReads == [["story-1": now]])
    }

    // MARK: Session lifecycle

    @Test func signingOutForgetsEverything() async {
        let (coordinator, port) = online()
        port.community.feed = .success([post("p1")])
        port.community.stories = .success([story("s1")])
        port.community.reads = .success(["s1": date(900)])
        await coordinator.refreshCommunity(at: now)

        await coordinator.stop()

        #expect(coordinator.community.posts.isEmpty)
        #expect(coordinator.community.stories.isEmpty)
        #expect(!coordinator.community.isRead("s1"))
        #expect(!coordinator.community.hasLoaded)
    }
}
