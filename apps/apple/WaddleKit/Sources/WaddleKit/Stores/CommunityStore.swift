import Foundation
import Observation

/// Observable state of the community feed and stories.
@MainActor
@Observable
public final class CommunityStore {
    public private(set) var posts: [FeedPost] = []
    /// Every story the server returned, expired ones included.
    public private(set) var stories: [Story] = []
    public private(set) var isLoading = false
    /// At least one refresh finished since the session started.
    public private(set) var hasLoaded = false
    public private(set) var feedFailed = false
    public private(set) var storiesFailed = false

    private var reactions: [String: StoryReactions] = [:]
    private(set) var reads: [String: Date] = [:]

    @ObservationIgnored private var serial = 0
    /// Whether `reads` includes what the server holds. Until it does,
    /// publishing would overwrite what another device stored.
    @ObservationIgnored private(set) var hasLoadedReads = false
    /// Counts this device's reaction changes per story, so a slower
    /// answer does not replace a newer change.
    @ObservationIgnored private var reactionEdits: [String: Int] = [:]
    /// Stories with a reaction change of this device still being sent.
    @ObservationIgnored private var unsentReactionEdits: [String: Int] = [:]

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

    func publish(posts: [FeedPost]) {
        self.posts = posts
    }

    func publish(stories: [Story]) {
        self.stories = stories
        let known = Set(stories.map(\.id))
        reactions = reactions.filter { known.contains($0.key) }
    }

    func finish(feedFailed: Bool, storiesFailed: Bool) {
        isLoading = false
        hasLoaded = true
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
    }

    func prepend(_ story: Story) {
        stories.removeAll { $0.id == story.id }
        stories.insert(story, at: 0)
    }

    func reactionEdit(of storyID: String) -> Int {
        reactionEdits[storyID] ?? 0
    }

    /// A change made on this device, shown before it is sent; returns
    /// its edit number. Pair with `sent` or `undo`.
    func edit(_ reactions: StoryReactions, of storyID: String) -> Int {
        let edit = reactionEdit(of: storyID) + 1
        reactionEdits[storyID] = edit
        unsentReactionEdits[storyID, default: 0] += 1
        self.reactions[storyID] = reactions
        return edit
    }

    func sent(_ storyID: String) {
        let unsent = (unsentReactionEdits[storyID] ?? 1) - 1
        unsentReactionEdits[storyID] = unsent > 0 ? unsent : nil
    }

    /// The server refused edit `edit`: back to `previous`, unless a
    /// newer change was made since.
    func undo(_ edit: Int, of storyID: String, to previous: StoryReactions) {
        sent(storyID)
        guard reactionEdit(of: storyID) == edit else { return }
        reactions[storyID] = previous
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
        hasLoadedReads = true
    }

    func replaceReads(_ reads: [String: Date]) {
        self.reads = reads
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
        hasLoadedReads = false
        isLoading = false
        hasLoaded = false
        feedFailed = false
        storiesFailed = false
    }
}
