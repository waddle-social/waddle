import Observation
import SwiftUI
import WaddleKit

/// A story being put together: one picture and an optional caption.
@MainActor
@Observable
final class StoryComposer {
    enum Phase: Equatable {
        case editing
        /// Uploading the picture; the fraction sent so far.
        case posting(Double)
    }

    /// Stories are smaller than chat attachments, as on web.
    static let maxBytes = 10 * 1024 * 1024

    private(set) var picture: AttachmentPayload? {
        didSet { preview = picture.flatMap { Image(storyPicture: $0.data) } }
    }
    /// The picture, decoded once.
    private(set) var preview: Image?
    var caption = ""
    private(set) var phase: Phase = .editing
    var errorMessage: String?

    init(picture: AttachmentPayload? = nil, caption: String = "", phase: Phase = .editing, errorMessage: String? = nil) {
        self.picture = picture
        preview = picture.flatMap { Image(storyPicture: $0.data) }
        self.caption = caption
        self.phase = phase
        self.errorMessage = errorMessage
    }

    /// A chosen picture is still being read; posting waits for it.
    private(set) var isPicking = false
    /// Counts choices, so a slow earlier one cannot replace a later one.
    private var choice = 0

    var isPosting: Bool { phase != .editing }
    var canPost: Bool { picture != nil && !isPosting && !isPicking }

    /// Takes a picture from the photo library, with its metadata removed.
    /// `read` fetches its bytes, which can take a while for a picture
    /// kept in iCloud. Only the latest choice is kept.
    func pick(
        mediaType: String?,
        fileExtension: String?,
        reading read: () async -> Data?
    ) async {
        choice += 1
        let choice = choice
        isPicking = true
        errorMessage = nil
        let outcome = await Self.load(mediaType: mediaType, fileExtension: fileExtension, reading: read)
        guard choice == self.choice else { return }
        isPicking = false
        switch outcome {
        case let .success(payload): picture = payload
        case let .failure(message): errorMessage = message.text
        }
    }

    private struct PickFailure: Error {
        let text: String
    }

    private static func load(
        mediaType: String?,
        fileExtension: String?,
        reading read: () async -> Data?
    ) async -> Result<AttachmentPayload, PickFailure> {
        guard let data = await read() else {
            return .failure(PickFailure(text: "Couldn't read that picture. Try another one."))
        }
        do {
            let payload = try await AttachmentLoader.payload(fromPhoto: data, mediaType: mediaType, fileExtension: fileExtension)
            guard payload.data.count <= maxBytes else {
                return .failure(PickFailure(text: "Pictures in a story can be up to 10 MB."))
            }
            return .success(payload)
        } catch {
            return .failure(PickFailure(text: AttachmentUploadError.message(for: error)))
        }
    }

    /// Uploads the picture, then posts the story. True once it is up.
    /// Cancelling the task stops the upload, and nothing is posted.
    func post(using session: SessionCoordinator) async -> Bool {
        guard let picture, !isPosting else { return false }
        phase = .posting(0)
        errorMessage = nil
        defer { phase = .editing }
        do {
            let file = try await AttachmentUploader(session: session).upload(Self.named(picture)) { [weak self] fraction in
                Task { @MainActor in
                    guard let self, self.isPosting else { return }
                    self.phase = .posting(fraction)
                }
            }
            try Task.checkCancellation()
            try await session.postStory(StoryDraft(body: caption, media: file.url, mediaType: picture.mediaType))
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
    /// carry the name the picture had on this device.
    private static func named(_ picture: AttachmentPayload) -> AttachmentPayload {
        let suffix = picture.filename.split(separator: ".").last.map { ".\($0)" } ?? ""
        return AttachmentPayload(
            data: picture.data,
            filename: "story-\(UUID().uuidString.lowercased())\(suffix)",
            mediaType: picture.mediaType,
            width: picture.width,
            height: picture.height
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
