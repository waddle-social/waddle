import Foundation
import WaddleKit

/// XEP-0363 upload.
extension FFIXmppPort {
    func requestUploadSlot(filename: String, size: Int, mediaType: String) async -> UploadSlot? {
        guard let bytes = UInt64(exactly: size), let service = await uploadServiceJID() else { return nil }
        let slot = await client.requestUploadSlot(
            serviceJid: service.description,
            filename: filename,
            size: bytes,
            contentType: mediaType
        )
        return slot.flatMap(FFIInbound.uploadSlot)
    }

    /// Discovered once per port; a failed discovery is retried next time.
    private func uploadServiceJID() async -> BareJID? {
        if let known = uploadService.withLock({ $0 }) {
            return known
        }
        guard let discovered = FFIInbound.bareJID(await client.discoverUploadService()) else { return nil }
        uploadService.withLock { $0 = discovered }
        return discovered
    }
}
