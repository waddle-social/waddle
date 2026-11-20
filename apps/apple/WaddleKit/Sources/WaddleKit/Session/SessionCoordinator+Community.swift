import Foundation

extension SessionCoordinator {
    /// How many posts and stories one refresh asks for.
    static var communityPageSize: Int { 50 }
    /// How many stories have their reactions fetched at once.
    static var storyReactionConcurrency: Int { 4 }

    /// Reloads the community feed, the stories, the reactions of the
    /// stories still up and, until it has been fetched once, which stories
    /// this account has seen. A part that fails keeps what was shown.
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
        if let feed { community.publish(posts: feed) }
        if let stories { community.publish(stories: stories) }

        await loadStoryReads()
        guard community.isCurrent(refresh) else { return }
        await loadStoryReactions(of: community.activeStories(at: now).map(\.id), refresh: refresh)
        guard community.isCurrent(refresh) else { return }
        community.finish(feedFailed: feed == nil, storiesFailed: stories == nil)
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
    /// devices. The whole state is published, so it is not sent before
    /// the server's copy has been fetched.
    public func markStoryRead(_ storyID: String, at now: Date = Date()) async {
        guard !community.isRead(storyID) else { return }
        community.markRead(storyID, at: now)
        let epoch = connectionEpoch
        guard connection == .online, await loadStoryReads(), epoch == connectionEpoch else { return }
        let reads = StoryReads.pruned(community.reads, at: now)
        community.replaceReads(reads)
        try? await port.publishStoryReads(reads)
    }

    /// Adds the user's reaction to a story, or takes it back. Shown at
    /// once and undone if the server refuses. Throws `PortError`.
    public func toggleStoryReaction(_ emoji: String, on storyID: String) async throws {
        guard !isStopped, connection == .online else { throw PortError.notConnected }
        let previous = community.reactions(of: storyID)
        guard let next = previous.toggling(emoji) else { throw PortError.invalidRequest }
        let edit = community.edit(next, of: storyID)
        do {
            try await port.setStoryReactions(next.mine, on: storyID)
            community.sent(storyID)
        } catch {
            community.undo(edit, of: storyID, to: previous)
            throw error
        }
    }

    /// Runs a write that needs the session, and drops its answer if the
    /// session ended meanwhile.
    private func whileOnline<Value: Sendable>(_ write: () async throws -> Value) async throws -> Value {
        guard !isStopped, connection == .online else { throw PortError.notConnected }
        let epoch = connectionEpoch
        let value = try await write()
        guard epoch == connectionEpoch else { throw PortError.notConnected }
        return value
    }

    /// Whether the server's "seen" state is part of the store's.
    @discardableResult
    private func loadStoryReads() async -> Bool {
        if community.hasLoadedReads { return true }
        let epoch = connectionEpoch
        let port = self.port
        guard let remote = await Self.attempt({ try await port.fetchStoryReads() }),
              epoch == connectionEpoch else { return false }
        community.mergeReads(remote)
        return true
    }

    private func loadStoryReactions(of storyIDs: [String], refresh: Int) async {
        let port = self.port
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

    private nonisolated static func attempt<Value: Sendable>(_ request: @Sendable () async throws -> Value) async -> Value? {
        try? await request()
    }
}
