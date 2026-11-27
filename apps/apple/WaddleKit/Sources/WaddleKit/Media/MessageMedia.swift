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
            case linkPreviewVideos
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

        /// The card the Apple timeline presents for a visible attachment.
        public enum Presentation: Equatable, Sendable {
            case image, audio, video, pdf, file
            case encrypted(EncryptedFileSource)

            public var hasInlineControls: Bool {
                switch self {
                case .audio, .video, .pdf: true
                default: false
                }
            }
        }

        public var presentation: Presentation {
            if let source = file.encrypted { return .encrypted(source) }
            switch MessageAttachmentKind.inlineKind(for: file) {
            case .image: return .image
            case .audio where file.inlineMediaURL != nil && !file.usesUnsupportedPlayerContainer: return .audio
            case .video where file.inlineMediaURL != nil && !file.usesUnsupportedPlayerContainer: return .video
            case .pdf where file.inlineMediaURL != nil: return .pdf
            default: return .file
            }
        }
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
        let links = MessageLinkPreviews(
            previews: item.tombstone == nil ? item.message.linkPreviews : [],
            files: item.tombstone == nil ? item.message.sharedFiles : []
        )
        let attachments = links.remainingFiles
        let previews = links.previews
        guard case let .withheld(host) = decision else {
            inlineBody = inlineImage.map(InlineBody.image) ?? .text
            visibleAttachments = attachments
            linkPreviews = previews
            stickerPackLocation = Self.stickerPackLocation(of: item)
            withheld = nil
            return
        }

        let hiddenFiles = attachments.filter(Self.gatesAttachment)
        let hiddenImages = hiddenFiles.filter { MessageAttachmentKind.inlineKind(for: $0) == .image }
        var items = hiddenImages.map { Withheld.Item.attachment($0.name ?? $0.url.absoluteString) }
        if let inlineImage { items.append(.inlineImage(inlineImage.absoluteString)) }
        if previews.contains(where: { $0.imageURL != nil }) { items.append(.linkPreviewImages) }
        if previews.contains(where: { $0.playback != nil }) { items.append(.linkPreviewVideos) }

        inlineBody = inlineImage == nil ? .text : .withheld
        visibleAttachments = hiddenFiles.isEmpty ? attachments : attachments.filter { !Self.gatesAttachment($0) }
        linkPreviews = previews.map { $0.withoutMedia() }
        stickerPackLocation = nil
        withheld = hiddenFiles.isEmpty && items.isEmpty ? nil : Withheld(host: host, items: items)
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

    static func gatesAttachment(_ file: SharedFile) -> Bool {
        if MessageAttachmentKind.inlineKind(for: file) != nil { return true }
        // A stream has no player of its own: a link card plays it, so it waits like the card's video.
        guard file.disposition == .inline else { return false }
        return LinkPreviewVideo.isHLSMediaType(file.mediaType?.lowercased() ?? "") || file.url.pathExtension.lowercased() == "m3u8"
    }
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
