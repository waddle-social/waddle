import Foundation
import WaddleKit

/// The community feed (XEP-0472) and stories (XEP-0501) on
/// `community.<domain>`, and the account's private "seen" state.
extension FFIXmppPort {
    private func communityService() throws -> String {
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

    func fetchStories(max: Int?) async throws -> [Story] {
        let service = try communityService()
        let stories = try await mappingPortErrors {
            try await client.fetchStories(communityJid: service, maxItems: max.flatMap(UInt32.init(exactly:)))
        }
        return stories.compactMap(FFIInbound.story)
    }

    func publishStory(_ draft: StoryDraft) async throws -> Story {
        let service = try communityService()
        let post = WaddleStoryPost(
            body: draft.body,
            mediaUrl: draft.media.absoluteString,
            mediaType: draft.mediaType,
            expiryHours: nil
        )
        let published = try await mappingPortErrors {
            try await client.publishStory(communityJid: service, post: post)
        }
        guard let story = FFIInbound.story(published) else { throw PortError.failed }
        return story
    }

    func fetchStoryReads() async throws -> [String: Date] {
        let reads = try await mappingPortErrors { try await client.fetchStoryReads() }
        return Dictionary(
            reads.map { ($0.storyId, Date(timeIntervalSince1970: TimeInterval($0.readAtMs) / 1000)) },
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
        let service = try communityService()
        return FFIInbound.storyReactions(try await mappingPortErrors {
            try await client.fetchStoryReactions(communityJid: service, storyId: storyID)
        })
    }

    func setStoryReactions(_ emojis: [String], on storyID: String) async throws {
        let service = try communityService()
        try await mappingPortErrors {
            try await client.setStoryReactions(communityJid: service, storyId: storyID, emojis: emojis)
        }
    }
}
