import Foundation

/// The destinations a row presents, in render order. Each entry records
/// whether it can load with the row or needs its own download tap.
struct MediaURLDigest: Hashable, Sendable {
    private enum Surface: UInt8 {
        case attachment, download, inlineImage, previewImage, previewVideo, previewEmbed
    }

    private let value: Data

    init(_ item: TimelineItem) {
        if item.tombstone == nil {
            self.init(body: item.body, files: item.message.sharedFiles, previews: item.message.linkPreviews)
        } else {
            self.init(body: "", files: [])
        }
    }

    init(body: String, files: [SharedFile], previews: [LinkPreview] = []) {
        var data = Data()
        func append(_ url: URL, as surface: Surface) {
            let bytes = url.absoluteString.utf8
            data.append(surface.rawValue)
            var length = UInt64(bytes.count).bigEndian
            withUnsafeBytes(of: &length) { data.append(contentsOf: $0) }
            data.append(contentsOf: bytes)
        }

        if files.isEmpty, let inlineImage = InlineImageBody.url(in: body) {
            append(inlineImage, as: .inlineImage)
        }
        for file in files {
            let url = file.encrypted == nil ? file.url : file.encrypted?.downloadURL
            if let url {
                append(url, as: MessageMedia.gatesAttachment(file) ? .attachment : .download)
            }
        }
        for preview in previews {
            if let url = preview.imageURL { append(url, as: .previewImage) }
            switch preview.playback {
            case let .video(video): append(video.url, as: .previewVideo)
            case let .embed(embed): append(embed.url, as: .previewEmbed)
            case nil: break
            }
        }
        value = DigestCheck.digest(of: data, using: .sha256)
    }
}
