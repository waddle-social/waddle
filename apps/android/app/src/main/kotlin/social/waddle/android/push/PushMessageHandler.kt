package social.waddle.android.push

import android.content.res.Resources
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import social.waddle.android.R
import social.waddle.android.client.ConnectionState
import social.waddle.android.client.XmppSessionManager
import social.waddle.android.client.prefs.UserPrefs
import social.waddle.android.service.PushSummary

/**
 * Turns a delivered push into a notification, or into nothing when the
 * live stream has it covered.
 *
 * Pushes are content-free, so the summary names the conversation and why
 * it notified, like the web and iOS banners. Room pushes fire even while
 * connected; when the stream claims to be ready the push is a reason to
 * doubt it (it may be half-open), so the stream is probed and given
 * [liveGraceMillis] to deliver the message live before the summary posts.
 */
class PushMessageHandler(
    private val sessionManager: XmppSessionManager,
    private val userPrefs: UserPrefs,
    private val notifier: PushSummarySink,
    private val isAppVisible: () -> Boolean,
    private val summarize: (PushEnvelope) -> PushSummary,
    private val liveGraceMillis: Long = LIVE_GRACE_MILLIS,
) {
    suspend fun handle(instance: String, content: ByteArray, decrypted: Boolean) {
        // Endpoints are bearer URLs; only what the Push Service encrypted
        // for this install's keys is trusted.
        if (!decrypted) return
        // Another account's leftover registration: never show its traffic.
        if (sessionManager.pushInstance() != instance) return
        val envelope = PushEnvelope.parse(content) ?: return
        if (!userPrefs.notificationsEnabled.first()) return
        // The open app shows the conversation live.
        if (isAppVisible()) return
        if (sessionManager.connectionState.value == ConnectionState.Ready) {
            sessionManager.requestLivenessCheck()
            delay(liveGraceMillis)
            if (notifier.handledLive(envelope.item)) return
        }
        notifier.postPushSummary(summarize(envelope))
    }

    companion object {
        /** How long a ready stream gets to deliver the pushed message itself. */
        const val LIVE_GRACE_MILLIS = 5_000L
    }
}

/** Where push summaries go: the notifier, which also knows what arrived live. */
interface PushSummarySink {
    fun handledLive(item: String): Boolean

    suspend fun postPushSummary(summary: PushSummary)
}

/**
 * The banner for a content-free push: the conversation's cached name, and
 * "Mentioned you" or the unread count, like the web and iOS banners.
 */
fun pushSummaryOf(
    envelope: PushEnvelope,
    resources: Resources,
    titleFor: (conversationJid: String, isGroupchat: Boolean) -> String,
): PushSummary {
    val unread = envelope.unread?.coerceIn(1L, Int.MAX_VALUE.toLong())?.toInt() ?: 1
    val text = if (envelope.isMention) {
        resources.getString(R.string.push_summary_mention)
    } else {
        resources.getQuantityString(R.plurals.push_summary_new_messages, unread, unread)
    }
    return PushSummary(
        conversationJid = envelope.conversation,
        isGroupchat = envelope.isGroupchat,
        isMention = envelope.isMention,
        item = envelope.item,
        title = titleFor(envelope.conversation, envelope.isGroupchat),
        text = text,
    )
}
