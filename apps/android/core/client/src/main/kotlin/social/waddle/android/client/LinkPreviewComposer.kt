package social.waddle.android.client

import social.waddle.client.ffi.WaddleLinkPreviewLookup
import social.waddle.client.ffi.WaddleLinkPreviewLookupStatus
import java.net.URI
import java.time.OffsetDateTime

/**
 * Composer-side link-preview helpers, ported from the web's
 * `chat/src/lib/xmpp/link-preview.ts` / `link-preview-composer.ts`:
 * the first eligible HTTPS URL of a draft is looked up via
 * `urn:waddle:link-preview:0`, and the minted token is attached to the
 * send ONLY while its `expires-at` instant is still in the future.
 * The lookup must never block a send beyond the web's 2 s budget.
 */

/** Web `LOOKUP_TIMEOUT_MS`: budget for the lookup IQ round-trip. */
const val LINK_PREVIEW_LOOKUP_TIMEOUT_MS = 2_000L

/** Web `HTTPS_URL_RE` (the lookup scanner, distinct from autolinking). */
private val LOOKUP_URL_PATTERN = Regex("""https://[^\s<>"']+""", RegexOption.IGNORE_CASE)

private val LEADING_BRACKETS = Regex("""^[(\[<{]+""")
private val TRAILING_PUNCTUATION = Regex("""[)\]}>.,!?;:]+$""")

/**
 * Web `firstEligibleHttpsUrl`: the first `https://` run whose trimmed
 * form parses with a dotted hostname; `null` when the draft has none.
 */
fun firstEligibleHttpsUrl(body: String): String? {
    for (match in LOOKUP_URL_PATTERN.findAll(body)) {
        val raw = match.value
            .replace(LEADING_BRACKETS, "")
            .replace(TRAILING_PUNCTUATION, "")
        val host = runCatching { URI(raw) }.getOrNull()
            ?.takeIf { it.scheme.equals("https", ignoreCase = true) }
            ?.host
        if (host != null && host.contains('.')) return raw
    }
    return null
}

/**
 * Web `composerLinkPreviewPayloadIsFresh` + `sendPayloadFromState`:
 * the token from a `Ready` lookup, or `null` when the lookup was not
 * ready or its `expires-at` is unparsable or already past — a stale
 * token must never ride a send.
 */
fun linkPreviewSendToken(lookup: WaddleLinkPreviewLookup?, nowMs: Long): String? {
    val preview = lookup?.preview ?: return null
    if (lookup.status != WaddleLinkPreviewLookupStatus.READY) return null
    val expiresAtMs = runCatching {
        OffsetDateTime.parse(preview.expiresAt).toInstant().toEpochMilli()
    }.getOrNull() ?: return null
    return if (expiresAtMs > nowMs) preview.token else null
}
