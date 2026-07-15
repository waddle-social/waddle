package social.waddle.android.client

/** `room@muc.example/nick` → `room@muc.example`. */
internal fun bareJid(jid: String): String = jid.substringBefore('/')

/** `room@muc.example/nick` → `nick`; `null` when there is no resource. */
internal fun resourcepart(jid: String): String? =
    jid.substringAfter('/', "").ifEmpty { null }
