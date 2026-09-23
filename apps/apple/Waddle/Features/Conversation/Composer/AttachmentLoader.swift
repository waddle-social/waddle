import Foundation
#if canImport(UniformTypeIdentifiers)
import UniformTypeIdentifiers
#endif

/// Turns picked files and photos into upload payloads, off the main actor.
enum AttachmentLoader {
    /// Reads a file chosen with the file importer or dropped on the
    /// composer. Rejects files over the upload limit before reading them.
    static func payload(fromFile url: URL) async throws -> AttachmentPayload {
        try await Task.detached(priority: .userInitiated) {
            try Self.readFile(url)
        }.value
    }

    /// Photos arrive as raw library bytes. Every still image is re-encoded
    /// as a JPEG from its pixels alone, dropping all metadata; a GIF keeps
    /// its animation unless it carries a location, in which case it is
    /// flattened too. A photo that cannot be re-encoded is refused, never
    /// sent as is.
    static func payload(fromPhoto data: Data, mediaType: String?, fileExtension: String?) async throws -> AttachmentPayload {
        try await Task.detached(priority: .userInitiated) {
            try Self.photoPayload(data, mediaType: mediaType, fileExtension: fileExtension)
        }.value
    }

    private static func readFile(_ url: URL) throws -> AttachmentPayload {
        #if os(iOS) || os(macOS)
        let scoped = url.startAccessingSecurityScopedResource()
        defer {
            if scoped { url.stopAccessingSecurityScopedResource() }
        }
        #endif
        let size = (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
        guard size <= AttachmentPolicy.maxBytes else { throw AttachmentUploadError.tooLarge }
        // Read fully: a mapped file could fault after scoped access ends.
        let data = try Data(contentsOf: url)
        guard data.count <= AttachmentPolicy.maxBytes else { throw AttachmentUploadError.tooLarge }
        let mediaType = Self.mediaType(forExtension: url.pathExtension)
        let dimensions = mediaType.hasPrefix("image/") ? AttachmentImageInfo.pixelSize(of: data) : nil
        return AttachmentPayload(
            data: data,
            filename: url.lastPathComponent,
            mediaType: mediaType,
            width: dimensions?.width,
            height: dimensions?.height
        )
    }

    private static func photoPayload(_ data: Data, mediaType: String?, fileExtension: String?) throws -> AttachmentPayload {
        let type = mediaType?.lowercased() ?? "application/octet-stream"
        let stamp = Int(Date().timeIntervalSince1970)
        let isImage = type.hasPrefix("image/")
        let mustReencode = isImage && (type != "image/gif" || AttachmentImageInfo.hasLocation(data))
        if mustReencode {
            guard let jpeg = AttachmentImageInfo.sanitizedJPEG(from: data) else { throw AttachmentUploadError.unsanitizable }
            let size = AttachmentImageInfo.pixelSize(of: jpeg)
            return AttachmentPayload(data: jpeg, filename: "Photo-\(stamp).jpg", mediaType: "image/jpeg", width: size?.width, height: size?.height)
        }
        let size = type.hasPrefix("image/") ? AttachmentImageInfo.pixelSize(of: data) : nil
        let name = type.hasPrefix("video/") ? "Video" : "Photo"
        let suffix = fileExtension.map { ".\($0)" } ?? ""
        return AttachmentPayload(data: data, filename: "\(name)-\(stamp)\(suffix)", mediaType: type, width: size?.width, height: size?.height)
    }

    static func mediaType(forExtension fileExtension: String) -> String {
        #if canImport(UniformTypeIdentifiers)
        if let type = UTType(filenameExtension: fileExtension), let mime = type.preferredMIMEType {
            return mime
        }
        #endif
        return "application/octet-stream"
    }
}
