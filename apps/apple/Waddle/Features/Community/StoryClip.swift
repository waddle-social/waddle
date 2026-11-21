import AVFoundation
import CoreTransferable
import UniformTypeIdentifiers

/// A clip picked for a story, made ready to share: put into an MP4 file,
/// which browsers and Android play, without where it was filmed or other
/// personal metadata; a story is read by the whole community.
enum StoryClip {
    struct Prepared: @unchecked Sendable {
        let payload: AttachmentPayload
        /// The first frame, for the preview.
        let poster: CGImage?
    }

    enum Failure: Error {
        case tooLarge
        /// Not a movie, or one that cannot go into an MP4 file as it is.
        case unusable
    }

    /// Prepares the clip at `source` and removes that file. The clip's
    /// streams are copied, not re-encoded.
    static func prepare(_ source: URL, maxBytes: Int) async throws -> Prepared {
        defer { try? FileManager.default.removeItem(at: source) }
        let size = (try? source.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
        guard size <= maxBytes else { throw Failure.tooLarge }

        let output = FileManager.default.temporaryDirectory
            .appendingPathComponent("story-\(UUID().uuidString)")
            .appendingPathExtension("mp4")
        defer { try? FileManager.default.removeItem(at: output) }
        guard let export = AVAssetExportSession(asset: AVURLAsset(url: source), presetName: AVAssetExportPresetPassthrough)
        else { throw Failure.unusable }
        export.outputURL = output
        export.outputFileType = .mp4
        export.metadataItemFilter = .forSharing()
        export.shouldOptimizeForNetworkUse = true
        await export.export()
        guard export.status == .completed else { throw Failure.unusable }

        let data = try Data(contentsOf: output)
        guard data.count <= maxBytes else { throw Failure.tooLarge }
        let frames = AVAssetImageGenerator(asset: AVURLAsset(url: output))
        frames.appliesPreferredTrackTransform = true
        frames.maximumSize = CGSize(width: 1080, height: 1080)
        let poster = try? await frames.image(at: .zero).image
        return Prepared(
            payload: AttachmentPayload(data: data, filename: "story.mp4", mediaType: "video/mp4", width: nil, height: nil),
            poster: poster
        )
    }
}

/// A movie from the photo library, copied to a file of its own.
struct PickedMovie: Transferable {
    let url: URL

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .movie) { received in
            let copy = FileManager.default.temporaryDirectory
                .appendingPathComponent("picked-\(UUID().uuidString)")
                .appendingPathExtension(received.file.pathExtension)
            try FileManager.default.copyItem(at: received.file, to: copy)
            return PickedMovie(url: copy)
        }
    }
}
