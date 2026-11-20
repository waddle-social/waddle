import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
import WaddleKit

enum AttachmentUploadError: Error, Equatable {
    case empty
    case tooLarge
    /// No XEP-0363 slot: offline, or the service refused the request.
    case slotUnavailable
    case rejected(status: Int)
    /// A photo's location data could not be removed, so it was not sent.
    case unsanitizable

    var message: String {
        switch self {
        case .empty: return "The file is empty."
        case .tooLarge: return "Files can be up to 25 MB."
        case .slotUnavailable: return "Couldn't start the upload. Check your connection."
        case let .rejected(status): return "The upload was rejected (\(status))."
        case .unsanitizable: return "Couldn't remove location data from this photo, so it wasn't attached."
        }
    }

    static func message(for error: Error) -> String {
        if let upload = error as? AttachmentUploadError {
            return upload.message
        }
        return "The upload failed. Check your connection."
    }
}

/// XEP-0363 upload: request a slot over XMPP, then HTTP PUT the bytes to
/// it (the PUT is part of XEP-0363), and describe the result as XEP-0447
/// file metadata. A chat attachment is sealed first (XEP-0448), so the
/// upload service stores only ciphertext; the key travels in the message.
@MainActor
struct AttachmentUploader {
    enum Protection {
        /// Sealed with a key of its own: chat attachments.
        case encrypted
        /// Stored as it is, for files a whole community reads.
        case plain
    }

    let session: SessionCoordinator
    let protection: Protection

    func upload(
        _ payload: AttachmentPayload,
        progress: @escaping @Sendable (Double) -> Void
    ) async throws -> SharedFile {
        guard !payload.data.isEmpty else { throw AttachmentUploadError.empty }
        guard payload.data.count <= AttachmentPolicy.maxBytes else { throw AttachmentUploadError.tooLarge }
        // An attachment removed while it waited or was being sealed asks
        // the server for nothing.
        try Task.checkCancellation()
        let sealed = try await seal(payload.data)
        try Task.checkCancellation()
        // What the upload service stores: the file, or ciphertext that
        // does not show its type.
        let stored = sealed.map {
            (data: $0.ciphertext, filename: "\(payload.filename).enc", mediaType: "application/octet-stream")
        } ?? (data: payload.data, filename: payload.filename, mediaType: payload.mediaType)
        guard let slot = await session.uploadSlot(
            filename: stored.filename,
            size: stored.data.count,
            mediaType: stored.mediaType
        ) else { throw AttachmentUploadError.slotUnavailable }
        try Task.checkCancellation()
        let request = Self.putRequest(for: slot, mediaType: stored.mediaType, byteCount: stored.data.count)
        let delegate = UploadProgressDelegate(onProgress: progress)
        let (_, response) = try await URLSession.shared.upload(for: request, from: stored.data, delegate: delegate)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else { throw AttachmentUploadError.rejected(status: status) }
        progress(1)
        return AttachmentPolicy.sharedFile(at: slot.getURL, for: payload, sealed: sealed)
    }

    /// Seals off the main actor; nil for a plain upload.
    private func seal(_ data: Data) async throws -> EncryptedUpload? {
        guard protection == .encrypted else { return nil }
        return try await Task.detached(priority: .userInitiated) {
            try EncryptedUpload(sealing: data)
        }.value
    }

    private static func putRequest(for slot: UploadSlot, mediaType: String, byteCount: Int) -> URLRequest {
        var request = URLRequest(url: slot.putURL)
        request.httpMethod = "PUT"
        for header in AttachmentPolicy.uploadHeaders(slot.headers) {
            request.addValue(header.value, forHTTPHeaderField: header.name)
        }
        request.setValue(mediaType, forHTTPHeaderField: "Content-Type")
        request.setValue(String(byteCount), forHTTPHeaderField: "Content-Length")
        return request
    }
}

/// Reports upload progress as a fraction.
private final class UploadProgressDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    private let onProgress: @Sendable (Double) -> Void

    init(onProgress: @escaping @Sendable (Double) -> Void) {
        self.onProgress = onProgress
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didSendBodyData bytesSent: Int64,
        totalBytesSent: Int64,
        totalBytesExpectedToSend: Int64
    ) {
        guard totalBytesExpectedToSend > 0 else { return }
        onProgress(min(1, Double(totalBytesSent) / Double(totalBytesExpectedToSend)))
    }
}
