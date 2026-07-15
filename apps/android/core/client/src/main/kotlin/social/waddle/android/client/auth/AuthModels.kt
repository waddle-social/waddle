package social.waddle.android.client.auth

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** `GET /api/auth/providers` entry (mirrors `chat/src/lib/server-auth.ts`). */
@Serializable
data class AuthProvider(
    val id: String,
    val kind: String,
    @SerialName("display_name") val displayName: String? = null,
)

/**
 * `POST /api/auth/device/start` response. Field handling mirrors the
 * Apple `DeviceStartResponse` decoder: `interval` defaults to 5 seconds
 * when absent, the verification URIs are optional.
 */
@Serializable
data class DeviceStartResponse(
    @SerialName("device_code") val deviceCode: String,
    @SerialName("user_code") val userCode: String,
    @SerialName("verification_uri") val verificationUri: String? = null,
    @SerialName("verification_uri_complete") val verificationUriComplete: String? = null,
    val interval: Int = 5,
    @SerialName("expires_in") val expiresIn: Int? = null,
)

/** Raw `POST /api/auth/device/poll` body: `{status, session_id?}`. */
@Serializable
internal data class DevicePollResponse(
    val status: String,
    @SerialName("session_id") val sessionId: String? = null,
)

/**
 * Typed poll outcome, mirroring Apple's `DevicePollResult`: any status
 * other than `complete` is still pending; HTTP errors surface as
 * [AuthError] failures instead.
 */
sealed interface DevicePollResult {
    data object Pending : DevicePollResult

    data class Complete(val sessionId: String) : DevicePollResult
}

/** `GET /api/auth/session` response (fields per `chat/src/lib/server-auth.ts`). */
@Serializable
data class WaddleSessionInfo(
    @SerialName("session_id") val sessionId: String,
    val username: String,
    @SerialName("avatar_url") val avatarUrl: String? = null,
    @SerialName("xmpp_localpart") val xmppLocalpart: String,
    val jid: String,
    @SerialName("xmpp_websocket_url") val xmppWebsocketUrl: String,
    @SerialName("link_preview_media_origin") val linkPreviewMediaOrigin: String? = null,
    @SerialName("is_expired") val isExpired: Boolean,
    @SerialName("expires_at") val expiresAt: String? = null,
)

@Serializable
internal data class DeviceStartRequest(val provider: String)

@Serializable
internal data class DevicePollRequest(@SerialName("device_code") val deviceCode: String)

@Serializable
internal data class LogoutRequest(@SerialName("session_id") val sessionId: String)

/** `{message?, error?}` error envelope (mirrors Apple's `APIErrorResponse`). */
@Serializable
internal data class ApiErrorResponse(
    val message: String? = null,
    val error: String? = null,
)
