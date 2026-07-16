package social.waddle.android.feature.call

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import social.waddle.client.ffi.WaddleLiveKitJoin
import java.util.Base64

/**
 * Pre-flight validation of the LiveKit join JWT before the SDK opens
 * its WebSocket (web engine.ts / dm-call-activity.ts parity, extended
 * to cross-check the grant against the join's `room`/`identity`). A
 * mismatched or misconfigured token otherwise fails deep inside
 * `Room.connect` with an opaque rejection; failing fast here turns it
 * into a clear, actionable local error. Pure — no I/O, no SDK types —
 * so it is unit-tested on the JVM.
 */
sealed interface LiveKitGrantCheck {
    data object Ok : LiveKitGrantCheck

    data class Invalid(val defect: LiveKitGrantDefect) : LiveKitGrantCheck
}

enum class LiveKitGrantDefect {
    /** Not a decodable three-segment JWT with a JSON object payload. */
    MALFORMED_TOKEN,

    /** No `video` grant object in the payload. */
    MISSING_GRANT,

    /** `video.roomJoin` is not `true`. */
    JOIN_NOT_GRANTED,

    /** `video.room` is absent or blank. */
    MISSING_ROOM,

    /** `video.room` names a different room than the join credentials. */
    ROOM_MISMATCH,

    /** The token's `sub` identity differs from the join identity. */
    IDENTITY_MISMATCH,
}

fun validateLiveKitGrant(join: WaddleLiveKitJoin): LiveKitGrantCheck {
    val payload = decodeJwtPayload(join.token)
        ?: return LiveKitGrantCheck.Invalid(LiveKitGrantDefect.MALFORMED_TOKEN)
    val grant = payload["video"] as? JsonObject
        ?: return LiveKitGrantCheck.Invalid(LiveKitGrantDefect.MISSING_GRANT)
    if ((grant["roomJoin"] as? JsonPrimitive)?.booleanOrNull != true) {
        return LiveKitGrantCheck.Invalid(LiveKitGrantDefect.JOIN_NOT_GRANTED)
    }
    val room = (grant["room"] as? JsonPrimitive)?.contentOrNull?.trim().orEmpty()
    if (room.isEmpty()) {
        return LiveKitGrantCheck.Invalid(LiveKitGrantDefect.MISSING_ROOM)
    }
    if (room != join.room) {
        return LiveKitGrantCheck.Invalid(LiveKitGrantDefect.ROOM_MISMATCH)
    }
    val subject = (payload["sub"] as? JsonPrimitive)?.contentOrNull
    if (subject != null && subject != join.identity) {
        return LiveKitGrantCheck.Invalid(LiveKitGrantDefect.IDENTITY_MISMATCH)
    }
    return LiveKitGrantCheck.Ok
}

private fun decodeJwtPayload(token: String): JsonObject? {
    val segment = token.split('.').getOrNull(1) ?: return null
    if (segment.isEmpty()) return null
    return try {
        val bytes = Base64.getUrlDecoder().decode(segment)
        Json.parseToJsonElement(bytes.decodeToString()).jsonObject
    } catch (_: IllegalArgumentException) {
        null
    } catch (_: kotlinx.serialization.SerializationException) {
        null
    }
}
