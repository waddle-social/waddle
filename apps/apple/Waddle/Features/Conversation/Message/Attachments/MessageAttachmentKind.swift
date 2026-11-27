import Foundation
import WaddleKit

extension MessageAttachmentKind {
    var symbolName: String {
        switch self {
        case .image: return "photo"
        case .video: return "film"
        case .audio: return "waveform"
        case .pdf: return "doc.richtext"
        case .archive: return "doc.zipper"
        case .other: return "doc"
        }
    }

    /// Short description for VoiceOver and previews.
    var noun: String {
        switch self {
        case .image: return "Image"
        case .video: return "Video"
        case .audio: return "Audio"
        case .pdf: return "PDF"
        case .archive: return "Archive"
        case .other: return "File"
        }
    }

    /// The kind and, when known, the file's formatted size.
    func detail(size: Int?) -> String {
        guard let size else { return noun }
        return "\(noun) · \(ByteCountFormatter.string(fromByteCount: Int64(size), countStyle: .file))"
    }
}

extension MessageContent {
    /// The body, unless it only repeats an attachment URL (attachment-only
    /// sends carry the first file URL as their body for older clients) or
    /// describes a sticker that is shown as its picture.
    static func visibleBody(of item: TimelineItem) -> String? {
        if item.message.isSticker, item.message.sharedFiles.contains(where: \.isImage) {
            return nil
        }
        let trimmed = item.body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if item.message.sharedFiles.contains(where: { $0.url.absoluteString == trimmed }) {
            return nil
        }
        return item.body
    }

    /// Inline image size capped to `maxWidth`, keeping the aspect ratio
    /// when the sender declared dimensions.
    static func mediaSize(width: Int?, height: Int?, maxWidth: Double, maxHeight: Double) -> (width: Double, height: Double)? {
        guard let width, let height, width > 0, height > 0 else { return nil }
        let scale = min(1, maxWidth / Double(width), maxHeight / Double(height))
        return (Double(width) * scale, Double(height) * scale)
    }
}
