import Foundation

/// Classifies the plaintext metadata without inspecting the file.
public enum MessageAttachmentKind: Hashable, Sendable {
    case image, video, audio, pdf, archive, other

    public init(_ file: SharedFile) {
        if let type = file.mediaType?.lowercased(), !type.isEmpty {
            if file.isImage {
                self = .image
            } else if type.hasPrefix("video/") {
                self = .video
            } else if type.hasPrefix("audio/") {
                self = .audio
            } else if type == "application/pdf" {
                self = .pdf
            } else if Self.archiveTypes.contains(type) {
                self = .archive
            } else {
                self = .other
            }
            return
        }

        let nameSuffix = file.name.map { ($0 as NSString).pathExtension.lowercased() } ?? ""
        let suffix = Self.knownExtensions.contains(nameSuffix) ? nameSuffix : file.url.pathExtension.lowercased()
        let extensionlessGiphyImage = nameSuffix.isEmpty && suffix.isEmpty && GifMedia.isGiphyMedia(file.url)
        if Self.imageExtensions.contains(suffix) || extensionlessGiphyImage {
            self = .image
        } else if Self.videoExtensions.contains(suffix) {
            self = .video
        } else if Self.audioExtensions.contains(suffix) {
            self = .audio
        } else if suffix == "pdf" {
            self = .pdf
        } else if Self.archiveExtensions.contains(suffix) {
            self = .archive
        } else {
            self = .other
        }
    }

    /// Explicit downloads stay on tap. Apple also displays declared images with no inline disposition.
    public static func inlineKind(for file: SharedFile) -> Self? {
        if file.isImage { return .image }
        guard file.disposition == .inline else { return nil }
        let kind = Self(file)
        switch kind {
        case .image, .video, .audio, .pdf: return kind
        case .archive, .other: return nil
        }
    }

    // Infer PNG, JPEG (.jpg and .jpeg), GIF, WebP and HEIC for the Apple image views.
    private static let imageExtensions = InlineImageBody.imageExtensions.union(["heic"])
    private static let videoExtensions: Set = ["mp4", "webm", "mov", "m4v", "ogv"]
    private static let audioExtensions: Set = ["mp3", "wav", "ogg", "oga", "m4a", "aac", "flac"]
    private static let archiveExtensions: Set = ["zip", "gz", "tar", "7z", "rar"]
    private static let archiveTypes: Set = ["application/zip", "application/gzip", "application/x-tar", "application/x-7z-compressed"]
    private static let knownExtensions = imageExtensions.union(videoExtensions).union(audioExtensions)
        .union(archiveExtensions).union(["pdf"])
}

extension SharedFile {
    /// Plain inline players accept HTTPS sources, matching the web's remote URL policy.
    public var inlineMediaURL: URL? {
        guard encrypted == nil, url.scheme?.lowercased() == "https", url.host?.isEmpty == false else { return nil }
        return url
    }

    // AVPlayer cannot decode Ogg (.ogg, .oga, .ogv) or WebM (.webm) attachments.
    // Use the file card when either extension or the declared media type identifies these containers.
    var usesUnsupportedPlayerContainer: Bool {
        let extensions: Set = ["ogg", "oga", "ogv", "webm"]
        let nameSuffix = name.map { ($0 as NSString).pathExtension.lowercased() } ?? ""
        let type = mediaType?.split(separator: ";", maxSplits: 1).first?
            .trimmingCharacters(in: .whitespaces).lowercased() ?? ""
        let types: Set = ["audio/ogg", "video/ogg", "application/ogg", "audio/x-ogg", "video/x-ogg",
                          "application/x-ogg", "audio/webm", "video/webm", "application/webm"]
        return extensions.contains(nameSuffix) || extensions.contains(url.pathExtension.lowercased()) || types.contains(type)
    }
}
