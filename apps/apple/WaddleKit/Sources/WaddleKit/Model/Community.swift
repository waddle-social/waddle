import Foundation

/// What a feed post the server wrote on a member's behalf reports.
public enum FeedSource: Hashable, Sendable {
    case mood
    case activity
    case tune
    case avatar
    case profile
    case eventReply
    /// A kind this client does not know yet.
    case other
}

/// One post in the community feed.
public struct FeedPost: Hashable, Sendable, Identifiable {
    public let id: String
    public let title: String?
    public let body: String
    public let author: BareJID?
    public let publishedAt: Date?
    public let link: URL?
    /// Nil for a post a member wrote.
    public let source: FeedSource?

    public init(
        id: String,
        title: String? = nil,
        body: String,
        author: BareJID? = nil,
        publishedAt: Date? = nil,
        link: URL? = nil,
        source: FeedSource? = nil
    ) {
        self.id = id
        self.title = title
        self.body = body
        self.author = author
        self.publishedAt = publishedAt
        self.link = link
        self.source = source
    }
}

/// A feed post being written, with an optional link.
public struct FeedPostDraft: Hashable, Sendable {
    /// What was typed for a post's link.
    public enum LinkInput: Hashable, Sendable {
        /// Nothing, or only spaces.
        case none
        case web(URL)
        case invalid
    }

    /// Longer addresses are refused, as browsers and servers commonly do.
    public static let linkLimit = 2048

    public let body: String
    public let link: URL?

    /// Trims the text; nil when nothing is left to post, or when `link`
    /// is filled in but is not a web address.
    public init?(body: String, link: String = "") {
        let body = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty else { return nil }
        self.body = body
        switch Self.link(from: link) {
        case .none: self.link = nil
        case let .web(url): self.link = url
        case .invalid: return nil
        }
    }

    /// An http(s) address with a dotted host and no user or password,
    /// `https://` assumed when no scheme is typed. Stricter than a link in
    /// a message: a post is read by the whole community, so
    /// `me@example.com` must not become `https://me@example.com`.
    public static func link(from text: String) -> LinkInput {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return .none }
        guard text.count <= linkLimit,
              let url = ComposerInsertion.linkURL(from: text),
              url.user == nil, url.password == nil,
              let host = url.host(), host.contains("."), !host.hasPrefix("."), !host.hasSuffix(".")
        else { return .invalid }
        return .web(url)
    }
}

/// One story: a picture or video that stays up for a limited time.
public struct Story: Hashable, Sendable, Identifiable {
    public let id: String
    public let body: String?
    public let media: URL
    public let mediaType: String?
    public let author: BareJID?
    public let postedAt: Date?
    /// Nil never expires.
    public let expiresAt: Date?

    public init(
        id: String,
        body: String? = nil,
        media: URL,
        mediaType: String? = nil,
        author: BareJID? = nil,
        postedAt: Date? = nil,
        expiresAt: Date? = nil
    ) {
        self.id = id
        self.body = body
        self.media = media
        self.mediaType = mediaType
        self.author = author
        self.postedAt = postedAt
        self.expiresAt = expiresAt
    }

    /// What a story shows.
    public enum Kind: Hashable, Sendable {
        case picture
        case video
        /// WebM, which browsers record and Apple's players do not decode.
        case webM
    }

    /// By the declared type, or by the file's extension when the author's
    /// client declared none.
    public var kind: Kind {
        if let type = mediaType?.lowercased() {
            if type == "video/webm" { return .webM }
            return type.hasPrefix("video/") ? .video : .picture
        }
        switch media.pathExtension.lowercased() {
        case "webm": return .webM
        case "mp4", "m4v", "mov": return .video
        default: return .picture
        }
    }

    public func isActive(at now: Date) -> Bool {
        expiresAt.map { $0 > now } ?? true
    }
}

/// A story being posted. The media is already uploaded.
public struct StoryDraft: Hashable, Sendable {
    public let body: String?
    public let media: URL
    public let mediaType: String?

    public init(body: String?, media: URL, mediaType: String?) {
        let body = body?.trimmingCharacters(in: .whitespacesAndNewlines)
        self.body = body.flatMap { $0.isEmpty ? nil : $0 }
        self.media = media
        self.mediaType = mediaType
    }
}

/// The reactions on one story.
public struct StoryReactions: Hashable, Sendable {
    public struct Count: Hashable, Sendable {
        public let emoji: String
        public let count: Int

        public init(emoji: String, count: Int) {
            self.emoji = emoji
            self.count = count
        }
    }

    /// The most reactions one member may leave on a story.
    public static let limit = 12
    public static let none = StoryReactions(counts: [], mine: [])

    public let counts: [Count]
    /// The emoji the signed-in user reacted with.
    public let mine: [String]

    public init(counts: [Count], mine: [String]) {
        self.counts = counts
        self.mine = mine
    }

    /// The reactions after the signed-in user adds `emoji`, or takes it
    /// back if it is already theirs. Nil when that would pass the limit.
    public func toggling(_ emoji: String) -> StoryReactions? {
        let wasMine = mine.contains(emoji)
        let mine = wasMine ? mine.filter { $0 != emoji } : mine + [emoji]
        guard mine.count <= Self.limit else { return nil }
        var counts = counts
        if let index = counts.firstIndex(where: { $0.emoji == emoji }) {
            let count = counts[index].count + (wasMine ? -1 : 1)
            if count > 0 {
                counts[index] = Count(emoji: emoji, count: count)
            } else {
                counts.remove(at: index)
            }
        } else if !wasMine {
            counts.append(Count(emoji: emoji, count: 1))
        }
        return StoryReactions(counts: counts, mine: mine)
    }
}

/// Which stories the signed-in user has seen, and when.
public enum StoryReads {
    /// How long a read is remembered. Stories live a day by default, so
    /// two days outlasts them.
    public static let lifetime: TimeInterval = 48 * 60 * 60
    public static let limit = 5000

    /// Drops reads older than `lifetime`, then all but the newest `limit`.
    public static func pruned(_ reads: [String: Date], at now: Date) -> [String: Date] {
        let cutoff = now.addingTimeInterval(-lifetime)
        let fresh = reads.filter { $0.value >= cutoff }
        guard fresh.count > limit else { return fresh }
        let newest = fresh.sorted { ($0.value, $0.key) > ($1.value, $1.key) }.prefix(limit)
        return Dictionary(uniqueKeysWithValues: newest.map { ($0.key, $0.value) })
    }

    /// Both devices' reads; the later time wins for a story both saw.
    public static func merged(_ local: [String: Date], with remote: [String: Date]) -> [String: Date] {
        local.merging(remote) { max($0, $1) }
    }
}
