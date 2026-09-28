package social.waddle.android.client

/** `room@muc.example/nick` → `room@muc.example`. */
internal fun bareJid(jid: String): String = jid.substringBefore('/')

/** `room@muc.example/nick` → `nick`; `null` when there is no resource. */
internal fun resourcepart(jid: String): String? =
    jid.substringAfter('/', "").ifEmpty { null }

/**
 * Cache-key form of a JID: resource dropped, localpart and domain
 * lowercased (both compare case-insensitively in XMPP), so case
 * variants of one account share a single entry.
 */
fun normalizedBareJid(jid: String): String = bareJid(jid.trim()).lowercase(java.util.Locale.ROOT)
