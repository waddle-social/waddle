import Foundation

/// One XEP-0449 sticker of a pack.
public struct Sticker: Hashable, Sendable {
    /// What the sticker shows, in words or emoji. Sent as the message
    /// text, for clients that do not draw stickers.
    public let description: String
    public let mediaType: String?
    public let size: Int?
    public let width: Int?
    public let height: Int?
    /// Digests of the picture (XEP-0300); a pack names at least one.
    public let digests: FileDigests
    /// Where the picture can be fetched, in the pack's order.
    public let sources: [URL]

    public init(
        description: String,
        mediaType: String? = nil,
        size: Int? = nil,
        width: Int? = nil,
        height: Int? = nil,
        digests: FileDigests = FileDigests(),
        sources: [URL]
    ) {
        self.description = description
        self.mediaType = mediaType
        self.size = size
        self.width = width
        self.height = height
        self.digests = digests
        self.sources = sources
    }

    /// The description, or "Sticker" for one published without any: a
    /// sticker message always carries text.
    public var text: String {
        description.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "Sticker" : description
    }

    /// The picture as the XEP-0447 file a sticker message carries.
    public var file: SharedFile? {
        guard let url = sources.first else { return nil }
        return SharedFile(
            url: url,
            mediaType: mediaType,
            size: size,
            width: width,
            height: height,
            description: text,
            disposition: .inline,
            digests: digests
        )
    }
}

/// A XEP-0449 sticker pack. `id` is derived from its content by the core.
public struct StickerPack: Hashable, Sendable, Identifiable {
    public let id: String
    public let name: String?
    public let summary: String?
    /// The author asked that others neither use nor copy the pack.
    public let isRestricted: Bool
    public let stickers: [Sticker]

    public init(id: String, name: String? = nil, summary: String? = nil, isRestricted: Bool = false, stickers: [Sticker]) {
        self.id = id
        self.name = name
        self.summary = summary
        self.isRestricted = isRestricted
        self.stickers = stickers
    }

    /// The name, or "Stickers" for a pack published without one.
    public var title: String {
        let name = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return name.isEmpty ? "Stickers" : name
    }
}

/// The pack a sticker message names (XEP-0449 `<sticker pack='…'/>`).
/// Without an owner the pack is one of the sender's own; `owner` and
/// `node` say where it is when it is kept somewhere else.
public struct StickerReference: Hashable, Sendable, Codable {
    public let packID: String
    public let owner: BareJID?
    public let node: String?

    public init(packID: String, owner: BareJID? = nil, node: String? = nil) {
        self.packID = packID
        self.owner = owner
        self.node = node
    }
}

/// Where a sticker pack can be fetched: whose it is, and its id. `node`
/// is nil for the usual place, the owner's own sticker packs.
public struct StickerPackLocation: Hashable, Sendable {
    public let owner: BareJID
    public let node: String?
    public let packID: String

    public init(owner: BareJID, node: String? = nil, packID: String) {
        self.owner = owner
        self.node = node
        self.packID = packID
    }
}

/// A sticker pack about to be published; its stickers' pictures are
/// already uploaded.
public struct StickerPackDraft: Hashable, Sendable {
    public var name: String
    public var summary: String?
    public var stickers: [Sticker]

    public init(name: String, summary: String? = nil, stickers: [Sticker]) {
        self.name = name
        self.summary = summary
        self.stickers = stickers
    }

    /// Trimmed, with an empty summary dropped; nil without a name or
    /// without stickers.
    var normalized: StickerPackDraft? {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let summary = summary?.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !stickers.isEmpty else { return nil }
        return StickerPackDraft(name: name, summary: summary?.isEmpty == false ? summary : nil, stickers: stickers)
    }
}
