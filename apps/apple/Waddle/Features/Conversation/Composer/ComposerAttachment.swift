import Foundation
import WaddleKit

/// An attachment waiting in the composer while it uploads.
struct ComposerAttachment: Identifiable, Hashable {
    enum Phase: Hashable {
        case uploading(fraction: Double)
        case uploaded(SharedFile)
        case failed(String)
    }

    let id: UUID
    let filename: String
    let mediaType: String
    let byteCount: Int
    /// Image bytes for the chip thumbnail.
    let thumbnail: Data?
    var phase: Phase

    var sharedFile: SharedFile? {
        if case let .uploaded(file) = phase { return file }
        return nil
    }

    var isFailed: Bool {
        if case .failed = phase { return true }
        return false
    }

    var isImage: Bool { mediaType.lowercased().hasPrefix("image/") }
}
