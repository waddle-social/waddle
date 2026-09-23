import Foundation
import WaddleKit

/// How a XEP-0447 shared file renders in a row, from its plaintext
/// metadata (for XEP-0448 files, once decrypted).
enum MessageAttachmentKind: Hashable {
    case image
    case video
    case audio
    case pdf
    case archive
    case other

    init(_ file: SharedFile) {
        let type = file.mediaType?.lowercased() ?? ""
        let fileExtension = (file.name.map(Self.fileExtension(of:)) ?? file.url.pathExtension).lowercased()
        if type.hasPrefix("image/") {
            self = .image
        } else if type.hasPrefix("video/") {
            self = .video
        } else if type.hasPrefix("audio/") {
            self = .audio
        } else if type == "application/pdf" || fileExtension == "pdf" {
            self = .pdf
        } else if ["application/zip", "application/gzip", "application/x-tar", "application/x-7z-compressed"].contains(type)
            || ["zip", "gz", "tar", "7z", "rar"].contains(fileExtension) {
            self = .archive
        } else {
            self = .other
        }
    }

    private static func fileExtension(of name: String) -> String {
        guard let dot = name.lastIndex(of: "."), dot != name.startIndex else { return "" }
        return String(name[name.index(after: dot)...])
    }

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
}

/// Which parts of a row's content are shown.
enum MessageContent {
    /// The body, unless it only repeats an attachment URL (attachment-only
    /// sends carry the first file URL as their body for older clients).
    static func visibleBody(of item: TimelineItem) -> String? {
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
