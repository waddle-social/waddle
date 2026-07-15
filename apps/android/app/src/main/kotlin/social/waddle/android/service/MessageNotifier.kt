package social.waddle.android.service

import android.Manifest
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.Person
import androidx.core.app.RemoteInput
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
import java.time.Instant
import java.time.OffsetDateTime
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import social.waddle.android.MainActivity
import social.waddle.android.R
import social.waddle.android.client.XmppEvent
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import social.waddle.android.jid.resourcepartOf
import social.waddle.client.ffi.WaddleMessage

/**
 * MessagingStyle notifications for messages arriving while the app is
 * backgrounded: one conversation per notification (grouped by JID),
 * tap navigates via the `waddle.navigate.jid` extra (a direct activity
 * PendingIntent — Android 12+ forbids trampolines), direct reply sends
 * through the session manager, dismiss drops the accumulated history.
 */
class MessageNotifier(
    private val context: Context,
    private val events: SharedFlow<XmppEvent>,
    private val userPrefs: UserPrefs,
    private val currentSession: StateFlow<WaddleSessionInfo?>,
    private val isAppVisible: () -> Boolean = {
        ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)
    },
) {
    private val historyLock = Any()
    private val queuedReplies =
        java.util.concurrent.ConcurrentHashMap<String, Pair<String, Boolean>>()

    /**
     * Serializes append+notify as one unit: the inbound collector and the
     * reply receiver run on different Default-pool coroutines, and two
     * unordered notify() calls for the same tag can leave SystemUI showing
     * the OLDER MessagingStyle snapshot (a reply or newest message
     * silently missing from the shade).
     */
    private val postMutex = Mutex()
    private val history = HashMap<String, MutableList<NotificationCompat.MessagingStyle.Message>>()

    fun start(scope: CoroutineScope) {
        scope.launch {
            events.collect { event ->
                // Per-event guard: notify() can throw at runtime (e.g.
                // a MessagingStyle bundle past the Binder limit, OEM
                // quirks) and an uncaught throw here would both crash
                // the app AND terminate this collector, silently
                // killing all future message notifications.
                when (event) {
                    is XmppEvent.Message -> runCatching { onMessage(event.message) }
                    is XmppEvent.DeliveryAcked -> queuedReplies.remove(event.stanzaId)
                    is XmppEvent.DeliveryFailed -> runCatching { onQueuedReplyFailed(event.stanzaId) }
                    else -> Unit
                }
            }
        }
    }

    /**
     * A direct reply that was offline-queued was already echoed into the
     * shade as delivered; if its replay is later dropped permanently the
     * shade must stop lying. Track the queued id → conversation so the
     * eventual DeliveryFailed can re-post the failure note.
     */
    fun trackQueuedReply(stanzaId: String, conversationJid: String, isGroupchat: Boolean) {
        queuedReplies[stanzaId] = conversationJid to isGroupchat
    }

    private suspend fun onQueuedReplyFailed(stanzaId: String) {
        val (conversationJid, isGroupchat) = queuedReplies.remove(stanzaId) ?: return
        notifyReplyFailed(conversationJid, isGroupchat)
    }

    /** Direct-reply echo: append the user's own message and re-post. */
    suspend fun appendOwnReply(conversationJid: String, isGroupchat: Boolean, body: String) {
        postMutex.withLock {
            val entry = NotificationCompat.MessagingStyle.Message(
                body,
                System.currentTimeMillis(),
                selfPerson(),
            )
            val messages = appendToHistory(conversationJid, entry)
            postNotification(conversationJid, isGroupchat, messages, silent = true)
        }
    }

    /**
     * Direct-reply send failed (not connected / transport error): re-post
     * the conversation WITHOUT echoing the reply, audibly, with an
     * explicit failure note so the loss is visible instead of silent.
     */
    suspend fun notifyReplyFailed(conversationJid: String, isGroupchat: Boolean) = postMutex.withLock {
        // Process death wipes the in-memory history while SystemUI still
        // owns the notification (stuck in the RemoteInput spinner). The
        // failure must be surfaced even then, so seed a minimal entry
        // instead of bailing on an empty cache.
        val messages = synchronized(historyLock) { history[conversationJid]?.toList() }
            ?: listOf(
                NotificationCompat.MessagingStyle.Message(
                    context.getString(R.string.notification_reply_failed),
                    System.currentTimeMillis(),
                    selfPerson(),
                ),
            )
        postNotification(conversationJid, isGroupchat, messages, silent = false, replyFailed = true)
    }

    /** Sign-out: drop every message notification and the cached history. */
    suspend fun clearAll() {
        // Under the post mutex: a message racing sign-out could otherwise
        // re-post a notification for the just-signed-out account after
        // the cancelAll.
        postMutex.withLock {
            synchronized(historyLock) { history.clear() }
            queuedReplies.clear()
            NotificationManagerCompat.from(context).cancelAll()
        }
    }

    /** The conversation is being read in-app: retire its notification. */
    suspend fun clearConversationNotification(conversationJid: String) {
        postMutex.withLock {
            synchronized(historyLock) { history.remove(conversationJid) }
            NotificationManagerCompat.from(context)
                .cancel(conversationJid, MESSAGE_NOTIFICATION_ID)
        }
    }

    /** Notification dismissed: forget the conversation's history. */
    fun clearConversation(conversationJid: String) {
        synchronized(historyLock) { history.remove(conversationJid) }
    }

    private suspend fun onMessage(message: WaddleMessage) {
        val body = message.body ?: return
        if (isAppVisible()) return
        if (!userPrefs.notificationsEnabled.first()) return
        val session = currentSession.value ?: return
        val isGroupchat = message.isMuc || message.messageType == MESSAGE_TYPE_GROUPCHAT
        if (!isGroupchat && message.messageType != MESSAGE_TYPE_CHAT) return
        val from = message.from ?: return
        val conversationJid = bareJidOf(from)
        val sender = if (isGroupchat) resourcepartOf(from) ?: localpartOf(from) else localpartOf(from)
        // Own DM carbons come from the account bare JID; own MUC echoes
        // from room/<own nick> — neither should notify.
        if (!isGroupchat && conversationJid == bareJidOf(session.jid)) return
        if (isGroupchat && sender == session.xmppLocalpart) return

        val person = Person.Builder().setName(sender).setKey(sender).build()
        val entry = NotificationCompat.MessagingStyle.Message(
            body,
            timestampMillis(message.timestamp),
            person,
        )
        postMutex.withLock {
            val messages = appendToHistory(conversationJid, entry)
            val silent = !userPrefs.messageSoundsEnabled.first()
            postNotification(conversationJid, isGroupchat, messages, silent)
        }
    }

    private fun appendToHistory(
        conversationJid: String,
        entry: NotificationCompat.MessagingStyle.Message,
    ): List<NotificationCompat.MessagingStyle.Message> = synchronized(historyLock) {
        val list = history.getOrPut(conversationJid) { mutableListOf() }
        list += entry
        while (list.size > MAX_HISTORY) {
            list.removeAt(0)
        }
        list.toList()
    }

    private fun postNotification(
        conversationJid: String,
        isGroupchat: Boolean,
        messages: List<NotificationCompat.MessagingStyle.Message>,
        silent: Boolean,
        replyFailed: Boolean = false,
    ) {
        val granted = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED
        if (!granted) return

        val style = NotificationCompat.MessagingStyle(selfPerson())
            .setGroupConversation(isGroupchat)
        if (isGroupchat) {
            style.setConversationTitle(localpartOf(conversationJid))
        }
        messages.forEach(style::addMessage)

        val builder = NotificationCompat.Builder(context, NotificationChannels.MESSAGES)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setStyle(style)
            .setGroup(conversationJid)
            .setAutoCancel(true)
            .setSilent(silent)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setContentIntent(contentIntent(conversationJid))
            .addAction(replyAction(conversationJid, isGroupchat))
            .setDeleteIntent(dismissIntent(conversationJid))
        if (replyFailed) {
            builder.setSubText(context.getString(R.string.notification_reply_failed))
        }
        // Tag = JID, fixed id: String.hashCode() collides across arbitrary
        // remote-chosen JIDs (and with the foreground-service id), which
        // would overwrite other conversations' notifications.
        NotificationManagerCompat.from(context)
            .notify(conversationJid, MESSAGE_NOTIFICATION_ID, builder.build())
    }

    private fun selfPerson(): Person = Person.Builder()
        .setName(
            currentSession.value?.username
                ?: context.getString(R.string.notification_self_fallback),
        )
        .setKey(SELF_PERSON_KEY)
        .build()

    private fun contentIntent(conversationJid: String): PendingIntent =
        PendingIntent.getActivity(
            context,
            0,
            Intent(context, MainActivity::class.java)
                .setData(conversationUri("open", conversationJid))
                .putExtra(MainActivity.EXTRA_NAVIGATE_JID, conversationJid)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

    /**
     * Per-conversation identity for PendingIntents: `Intent.filterEquals`
     * includes the data URI, so distinct JIDs can never share (and thus
     * FLAG_UPDATE_CURRENT-overwrite) each other's intents — extras alone
     * do not participate in intent identity.
     */
    private fun conversationUri(kind: String, conversationJid: String): android.net.Uri =
        android.net.Uri.fromParts("waddle-notify", kind, conversationJid)

    private fun replyAction(
        conversationJid: String,
        isGroupchat: Boolean,
    ): NotificationCompat.Action {
        val remoteInput = RemoteInput.Builder(ReplyReceiver.KEY_REPLY_TEXT)
            .setLabel(context.getString(R.string.notification_reply_label))
            .build()
        val intent = Intent(context, ReplyReceiver::class.java)
            .setAction(ReplyReceiver.ACTION_REPLY)
            .setData(conversationUri("reply", conversationJid))
            .putExtra(ReplyReceiver.EXTRA_CONVERSATION_JID, conversationJid)
            .putExtra(ReplyReceiver.EXTRA_IS_GROUPCHAT, isGroupchat)
        // FLAG_MUTABLE: the system fills in the RemoteInput result.
        val pendingIntent = PendingIntent.getBroadcast(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
        )
        return NotificationCompat.Action.Builder(
            R.drawable.ic_launcher_foreground,
            context.getString(R.string.notification_reply_label),
            pendingIntent,
        )
            .addRemoteInput(remoteInput)
            .setAllowGeneratedReplies(false)
            .build()
    }

    private fun dismissIntent(conversationJid: String): PendingIntent =
        PendingIntent.getBroadcast(
            context,
            0,
            Intent(context, NotificationDismissReceiver::class.java)
                .setAction(NotificationDismissReceiver.ACTION_DISMISS)
                .setData(conversationUri("dismiss", conversationJid))
                .putExtra(NotificationDismissReceiver.EXTRA_CONVERSATION_JID, conversationJid),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

    private fun timestampMillis(timestamp: String?): Long {
        timestamp ?: return System.currentTimeMillis()
        return runCatching { Instant.parse(timestamp).toEpochMilli() }.getOrElse {
            runCatching { OffsetDateTime.parse(timestamp).toInstant().toEpochMilli() }
                .getOrDefault(System.currentTimeMillis())
        }
    }

    private companion object {
        /**
         * Fixed id; the conversation identity lives in the notify() tag.
         * Distinct from WaddleConnectionService.NOTIFICATION_ID.
         */
        const val MESSAGE_NOTIFICATION_ID = 2
        const val MAX_HISTORY = 25
        const val SELF_PERSON_KEY = "me"
        const val MESSAGE_TYPE_GROUPCHAT = "groupchat"
        const val MESSAGE_TYPE_CHAT = "chat"
    }
}
