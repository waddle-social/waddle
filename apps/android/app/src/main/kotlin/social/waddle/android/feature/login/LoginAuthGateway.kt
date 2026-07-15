package social.waddle.android.feature.login

import social.waddle.android.client.auth.AuthProvider
import social.waddle.android.client.auth.DevicePollResult
import social.waddle.android.client.auth.DeviceStartResponse
import social.waddle.android.client.auth.WaddleAuthApi
import social.waddle.android.client.auth.WaddleSessionInfo

/** The slice of the REST auth client the login flow uses, as a test seam. */
interface LoginAuthGateway {
    suspend fun providers(): Result<List<AuthProvider>>

    suspend fun startDeviceAuth(providerId: String): Result<DeviceStartResponse>

    suspend fun pollDeviceAuth(deviceCode: String): Result<DevicePollResult>

    suspend fun session(sessionId: String): Result<WaddleSessionInfo?>
}

/** Production gateway delegating to the shared [WaddleAuthApi]. */
class WaddleLoginAuthGateway(private val api: WaddleAuthApi) : LoginAuthGateway {
    override suspend fun providers(): Result<List<AuthProvider>> = api.providers()

    override suspend fun startDeviceAuth(providerId: String): Result<DeviceStartResponse> =
        api.startDeviceAuth(providerId)

    override suspend fun pollDeviceAuth(deviceCode: String): Result<DevicePollResult> =
        api.pollDeviceAuth(deviceCode)

    override suspend fun session(sessionId: String): Result<WaddleSessionInfo?> =
        api.session(sessionId)
}
