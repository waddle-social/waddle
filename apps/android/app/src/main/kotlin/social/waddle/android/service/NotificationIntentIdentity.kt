package social.waddle.android.service

import java.net.URLEncoder
import java.nio.charset.StandardCharsets

/**
 * Complete account-scoped identity shared by notification tags, groups, and
 * PendingIntent data URIs. Android compares PendingIntent intents by filtered
 * identity, not extras, so the owner must be part of the URI.
 */
internal data class NotificationConversationKey(
    val ownerBareJid: String,
    val conversationJid: String,
) {
    init {
        require(ownerBareJid.isNotBlank()) { "notification owner must not be blank" }
        require(conversationJid.isNotBlank()) { "notification conversation must not be blank" }
    }

    val notificationTag: String
        get() = "$ownerBareJid\u001f$conversationJid"

    val notificationGroup: String
        get() = "waddle:$ownerBareJid"
}

internal enum class NotificationIntentKind(val authority: String) {
    OPEN("open"),
    REPLY("reply"),
    MARK_READ("mark-read"),
    DISMISS("dismiss"),
}

internal data class NotificationIntentIdentity(
    val kind: NotificationIntentKind,
    val key: NotificationConversationKey,
    val dataUri: String,
)

internal fun notificationIntentIdentity(
    kind: NotificationIntentKind,
    key: NotificationConversationKey,
): NotificationIntentIdentity = NotificationIntentIdentity(
    kind = kind,
    key = key,
    dataUri = buildString {
        append("waddle-notify://")
        append(kind.authority)
        append('/')
        append(key.ownerBareJid.encodePathSegment())
        append('/')
        append(key.conversationJid.encodePathSegment())
    },
)

internal fun notificationOwnerMatches(
    currentOwnerBareJid: String?,
    expectedOwnerBareJid: String,
): Boolean = currentOwnerBareJid == expectedOwnerBareJid

private fun String.encodePathSegment(): String =
    URLEncoder.encode(this, StandardCharsets.UTF_8.name())
        .replace("+", "%20")
