import Foundation
import Observation
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

    private(set) var picture: AttachmentPayload?
    var caption = ""
    private(set) var phase: Phase = .editing
    var errorMessage: String?

    init(picture: AttachmentPayload? = nil, caption: String = "", phase: Phase = .editing, errorMessage: String? = nil) {
        self.picture = picture
        self.caption = caption
        self.phase = phase
        self.errorMessage = errorMessage
    }

    var isPosting: Bool { phase != .editing }
    var canPost: Bool { picture != nil && !isPosting }

    /// Takes a picture from the photo library, with its metadata removed.
    func pick(_ data: Data, mediaType: String?, fileExtension: String?) async {
        errorMessage = nil
        do {
            let payload = try await AttachmentLoader.payload(fromPhoto: data, mediaType: mediaType, fileExtension: fileExtension)
            guard payload.data.count <= Self.maxBytes else {
                errorMessage = "Pictures in a story can be up to 10 MB."
                return
            }
            picture = payload
        } catch {
            errorMessage = AttachmentUploadError.message(for: error)
        }
    }

    func removePicture() {
        picture = nil
    }

    /// Uploads the picture, then posts the story. True once it is up.
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
            try await session.postStory(StoryDraft(body: caption, media: file.url, mediaType: picture.mediaType))
            return true
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
