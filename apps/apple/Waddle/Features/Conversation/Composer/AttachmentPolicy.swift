import Foundation
import WaddleKit

/// Bytes the composer uploads for one attachment.
struct AttachmentPayload: Sendable {
    let data: Data
    let filename: String
    let mediaType: String
    let width: Int?
    let height: Int?
}

/// Upload limits and the XEP-0447 file metadata an upload produces.
enum AttachmentPolicy {
    static let maxBytes = 25 * 1024 * 1024

    /// Media that renders in place (XEP-0447 `disposition='inline'`).
    static func disposition(forMediaType mediaType: String) -> SharedFile.Disposition {
        let type = mediaType.lowercased()
        if type.hasPrefix("image/") || type.hasPrefix("video/") || type.hasPrefix("audio/") {
            return .inline
        }
        return .attachment
    }

    static func sharedFile(at url: URL, for payload: AttachmentPayload) -> SharedFile {
        SharedFile(
            url: url,
            name: payload.filename,
            mediaType: payload.mediaType,
            size: payload.data.count,
            width: payload.width,
            height: payload.height,
            disposition: disposition(forMediaType: payload.mediaType)
        )
    }

    /// XEP-0363 §5: only Authorization, Cookie and Expires may be copied
    /// from the slot, with newlines stripped from names and values.
    static func uploadHeaders(_ slotHeaders: [String: String]) -> [(name: String, value: String)] {
        let allowed: Set<String> = ["authorization", "cookie", "expires"]
        return slotHeaders
            .map { (name: stripNewlines($0.key), value: stripNewlines($0.value)) }
            .filter { allowed.contains($0.name.lowercased()) }
            .sorted { $0.name.lowercased() < $1.name.lowercased() }
    }

    private static func stripNewlines(_ value: String) -> String {
        value.filter { $0 != "\n" && $0 != "\r" && $0 != "\r\n" }
    }
}
