package social.waddle.android.client

import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

/**
 * Link-preview media trust rules, ported from the web's
 * `chat/src/lib/xmpp/link-preview.ts`: preview images load ONLY from
 * the server's own XEP-0363 cache (`link-preview-…` paths on the
 * trusted media origin) — never from a third-party og:image URL, which
 * would leak the reader's IP to arbitrary hosts. Player embeds are
 * origin-allowlisted; card taps are HTTPS-only.
 */

private val LOOPBACK_HOSTS = setOf("localhost", "127.0.0.1", "::1", "[::1]")

/** Web `isLinkPreviewXep0363FilePath`. */
private val PREVIEW_FILE_PATH = Regex(
    "^/api/files/" +
        "[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}" +
        "/link-preview-[a-f0-9]{64}\\.(png|jpg|gif|webp)$",
    RegexOption.IGNORE_CASE,
)

/** Web `isAllowedPlayerEmbedOrigin` allowlist. */
private val PLAYER_EMBED_HOSTS = setOf("www.youtube-nocookie.com", "player.vimeo.com")

private const val HTTPS_PORT = 443

/**
 * Web `isTrustedCachedPreviewImageUrl`: same protocol (HTTPS, or HTTP
 * only between loopback hosts), same host, same effective port as
 * [trustedMediaOrigin], and a server-cached `link-preview-…` XEP-0363
 * path. `false` whenever no trusted origin is configured.
 */
fun isTrustedCachedPreviewImageUrl(value: String, trustedMediaOrigin: String?): Boolean {
    val trusted = trustedMediaOrigin?.toHttpUrlOrNull() ?: return false
    val url = value.toHttpUrlOrNull() ?: return false
    if (url.scheme != trusted.scheme) return false
    if (url.scheme == "http" && !(url.isLoopback() && trusted.isLoopback())) return false
    return url.host == trusted.host &&
        url.port == trusted.port &&
        PREVIEW_FILE_PATH.matches(url.encodedPath)
}

/**
 * Web `isAllowedPlayerEmbedOrigin`: HTTPS on the default port, no
 * credentials, host on the player allowlist.
 */
fun isAllowedPlayerEmbedOrigin(value: String): Boolean {
    val url = value.toHttpUrlOrNull() ?: return false
    return url.scheme == "https" &&
        url.port == HTTPS_PORT &&
        url.username.isEmpty() &&
        url.password.isEmpty() &&
        url.host in PLAYER_EMBED_HOSTS
}

/** Web `linkPreviewHost`: display hostname with a `www.` prefix dropped. */
fun linkPreviewHost(url: String): String? =
    url.toHttpUrlOrNull()?.host?.removePrefix("www.")

/** Web `linkPreviewHref`: card taps open HTTPS URLs only. */
fun httpsHrefOrNull(url: String): String? =
    url.toHttpUrlOrNull()?.takeIf { it.scheme == "https" }?.toString()

private fun HttpUrl.isLoopback(): Boolean = host in LOOPBACK_HOSTS
