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
import androidx.core.net.toUri
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
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
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.auth.WaddleSessionInfo
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.jid.localpartOf
import social.waddle.client.ffi.WaddleMessage
import social.waddle.client.ffi.WaddleNotifyMode
import java.time.Instant
import java.time.OffsetDateTime

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
    /** XEP-0492 effective-mode snapshot (see [NotificationPolicy]). */
    private val notifyModeFor: (conversationJid: String, isGroupchat: Boolean) -> WaddleNotifyMode =
        { _, _ -> WaddleNotifyMode.ALWAYS },
) {
    private val policy = NotificationPolicy(
        userPrefs = userPrefs,
        currentSession = currentSession,
        isAppVisible = isAppVisible,
        notifyModeFor = notifyModeFor,
    )

    private val historyLock = Any()

    /**
     * Serializes append+notify as one unit: the inbound collector and the
     * reply receiver run on different Default-pool coroutines, and two
     * unordered notify() calls for the same tag can leave SystemUI showing
     * the OLDER MessagingStyle snapshot (a reply or newest message
     * silently missing from the shade).
     */
    private val postMutex = Mutex()
    private val history =
        HashMap<NotificationConversationKey, MutableList<NotificationCompat.MessagingStyle.Message>>()

    /**
     * Newest mark-as-read target per conversation. Every re-post
     * (direct-reply append, reply-failed banner) rebuilds the actions
     * with FLAG_UPDATE_CURRENT — reading the target from here keeps the
     * process-death read dispatch intact instead of wiping the extras.
     */
    private val displayedTargets =
        HashMap<NotificationConversationKey, XmppSessionManager.DisplayedTarget>()

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
                    is XmppEvent.DeliveryFailed -> runCatching {
                        val source = event.delivery.source as? DeliverySource.DirectReply
                            ?: return@runCatching
                        val owner = event.delivery.identity.ownerBareJid
                        if (currentOwnerBareJid() != owner) return@runCatching
                        notifyReplyFailed(
                            ownerBareJid = owner,
                            conversationJid = source.conversationJid,
                            isGroupchat = source.isGroupchat,
                        )
                    }
                    // Read on a sibling device (XEP-0490): the shade
                    // notification is stale.
                    is XmppEvent.ReadSynced -> currentOwnerBareJid()?.let { owner ->
                        runCatching {
                            clearConversationNotification(owner, event.conversationJid)
                        }
                    }
                    else -> Unit
                }
            }
        }
    }

    /** Direct-reply echo: append the user's own message and re-post. */
    suspend fun appendOwnReply(
        ownerBareJid: String,
        conversationJid: String,
        isGroupchat: Boolean,
        body: String,
    ) {
        if (!notificationOwnerMatches(currentOwnerBareJid(), ownerBareJid)) return
        postMutex.withLock {
            val key = NotificationConversationKey(ownerBareJid, conversationJid)
            val entry = NotificationCompat.MessagingStyle.Message(
                body,
                System.currentTimeMillis(),
                selfPerson(),
            )
            val messages = appendToHistory(key, entry)
            postNotification(key, isGroupchat, messages, silent = true)
        }
    }

    /**
     * Direct-reply send failed (not connected / transport error): re-post
     * the conversation WITHOUT echoing the reply, audibly, with an
     * explicit failure note so the loss is visible instead of silent.
     */
    suspend fun notifyReplyFailed(
        ownerBareJid: String,
        conversationJid: String,
        isGroupchat: Boolean,
    ) = postMutex.withLock {
        if (!notificationOwnerMatches(currentOwnerBareJid(), ownerBareJid)) return@withLock
        val key = NotificationConversationKey(ownerBareJid, conversationJid)
        // Process death wipes the in-memory history while SystemUI still
        // owns the notification (stuck in the RemoteInput spinner). The
        // failure must be surfaced even then, so seed a minimal entry
        // instead of bailing on an empty cache.
        val messages = synchronized(historyLock) { history[key]?.toList() }
            ?: listOf(
                NotificationCompat.MessagingStyle.Message(
                    context.getString(R.string.notification_reply_failed),
                    System.currentTimeMillis(),
                    selfPerson(),
                ),
            )
        postNotification(key, isGroupchat, messages, silent = false, replyFailed = true)
    }

    /** Sign-out: drop every message notification and the cached history. */
    suspend fun clearAll() {
        // Under the post mutex: a message racing sign-out could otherwise
        // re-post a notification for the just-signed-out account after
        // the cancelAll.
        postMutex.withLock {
            synchronized(historyLock) {
                history.clear()
                displayedTargets.clear()
            }
            NotificationManagerCompat.from(context).cancelAll()
        }
    }

    /** The conversation is being read in-app: retire its notification. */
    suspend fun clearConversationNotification(ownerBareJid: String, conversationJid: String) {
        if (!notificationOwnerMatches(currentOwnerBareJid(), ownerBareJid)) return
        val key = NotificationConversationKey(ownerBareJid, conversationJid)
        postMutex.withLock {
            synchronized(historyLock) {
                history.remove(key)
                displayedTargets.remove(key)
            }
            NotificationManagerCompat.from(context)
                .cancel(key.notificationTag, MESSAGE_NOTIFICATION_ID)
        }
    }

    /** Notification dismissed: forget the conversation's history. */
    fun clearConversation(ownerBareJid: String, conversationJid: String) {
        if (!notificationOwnerMatches(currentOwnerBareJid(), ownerBareJid)) return
        val key = NotificationConversationKey(ownerBareJid, conversationJid)
        synchronized(historyLock) {
            history.remove(key)
            displayedTargets.remove(key)
        }
    }

    private suspend fun onMessage(message: WaddleMessage) {
        val ownerBareJid = currentOwnerBareJid() ?: return
        val decision = policy.decide(message) ?: return
        val key = NotificationConversationKey(ownerBareJid, decision.conversationJid)

        val person = Person.Builder().setName(decision.sender).setKey(decision.sender).build()
        val entry = NotificationCompat.MessagingStyle.Message(
            decision.body,
            timestampMillis(message.timestamp),
            person,
        )
        postMutex.withLock {
            decision.displayedTarget?.let { target ->
                synchronized(historyLock) { displayedTargets[key] = target }
            }
            val messages = appendToHistory(key, entry)
            val silent = !userPrefs.messageSoundsEnabled.first()
            postNotification(
                key,
                decision.isGroupchat,
                messages,
                silent,
                mention = decision.isMention,
            )
        }
    }

    private fun appendToHistory(
        key: NotificationConversationKey,
        entry: NotificationCompat.MessagingStyle.Message,
    ): List<NotificationCompat.MessagingStyle.Message> = synchronized(historyLock) {
        val list = history.getOrPut(key) { mutableListOf() }
        list += entry
        while (list.size > MAX_HISTORY) {
            list.removeAt(0)
        }
        list.toList()
    }

    private fun postNotification(
        key: NotificationConversationKey,
        isGroupchat: Boolean,
        messages: List<NotificationCompat.MessagingStyle.Message>,
        silent: Boolean,
        replyFailed: Boolean = false,
        mention: Boolean = false,
    ) {
        if (!notificationOwnerMatches(currentOwnerBareJid(), key.ownerBareJid)) return
        val displayedTarget = synchronized(historyLock) { displayedTargets[key] }
        val granted = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED
        if (!granted) return

        val style = NotificationCompat.MessagingStyle(selfPerson())
            .setGroupConversation(isGroupchat)
        if (isGroupchat) {
            style.setConversationTitle(localpartOf(key.conversationJid))
        }
        messages.forEach(style::addMessage)

        val builder = NotificationCompat.Builder(context, NotificationChannels.MESSAGES)
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setStyle(style)
            .setGroup(key.notificationGroup)
            .setAutoCancel(true)
            .setSilent(silent)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            // XEP-0372: broadcast/self-mentions outrank plain traffic in
            // the shade (pre-O ranking; O+ ranking is channel-driven).
            .setPriority(
                if (mention) NotificationCompat.PRIORITY_HIGH else NotificationCompat.PRIORITY_DEFAULT,
            )
            .setContentIntent(contentIntent(key))
            .addAction(replyAction(key, isGroupchat))
            .addAction(markReadAction(key, isGroupchat, displayedTarget))
            .setDeleteIntent(dismissIntent(key))
        if (replyFailed) {
            builder.setSubText(context.getString(R.string.notification_reply_failed))
        }
        // The account-scoped tag prevents an old account's notification
        // callback from replacing or mutating the same JID under a newly
        // active account.
        NotificationManagerCompat.from(context)
            .notify(key.notificationTag, MESSAGE_NOTIFICATION_ID, builder.build())
    }

    private fun selfPerson(): Person = Person.Builder()
        .setName(
            currentSession.value?.username
                ?: context.getString(R.string.notification_self_fallback),
        )
        .setKey(SELF_PERSON_KEY)
        .build()

    private fun contentIntent(key: NotificationConversationKey): PendingIntent =
        PendingIntent.getActivity(
            context,
            0,
            Intent(context, MainActivity::class.java)
                .setData(conversationUri(NotificationIntentKind.OPEN, key))
                .putExtra(MainActivity.EXTRA_NAVIGATE_JID, key.conversationJid)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

    /**
     * Per-conversation identity for PendingIntents: `Intent.filterEquals`
     * includes the data URI, so distinct JIDs can never share (and thus
     * FLAG_UPDATE_CURRENT-overwrite) each other's intents — extras alone
     * do not participate in intent identity.
     */
    private fun conversationUri(
        kind: NotificationIntentKind,
        key: NotificationConversationKey,
    ): android.net.Uri =
        notificationIntentIdentity(kind, key).dataUri.toUri()

    private fun replyAction(
        key: NotificationConversationKey,
        isGroupchat: Boolean,
    ): NotificationCompat.Action {
        val remoteInput = RemoteInput.Builder(ReplyReceiver.KEY_REPLY_TEXT)
            .setLabel(context.getString(R.string.notification_reply_label))
            .build()
        val intent = Intent(context, ReplyReceiver::class.java)
            .setAction(ReplyReceiver.ACTION_REPLY)
            .setData(conversationUri(NotificationIntentKind.REPLY, key))
            .putExtra(ReplyReceiver.EXTRA_OWNER_BARE_JID, key.ownerBareJid)
            .putExtra(ReplyReceiver.EXTRA_CONVERSATION_JID, key.conversationJid)
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

    private fun markReadAction(
        key: NotificationConversationKey,
        isGroupchat: Boolean,
        displayedTarget: XmppSessionManager.DisplayedTarget?,
    ): NotificationCompat.Action {
        val intent = Intent(context, MarkReadReceiver::class.java)
            .setAction(MarkReadReceiver.ACTION_MARK_READ)
            .setData(conversationUri(NotificationIntentKind.MARK_READ, key))
            .putExtra(MarkReadReceiver.EXTRA_OWNER_BARE_JID, key.ownerBareJid)
            .putExtra(MarkReadReceiver.EXTRA_CONVERSATION_JID, key.conversationJid)
            .putExtra(MarkReadReceiver.EXTRA_IS_GROUPCHAT, isGroupchat)
        displayedTarget?.let { target ->
            intent
                .putExtra(MarkReadReceiver.EXTRA_MARKER_ID, target.markerId)
                .putExtra(MarkReadReceiver.EXTRA_STANZA_ID, target.stanzaId)
                .putExtra(MarkReadReceiver.EXTRA_STANZA_ID_BY, target.stanzaIdBy)
                .putExtra(MarkReadReceiver.EXTRA_MARKER_REQUESTED, target.markerRequested)
        }
        val pendingIntent = PendingIntent.getBroadcast(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Action.Builder(
            R.drawable.ic_launcher_foreground,
            context.getString(R.string.notification_mark_read_label),
            pendingIntent,
        ).build()
    }

    private fun dismissIntent(key: NotificationConversationKey): PendingIntent =
        PendingIntent.getBroadcast(
            context,
            0,
            Intent(context, NotificationDismissReceiver::class.java)
                .setAction(NotificationDismissReceiver.ACTION_DISMISS)
                .setData(conversationUri(NotificationIntentKind.DISMISS, key))
                .putExtra(NotificationDismissReceiver.EXTRA_OWNER_BARE_JID, key.ownerBareJid)
                .putExtra(
                    NotificationDismissReceiver.EXTRA_CONVERSATION_JID,
                    key.conversationJid,
                ),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

    private fun currentOwnerBareJid(): String? =
        currentSession.value?.jid?.substringBefore('/')?.takeIf(String::isNotBlank)

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
    }
}
