package social.waddle.android.push

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.longOrNull

/**
 * The decrypted Web Push payload the Push Service sends
 * (`waddle-xmpp/src/push/envelope.rs`, schema `v: 1`). It carries no
 * message content: which conversation, why it notified, which archived
 * message ([item], its XEP-0359 stanza-id) and the unread count.
 */
data class PushEnvelope(
    val pushClass: PushClass,
    /** Bare JID of the DM peer or the room. */
    val conversation: String,
    val thread: String?,
    val item: String,
    val unread: Long?,
) {
    /** DMs and DM mentions are one-to-one; everything else is a room. */
    val isGroupchat: Boolean get() = pushClass != PushClass.DM && pushClass != PushClass.DM_MENTION

    val isMention: Boolean get() = pushClass != PushClass.DM && pushClass != PushClass.NOTIFY_ALL

    companion object {
        private const val SCHEMA_VERSION = 1
        private const val MAX_JID_LENGTH = 3071
        private val json = Json { ignoreUnknownKeys = true }

        /**
         * Parse [content]; `null` for anything that is not a well-formed v1
         * envelope naming a plausible bare JID. The JID ends up in intents
         * and notification tags, so it is validated here, once.
         */
        fun parse(content: ByteArray): PushEnvelope? {
            // RFC 8188 padding is stripped by the decrypter; trailing NULs
            // from a lenient one must not break the parse.
            val text = content.decodeToString().trimEnd('\u0000')
            val root = runCatching { json.parseToJsonElement(text) as? JsonObject }
                .getOrNull() ?: return null
            if (root.int("v") != SCHEMA_VERSION) return null
            val pushClass = root.string("class")?.let(PushClass::fromWire) ?: return null
            val conversation = root.string("conversation")?.takeIf(::isPlausibleBareJid) ?: return null
            val item = root.string("item")?.takeIf { it.isNotBlank() } ?: return null
            return PushEnvelope(
                pushClass = pushClass,
                conversation = conversation,
                thread = root.string("thread"),
                item = item,
                unread = (root["unread"] as? JsonPrimitive)?.longOrNull,
            )
        }

        private fun JsonObject.string(key: String): String? =
            (this[key] as? JsonPrimitive)?.takeIf { it.isString }?.content

        private fun JsonObject.int(key: String): Int? = (this[key] as? JsonPrimitive)?.intOrNull

        private fun isPlausibleBareJid(jid: String): Boolean =
            jid.length in 1..MAX_JID_LENGTH &&
                jid.none { it == '/' || it.isWhitespace() || it.isISOControl() } &&
                jid.count { it == '@' } <= 1 &&
                !jid.startsWith('@') &&
                !jid.endsWith('@')
    }
}

/** Why the server pushed (`NotificationClass` on the wire). */
enum class PushClass(val wire: String) {
    DM("dm"),
    DM_MENTION("dm_mention"),
    PERSONAL_MENTION("personal_mention"),
    CHANNEL_MENTION("channel_mention"),
    ACTIVE_CHANNEL_MENTION("active_channel_mention"),
    NOTIFY_ALL("notify_all"),
    ;

    companion object {
        fun fromWire(value: String): PushClass? = entries.firstOrNull { it.wire == value }
    }
}
