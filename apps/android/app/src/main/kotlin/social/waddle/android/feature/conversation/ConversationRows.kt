package social.waddle.android.feature.conversation

import social.waddle.android.client.store.TimelineItem

/**
 * Union of every stored identity: the MUC reflection is keyed by the
 * room-assigned XEP-0359 stanza-id, but the id the send returned is the
 * client origin-id — matching only the collapsed key would leave every
 * sent channel message duplicated (unconfirmed bubble + stored echo).
 */
fun storedIdentityIdsOf(items: List<TimelineItem>): Set<String> {
    val storedIds = HashSet<String>()
    items.forEach { item ->
        storedIds += item.id
        storedIds += item.identityIds
    }
    return storedIds
}

/**
 * The rendered rows: stored history filtered to the feed or to
 * [threadId], followed by the optimistic pending rows that have not
 * echoed back yet.
 */
fun visibleRows(
    items: List<TimelineItem>,
    pending: List<PendingMessage>,
    threadId: String?,
): List<ConversationRow> {
    val storedIds = storedIdentityIdsOf(items)
    val visiblePending = pending.filter { message ->
        message.stanzaId == null || message.stanzaId !in storedIds
    }.filter { message ->
        // Feed mode hides thread-targeted pending rows (their stored
        // echo will be feed-hidden); a thread screen shows only its own
        // thread's sends.
        val pendingThread = message.extras?.threadId
        if (threadId == null) pendingThread == null else pendingThread == threadId
    }
    val visibleItems = if (threadId != null) {
        items.filter { it.threadId == threadId || threadId in it.identityIds }
    } else {
        items.filter { it.isFeedVisible }
    }
    return visibleItems.map { ConversationRow.Stored(it) } +
        visiblePending.map { ConversationRow.Unconfirmed(it) }
}

/**
 * Reply-count chips on feed roots: replies grouped by the thread they
 * carry, matched to the root via its identities.
 */
fun replyCountsOf(items: List<TimelineItem>): Map<String, Int> {
    val byThread = HashMap<String, Int>()
    items.forEach { item ->
        val thread = item.threadId ?: return@forEach
        if (item.hasCallThread) return@forEach
        if (thread !in item.identityIds) byThread.merge(thread, 1, Int::plus)
    }
    return byThread
}

/**
 * Loaded-history threads overview, newest activity first. Purely
 * client-side (the FFI exposes no server thread listing yet): only
 * threads whose messages are in the loaded window appear.
 */
fun threadSummariesOf(
    items: List<TimelineItem>,
    authorNameOf: (TimelineItem) -> String?,
): List<ThreadSummary> {
    val byThread = LinkedHashMap<String, MutableList<TimelineItem>>()
    val lastActivityIndex = HashMap<String, Int>()
    items.forEachIndexed { index, item ->
        val thread = item.threadId ?: return@forEachIndexed
        // Call anchors carry a thread id (the call sid) but are feed
        // rows, not thread members.
        if (item.hasCallThread) return@forEachIndexed
        byThread.getOrPut(thread) { mutableListOf() }.add(item)
        lastActivityIndex[thread] = index
    }
    // Roots carry no <thread/> of their own (Waddle implicit-root):
    // resolve them by identity so previews show the root body.
    return byThread.map { (thread, members) ->
        val root = items.firstOrNull { thread in it.identityIds }
        val previewSource = root ?: members.first()
        ThreadSummary(
            threadId = thread,
            rootAuthor = previewSource.let(authorNameOf),
            // Tombstoned content must never leak through previews
            // (XEP-0424/0425); the UI substitutes the placeholder.
            rootPreview = if (previewSource.tombstone != null) "" else previewSource.body,
            rootTombstoned = previewSource.tombstone != null,
            replyCount = members.count { thread !in it.identityIds },
            lastTimestamp = members.last().timestamp,
        )
        // Newest activity first BY TIMELINE ORDER — live replies carry
        // no timestamp and a string sort would sink them.
    }.sortedByDescending { lastActivityIndex[it.threadId] ?: -1 }
}
