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
    public let stickers: [Sticker]

    public init(id: String, name: String? = nil, summary: String? = nil, stickers: [Sticker]) {
        self.id = id
        self.name = name
        self.summary = summary
        self.stickers = stickers
    }

    /// The name, or "Stickers" for a pack published without one.
    public var title: String {
        let name = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return name.isEmpty ? "Stickers" : name
    }
}

/// The pack a sent sticker is from (XEP-0449 `<sticker pack='…'/>`), on
/// the sender's own account.
public struct StickerReference: Hashable, Sendable, Codable {
    public let packID: String

    public init(packID: String) {
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
