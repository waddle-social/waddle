import PhotosUI
import SwiftUI
import UniformTypeIdentifiers
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

    /// Dragged items: a file (from Finder or Files) attaches as that file;
    /// a picture dragged out of Photos, a browser or another app, which
    /// arrives as image data, goes through the photo path like a paste.
    /// Returns whether anything was taken.
    func attachDropped(_ providers: [NSItemProvider]) -> Bool {
        guard !model.isEditing else {
            model.errorMessage = "Attachments can't be added while editing a message."
            return false
        }
        var taken = false
        for provider in providers {
            if provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
                taken = true
                Task {
                    guard let url = await Self.fileURL(from: provider) else { return }
                    attachFiles([url])
                }
            } else if let type = Self.imageType(of: provider) {
                taken = true
                Task {
                    guard let data = await Self.data(of: type, from: provider) else {
                        model.errorMessage = "Couldn't read that image."
                        return
                    }
                    await attachImage(data, mediaType: type.preferredMIMEType, fileExtension: type.preferredFilenameExtension)
                }
            }
        }
        return taken
    }

    /// A GIF first, to keep its animation, then the provider's own image
    /// type.
    private static func imageType(of provider: NSItemProvider) -> UTType? {
        if provider.hasItemConformingToTypeIdentifier(UTType.gif.identifier) { return .gif }
        return provider.registeredTypeIdentifiers.lazy
            .compactMap { UTType($0) }
            .first { $0.conforms(to: .image) }
    }

    // Nonisolated: the providers call back off the main thread.
    private nonisolated static func fileURL(from provider: NSItemProvider) async -> URL? {
        await withCheckedContinuation { continuation in
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                continuation.resume(returning: url.flatMap { $0.isFileURL ? $0 : nil })
            }
        }
    }

    private nonisolated static func data(of type: UTType, from provider: NSItemProvider) async -> Data? {
        await withCheckedContinuation { continuation in
            _ = provider.loadDataRepresentation(forTypeIdentifier: type.identifier) { data, _ in
                continuation.resume(returning: data)
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
