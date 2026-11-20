import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

private let now = date(1000)
private let picture = URL(string: "https://upload.waddle.test/rink.jpg")!

private func post(_ id: String, _ body: String = "hello") -> FeedPost {
    FeedPost(id: id, body: body, author: bob, publishedAt: date(0))
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
        port.community.feed = .success([post("p1"), post("p2")])
        port.community.stories = .success([story("s1")])

        await coordinator.refreshCommunity(at: now)

        #expect(coordinator.community.posts.map(\.id) == ["p1", "p2"])
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
        ])

        #expect(store.activeStories(at: now).map(\.id) == ["new", "forever", "old"])
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
