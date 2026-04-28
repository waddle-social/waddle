package social.waddle.android.domain.auth

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import java.io.IOException

public class WaddleApiException(
    public val statusCode: Int,
    public val detail: String,
) : IOException("HTTP $statusCode: $detail")

@Serializable
private data class ProviderStartRequest(val provider: String)

@Serializable
private data class DevicePollRequest(@kotlinx.serialization.SerialName("device_code") val deviceCode: String)

@Serializable
private data class SessionLogoutRequest(@kotlinx.serialization.SerialName("session_id") val sessionId: String)

/**
 * Thin Kotlin port of the Apple `WaddleAPIClient`. Talks the same REST
 * surface against the same Waddle server: provider discovery, RFC 8628
 * device-code OAuth, session retrieval, and logout. The XMPP transport
 * runs separately via the FFI client once a session is in hand.
 */
public class WaddleAuthApi(
    private val client: OkHttpClient = OkHttpClient(),
    private val json: Json = Json { ignoreUnknownKeys = true },
) {
    public suspend fun providers(serverUrl: String): List<AuthProvider> = withContext(Dispatchers.IO) {
        val body = get(serverUrl, "/api/auth/providers")
        json.decodeFromString(body)
    }

    public suspend fun startDeviceAuth(serverUrl: String, providerId: String): DeviceFlow =
        withContext(Dispatchers.IO) {
            val body = postJson(serverUrl, "/api/auth/device/start", ProviderStartRequest(providerId))
            json.decodeFromString(body)
        }

    public suspend fun pollDeviceAuth(serverUrl: String, deviceCode: String): DevicePollResult =
        withContext(Dispatchers.IO) {
            val body = postJson(serverUrl, "/api/auth/device/poll", DevicePollRequest(deviceCode))
            val element = json.parseToJsonElement(body).jsonObject
            when (element["status"]?.jsonPrimitive?.contentOrNull) {
                "complete" -> {
                    val sessionId = element["session_id"]?.jsonPrimitive?.contentOrNull
                        ?: throw WaddleApiException(200, "complete poll response missing session_id")
                    DevicePollResult.Complete(sessionId)
                }
                else -> DevicePollResult.Pending
            }
        }

    /**
     * Returns the current [AuthSession] for [sessionId], or `null` when
     * the server treats it as missing/expired (HTTP 401/404). Other
     * non-2xx responses raise [WaddleApiException].
     */
    public suspend fun session(serverUrl: String, sessionId: String): AuthSession? =
        withContext(Dispatchers.IO) {
            val url = buildUrl(serverUrl, "/api/auth/session")
                .newBuilder()
                .addQueryParameter("session_id", sessionId)
                .build()
            val request = Request.Builder().url(url).get().build()
            client.newCall(request).execute().use { response ->
                when (response.code) {
                    in 200..299 -> json.decodeFromString<AuthSession>(response.body.string())
                    401, 404 -> null
                    else -> throw apiError(response.code, response.body.string())
                }
            }
        }

    public suspend fun logout(serverUrl: String, sessionId: String): Unit = withContext(Dispatchers.IO) {
        postJson(serverUrl, "/api/auth/logout", SessionLogoutRequest(sessionId))
    }

    private fun get(serverUrl: String, path: String): String {
        val request = Request.Builder().url(buildUrl(serverUrl, path)).get().build()
        return client.newCall(request).execute().use { response ->
            if (!response.isSuccessful) throw apiError(response.code, response.body.string())
            response.body.string()
        }
    }

    private inline fun <reified T> postJson(serverUrl: String, path: String, payload: T): String {
        val body: RequestBody = json.encodeToString(payload).toRequestBody(JSON)
        val request = Request.Builder().url(buildUrl(serverUrl, path)).post(body).build()
        return client.newCall(request).execute().use { response ->
            if (!response.isSuccessful) throw apiError(response.code, response.body.string())
            response.body.string()
        }
    }

    private fun apiError(code: Int, body: String): WaddleApiException {
        val detail = runCatching {
            val obj = json.parseToJsonElement(body).jsonObject
            obj.stringField("message") ?: obj.stringField("error") ?: ""
        }.getOrDefault("")
        return WaddleApiException(code, detail)
    }

    private fun JsonElement.stringField(name: String): String? =
        jsonObject[name]?.jsonPrimitive?.contentOrNull

    private fun buildUrl(serverUrl: String, path: String): HttpUrl {
        val base = serverUrl.toHttpUrlOrNull()
            ?: throw WaddleApiException(0, "invalid server URL: $serverUrl")
        val builder = base.newBuilder()
        path.trimStart('/').split('/').forEach { segment ->
            if (segment.isNotEmpty()) builder.addPathSegment(segment)
        }
        return builder.build()
    }

    private companion object {
        val JSON = "application/json".toMediaType()
    }
}
