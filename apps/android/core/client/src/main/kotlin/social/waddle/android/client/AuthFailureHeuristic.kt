package social.waddle.android.client

/**
 * Terminal-vs-recoverable classification of FFI `Error` diagnostics: an
 * auth-shaped description means the session id was rejected and retrying
 * cannot help — force re-login. Everything else is treated as a
 * transport error and retried with backoff.
 */
internal fun isAuthShapedError(description: String): Boolean {
    val normalized = description.lowercase()
    return AUTH_ERROR_MARKERS.any { normalized.contains(it) }
}

private val AUTH_ERROR_MARKERS = listOf(
    "not-authorized",
    "not authorized",
    "unauthorized",
    "credentials",
    "sasl",
    "account-disabled",
    "forbidden",
)
