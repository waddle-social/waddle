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

    var message: String {
        switch self {
        case .empty: return "The file is empty."
        case .tooLarge: return "Files can be up to 25 MB."
        case .slotUnavailable: return "Couldn't start the upload. Check your connection."
        case let .rejected(status): return "The upload was rejected (\(status))."
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
/// file metadata.
@MainActor
struct AttachmentUploader {
    let session: SessionCoordinator

    func upload(
        _ payload: AttachmentPayload,
        progress: @escaping @Sendable (Double) -> Void
    ) async throws -> SharedFile {
        guard !payload.data.isEmpty else { throw AttachmentUploadError.empty }
        guard payload.data.count <= AttachmentPolicy.maxBytes else { throw AttachmentUploadError.tooLarge }
        guard let slot = await session.uploadSlot(
            filename: payload.filename,
            size: payload.data.count,
            mediaType: payload.mediaType
        ) else { throw AttachmentUploadError.slotUnavailable }
        try Task.checkCancellation()
        let request = Self.putRequest(for: slot, payload: payload)
        let delegate = UploadProgressDelegate(onProgress: progress)
        let (_, response) = try await URLSession.shared.upload(for: request, from: payload.data, delegate: delegate)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else { throw AttachmentUploadError.rejected(status: status) }
        progress(1)
        return AttachmentPolicy.sharedFile(at: slot.getURL, for: payload)
    }

    private static func putRequest(for slot: UploadSlot, payload: AttachmentPayload) -> URLRequest {
        var request = URLRequest(url: slot.putURL)
        request.httpMethod = "PUT"
        for header in AttachmentPolicy.uploadHeaders(slot.headers) {
            request.addValue(header.value, forHTTPHeaderField: header.name)
        }
        request.setValue(payload.mediaType, forHTTPHeaderField: "Content-Type")
        request.setValue(String(payload.data.count), forHTTPHeaderField: "Content-Length")
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
