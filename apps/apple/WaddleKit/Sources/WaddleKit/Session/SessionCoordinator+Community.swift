import Foundation

extension SessionCoordinator {
    /// How many posts and stories one refresh asks for.
    static var communityPageSize: Int { 50 }
    /// How many stories have their reactions fetched at once.
    static var storyReactionConcurrency: Int { 4 }

    /// Reloads the community feed, the stories, which of them this account
    /// has seen, and the reactions of the stories still up. A part that
    /// fails keeps what was shown.
    public func refreshCommunity(at now: Date = Date()) async {
        let refresh = community.begin()
        guard connection == .online else {
            community.cancel(refresh)
            return
        }
        let port = self.port
        async let feedAnswer = Self.attempt { try await port.fetchFeed(max: Self.communityPageSize) }
        async let storiesAnswer = Self.attempt { try await port.fetchStories(max: Self.communityPageSize) }
        let (feed, stories) = await (feedAnswer, storiesAnswer)
        guard community.isCurrent(refresh) else { return }
        if let feed { community.publish(posts: feed, refresh: refresh) }
        if let stories { community.publish(stories: stories, refresh: refresh) }

        await syncStoryReads(at: now)
        guard community.isCurrent(refresh) else { return }
        await loadStoryReactions(of: community.activeStories(at: now).map(\.id), refresh: refresh)
        guard community.isCurrent(refresh) else { return }
        community.finish(feedFailed: feed == nil, storiesFailed: stories == nil, at: now)
    }

    /// Posts to the feed and shows the post first. Throws `PortError`.
    public func postToFeed(_ draft: FeedPostDraft) async throws {
        let post = try await whileOnline { try await port.publishFeedPost(draft) }
        community.prepend(post)
    }

    /// Posts a story whose media is already uploaded. The author has seen
    /// their own story. Throws `PortError`.
    public func postStory(_ draft: StoryDraft, at now: Date = Date()) async throws {
        let story = try await whileOnline { try await port.publishStory(draft) }
        community.prepend(story)
        await markStoryRead(story.id, at: now)
    }

    /// Remembers that the user saw a story, here and on their other
    /// devices. A story seen while that cannot be sent goes out with the
    /// next one, or the next refresh.
    public func markStoryRead(_ storyID: String, at now: Date = Date()) async {
        guard !community.isRead(storyID) else { return }
        community.markRead(storyID, at: now)
        await syncStoryReads(at: now)
    }

    /// Adds the user's reaction to a story, or takes it back. Shown at
    /// once and undone if the server refuses. Changes to one story are
    /// sent one after another, each with all the user's emoji, so quick
    /// taps end as the last one left them. Throws `PortError`.
    public func toggleStoryReaction(_ emoji: String, on storyID: String) async throws {
        guard !isStopped, connection == .online else { throw PortError.notConnected }
        let previous = community.reactions(of: storyID)
        guard let next = previous.toggling(emoji) else { throw PortError.invalidRequest }
        let edit = community.edit(next, of: storyID)
        let port = self.port
        let write = community.afterEarlierReactionWrites(of: storyID) {
            try await port.setStoryReactions(next.mine, on: storyID)
        }
        do {
            try await write.value
            community.sent(next, of: storyID)
        } catch {
            community.undo(edit, of: storyID)
            throw error
        }
    }

    /// Runs a write that needs the session, and drops its answer if the
    /// session ended meanwhile.
    func whileOnline<Value: Sendable>(_ write: () async throws -> Value) async throws -> Value {
        guard !isStopped, connection == .online else { throw PortError.notConnected }
        let epoch = connectionEpoch
        let value = try await write()
        guard epoch == connectionEpoch else { throw PortError.notConnected }
        return value
    }

    /// Brings this device's "seen" state and the server's together. The
    /// server holds one item for every device, so its copy is fetched and
    /// merged each time, and published only when this device adds to it:
    /// never over a copy that could not be fetched.
    private func syncStoryReads(at now: Date) async {
        guard !isStopped, connection == .online else { return }
        let epoch = connectionEpoch
        let port = self.port
        guard let remote = await Self.attempt({ try await port.fetchStoryReads() }),
              epoch == connectionEpoch else { return }
        community.mergeReads(remote)
        let merged = StoryReads.pruned(community.reads, at: now)
        guard merged != StoryReads.pruned(remote, at: now) else { return }
        try? await port.publishStoryReads(merged)
    }

    private func loadStoryReactions(of storyIDs: [String], refresh: Int) async {
        let port = self.port
        let storyIDs = Array(Set(storyIDs))
        let edits = Dictionary(uniqueKeysWithValues: storyIDs.map { ($0, community.reactionEdit(of: $0)) })
        await withTaskGroup(of: (String, StoryReactions?).self) { group in
            var pending = storyIDs[...]
            func addNext() {
                guard let id = pending.popFirst() else { return }
                group.addTask { await (id, Self.attempt { try await port.fetchStoryReactions(of: id) }) }
            }
            for _ in 0..<Self.storyReactionConcurrency { addNext() }
            for await (id, reactions) in group {
                if let reactions, community.isCurrent(refresh) {
                    community.set(reactions, of: id, askedAt: edits[id] ?? 0)
                }
                addNext()
            }
        }
    }

    nonisolated static func attempt<Value: Sendable>(_ request: @Sendable () async throws -> Value) async -> Value? {
        try? await request()
    }
}
