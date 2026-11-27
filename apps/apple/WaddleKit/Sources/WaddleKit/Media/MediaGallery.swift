import Foundation

/// A selection in the images of a loaded feed or thread, in display order.
public struct MediaGallery: Equatable, Sendable {
    public enum Direction: Sendable {
        case previous
        case next
    }

    public enum Source: Hashable, Sendable {
        case inline(URL)
        case attachment(MessageMedia.Attachment.ID)
    }

    public struct Image: Identifiable, Hashable, Sendable {
        public struct ID: Hashable, Sendable {
            public let row: String
            public let source: Source

            public init(row: String, source: Source) {
                self.row = row
                self.source = source
            }
        }

        public let id: ID
        public let file: SharedFile

        public var name: String { file.displayName.isEmpty ? "Image" : file.displayName }
        public var accessibilityName: String {
            guard let description = file.description, !description.isEmpty else { return name }
            return description
        }
    }

    public let images: [Image]
    private let index: Int

    public var current: Image { images[index] }
    public var position: Int { index + 1 }
    public var count: Int { images.count }

    public init?(
        items: [TimelineItem], selected: Image.ID, decision: (TimelineItem) -> RemoteMediaDecision
    ) {
        let images = items.flatMap { item in
            Self.images(in: item, media: MessageMedia(item: item, decision: decision(item)))
        }
        guard let index = images.firstIndex(where: { $0.id == selected }) else { return nil }
        self.images = images
        self.index = index
    }

    public func neighbor(_ direction: Direction) -> Image.ID? {
        let next = index + (direction == .next ? 1 : -1)
        return images.indices.contains(next) ? images[next].id : nil
    }

    public static func images(in item: TimelineItem, media: MessageMedia) -> [Image] {
        guard item.tombstone == nil, item.callAnchor == nil else { return [] }
        if case let .image(url) = media.inlineBody {
            return [Image(id: .init(row: item.presentationID, source: .inline(url)), file: MessageContent.inlineImageFile(url))]
        }
        // A sticker attachment opens its pack from the row.
        guard !item.message.isSticker else { return [] }
        return media.attachments.compactMap { attachment in
            guard attachment.presentation == .image else { return nil }
            return Image(id: .init(row: item.presentationID, source: .attachment(attachment.id)), file: attachment.file)
        }
    }
}
