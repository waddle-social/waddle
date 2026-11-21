import AVFoundation
import CoreTransferable
import UniformTypeIdentifiers

/// A clip picked for a story, made ready to share as an MP4 file that
/// browsers and Android play: H.264 video with AAC sound. Where it was
/// filmed and other personal metadata are left out, since a story is read
/// by the whole community.
enum StoryClip {
    struct Prepared: @unchecked Sendable {
        let payload: AttachmentPayload
        /// The first frame, for the preview; nil when none could be made.
        let poster: CGImage?
    }

    enum Failure: Error {
        /// What would be uploaded is over the cap.
        case tooLarge
        /// Not a movie, or one that cannot go into an MP4 file.
        case unusable
        /// It needed converting to H.264, and that failed.
        case notConverted
    }

    /// How a clip gets into the MP4 file.
    enum Encoding: Equatable {
        /// Its streams are copied as they are.
        case copy
        /// Converted to H.264 and AAC, at most 1280×720.
        case convert
    }

    /// Only H.264 video with AAC sound, or none, is copied: HEVC and other
    /// codecs do not play in every browser or on every Android device.
    static func encoding(video: FourCharCode, audio: [FourCharCode]) -> Encoding {
        video == kCMVideoCodecType_H264 && audio.allSatisfy { $0 == kAudioFormatMPEG4AAC } ? .copy : .convert
    }

    /// Prepares the clip at `source` and removes that file, also when the
    /// task is cancelled (which stops the export and throws
    /// `CancellationError`). `maxBytes` caps the file that would be
    /// uploaded.
    static func prepare(_ source: URL, maxBytes: Int) async throws -> Prepared {
        defer { try? FileManager.default.removeItem(at: source) }
        try Task.checkCancellation()
        let asset = AVURLAsset(url: source)
        guard let video = try? await codecs(of: .video, in: asset).first,
              let audio = try? await codecs(of: .audio, in: asset)
        else { throw Failure.unusable }
        let encoding = encoding(video: video, audio: audio)
        // A copy is as large as its source, so a large one is refused before
        // anything is written.
        if encoding == .copy {
            let size = (try? source.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
            guard size <= maxBytes else { throw Failure.tooLarge }
        }

        let output = FileManager.default.temporaryDirectory
            .appendingPathComponent("story-\(UUID().uuidString)")
            .appendingPathExtension("mp4")
        defer { try? FileManager.default.removeItem(at: output) }
        let preset = encoding == .copy ? AVAssetExportPresetPassthrough : AVAssetExportPreset1280x720
        let failure = encoding == .copy ? Failure.unusable : Failure.notConverted
        guard let export = AVAssetExportSession(asset: asset, presetName: preset) else { throw failure }
        export.outputURL = output
        export.outputFileType = .mp4
        export.metadataItemFilter = .forSharing()
        export.shouldOptimizeForNetworkUse = true
        if encoding == .convert {
            // A conversion that cannot fit stops at the cap instead of
            // writing on; the shorter clip it leaves is refused below.
            export.fileLengthLimit = Int64(maxBytes)
        }
        nonisolated(unsafe) let session = export
        await withTaskCancellationHandler {
            await session.export()
        } onCancel: {
            session.cancelExport()
        }
        try Task.checkCancellation()
        // Measured on disk, so a file far over the cap is never read. An
        // export that ended at the cap is too large, whatever its status.
        let size = (try? output.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
        guard size <= maxBytes else { throw Failure.tooLarge }
        guard export.status == .completed else { throw failure }
        if encoding == .convert, try await isCutShort(output, comparedWith: asset) {
            throw Failure.tooLarge
        }
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

    /// Whether the export stopped before the end of the clip: what
    /// `fileLengthLimit` does when the clip does not fit.
    private static func isCutShort(_ output: URL, comparedWith source: AVURLAsset) async throws -> Bool {
        let whole = try await source.load(.duration).seconds
        let written = try await AVURLAsset(url: output).load(.duration).seconds
        // A few frames of difference is rounding, not a cut, however long
        // the clip is.
        return written < whole - 0.1
    }

    private static func codecs(of type: AVMediaType, in asset: AVURLAsset) async throws -> [FourCharCode] {
        var codecs: [FourCharCode] = []
        for track in try await asset.loadTracks(withMediaType: type) {
            codecs += try await track.load(.formatDescriptions).map(\.mediaSubType.rawValue)
        }
        return codecs
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
