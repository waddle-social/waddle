import Observation
import SwiftUI
import WaddleKit

/// A story being put together: one picture or video, and an optional
/// caption.
@MainActor
@Observable
final class StoryComposer {
    enum Phase: Equatable {
        case editing
        /// Uploading the picture or video; the fraction sent so far.
        case posting(Double)
    }

    /// Stories are smaller than chat attachments, as on web.
    static let maxBytes = 10 * 1024 * 1024

    /// The picture or video to upload.
    private(set) var media: AttachmentPayload?
    /// The picture, or the video's first frame, decoded once.
    private(set) var preview: Image?
    var caption = ""
    private(set) var phase: Phase = .editing
    var errorMessage: String?

    /// `preview` stands in for a video's first frame; a picture's is
    /// decoded from it.
    init(
        media: AttachmentPayload? = nil,
        preview: Image? = nil,
        caption: String = "",
        phase: Phase = .editing,
        errorMessage: String? = nil
    ) {
        self.media = media
        self.preview = preview ?? media.flatMap { Image(storyPicture: $0.data) }
        self.caption = caption
        self.phase = phase
        self.errorMessage = errorMessage
    }

    /// A chosen picture or video is still being read; posting waits for it.
    private(set) var isPicking = false
    /// Counts choices, so a slow earlier one cannot replace a later one.
    private var choice = 0

    var isVideo: Bool { media?.mediaType.hasPrefix("video/") == true }
    var isPosting: Bool { phase != .editing }
    var canPost: Bool { media != nil && !isPosting && !isPicking }

    /// Takes a picture from the photo library, with its metadata removed.
    /// `read` fetches its bytes, which can take a while for a picture
    /// kept in iCloud. Only the latest choice is kept.
    func pick(
        mediaType: String?,
        fileExtension: String?,
        reading read: () async -> Data?
    ) async {
        await choose {
            guard let data = await read() else {
                return .failure(PickFailure(text: "Couldn't read that picture. Try another one."))
            }
            do {
                let payload = try await AttachmentLoader.payload(fromPhoto: data, mediaType: mediaType, fileExtension: fileExtension)
                guard payload.data.count <= Self.maxBytes else {
                    return .failure(PickFailure(text: "Pictures in a story can be up to 10 MB."))
                }
                return .success(Chosen(media: payload, preview: Image(storyPicture: payload.data)))
            } catch {
                return .failure(PickFailure(text: AttachmentUploadError.message(for: error)))
            }
        }
    }

    /// Takes a clip from the photo library. `read` copies it to a file,
    /// which this removes. The clip is shared as MP4, without where it was
    /// filmed (see `StoryClip`). Only the latest choice is kept.
    func pickVideo(reading read: () async -> URL?) async {
        await choose {
            guard let source = await read() else {
                return .failure(PickFailure(text: "Couldn't read that video. Try another one."))
            }
            let limit = Self.maxBytes
            do {
                let clip = try await Task.detached(priority: .userInitiated) {
                    try await StoryClip.prepare(source, maxBytes: limit)
                }.value
                return .success(Chosen(media: clip.payload, preview: clip.poster.map { Image(decorative: $0, scale: 1) }))
            } catch StoryClip.Failure.tooLarge {
                return .failure(PickFailure(text: "Videos in a story can be up to 10 MB."))
            } catch {
                return .failure(PickFailure(text: "Couldn't use that video. Try another one."))
            }
        }
    }

    private struct Chosen {
        let media: AttachmentPayload
        let preview: Image?
    }

    private struct PickFailure: Error {
        let text: String
    }

    private func choose(_ load: () async -> Result<Chosen, PickFailure>) async {
        choice += 1
        let choice = choice
        isPicking = true
        errorMessage = nil
        let outcome = await load()
        guard choice == self.choice else { return }
        isPicking = false
        switch outcome {
        case let .success(chosen):
            media = chosen.media
            preview = chosen.preview
        case let .failure(failure):
            errorMessage = failure.text
        }
    }

    /// Uploads the picture or video as it is (the whole community reads
    /// it), then posts the story. True once it is up. Cancelling the task
    /// stops the upload, and nothing is posted.
    func post(using session: SessionCoordinator) async -> Bool {
        guard let media, !isPosting else { return false }
        phase = .posting(0)
        errorMessage = nil
        defer { phase = .editing }
        do {
            let file = try await AttachmentUploader(session: session, protection: .plain).upload(Self.named(media)) { [weak self] fraction in
                Task { @MainActor in
                    guard let self, self.isPosting else { return }
                    self.phase = .posting(fraction)
                }
            }
            try Task.checkCancellation()
            try await session.postStory(StoryDraft(body: caption, media: file.url, mediaType: media.mediaType))
            return true
        } catch where Task.isCancelled {
            return false
        } catch let error as AttachmentUploadError {
            errorMessage = error.message
        } catch {
            errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't post your story. Try again.")
        }
        return false
    }

    /// Everyone in the community can open a story's file, so it does not
    /// carry the name the file had on this device.
    private static func named(_ media: AttachmentPayload) -> AttachmentPayload {
        let suffix = media.filename.split(separator: ".").last.map { ".\($0)" } ?? ""
        return AttachmentPayload(
            data: media.data,
            filename: "story-\(UUID().uuidString.lowercased())\(suffix)",
            mediaType: media.mediaType,
            width: media.width,
            height: media.height
        )
    }
}

private extension Image {
    init?(storyPicture data: Data) {
        #if os(macOS)
        guard let image = NSImage(data: data) else { return nil }
        self.init(nsImage: image)
        #else
        guard let image = UIImage(data: data) else { return nil }
        self.init(uiImage: image)
        #endif
    }
}
