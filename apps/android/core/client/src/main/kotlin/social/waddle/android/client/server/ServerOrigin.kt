package social.waddle.android.client.server

import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

/** Why a typed server address was rejected before any network call. */
enum class ServerOriginError {
    EMPTY,
    INVALID,

    /** Cleartext `http` is accepted for loopback only. */
    INSECURE_SCHEME,
}

/**
 * Normalizes what a person types into the "server" field.
 *
 * Accepted: `waddle.example`, `waddle.example:8443`, `https://waddle.example/`.
 * The result is an origin (`https://host[:port]`): paths, queries,
 * fragments and credentials are dropped, because the auth API is always
 * rooted at the origin.
 */
object ServerOrigin {
    fun normalize(input: String): Result<String> {
        val trimmed = input.trim()
        if (trimmed.isEmpty()) return Result.failure(ServerOriginException(ServerOriginError.EMPTY))
        val scheme = trimmed.substringBefore("://", missingDelimiterValue = "")
        if (scheme.isNotEmpty() && !scheme.equals("https", ignoreCase = true) &&
            !scheme.equals("http", ignoreCase = true)
        ) {
            return Result.failure(ServerOriginException(ServerOriginError.INVALID))
        }
        val withScheme = if (scheme.isEmpty()) "https://$trimmed" else trimmed
        val url = withScheme.toHttpUrlOrNull()
            ?: return Result.failure(ServerOriginException(ServerOriginError.INVALID))
        // Cleartext only for loopback (dev server through `adb reverse`);
        // the network security config grants the same exemption.
        if (!url.isHttps && !isLoopback(url.host)) {
            return Result.failure(ServerOriginException(ServerOriginError.INSECURE_SCHEME))
        }
        return Result.success(origin(url))
    }

    private fun isLoopback(host: String): Boolean =
        host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1"

    /** `scheme://host[:port]` with the default port elided. */
    fun origin(url: HttpUrl): String = url.newBuilder()
        .username("")
        .password("")
        .encodedPath("/")
        .query(null)
        .fragment(null)
        .build()
        .toString()
        .trimEnd('/')

    /** The user-facing label for a stored origin: host (and a non-default port). */
    fun display(origin: String): String =
        origin.toHttpUrlOrNull()?.let { url ->
            if (url.port == HttpUrl.defaultPort(url.scheme)) url.host else "${url.host}:${url.port}"
        } ?: origin
}

class ServerOriginException(val error: ServerOriginError) : Exception(error.name)
