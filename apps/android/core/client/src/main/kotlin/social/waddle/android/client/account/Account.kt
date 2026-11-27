package social.waddle.android.client.account

import kotlinx.serialization.Serializable
import java.security.MessageDigest

/**
 * One signed-in identity: a (server origin, bare JID) pair. The [key] is
 * the stable handle used in file names, notification tags and intent
 * extras, so nothing outside this class ever derives identity from a JID
 * alone (two servers can hold the same room or user JID text).
 *
 * The bearer session id is deliberately NOT here: the registry holds only
 * non-secret metadata, the credential lives in the account's own store.
 */
@Serializable
data class Account(
    val key: String,
    /** Normalized https origin (loopback may be http). */
    val origin: String,
    val bareJid: String,
    /** Display name for the account tile; editable later, defaults to the origin host. */
    val label: String,
    val addedAtEpochMillis: Long,
    /** The server rejected the stored session: keep the data, ask the user to sign in again. */
    val needsSignIn: Boolean = false,
) {
    companion object {
        fun create(origin: String, bareJid: String, now: Long = System.currentTimeMillis()): Account =
            Account(
                key = accountKey(origin, bareJid),
                origin = origin,
                bareJid = bareJid,
                label = labelOf(origin),
                addedAtEpochMillis = now,
            )

        private fun labelOf(origin: String): String =
            origin.substringAfter("://").substringBefore('/')
    }
}

private const val KEY_HEX_CHARS = 16

/** `hex(sha256(origin + "\n" + bareJid))[0..16]`, shared by every Waddle client. */
fun accountKey(origin: String, bareJid: String): String {
    val digest = MessageDigest.getInstance("SHA-256")
        .digest("$origin\n${bareJid.lowercase()}".toByteArray(Charsets.UTF_8))
    return digest.joinToString("") { "%02x".format(it) }.take(KEY_HEX_CHARS)
}
