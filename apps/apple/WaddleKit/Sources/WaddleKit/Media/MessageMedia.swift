import Foundation

/// The media a row may render. Withheld URLs never reach the image or
/// attachment views, including their cache lookups and appearance tasks.
public struct MessageMedia: Sendable {
    public enum InlineBody: Equatable, Sendable {
        case text
        case image(URL)
        case withheld
    }

    public struct Withheld: Sendable {
        public enum Item: Equatable, Sendable {
            case attachment(String)
            case inlineImage(String)
            case linkPreviewImages
        }

        public let host: Host?
        public let items: [Item]
    }

    public let inlineBody: InlineBody
    public let visibleAttachments: [SharedFile]
    public let linkPreviews: [LinkPreview]
    public let stickerPackLocation: StickerPackLocation?
    public let withheld: Withheld?

    public struct Attachment: Identifiable, Sendable {
        public struct ID: Hashable, Sendable {
            let file: SharedFile
            let occurrence: Int
        }

        public let id: ID
        public let file: SharedFile
    }

    /// A row can repeat a file. Keep the two taps distinct while preserving
    /// selection across edits to unrelated attachments.
    public var attachments: [Attachment] {
        var occurrences: [SharedFile: Int] = [:]
        return visibleAttachments.map { file in
            let occurrence = occurrences[file, default: 0]
            occurrences[file] = occurrence + 1
            return Attachment(id: .init(file: file, occurrence: occurrence), file: file)
        }
    }

    public init(item: TimelineItem, decision: RemoteMediaDecision) {
        let inlineImage = Self.inlineImageURL(of: item)
        let attachments = item.tombstone == nil ? item.message.sharedFiles : []
        let previews = item.tombstone == nil ? item.message.linkPreviews : []
        guard case let .withheld(host) = decision else {
            inlineBody = inlineImage.map(InlineBody.image) ?? .text
            visibleAttachments = attachments
            linkPreviews = previews
            stickerPackLocation = Self.stickerPackLocation(of: item)
            withheld = nil
            return
        }

        let hiddenFiles = attachments.filter(Self.gatesAttachment)
        var items = hiddenFiles.map { Withheld.Item.attachment($0.name ?? $0.url.absoluteString) }
        if let inlineImage { items.append(.inlineImage(inlineImage.absoluteString)) }
        if previews.contains(where: { $0.imageURL != nil }) { items.append(.linkPreviewImages) }

        inlineBody = inlineImage == nil ? .text : .withheld
        visibleAttachments = hiddenFiles.isEmpty ? attachments : attachments.filter { !Self.gatesAttachment($0) }
        linkPreviews = previews.map { preview in
            LinkPreview(
                url: preview.url, title: preview.title, summary: preview.summary, imageURL: nil, imageAlt: nil,
                originalURL: preview.originalURL
            )
        }
        stickerPackLocation = nil
        withheld = items.isEmpty ? nil : Withheld(host: host, items: items)
    }

    /// Also used by text summaries that describe an inline image without loading it.
    public static func inlineImageURL(of item: TimelineItem) -> URL? {
        guard item.tombstone == nil, item.message.sharedFiles.isEmpty else { return nil }
        return InlineImageBody.url(in: item.body)
    }

    /// Opening a sticker pack sends a request to its sender's account.
    static func stickerPackLocation(of item: TimelineItem) -> StickerPackLocation? {
        guard item.tombstone == nil, item.message.isSticker,
              let reference = item.message.stickerPack,
              let sender = item.authorJID,
              reference.owner == nil || reference.owner == sender
        else { return nil }
        return StickerPackLocation(owner: sender, node: reference.node, packID: reference.packID)
    }

    // The web withholds inline image/video/audio/PDF files (message-attachments.ts).
    // Apple also renders an image MIME type regardless of disposition.
    static func gatesAttachment(_ file: SharedFile) -> Bool {
        if file.isImage { return true }
        guard file.disposition == .inline else { return false }
        let type = file.mediaType?.lowercased() ?? ""
        return type.hasPrefix("video/") || type.hasPrefix("audio/") || type == "application/pdf"
            || inlineExtensions.contains(file.url.pathExtension.lowercased()) || GifMedia.isGiphyMedia(file.url)
    }

    private static let inlineExtensions = InlineImageBody.imageExtensions.union([
        "avif", "bmp", "svg", "mp4", "webm", "mov", "m4v", "ogv", "mp3", "wav", "ogg", "oga", "m4a", "aac", "flac", "pdf",
    ])
}

public enum MessageContent {
    /// The inline image of a body as a file, so it renders and previews like a shared image.
    public static func inlineImageFile(_ url: URL) -> SharedFile {
        SharedFile(url: url, description: inlineImageNoun(url))
    }

    /// "GIF" or "Image", for VoiceOver and previews.
    public static func inlineImageNoun(_ url: URL) -> String {
        GifMedia.isGIF(mediaType: nil, url: url) ? "GIF" : "Image"
    }
}
