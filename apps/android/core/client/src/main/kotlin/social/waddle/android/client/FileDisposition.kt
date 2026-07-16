package social.waddle.android.client

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * XEP-0447 file disposition: `inline` renders in the message body,
 * `attachment` is offered as a download. The @SerialName values keep the
 * persisted outbound-queue JSON identical to the previous raw strings.
 */
@Serializable
enum class FileDisposition(val wire: String) {
    @SerialName("inline")
    INLINE("inline"),

    @SerialName("attachment")
    ATTACHMENT("attachment"),
    ;

    companion object {
        /** Web `inferredFileDisposition` parity. */
        fun forMediaType(mediaType: String?): FileDisposition {
            val type = mediaType.orEmpty()
            val inline = type.startsWith("image/") || type.startsWith("video/") ||
                type.startsWith("audio/") || type == "application/pdf"
            return if (inline) INLINE else ATTACHMENT
        }

        /** Wire string → typed; unknown or absent degrades to [ATTACHMENT]. */
        fun fromWire(value: String?): FileDisposition =
            if (value == INLINE.wire) INLINE else ATTACHMENT
    }
}
