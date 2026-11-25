import Foundation
import WaddleKit

/// The community feed (XEP-0472) on `community.<domain>`, the stories
/// (XEP-0501) on each account's own PEP node, and the account's private
/// "seen" state.
extension FFIXmppPort {
    func communityService() throws -> String {
        guard let service = account.flatMap({ BareJID(localpart: nil, domain: "community.\($0.domain)") }) else {
            throw PortError.invalidRequest
        }
        return service.description
    }

    func fetchFeed(max: Int?) async throws -> [FeedPost] {
        let service = try communityService()
        let entries = try await mappingPortErrors {
            try await client.fetchFeed(communityJid: service, maxItems: max.flatMap(UInt32.init(exactly:)))
        }
        return entries.map(FFIInbound.feedPost)
    }

    func publishFeedPost(_ draft: FeedPostDraft) async throws -> FeedPost {
        let service = try communityService()
        let post = WaddleFeedPost(title: nil, body: draft.body, link: draft.link?.absoluteString)
        return FFIInbound.feedPost(try await mappingPortErrors {
            try await client.publishFeedPost(communityJid: service, post: post)
        })
    }

    /// The account's stories and those of its contacts: each account hosts
    /// its own stories node (XEP-0501). A contact whose node cannot be read
    /// (no presence relationship, nothing posted) is skipped; a failure on
    /// the account's own node is reported.
    func fetchStories(max: Int?) async throws -> [Story] {
        guard let account else { throw PortError.invalidRequest }
        let contacts = ((try? await mappingPortErrors { try await client.fetchRosterContacts() }) ?? [])
            .compactMap { FFIInbound.bareJID($0.jid) }
        let authors = [account] + contacts.filter { $0 != account }
        let limit = max.flatMap(UInt32.init(exactly:))
        var stories: [Story] = []
        var hosts: [String: BareJID] = [:]
        for author in authors {
            do {
                let authored = try await mappingPortErrors {
                    try await client.fetchStories(authorJid: author.description, maxItems: limit)
                }.compactMap(FFIInbound.story)
                for story in authored { hosts[story.id] = author }
                stories += authored
            } catch {
                if author == account { throw error }
            }
        }
        storyHosts.withLock { $0 = hosts }
        return stories
    }

    func publishStory(_ draft: StoryDraft) async throws -> Story {
        guard let account else { throw PortError.invalidRequest }
        let post = WaddleStoryPost(
            body: draft.body,
            mediaUrl: draft.media.absoluteString,
            mediaType: draft.mediaType,
            expiryHours: nil
        )
        let published = try await mappingPortErrors {
            try await client.publishStory(post: post)
        }
        guard let story = FFIInbound.story(published) else { throw PortError.failed }
        storyHosts.withLock { $0[story.id] = account }
        return story
    }

    /// The author hosting `storyID`'s reactions; a story the port has not
    /// listed or posted has no known host.
    private func storyHost(_ storyID: String) throws -> String {
        guard let host = storyHosts.withLock({ $0[storyID] }) else { throw PortError.invalidRequest }
        return host.description
    }

    func fetchStoryReads() async throws -> [String: Date] {
        let reads = try await mappingPortErrors { try await client.fetchStoryReads() }
        return Dictionary(
            reads.map { ($0.storyId, FFIInbound.date(milliseconds: $0.readAtMs)) },
            uniquingKeysWith: max
        )
    }

    func publishStoryReads(_ reads: [String: Date]) async throws {
        let entries = reads.map {
            WaddleStoryRead(storyId: $0.key, readAtMs: Int64(($0.value.timeIntervalSince1970 * 1000).rounded()))
        }
        try await mappingPortErrors { try await client.publishStoryReads(reads: entries) }
    }

    func fetchStoryReactions(of storyID: String) async throws -> StoryReactions {
        let host = try storyHost(storyID)
        return FFIInbound.storyReactions(try await mappingPortErrors {
            try await client.fetchStoryReactions(authorJid: host, storyId: storyID)
        })
    }

    func setStoryReactions(_ emojis: [String], on storyID: String) async throws {
        let host = try storyHost(storyID)
        try await mappingPortErrors {
            try await client.setStoryReactions(authorJid: host, storyId: storyID, emojis: emojis)
        }
    }
}
