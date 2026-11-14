package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import social.waddle.android.client.auth.await

/**
 * GIF search uses the web client's `/api/giphy` proxy contract. The
 * production proxy is hosted by the chat app; custom deployments use their
 * server URL unless they configure a separate proxy. This HTTP request carries
 * no XMPP semantics or authentication credentials.
 */

/** One GIF search hit (web `GiphyGif` subset). */
data class GifResult(
    val id: String,
    val title: String,
    /** `images.fixed_height_small.url` — the picker grid thumbnail. */
    val previewUrl: String,
    /** `images.original.url` — sent as the message body on select. */
    val originalUrl: String,
)

/** Typed outcome of one search/trending request. */
sealed interface GifSearchResult {
    data class Results(val gifs: List<GifResult>) : GifSearchResult

    /** The origin has no GIF proxy or no API key (web 503 state). */
    data object NotConfigured : GifSearchResult

    /** Proxy rate limit hit (web 429 state). */
    data object RateLimited : GifSearchResult

    /** Upstream failure or malformed payload (web 502 state). */
    data object Unavailable : GifSearchResult
}

/** Transport seam so picker UI and tests never touch the network. */
interface GifSearchGateway {
    /** Empty [query] = trending (web parity). */
    suspend fun search(query: String): GifSearchResult
}

/** Web GifPicker request size. */
const val GIF_SEARCH_LIMIT = 24

/** Web GifPicker debounce. */
const val GIF_SEARCH_DEBOUNCE_MS = 300L

/**
 * Map a proxy response to the web's state machine: 503 → not
 * configured (404 too — a server origin without the route at all),
 * 429 → rate limited, other non-2xx or unparsable → unavailable.
 */
fun classifyGifSearchResponse(statusCode: Int, body: String?): GifSearchResult = when {
    statusCode == STATUS_NOT_CONFIGURED || statusCode == STATUS_NOT_FOUND ->
        GifSearchResult.NotConfigured
    statusCode == STATUS_RATE_LIMITED -> GifSearchResult.RateLimited
    statusCode !in SUCCESS_RANGE -> GifSearchResult.Unavailable
    else -> body?.let(::parseGifSearchPayload) ?: GifSearchResult.Unavailable
}

/** Parse the proxy's `{ data: [...] }` payload; `null` when malformed. */
internal fun parseGifSearchPayload(body: String): GifSearchResult? = runCatching {
    val data = Json.parseToJsonElement(body).jsonObject["data"]?.jsonArray ?: return null
    val gifs = data.mapNotNull { entry ->
        val gif = entry.jsonObject
        val images = gif["images"]?.jsonObject ?: return@mapNotNull null
        GifResult(
            id = gif["id"]?.jsonPrimitive?.content ?: return@mapNotNull null,
            title = gif["title"]?.jsonPrimitive?.content.orEmpty(),
            previewUrl = images["fixed_height_small"]?.jsonObject
                ?.get("url")?.jsonPrimitive?.content ?: return@mapNotNull null,
            originalUrl = images["original"]?.jsonObject
                ?.get("url")?.jsonPrimitive?.content ?: return@mapNotNull null,
        )
    }
    GifSearchResult.Results(gifs)
}.getOrNull()

/** Fetches `GET {proxyBase}/api/giphy?limit=24[&q=…]`. */
class GiphyProxyGateway(
    serverUrl: String,
    private val httpClient: OkHttpClient,
    proxyUrl: String? = null,
) : GifSearchGateway {
    private val baseUrl: HttpUrl? = gifProxyBaseUrl(serverUrl, proxyUrl)

    override suspend fun search(query: String): GifSearchResult {
        val base = baseUrl ?: return GifSearchResult.NotConfigured
        val url = base.newBuilder()
            .addPathSegments("api/giphy")
            .addQueryParameter("limit", GIF_SEARCH_LIMIT.toString())
            .apply { query.trim().takeIf { it.isNotEmpty() }?.let { addQueryParameter("q", it) } }
            .build()
        return try {
            httpClient.newCall(Request.Builder().url(url).build()).await().use { response ->
                classifyGifSearchResponse(response.code, response.body.string())
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Exception) {
            GifSearchResult.Unavailable
        }
    }
}

private fun gifProxyBaseUrl(serverUrl: String, proxyUrl: String?): HttpUrl? {
    val override = proxyUrl?.trim().orEmpty()
    val base = (override.ifEmpty { serverUrl.trim() }).trimEnd('/').toHttpUrlOrNull() ?: return null
    if (base.username.isNotEmpty() || base.password.isNotEmpty() || base.query != null || base.fragment != null) {
        return null
    }
    if (override.isEmpty() && base.scheme == "https" && base.host == "xmpp.waddle.social" &&
        base.port == 443 && base.encodedPath == "/"
    ) {
        return "https://waddle.chat".toHttpUrlOrNull()
    }
    return base
}

private const val STATUS_NOT_CONFIGURED = 503
private const val STATUS_NOT_FOUND = 404
private const val STATUS_RATE_LIMITED = 429
private val SUCCESS_RANGE = 200..299
