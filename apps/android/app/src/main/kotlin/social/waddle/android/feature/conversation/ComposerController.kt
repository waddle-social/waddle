package social.waddle.android.feature.conversation

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.prefs.SharedFileRef
import social.waddle.android.client.store.TimelineItem

/**
 * The composer's mode state machine: normal sends, XEP-0461 replies,
 * and XEP-0308 edits — including the restore-after-failed-correction
 * race and the wire extras each mode produces.
 */
class ComposerController(
    private val conversationJid: String,
    private val isGroupchat: Boolean,
    /**
     * Non-null on a thread screen: every send targets the thread
     * (XEP-0201) and reply extras fall back to it.
     */
    private val screenThreadId: String?,
) {
    private val _mode = MutableStateFlow<ComposerMode>(ComposerMode.Normal)

    /** Normal sends vs replying vs editing an existing own message. */
    val mode: StateFlow<ComposerMode> = _mode

    /** Enter reply mode targeting [item] (XEP-0461). */
    fun startReply(item: TimelineItem) {
        if (item.tombstone != null) return
        val target = actionTargetIdOf(item, isGroupchat, conversationJid) ?: return
        val author = item.from?.let { if (isGroupchat) it else bareJidOf(it) } ?: return
        _mode.value = ComposerMode.Replying(
            targetId = target,
            authorJid = author,
            authorName = authorNameOf(item, isGroupchat) ?: author,
            previewBody = item.body,
            // DM replies root an implicit thread at the parent when none
            // exists (web chat-send parity: every DM reply carries a
            // <thread/>); channels only thread explicit replies.
            threadId = item.threadId
                ?: if (isGroupchat) null else (item.originId ?: item.messageId),
        )
    }

    fun cancelReply() {
        if (_mode.value is ComposerMode.Replying) {
            _mode.value = ComposerMode.Normal
        }
    }

    /** Enter edit mode for an own, non-tombstoned message. */
    fun startEdit(item: TimelineItem) {
        if (!item.isMine || item.tombstone != null) return
        val target = correctionTargetIdOf(item) ?: return
        _mode.value = ComposerMode.Editing(
            targetId = target,
            originalBody = item.body,
            threadId = item.threadId,
        )
    }

    fun cancelEdit() {
        _mode.value = ComposerMode.Normal
    }

    /**
     * A correction failed (corrections are never queued — a replayed
     * edit could outrank a newer one): restore edit mode with the
     * attempted text instead of silently discarding it. CAS: only when
     * the composer is still Normal — the user may have started another
     * edit or a reply while the send was in flight, and clobbering that
     * state would destroy their newer intent.
     */
    fun restoreFailedEdit(mode: ComposerMode.Editing, attemptedBody: String) {
        _mode.compareAndSet(
            ComposerMode.Normal,
            ComposerMode.Editing(
                targetId = mode.targetId,
                originalBody = attemptedBody,
                threadId = mode.threadId,
                attempt = mode.attempt + 1,
            ),
        )
    }

    /**
     * The wire extras of a send dispatched under [mode]. A reply echoes
     * the parent's thread (web parity), falling back to the screen's
     * thread; [files] ride the same stanza — a file sent while replying
     * IS the reply (web parity: files and replyTo share one stanza).
     */
    fun extrasFor(mode: ComposerMode, files: List<SharedFileRef> = emptyList()): MessageSendExtras? =
        when {
            mode is ComposerMode.Replying -> MessageSendExtras(
                replyToId = mode.targetId,
                replyToAuthorJid = mode.authorJid,
                replyParentBody = mode.previewBody,
                threadId = mode.threadId ?: screenThreadId,
                sharedFiles = files,
            )
            files.isNotEmpty() -> MessageSendExtras(threadId = screenThreadId, sharedFiles = files)
            screenThreadId != null -> MessageSendExtras(threadId = screenThreadId)
            else -> null
        }
}
