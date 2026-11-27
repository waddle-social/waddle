package social.waddle.android.client.server

import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import social.waddle.android.client.auth.AuthError
import social.waddle.android.client.auth.WaddleAuthApi
import social.waddle.android.client.auth.await
import java.io.IOException
import java.util.concurrent.TimeUnit

/** Why [ServerDiscovery.discover] could not settle on a Waddle server. */
enum class ServerDiscoveryError {
    /** The typed address is not a usable `https` origin. */
    INVALID_ADDRESS,

    /** Cleartext (`http://`) was typed; only `https` is accepted. */
    INSECURE_SCHEME,

    /** DNS, TLS or connection failure: nothing answered. */
    UNREACHABLE,

    /** Something answered, but it is not a Waddle auth service. */
    NOT_A_WADDLE_SERVER,
}

class ServerDiscoveryException(val error: ServerDiscoveryError, cause: Throwable? = null) :
    Exception(error.name, cause)

/**
 * Turns a typed address into the origin that serves the Waddle REST
 * auth API (the `/api/auth` routes), which is also what hands back the XMPP
 * WebSocket URL in the session — so one validated origin drives both
 * auth and the XMPP connection.
 *
 * 1. The typed origin itself: it is accepted when `GET /api/auth/providers`
 *    answers with a provider list.
 * 2. Otherwise XEP-0156 host-meta (`/.well-known/host-meta.json`) on the
 *    typed origin: a `urn:xmpp:alt-connections:websocket` link names the
 *    service host (the `waddle.example` → `xmpp.waddle.example`
 *    delegation), and that origin is probed the same way.
 */
class ServerDiscovery(private val client: OkHttpClient) {
    private val probeClient: OkHttpClient = client.newBuilder()
        .callTimeout(PROBE_TIMEOUT_SECONDS, TimeUnit.SECONDS)
        .build()

    suspend fun discover(input: String): Result<String> {
        val origin = ServerOrigin.normalize(input).getOrElse { failure ->
            val error = when ((failure as? ServerOriginException)?.error) {
                ServerOriginError.INSECURE_SCHEME -> ServerDiscoveryError.INSECURE_SCHEME
                else -> ServerDiscoveryError.INVALID_ADDRESS
            }
            return Result.failure(ServerDiscoveryException(error))
        }
        val direct = probe(origin)
        if (direct.isSuccess) return Result.success(origin)
        val directFailure = direct.exceptionOrNull()
        if ((directFailure as? ServerDiscoveryException)?.error == ServerDiscoveryError.UNREACHABLE) {
            return Result.failure(directFailure)
        }
        val delegated = hostMetaOrigin(origin)
        if (delegated != null && delegated != origin && probe(delegated).isSuccess) {
            return Result.success(delegated)
        }
        return Result.failure(ServerDiscoveryException(ServerDiscoveryError.NOT_A_WADDLE_SERVER, directFailure))
    }

    private suspend fun probe(origin: String): Result<Unit> =
        WaddleAuthApi(origin, probeClient).providers().fold(
            onSuccess = { Result.success(Unit) },
            onFailure = { failure ->
                val error = if (failure is AuthError.Network) {
                    ServerDiscoveryError.UNREACHABLE
                } else {
                    ServerDiscoveryError.NOT_A_WADDLE_SERVER
                }
                Result.failure(ServerDiscoveryException(error, failure))
            },
        )

    /** The origin of the first secure WebSocket link in host-meta, or `null`. */
    private suspend fun hostMetaOrigin(origin: String): String? {
        val request = Request.Builder().url("$origin/.well-known/host-meta.json").build()
        val body = try {
            probeClient.newCall(request).await().use { response ->
                if (response.isSuccessful) response.body.string() else null
            }
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: IOException) {
            null
        } ?: return null
        return websocketOrigin(body)
    }

    companion object {
        private const val PROBE_TIMEOUT_SECONDS = 10L
        private const val WEBSOCKET_REL = "urn:xmpp:alt-connections:websocket"
        private val json = Json { ignoreUnknownKeys = true }

        /**
         * Pure host-meta parsing, split out for tests: the `https`
         * origin of the first `wss://` websocket link, or `null`.
         */
        internal fun websocketOrigin(hostMetaJson: String): String? {
            val links = runCatching {
                (json.parseToJsonElement(hostMetaJson).jsonObject["links"] as? JsonArray)?.map { it.jsonObject }
            }.getOrNull() ?: return null
            val href = links.firstNotNullOfOrNull { link -> link.hrefFor(WEBSOCKET_REL) } ?: return null
            if (!href.startsWith("wss://", ignoreCase = true)) return null
            // HttpUrl only parses http(s); the origin is the same host and port.
            val url = ("https://" + href.substring("wss://".length)).toHttpUrlOrNull() ?: return null
            return ServerOrigin.origin(url)
        }

        private fun JsonObject.hrefFor(rel: String): String? {
            val matches = runCatching { this["rel"]?.jsonPrimitive?.content == rel }.getOrDefault(false)
            return if (matches) runCatching { this["href"]?.jsonPrimitive?.content }.getOrNull() else null
        }
    }
}
