package social.waddle.android.jid

/** `alice@waddle.social/phone` → `alice@waddle.social`. */
fun bareJidOf(jid: String): String = jid.substringBefore('/')

/** `alice@waddle.social/phone` → `alice`. */
fun localpartOf(jid: String): String = bareJidOf(jid).substringBefore('@')

/** `room@muc.waddle.social/nick` → `nick`; `null` without a resource. */
fun resourcepartOf(jid: String): String? =
    jid.substringAfter('/', "").ifEmpty { null }
