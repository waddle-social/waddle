package social.waddle.android.domain.auth

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
public data class AuthProvider(
    public val id: String,
    public val kind: String,
    @SerialName("display_name") public val displayName: String? = null,
)

@Serializable
public data class AuthSession(
    @SerialName("session_id") public val sessionId: String,
    @SerialName("user_id") public val userId: String,
    public val username: String,
    @SerialName("avatar_url") public val avatarUrl: String? = null,
    @SerialName("xmpp_localpart") public val xmppLocalpart: String,
    public val jid: String,
    @SerialName("xmpp_websocket_url") public val xmppWebsocketUrl: String,
    @SerialName("is_expired") public val isExpired: Boolean = false,
    @SerialName("expires_at") public val expiresAt: String? = null,
)

@Serializable
public data class DeviceFlow(
    @SerialName("device_code") public val deviceCode: String,
    @SerialName("user_code") public val userCode: String,
    @SerialName("verification_uri") public val verificationUri: String? = null,
    @SerialName("verification_uri_complete") public val verificationUriComplete: String? = null,
    public val interval: Int = 5,
    @SerialName("expires_in") public val expiresIn: Int? = null,
)

public sealed interface DevicePollResult {
    public data object Pending : DevicePollResult
    public data class Complete(val sessionId: String) : DevicePollResult
}
