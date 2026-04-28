package social.waddle.android.domain

import social.waddle.android.ffi.WaddleClientHandle
import uniffi.waddle_xmpp_client.WaddleSharedFile
import uniffi.waddle_xmpp_client.WaddleUploadSlot

public sealed interface UploadResult {
    public data class Ready(val sharedFile: WaddleSharedFile, val slot: WaddleUploadSlot) : UploadResult
    public data class Failed(val reason: String) : UploadResult
}

/**
 * Orchestrates the XEP-0363 upload negotiation. The actual HTTP `PUT` is
 * left to the caller — wiring OkHttp inside the repository would couple
 * `:domain` to a transport. The repository returns the slot + a
 * skeleton `WaddleSharedFile`; the caller PUTs the bytes, then includes
 * the populated `WaddleSharedFile` in the next `send_*_message` call.
 */
public class UploadRepository(private val client: WaddleClientHandle) {
    public suspend fun negotiate(
        filename: String,
        size: ULong,
        contentType: String,
        disposition: String = "attachment",
    ): UploadResult {
        val service = client.discoverUploadService()
            ?: return UploadResult.Failed("server does not advertise an HTTP upload service")
        val slot = client.requestUploadSlot(service, filename, size, contentType)
            ?: return UploadResult.Failed("upload slot request was rejected")
        val sharedFile = WaddleSharedFile(
            url = slot.getUrl,
            name = filename,
            mediaType = contentType,
            size = size,
            width = null,
            height = null,
            disposition = disposition,
        )
        return UploadResult.Ready(sharedFile, slot)
    }
}
