import PhotosUI
import SwiftUI
import WaddleKit

/// Loads picked, dropped and pasted files into upload payloads and adds
/// them to the composer.
@MainActor
struct ComposerAttachmentIntake {
    let model: ComposerModel
    let uploader: AttachmentUploader

    func attachFiles(_ urls: [URL]) {
        let model = self.model
        let uploader = self.uploader
        for url in urls {
            Task {
                do {
                    let payload = try await AttachmentLoader.payload(fromFile: url)
                    model.addAttachment(payload, thumbnail: Self.thumbnail(for: payload), uploader: uploader)
                } catch {
                    model.errorMessage = "\(url.lastPathComponent): \(AttachmentUploadError.message(for: error))"
                }
            }
        }
    }

    func attachPhotos(_ items: [PhotosPickerItem]) {
        for item in items {
            let contentType = item.supportedContentTypes.first
            Task {
                guard let data = try? await item.loadTransferable(type: Data.self) else {
                    model.errorMessage = "Couldn't read that photo."
                    return
                }
                await attachImage(
                    data,
                    mediaType: contentType?.preferredMIMEType,
                    fileExtension: contentType?.preferredFilenameExtension
                )
            }
        }
    }

    func attachPasted(_ contents: [ComposerPasteContent]) {
        for content in contents {
            switch content {
            case let .file(url):
                attachFiles([url])
            case let .image(data, mediaType, fileExtension):
                Task { await attachImage(data, mediaType: mediaType, fileExtension: fileExtension) }
            }
        }
    }

    /// Library and pasted images go through the photo path, which strips
    /// metadata and keeps a GIF's animation.
    private func attachImage(_ data: Data, mediaType: String?, fileExtension: String?) async {
        do {
            let payload = try await AttachmentLoader.payload(fromPhoto: data, mediaType: mediaType, fileExtension: fileExtension)
            model.addAttachment(payload, thumbnail: Self.thumbnail(for: payload), uploader: uploader)
        } catch {
            model.errorMessage = AttachmentUploadError.message(for: error)
        }
    }

    private static func thumbnail(for payload: AttachmentPayload) -> Data? {
        payload.mediaType.hasPrefix("image/") ? AttachmentImageInfo.thumbnail(from: payload.data) : nil
    }
}
