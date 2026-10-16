package social.waddle.android.feature.conversation

import androidx.compose.runtime.Immutable
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.authorBareJidOf
import java.time.Instant

/** One person who replied in a thread: the face on the thread chip. */
@Immutable
data class ThreadParticipant(val name: String, val jid: String?)

/**
 * What the thread chip under a root shows (web `.chat-thread-chip`):
 * the loaded reply count, up to [MAX_THREAD_FACES] repliers (most
 * recent first), how many more there are, and when the last reply came.
 */
@Immutable
data class ThreadActivity(
    val replyCount: Int,
    val participants: List<ThreadParticipant>,
    /** Distinct repliers beyond [participants]. */
    val moreParticipants: Int,
    /** Wire time of the newest loaded reply. */
    val lastReplyAt: Instant?,
)

/**
 * Loaded replies grouped by the thread they carry (keyed by the
 * `<thread/>` id, matched to roots by identity like [replyCountsOf]).
 * Repliers dedupe by real bare JID when known, else by display name.
 */
fun threadActivityOf(items: List<TimelineItem>, selfBareJid: String?): Map<String, ThreadActivity> {
    val replies = LinkedHashMap<String, MutableList<TimelineItem>>()
    items.forEach { item ->
        val thread = item.threadId ?: return@forEach
        if (item.hasCallThread || thread in item.identityIds) return@forEach
        replies.getOrPut(thread) { mutableListOf() } += item
    }
    return replies.mapValues { (_, members) -> activityOf(members, selfBareJid) }
}

private fun activityOf(members: List<TimelineItem>, selfBareJid: String?): ThreadActivity {
    val people = LinkedHashMap<String, ThreadParticipant>()
    // Newest first, so the most recent repliers lead the stack.
    members.asReversed().forEach { reply ->
        val jid = authorBareJidOf(reply, selfBareJid)
        val name = authorNameOf(reply, isGroupchatRow(reply)) ?: return@forEach
        people.putIfAbsent(jid ?: "name:$name", ThreadParticipant(name, jid))
    }
    val faces = people.values.take(MAX_THREAD_FACES)
    return ThreadActivity(
        replyCount = members.size,
        participants = faces,
        moreParticipants = people.size - faces.size,
        lastReplyAt = members.maxOfOrNull { sentAtOf(it) },
    )
}

/** Faces on a thread chip before "+N" (web parity). */
const val MAX_THREAD_FACES = 5
