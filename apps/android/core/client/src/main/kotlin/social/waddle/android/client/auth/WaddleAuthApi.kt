package social.waddle.android.client.auth

import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.Json
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import java.io.IOException

/**
 * Device-authorization-flow REST client, mirroring the Apple
 * `WaddleAPIClient` endpoint set and response handling. Every call is a
 * suspend function returning `Result` with typed [AuthError] failures.
 */
class WaddleAuthApi(
    baseUrl: String,
    private val client: OkHttpClient,
    private val json: Json = defaultAuthJson,
) {
    private val baseUrl: HttpUrl = baseUrl.trimEnd('/').toHttpUrl()

    /** `GET /api/auth/providers`. */
    suspend fun providers(): Result<List<AuthProvider>> =
        get("api/auth/providers").parse<List<AuthProvider>>()

    /** `POST /api/auth/device/start {provider}`. */
    suspend fun startDeviceAuth(providerId: String): Result<DeviceStartResponse> =
        post("api/auth/device/start", json.encodeToString(DeviceStartRequest(providerId)))
            .parse<DeviceStartResponse>()

    /**
     * `POST /api/auth/device/poll {device_code}`. Mirrors Apple's
     * handling: `status == "complete"` yields the session id, every
     * other status is [DevicePollResult.Pending], and a complete
     * response without a `session_id` is an invalid response.
     */
    suspend fun pollDeviceAuth(deviceCode: String): Result<DevicePollResult> =
        post("api/auth/device/poll", json.encodeToString(DevicePollRequest(deviceCode)))
            .parse<DevicePollResponse>()
            .mapCatching { response ->
                when {
                    response.status != STATUS_COMPLETE -> DevicePollResult.Pending
                    response.sessionId != null -> DevicePollResult.Complete(response.sessionId)
                    else -> throw AuthError.InvalidResponse(null)
                }
            }

    /**
     * `GET /api/auth/session?session_id=…`. 401/404 mean "no session"
     * and resolve to `null` (Apple's `treatStatusesAsNil` behavior).
     */
    suspend fun session(sessionId: String?): Result<WaddleSessionInfo?> {
        val url = baseUrl.newBuilder()
            .addPathSegments("api/auth/session")
            .apply { sessionId?.let { addQueryParameter("session_id", it) } }
            .build()
        return execute(Request.Builder().url(url).build()).mapCatching { response ->
            when (response.code) {
                401, 404 -> null
                else -> decodeBody<WaddleSessionInfo>(requireSuccess(response))
            }
        }
    }

    /**
     * `POST /api/auth/logout {session_id}`. 204 is the success shape;
     * 401/404 mean "nothing to log out" and also succeed (web
     * `server-auth.ts` parity).
     */
    suspend fun logout(sessionId: String?): Result<Unit> {
        val body = sessionId?.let { json.encodeToString(LogoutRequest(it)) } ?: ""
        return post("api/auth/logout", body, successStatuses = setOf(204, 401, 404)).map { }
    }

    private suspend fun get(path: String): Result<HttpResult> =
        execute(Request.Builder().url(baseUrl.newBuilder().addPathSegments(path).build()).build())
            .mapCatching(::requireSuccess)

    private suspend fun post(
        path: String,
        body: String,
        successStatuses: Set<Int> = emptySet(),
    ): Result<HttpResult> {
        val request = Request.Builder()
            .url(baseUrl.newBuilder().addPathSegments(path).build())
            .post(body.toRequestBody(JSON_MEDIA_TYPE))
            .build()
        return execute(request).mapCatching { requireSuccess(it, successStatuses) }
    }

    private suspend fun execute(request: Request): Result<HttpResult> = try {
        client.newCall(request).await().use { response ->
            Result.success(HttpResult(response.code, response.body.string()))
        }
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (io: IOException) {
        Result.failure(AuthError.Network(io))
    }

    /** Non-2xx (and non-allowlisted) statuses become [AuthError.Server]. */
    private fun requireSuccess(result: HttpResult, extraSuccess: Set<Int> = emptySet()): HttpResult {
        if (result.code in 200..299 || result.code in extraSuccess) return result
        val detail = runCatching { json.decodeFromString<ApiErrorResponse>(result.body) }.getOrNull()
        throw AuthError.Server(result.code, detail?.message ?: detail?.error ?: "")
    }

    private inline fun <reified T> decodeBody(result: HttpResult): T = try {
        json.decodeFromString<T>(result.body)
    } catch (parse: Exception) {
        throw AuthError.InvalidResponse(parse)
    }

    private inline fun <reified T> Result<HttpResult>.parse(): Result<T> =
        mapCatching { decodeBody<T>(it) }

    private data class HttpResult(val code: Int, val body: String)

    companion object {
        private const val STATUS_COMPLETE = "complete"
        private val JSON_MEDIA_TYPE = "application/json".toMediaType()

        /**
         * Unknown keys are ignored, and `null` for a field with a
         * default (e.g. `interval`) coerces to the default — the same
         * effective behavior as Apple's `decodeIfPresent ?? 5`.
         */
        val defaultAuthJson: Json = Json {
            ignoreUnknownKeys = true
            coerceInputValues = true
        }
    }
}
