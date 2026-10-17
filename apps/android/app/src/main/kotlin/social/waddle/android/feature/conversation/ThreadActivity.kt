package social.waddle.android.feature.conversation

import androidx.compose.runtime.Immutable
import social.waddle.android.client.isImageUrl
import social.waddle.android.client.store.TimelineItem
import social.waddle.android.client.store.authorBareJidOf
import social.waddle.client.ffi.WaddleSharedFile
import java.time.Instant

/** One person who replied in a thread: a face on the reply card. */
@Immutable
data class ThreadParticipant(val name: String, val jid: String?)

/**
 * What the reply card under a root shows: the loaded reply count, up to
 * [MAX_THREAD_FACES] repliers (most recent first), how many more there
 * are, when the last reply came and what it said.
 */
@Immutable
data class ThreadActivity(
    val replyCount: Int,
    val participants: List<ThreadParticipant>,
    /** Distinct repliers beyond [participants]. */
    val moreParticipants: Int,
    /** Wire time of the newest loaded reply. */
    val lastReplyAt: Instant?,
    /** The newest loaded reply, when its author is known. */
    val latestReply: LatestReply? = null,
)

/** The newest loaded reply in a thread, as the reply card quotes it. */
@Immutable
data class LatestReply(val author: String, val snippet: ReplySnippet, val sentAt: Instant)

/** One line standing for a reply: plain text, or a word for what it carries. */
@Immutable
sealed interface ReplySnippet {
    /** Plain text with whitespace collapsed to single spaces; may be empty. */
    data class Text(val text: String) : ReplySnippet

    /** Only images (photos, GIFs, stickers). */
    data object Photo : ReplySnippet

    /** Only files, at least one of them not an image. */
    data object Attachment : ReplySnippet

    /** Retracted or moderated. */
    data object Deleted : ReplySnippet
}

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
    // The last of equal timestamps wins: rows arrive oldest first.
    val newest = members.withIndex().maxWithOrNull(compareBy({ sentAtOf(it.value) }, { it.index }))?.value
    return ThreadActivity(
        replyCount = members.size,
        participants = faces,
        moreParticipants = people.size - faces.size,
        lastReplyAt = newest?.let(::sentAtOf),
        latestReply = newest?.let(::latestReplyOf),
    )
}

private fun latestReplyOf(reply: TimelineItem): LatestReply? {
    val author = authorNameOf(reply, isGroupchatRow(reply)) ?: return null
    return LatestReply(author = author, snippet = replySnippetOf(reply), sentAt = sentAtOf(reply))
}

/**
 * The reply card's line for [item], following what the row itself shows
 * (its content): a retraction is "deleted"; a body that never renders
 * as text (sticker alt-text, a lone image URL, an echoed file URL, blank)
 * stands for its media; anything else is the body as one plain line. The
 * body is plain already: XEP-0394 markup rides beside it and the XEP-0461
 * fallback is stripped when the row is stored.
 */
fun replySnippetOf(item: TimelineItem): ReplySnippet {
    if (item.tombstone != null) return ReplySnippet.Deleted
    val files = item.sharedFiles
    val body = item.body.trim()
    val loneImageUrl = files.isEmpty() && isImageUrl(body)
    val bodyHidden = body.isEmpty() || item.isSticker || loneImageUrl || files.any { it.url == body }
    return when {
        !bodyHidden -> ReplySnippet.Text(collapseWhitespace(body))
        loneImageUrl || (files.isNotEmpty() && files.all(::isImageFile)) -> ReplySnippet.Photo
        files.isNotEmpty() -> ReplySnippet.Attachment
        else -> ReplySnippet.Text(collapseWhitespace(body))
    }
}

private fun isImageFile(file: WaddleSharedFile): Boolean = file.mediaType?.startsWith("image/") == true

private fun collapseWhitespace(text: String): String =
    WHITESPACE.replace(text, " ").trim().take(MAX_SNIPPET_CHARS)

private val WHITESPACE = Regex("\\s+")

/** The card ellipsises to one line; this only bounds the work. */
private const val MAX_SNIPPET_CHARS = 280

/** Faces on a reply card before "+N". */
const val MAX_THREAD_FACES = 3
