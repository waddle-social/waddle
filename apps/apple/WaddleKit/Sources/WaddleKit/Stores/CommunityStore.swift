import Foundation
import Observation

/// Observable state of the community feed and stories.
@MainActor
@Observable
public final class CommunityStore {
    /// Newest first.
    public private(set) var posts: [FeedPost] = []
    /// Every story the server returned, expired ones included.
    public private(set) var stories: [Story] = []
    public private(set) var isLoading = false
    /// At least one refresh finished since the session started.
    public private(set) var hasLoaded = false
    /// When the last refresh finished.
    public private(set) var loadedAt: Date?
    public private(set) var feedFailed = false
    public private(set) var storiesFailed = false

    private var reactions: [String: StoryReactions] = [:]
    private(set) var reads: [String: Date] = [:]

    @ObservationIgnored private var serial = 0
    /// Counts this device's reaction changes per story, so a slower
    /// answer does not replace a newer change.
    @ObservationIgnored private var reactionEdits: [String: Int] = [:]
    /// Stories with a reaction change of this device still being sent.
    @ObservationIgnored private var unsentReactionEdits: [String: Int] = [:]
    /// What the server last agreed to, for stories with unsent changes.
    @ObservationIgnored private var confirmedReactions: [String: StoryReactions] = [:]
    /// The newest reaction write of each story; the next one waits for it.
    @ObservationIgnored private var reactionWrites: [String: Task<Void, any Error>] = [:]
    /// Posts and stories published here, with the refresh number at the
    /// time: a refresh that began earlier does not know them yet.
    @ObservationIgnored private var postedHere: [(post: FeedPost, during: Int)] = []
    @ObservationIgnored private var storiesPostedHere: [(story: Story, during: Int)] = []

    public init() {}

    /// Stories still up at `now`, newest first.
    public func activeStories(at now: Date) -> [Story] {
        stories
            .filter { $0.isActive(at: now) }
            .sorted { ($0.postedAt ?? .distantPast, $0.id) > ($1.postedAt ?? .distantPast, $1.id) }
    }

    public func reactions(of storyID: String) -> StoryReactions {
        reactions[storyID] ?? .none
    }

    public func isRead(_ storyID: String) -> Bool {
        reads[storyID] != nil
    }

    // MARK: Refresh

    /// Starts a refresh; results carrying an older serial are dropped.
    func begin() -> Int {
        serial += 1
        isLoading = true
        return serial
    }

    func isCurrent(_ refresh: Int) -> Bool {
        refresh == serial
    }

    /// The answer of refresh `refresh`. The server lists posts oldest
    /// first; the Feed shows the newest first. What was posted here after
    /// that refresh began is kept, since its answer cannot include it.
    func publish(posts fetched: [FeedPost], refresh: Int) {
        postedHere.removeAll { $0.during < refresh }
        let fetchedIDs = Set(fetched.map(\.id))
        let unlisted = postedHere.map(\.post).filter { !fetchedIDs.contains($0.id) }
        posts = (unlisted + fetched).enumerated().sorted { first, second in
            let (earlier, later) = (first.element.publishedAt, second.element.publishedAt)
            // Undated posts keep the server's order, newest last.
            return earlier == later ? first.offset > second.offset : (earlier ?? .distantPast) > (later ?? .distantPast)
        }.map(\.element)
    }

    func publish(stories fetched: [Story], refresh: Int) {
        storiesPostedHere.removeAll { $0.during < refresh }
        let fetchedIDs = Set(fetched.map(\.id))
        stories = storiesPostedHere.map(\.story).filter { !fetchedIDs.contains($0.id) } + fetched
        let known = Set(stories.map(\.id))
        reactions = reactions.filter { known.contains($0.key) }
    }

    func finish(feedFailed: Bool, storiesFailed: Bool, at now: Date = Date()) {
        isLoading = false
        hasLoaded = true
        loadedAt = now
        self.feedFailed = feedFailed
        self.storiesFailed = storiesFailed
    }

    /// Ends a refresh that could not ask anything.
    func cancel(_ refresh: Int) {
        guard isCurrent(refresh) else { return }
        isLoading = false
    }

    // MARK: Changes made here

    func prepend(_ post: FeedPost) {
        posts.removeAll { $0.id == post.id }
        posts.insert(post, at: 0)
        postedHere.append((post, serial))
    }

    func prepend(_ story: Story) {
        stories.removeAll { $0.id == story.id }
        stories.insert(story, at: 0)
        storiesPostedHere.append((story, serial))
    }

    func reactionEdit(of storyID: String) -> Int {
        reactionEdits[storyID] ?? 0
    }

    /// A change made on this device, shown before it is sent; returns
    /// its edit number. Pair with `sent` or `undo`.
    func edit(_ reactions: StoryReactions, of storyID: String) -> Int {
        let edit = reactionEdit(of: storyID) + 1
        reactionEdits[storyID] = edit
        if unsentReactionEdits[storyID] == nil {
            confirmedReactions[storyID] = self.reactions(of: storyID)
        }
        unsentReactionEdits[storyID, default: 0] += 1
        self.reactions[storyID] = reactions
        return edit
    }

    /// The server took `reactions`.
    func sent(_ reactions: StoryReactions, of storyID: String) {
        confirmedReactions[storyID] = reactions
        settle(storyID)
    }

    /// The server refused edit `edit`: back to what it last agreed to,
    /// unless a newer change is on its way and will say what is wanted.
    func undo(_ edit: Int, of storyID: String) {
        let confirmed = confirmedReactions[storyID]
        settle(storyID)
        guard reactionEdit(of: storyID) == edit, let confirmed else { return }
        reactions[storyID] = confirmed
    }

    private func settle(_ storyID: String) {
        let unsent = (unsentReactionEdits[storyID] ?? 1) - 1
        unsentReactionEdits[storyID] = unsent > 0 ? unsent : nil
        if unsent <= 0 {
            confirmedReactions[storyID] = nil
            reactionWrites[storyID] = nil
        }
    }

    /// Runs `write` after the story's earlier reaction writes, so the
    /// server ends up with the last thing the user chose.
    func afterEarlierReactionWrites(
        of storyID: String,
        _ write: @escaping @Sendable () async throws -> Void
    ) -> Task<Void, any Error> {
        let earlier = reactionWrites[storyID]
        let task = Task {
            _ = await earlier?.result
            try await write()
        }
        reactionWrites[storyID] = task
        return task
    }

    /// An answer from the server, asked for at edit `edit`. Dropped if
    /// the story was changed here since, or a change is still being sent.
    func set(_ reactions: StoryReactions, of storyID: String, askedAt edit: Int) {
        guard reactionEdit(of: storyID) == edit, unsentReactionEdits[storyID] == nil else { return }
        self.reactions[storyID] = reactions
    }

    func markRead(_ storyID: String, at date: Date) {
        reads[storyID] = date
    }

    /// Adds what the server holds to what was seen on this device.
    func mergeReads(_ remote: [String: Date]) {
        reads = StoryReads.merged(reads, with: remote)
    }

    /// Sign-out or a fresh session: drop everything, including answers
    /// still in flight.
    func reset() {
        serial += 1
        posts = []
        stories = []
        reactions = [:]
        reads = [:]
        reactionEdits = [:]
        unsentReactionEdits = [:]
        confirmedReactions = [:]
        reactionWrites = [:]
        postedHere = []
        storiesPostedHere = []
        loadedAt = nil
        isLoading = false
        hasLoaded = false
        feedFailed = false
        storiesFailed = false
    }
}
